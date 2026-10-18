// The client against a real server.
//
// Everything else in this directory tests the client's reasoning with the
// server faked. This one runs the same code against `ovsd` itself, because the
// interesting failures are the ones where both halves are individually right
// about a contract they read differently.
//
// It is skipped unless a server is named, so an ordinary `npm test` stays a
// unit run. `tools/client-live.sh` is what names one: it starts an `ovsd` of
// its own, with an empty database in a temporary directory and no renderer, and
// takes it away again afterwards.
//
//   ./tools/client-live.sh
//
// **Point it at a throwaway server, never at one running a show.** These tests
// create a show and move the live master, which is what makes them worth
// having and also what makes them destructive. The script exists so that is
// never a decision anybody has to remember to make.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';
import WebSocket from 'ws';

import type {
  ModuleParameter,
  ParameterType,
  ParameterValue,
  Show,
  Slot,
  SourceRef,
  ValidationError,
} from '@ovs/schema';

import { Session } from '../src/main/session.js';
import type { OpenSocket } from '../src/shared/connection.js';
import type { ConnectionStatus } from '../src/shared/connection.js';
import { files } from '../src/main/files.js';
import type { ShowView } from '../src/shared/view.js';
import {
  addEffect,
  addLayer,
  assignSlot,
  isDirty,
  markSaved,
  openDraft,
  setComposition,
  setEffectParameter,
  setGeneratorParameter,
  setLayer,
  setMaster,
  setNotes,
  setSlot,
  validate,
  type Edit,
} from '../src/shared/editor.js';
import { BLACK_SLOT_INDEX, MAX_BPM } from '../src/shared/limits.js';
import {
  defaultValue,
  findEffect,
  findGenerator,
  type Catalogue,
  type Module,
} from '../src/shared/modules.js';
import type { Answer, Command } from '../src/shared/types.js';

const address = process.env['OVS_LIVE_SERVER'];
const password = process.env['OVS_LIVE_PASSWORD'] ?? 'correct horse battery staple';
const username = process.env['OVS_LIVE_USER'] ?? 'operator';

/** A file in the repository, which is where the committed documents are. */
const repository = (path: string) => fileURLToPath(new URL(`../../../${path}`, import.meta.url));

const openSocket: OpenSocket = (url, handlers) => {
  const socket = new WebSocket(url);
  socket.on('open', () => handlers.opened());
  socket.on('message', (data: Buffer) => handlers.received(data.toString()));
  socket.on('close', () => handlers.closed('socket closed'));
  socket.on('error', () => undefined);
  return {
    send: (text) => {
      if (socket.readyState === WebSocket.OPEN) {
        socket.send(text);
      }
    },
    close: () => socket.close(),
  };
};

/** Waits for something to become true, or gives up with what it last saw. */
async function until<T>(what: string, read: () => T, ready: (value: T) => boolean): Promise<T> {
  const deadline = Date.now() + 15_000;
  let seen = read();
  while (Date.now() < deadline) {
    seen = read();
    if (ready(seen)) {
      return seen;
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`${what}: last saw ${JSON.stringify(seen)}`);
}

/** One session, and everything it reported while it was open. */
interface Live {
  session: Session;
  statuses: ConnectionStatus[];
  views: ShowView[];
  faults: string[];
}

/** A session that records what it is told, rather than drawing it. */
function newSession(): Live {
  const statuses: ConnectionStatus[] = [];
  const views: ShowView[] = [];
  const faults: string[] = [];

  const session = new Session(
    { fetch: (url, init) => fetch(url, init), open: openSocket, clientVersion: '0.0.1-test', files },
    {
      status: (status) => statuses.push(status),
      view: (view) => views.push(view),
      telemetry: () => undefined,
      ndiSources: () => undefined,
      connectors: () => undefined,
      fault: (code, message) => faults.push(`${code}: ${message}`),
      ingest: () => undefined,
    },
  );

  return { session, statuses, views, faults };
}

/**
 * Signs in, creating the operator account on a server that has none yet.
 *
 * The same two branches the sign-in panel takes, because `inspect` answering
 * "not configured" is the only thing that distinguishes a fresh server from one
 * whose password this run has wrong.
 */
async function signIn(live: Live): Promise<void> {
  const server = await live.session.inspect(address!);
  if (server.configured) {
    await live.session.connect(address!, username, password);
  } else {
    await live.session.createAccount(address!, username, password);
  }

  await until(
    'the connection never went live',
    () => live.session.snapshot.status,
    (status) => status.state === 'live',
  );
}

/** The answer, or a failure that says what the server refused and why. */
function must<T>(answer: Answer<T>): T {
  if (!answer.ok) {
    throw new Error(`the server refused: ${answer.code}: ${answer.message}`);
  }
  return answer.value;
}

/** The edited document, or a failure that says which edit the client refused. */
function apply(edit: Edit): Show {
  if (!edit.ok) {
    throw new Error(`the editor refused: ${edit.reason}`);
  }
  return edit.show;
}

/** Both catalogues, in the shape the editor checks a document against. */
async function catalogueOf(live: Live): Promise<Catalogue> {
  const published = must(await live.session.catalogues());
  return { loaded: true, effects: published.effects, generators: published.generators };
}

/**
 * A value inside a parameter's published range, and not the one it starts at.
 *
 * Every number here is one the server itself sent, so it survives the round trip
 * exactly; a midpoint of a range would be a number this test invented and a
 * float comparison waiting to happen.
 */
function otherValue(parameter: ModuleParameter): ParameterValue {
  const type = parameter.parameter_type;
  switch (type.kind) {
    case 'text':
      // Something short and in the font's own alphabet, so a value that came
      // back different would be this client's doing rather than the font's.
      return { type: 'text', value: type.default === 'OVS' ? 'OTHER' : 'OVS' };
    case 'choice': {
      // Any published choice that is not the default one. A list of one has no
      // other value, and a manifest is not allowed to publish one.
      const other = type.choices.find((choice) => choice.id !== type.default);
      return { type: 'choice', value: other?.id ?? type.default };
    }
    case 'image':
      // Any well-formed identifier. The server checks the shape of one here
      // and leaves "is it still in the library" to the places that have a
      // library to ask, so this round trip does not need a real picture.
      return { type: 'text', value: '01HQX7Y0000000000000000000' };
    case 'number':
      return { type: 'float', value: type.default === type.max ? type.min : type.max };
    case 'colour':
      // The published channels, in the other order: still four values the
      // server sent, and different from the default unless the colour is grey.
      return {
        type: 'color',
        value: [type.default[3], type.default[2], type.default[1], type.default[0]],
      };
    case 'toggle':
      return { type: 'bool', value: !type.default };
  }
}

describe.skipIf(address === undefined)('against a real server', () => {
  it('signs in, holds the show state, and sees its own commands land', async () => {
    const live = newSession();

    try {
      await signIn(live);

      // The snapshot arrives without being asked for, and it is the whole state.
      const first = await until(
        'no snapshot arrived',
        () => live.session.snapshot.view,
        (view) => view.master !== null,
      );
      expect(first.sequence).toBeGreaterThan(0);

      // A command goes over HTTP and comes back over the socket. That round
      // trip is the client's whole architecture in one assertion: the dimmer
      // moves because the server said so, not because this client asked.
      const wanted = first.master!.dimmer > 0.5 ? 0.25 : 0.75;
      expect(await live.session.run({ do: 'set_master', dimmer: wanted })).toEqual({ ok: true });

      const after = await until(
        'the server never reported the dimmer this client set',
        () => live.session.snapshot.view,
        (view) => Math.abs((view.master?.dimmer ?? -1) - wanted) < 0.001,
      );
      expect(after.sequence).toBeGreaterThan(first.sequence);

      // Every update followed the one before it, or the mirror would have
      // reported a gap rather than applying them.
      expect(live.faults).toEqual([]);
      expect(live.statuses.map((status) => status.state)).toContain('live');
      expect(live.views.length).toBeGreaterThan(1);
    } finally {
      live.session.disconnect();
    }
  }, 30_000);

  it('refuses a password the server does not know, and says which failure it was', async () => {
    const live = newSession();

    await expect(live.session.connect(address!, username, 'not the password')).rejects.toMatchObject(
      { status: 401 },
    );
    expect(live.session.snapshot.status).toEqual({ state: 'offline' });
  });

  // The most valuable one in this file.
  //
  // A show is saved whole and loaded whole, so the two halves have to agree
  // about every field of a document neither of them has any reason to look at
  // twice. A field this client writes and the server drops — because the Rust
  // renamed it, or because serde skips it, or because the generated TypeScript
  // says one thing and the model another — produces no error anywhere: the save
  // is accepted, the operator closes the show, and the thing they set is
  // simply not there the next time they open it. Nothing short of sending a
  // document and reading it back finds that.
  //
  // It is built with the editor's own functions rather than by hand, because the
  // editor is what an operator's edits actually go through, and a literal
  // written here would be this test agreeing with itself.
  it('saves a show the client built and gets the same document back', async () => {
    const live = newSession();

    try {
      await signIn(live);
      const catalogue = await catalogueOf(live);

      const generator = catalogue.generators.find((each) => each.parameters.length > 0);
      const effects = catalogue.effects.filter((each) => each.parameters.length > 0);
      expect(generator, 'this server publishes no generator with parameters').toBeDefined();
      expect(effects.length, 'this server publishes fewer than two effects with parameters')
        .toBeGreaterThanOrEqual(2);

      let show = must(await live.session.createShow('live suite round trip'));

      show = apply(setNotes(show, 'written by the live suite, and read back'));
      show = apply(
        setMaster(show, {
          default_bpm: 137,
          beats_per_bar: 3,
          default_speed_scale: 2,
          default_dimmer: 0.25,
          default_crossfader: 0.75,
          crossfader_curve: 'equal_power',
        }),
      );
      show = apply(
        setComposition(show, {
          size: { width: 1280, height: 720 },
          render_rate: { numerator: 30000, denominator: 1001 },
        }),
      );

      show = apply(addLayer(show, 'Upstage wall'));
      const layerId = show.layers[0]!.id;
      show = apply(
        setLayer(show, layerId, {
          enabled: false,
          route: 'B',
          blend_mode: 'screen',
          default_opacity: 0.5,
          default_transform: { x: 0.5, y: -0.25, scale_x: 2, scale_y: 0.5, rotation_deg: 90 },
        }),
      );

      // Two slots in two different banks, because the bank a slot falls in is
      // arithmetic on an index rather than a stored field, and an index the
      // server narrowed would show up here and nowhere else.
      const source: SourceRef = {
        kind: 'generator',
        type_id: generator!.type_id,
        version: generator!.version,
        parameters: {},
      };
      for (const index of [1, 13]) {
        show = apply(assignSlot(show, layerId, index, source));
        show = apply(
          setSlot(show, layerId, index, {
            playback_mode: 'bounce',
            direction: 'reverse',
            transform: { x: -1, y: 1, scale_x: 0.5, scale_y: 4, rotation_deg: 180 },
          }),
        );
        for (const parameter of generator!.parameters) {
          show = apply(
            setGeneratorParameter(
              show,
              catalogue,
              layerId,
              index,
              parameter.key,
              otherValue(parameter),
            ),
          );
        }
      }

      // A chain on the layer and a chain on a slot. The renderer draws only the
      // first of those, and the second is saved regardless, which
      // is exactly the kind of field that gets quietly dropped.
      for (const [position, effect] of effects.slice(0, 2).entries()) {
        const chain = position === 0 ? { layerId } : { layerId, slotIndex: 1 };
        show = apply(addEffect(show, catalogue, chain, effect.type_id));

        const chainNow =
          position === 0
            ? show.layers[0]!.effects
            : show.layers[0]!.slots.find((slot) => slot.index === 1)!.effects;
        const effectId = chainNow[chainNow.length - 1]!.id;
        for (const parameter of effect.parameters) {
          show = apply(
            setEffectParameter(
              show,
              catalogue,
              chain,
              effectId,
              parameter.key,
              otherValue(parameter),
            ),
          );
        }
      }

      // Nothing the client itself would refuse, so a refusal from here on is
      // the server disagreeing rather than a document this test built wrong.
      expect(validate(show, catalogue)).toEqual([]);

      const saved = must(await live.session.saveShow(show));
      const sent = markSaved(openDraft(show), saved.updated_at).show;

      const reloaded = must(await live.session.loadShow(show.id));
      expect(reloaded).toEqual(sent);

      // The same thing the editor itself asks, and the thing an operator would
      // notice: a document that came back different reads as unsaved work.
      expect(isDirty({ saved: reloaded, show: sent })).toBe(false);
      expect(live.faults).toEqual([]);
    } finally {
      live.session.disconnect();
    }
  }, 60_000);

  // A refusal has to arrive as a refusal.
  //
  // The client checks a document before it sends one, so in normal use the
  // server's own validation is never reached. That is precisely why it is worth
  // testing: the day the two disagree, the client sends something it believes
  // is fine, and what comes back must be a refusal an operator can act on —
  // carrying the server's words and the path of the field they are about — not
  // an exception, and above all not a save that quietly did nothing.
  //
  // Both of these are refused by the editor, so they are assembled by hand: the
  // point is what the *server* does with a document that got past the client.
  it('reports a document the server will not have as a refusal, in the server’s words', async () => {
    const live = newSession();

    try {
      await signIn(live);
      const catalogue = await catalogueOf(live);
      const generator = catalogue.generators[0]!;

      const start = must(await live.session.createShow('live suite refusals'));
      const withLayer = apply(addLayer(start, 'Refused'));
      must(await live.session.saveShow(withLayer));

      const layer = withLayer.layers[0]!;
      const source: SourceRef = {
        kind: 'generator',
        type_id: generator.type_id,
        version: generator.version,
      };

      // Slot 0 is permanently Black on every layer and is not a slot a document
      // may configure (`Layer::BLACK_SLOT_INDEX`).
      const black: Slot = {
        index: BLACK_SLOT_INDEX,
        source,
        playback_mode: 'loop',
        direction: 'forward',
        transform: { x: 0, y: 0, scale_x: 1, scale_y: 1, rotation_deg: 0 },
        effects: [],
      };
      const forbiddenSlot: Show = {
        ...withLayer,
        layers: [{ ...layer, slots: [black] }],
      };

      // A tempo no clock in this system will run at (`MasterConfig::validate`).
      const outOfRange: Show = {
        ...withLayer,
        master: { ...withLayer.master, default_bpm: MAX_BPM + 1 },
      };

      for (const [what, document, expected] of [
        [
          'slot 0',
          forbiddenSlot,
          {
            path: 'layers[0].slots[0].index',
            message: 'slot 0 is permanently Black and cannot be configured',
          },
        ],
        [
          'a BPM above the range',
          outOfRange,
          { path: 'master.default_bpm', message: 'must be between 20 and 999' },
        ],
      ] as [string, Show, ValidationError][]) {
        const answer = await live.session.saveShow(document);

        expect(answer.ok, `${what} was accepted`).toBe(false);
        if (answer.ok) {
          continue;
        }
        expect(answer.code).toBe('invalid');
        expect(answer.message).not.toBe('');
        // The path matters as much as the message: it is what puts the problem
        // beside the field rather than in a list at the bottom of the screen.
        expect(answer.details).toContainEqual(expected);

        // And the client would have said the same thing, at the same path, in
        // the same words. Where this fails, one of the two has moved and the
        // editor is about to start offering an operator something the server
        // refuses.
        expect(validate(document, catalogue)).toContainEqual(expected);
      }

      // Refused means refused: neither document reached the database.
      const stored = must(await live.session.loadShow(start.id));
      expect(stored.layers).toEqual(withLayer.layers);
      expect(stored.master.default_bpm).toBe(withLayer.master.default_bpm);
      expect(live.faults).toEqual([]);
    } finally {
      live.session.disconnect();
    }
  }, 60_000);

  // The catalogues against the document an operator reads.
  //
  // `reference/MODULES.md` and `reference/FIXTURES.md` are generated from the manifests
  // and committed, and a golden test in the Rust keeps them honest about what
  // that build publishes. What nothing checked until now is whether the client
  // reads the *served* catalogue the same way — the editor draws every control
  // from it, and the patch editor lays a patch out from the footprints, so a
  // parameter that changed kind or a personality that gained a channel is a
  // control drawn wrong or a fixture patched on top of the next one.
  //
  // A generator gaining a parameter breaks this, which is the point: the
  // document is meant to say so. Modules are compared by type alone, because
  // there is one per type and no versions of it (owner, 2026-10-18).
  it('publishes the generators, effects and personalities the documents list', async () => {
    const live = newSession();

    try {
      await signIn(live);
      const catalogue = await catalogueOf(live);
      const modules = readFileSync(repository('reference/MODULES.md'), 'utf8');

      // Looked up through the client's own `findGenerator` and `findEffect`,
      // because how this client finds a published module by its type is half
      // of what is being checked: a module it cannot find is a control the
      // editor will not draw, however correctly the server published it.
      const sections: [string, Module[], (typeId: string) => Module | null][] = [
        ['Generators', catalogue.generators, (id) => findGenerator(catalogue, id)],
        ['Effects', catalogue.effects, (id) => findEffect(catalogue, id)],
      ];

      for (const [section, published, find] of sections) {
        const documented = documentedModules(modules, section);
        expect(documented.length, `${section} lists nothing`).toBeGreaterThan(0);

        // Both directions. A module the document has lost is as wrong as one
        // the server has gained.
        expect(published.map(named).sort()).toEqual(documented.map(named).sort());

        for (const module of documented) {
          const found = find(module.type_id);
          expect(found, `${named(module)} is documented and not published`).not.toBeNull();
          expect(found!.parameters.map((parameter) => parameter.key)).toEqual(
            module.parameters.map((parameter) => parameter.key),
          );

          for (const [position, parameter] of module.parameters.entries()) {
            const server = found!.parameters[position]!;
            expect(rounded(server.parameter_type), `${named(module)} ${parameter.key}`).toEqual(
              rounded(parameter.parameter_type),
            );
            // What this client makes of it, which is what an untouched control
            // is drawn at and what a show that mentions nothing gets.
            expect(defaultValue(server)).toEqual(defaultValue(parameter));
          }
        }
      }

      const personalities = must(await live.session.fixtures());
      const documented = documentedPersonalities(
        readFileSync(repository('reference/FIXTURES.md'), 'utf8'),
      );
      expect(documented.length, 'FIXTURES.md lists nothing').toBeGreaterThan(0);
      expect(personalities.map(named).sort()).toEqual(documented.map(named).sort());

      for (const personality of documented) {
        const found = personalities.find((each) => each.type_id === personality.type_id);
        expect(found, `${named(personality)} is documented and not published`).toBeDefined();
        // The footprint is what a patch entry reserves, so a console programmed
        // from the document and a patch laid out from the catalogue have to
        // agree about it down to the channel.
        expect(found!.footprint).toBe(personality.footprint);
        expect(found!.channels.map((channel) => channel.control)).toEqual(personality.controls);
      }
    } finally {
      live.session.disconnect();
    }
  }, 60_000);

  // A run of commands, and not one gap.
  //
  // The mirror refuses to apply an update that does not follow the last one, and
  // reports a gap instead. That rule is tested against a faked
  // server in `mirror.test.ts`, where the sequence numbers are whatever the test
  // wrote. What it cannot tell you is whether the real server numbers its
  // updates the way the client counts them — off by one in either direction and
  // every command an operator gives during a show costs a full resynchronisation,
  // which is the whole state over the wire for a dimmer move.
  //
  // Master commands only, on purpose: they are the ones that need no show
  // open, so this says nothing about whatever the server is showing.
  it('applies every update in a run of commands, and never reports a gap', async () => {
    const live = newSession();

    try {
      await signIn(live);
      let view = await until(
        'no snapshot arrived',
        () => live.session.snapshot.view,
        (each) => each.master !== null,
      );

      // Each one is decided from what the master is doing at the time, because a
      // command that sets a value to what it already is changes nothing, emits
      // nothing, and would make the arithmetic below wrong for the right reason.
      const movers: ((master: NonNullable<ShowView['master']>) => Command)[] = [
        (master) => ({ do: 'set_master', dimmer: master.dimmer > 0.5 ? 0.25 : 0.75 }),
        (master) => ({ do: 'set_master', crossfader: master.crossfader > 0.5 ? 0.25 : 0.75 }),
        (master) => ({ do: 'set_master', bpm: master.bpm === 137 ? 143 : 137 }),
        (master) => ({ do: 'set_master', speedScale: master.speedScale === 2 ? 1 : 2 }),
        (master) => ({ do: 'set_master', freeze: !master.freeze }),
        (master) => ({ do: 'set_master', freeze: !master.freeze }),
        // Four fields at once is still one change, because the server reports
        // what the master now is rather than what was asked of it.
        (master) => ({
          do: 'set_master',
          dimmer: master.dimmer > 0.5 ? 0.25 : 0.75,
          crossfader: master.crossfader > 0.5 ? 0.25 : 0.75,
          bpm: master.bpm === 137 ? 143 : 137,
          speedScale: master.speedScale === 2 ? 1 : 2,
        }),
      ];

      for (const mover of movers) {
        const before = view.sequence;
        const command = mover(view.master!);

        expect(await live.session.run(command), JSON.stringify(command)).toEqual({ ok: true });
        view = await until(
          `the server never reported ${JSON.stringify(command)}`,
          () => live.session.snapshot.view,
          (each) => each.sequence > before,
        );

        // One update, applied. Two would mean the server said something this
        // client did not ask for; more than one step would mean the client
        // missed one and applied the next anyway.
        expect(view.sequence, JSON.stringify(command)).toBe(before + 1);
      }

      // A gap arrives as a fault, so an empty list is the assertion: the mirror
      // accounted for every update in the run.
      expect(live.faults).toEqual([]);
      expect(live.session.snapshot.status.state).toBe('live');
    } finally {
      live.session.disconnect();
    }
  }, 60_000);
});

// --- reading the committed documents ----------------------------------------
//
// These parse `reference/MODULES.md` and `reference/FIXTURES.md` rather than importing
// anything, because the document is the artefact under test: it is what somebody
// programming a console or writing a show by hand actually reads, and a
// comparison against the constants it was generated from would only prove the
// generator runs.

/** One module as the document describes it. */
interface DocumentedModule {
  type_id: string;
  parameters: ModuleParameter[];
}

/** One personality as the document describes it. */
interface DocumentedPersonality {
  type_id: string;
  footprint: number;
  controls: string[];
}

/** How a module or a personality is named in a failure: by its type, which is all it has. */
function named(module: { type_id: string }): string {
  return module.type_id;
}

/**
 * Colour defaults, to the precision the document prints them at.
 *
 * `markdown()` writes a colour to two decimals, so a default with more
 * precision than that cannot be read back out of the document. Compared at the
 * document's precision rather than the server's, so that a failure here means
 * the two disagree rather than that the document rounded.
 */
function rounded(type: ParameterType): ParameterType {
  if (type.kind !== 'colour') {
    return type;
  }
  const [red, green, blue, alpha] = type.default;
  const place = (channel: number) => Math.round(channel * 100) / 100;
  return { kind: 'colour', default: [place(red), place(green), place(blue), place(alpha)] };
}

/** The modules one section of `MODULES.md` lists, in the order it lists them. */
function documentedModules(markdown: string, section: string): DocumentedModule[] {
  const modules: DocumentedModule[] = [];
  let reading = false;
  let current: DocumentedModule | null = null;

  for (const line of markdown.split('\n')) {
    if (line.startsWith('## ')) {
      reading = line.trim() === `## ${section}`;
      current = null;
      continue;
    }
    if (!reading) {
      continue;
    }

    // A version after the type, where the heading prints one, is read past
    // rather than kept: there is one module per type and the number names
    // nothing a show could be told apart by.
    const heading = /^### .* — `([^`]+)`(?: v\d+)?\s*$/.exec(line);
    if (heading) {
      current = { type_id: heading[1]!, parameters: [] };
      modules.push(current);
      continue;
    }

    // A parameter row. The header and the rule under it carry no backticks, so
    // they do not match and need no special case.
    const row = /^\|([^|]*)\|\s*`([^`]+)`\s*\|([^|]*)\|([^|]*)\|\s*$/.exec(line);
    if (row && current) {
      current.parameters.push({
        key: row[2]!,
        name: row[1]!.trim(),
        parameter_type: documentedType(row[3]!.trim(), row[4]!.trim()),
      });
    }
  }

  return modules;
}

/** What one row of a parameter table says the parameter takes. */
function documentedType(takes: string, fallback: string): ParameterType {
  const number = /^a number from (-?[\d.]+) to (-?[\d.]+)$/.exec(takes);
  if (number) {
    return {
      kind: 'number',
      min: Number(number[1]),
      max: Number(number[2]),
      default: Number(fallback),
    };
  }
  if (takes === 'a colour, linear RGBA') {
    const [red, green, blue, alpha] = fallback.split(',').map((channel) => Number(channel.trim()));
    return { kind: 'colour', default: [red!, green!, blue!, alpha!] };
  }
  if (takes === 'on or off') {
    return { kind: 'toggle', default: fallback === 'on' };
  }
  // `one of \`ellipse\` (Ellipse), \`rectangle\` (Rectangle)` — the identifier
  // is what a show stores and what this compares; the label beside it is for
  // the reader of the document and is deliberately not checked here, because a
  // rewording is not a contract change.
  if (takes.startsWith('one of ')) {
    const choices = [...takes.slice('one of '.length).matchAll(/`([^`]+)`\s*\(([^)]*)\)/g)].map(
      (found) => ({ id: found[1]!, name: found[2]! }),
    );
    if (choices.length === 0) {
      throw new Error(`MODULES.md lists no choices in ${JSON.stringify(takes)}`);
    }
    return { kind: 'choice', choices, default: unquote(fallback) };
  }
  const text = /^text, up to (\d+) characters$/.exec(takes);
  if (text) {
    return {
      kind: 'text',
      max_chars: Number(text[1]),
      default: fallback === 'nothing' ? '' : unquote(fallback),
    };
  }
  throw new Error(`MODULES.md says a parameter takes ${JSON.stringify(takes)}, which this cannot read`);
}

/** A default the document wrote in backticks, as the value it names. */
function unquote(fallback: string): string {
  const quoted = /^`(.*)`$/.exec(fallback);
  return quoted ? quoted[1]! : fallback;
}

/** The personalities `FIXTURES.md` lists, with their channels in address order. */
function documentedPersonalities(markdown: string): DocumentedPersonality[] {
  const personalities: DocumentedPersonality[] = [];
  let current: DocumentedPersonality | null = null;
  // The tables under a `###` are one channel's own value bands, not channels.
  let inChannels = false;

  for (const line of markdown.split('\n')) {
    // Read past a version the heading may print, as `documentedModules` does.
    const heading = /^## .* — `([^`]+)`(?: v\d+)?\s*$/.exec(line);
    if (heading) {
      current = { type_id: heading[1]!, footprint: 0, controls: [] };
      personalities.push(current);
      inChannels = true;
      continue;
    }
    if (line.startsWith('###')) {
      inChannels = false;
      continue;
    }
    if (!current || !inChannels) {
      continue;
    }

    const footprint = /^Footprint: \*\*(\d+) channels?\*\*/.exec(line);
    if (footprint) {
      current.footprint = Number(footprint[1]);
      continue;
    }

    const row = /^\|\s*[\d–-]+\s*\|[^|]*\|\s*`([^`]+)`\s*\|/.exec(line);
    if (row) {
      current.controls.push(row[1]!);
    }
  }

  return personalities;
}
