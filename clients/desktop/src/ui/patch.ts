// The DMX-over-IP patch page.
//
// Its own mode beside LIVE, EDIT and MEDIA, because patching a rig is its own
// sitting: an operator does it once with the console's show file open beside
// them, and what they need on screen is every fixture, every universe and every
// channel at once.
//
// It edits the same document the show editor does — the patch lives in
// `Show.fixture_patch` — so the Save on this page is the editor's Save, the
// server validates the whole document, and a refusal lands against the fields it
// names. Nothing here is live control: this page says which channel moves the
// master dimmer, never what the dimmer is doing.
//
// Four things it will not do.
//
// **It will not invent a personality.** Every channel count, footprint, channel
// name and control comes from `GET /fixtures`. A personality this build has
// never heard of is patched correctly; a personality it *has* heard of is still
// read from the server, because the server's is the one the show will be
// validated against.
//
// **It will not work out an address.** `GET /shows/{id}/patch/address` knows
// what is already patched, and this page asks it and uses what comes back. It
// says out loud that the answer is about the show as the server has it on
// disk, because that is the one thing about it an operator could be caught by.
//
// **It will not hide an overlap.** Two fixtures wanting the same channels is the
// mistake this page exists to catch. Both of them go red, both of them say what
// they meet, the universe bar shows where, and it happens the moment the address
// is typed rather than when the save comes back.
//
// **It will not tell a different story from the show editor.** Deleting a
// layer takes its patch entry with it and says so, in the editor's own sentence;
// this page simply has one fewer row afterwards. A patch entry pointing at a
// layer that is not in the document — which is a show written elsewhere — is
// reported in the server's words, with an Unpatch beside it.

import type {
  DmxProtocols,
  FixtureInstance,
  FixtureTarget,
  Personality,
  Show,
} from '@ovs/schema';

import type { Edit } from '../shared/editor.js';
import {
  MAX_DMX_ADDRESS,
  MAX_UNIVERSE,
  MIN_DMX_ADDRESS,
  MIN_UNIVERSE,
} from '../shared/limits.js';
import {
  absoluteChannel,
  channelRange,
  collides,
  describeTarget,
  effectPlaces,
  findPersonality,
  fixtureForEffect,
  fixtureForLayer,
  fixturePath,
  fixturesIn,
  findFixture,
  layerPersonalities,
  masterFixture,
  masterPersonalities,
  modulePersonalities,
  moveFixture,
  patchAdvisories,
  patchFixture,
  readPatch,
  setFixtureEnabled,
  unpatchFixture,
  universesOf,
  type FixtureCatalogue,
} from '../shared/patch.js';
import { button, element, make } from './dom.js';

/** What the patch page needs from the editor that owns the document. */
export interface PatchDeps {
  /** The document in hand, or null when no show is open. */
  show: () => Show | null;
  /** Applies an edit and redraws the whole editor, or says why it was refused. */
  apply: (edit: Edit) => void;
  /** Says something to the operator in the fault line at the foot of the window. */
  note: (message: string) => void;
  /** Everything wrong at or under one path, from validation or a refused save. */
  problems: (path: string) => string[];
  /** Every problem anywhere in the document, so none of them is invisible here. */
  problemList: () => Array<{ path: string; reason: string }>;
  /** The personalities this server publishes. Empty until it has said. */
  catalogue: () => FixtureCatalogue;
  /** Saves the whole document, which is the only way a patch is saved. */
  save: () => void;
  /** Throws the edits away, back to what the server last confirmed. */
  revert: () => void;
  /** Whether the document differs from what the server has. */
  dirty: () => boolean;
  /** Whether a save is in flight. */
  saving: () => boolean;
  /** The show the server currently has open, or null. */
  liveShowId: () => string | null;
  /**
   * What reaches this show, and setting it.
   *
   * A server setting rather than a show's, so it does not go through the
   * document at all: it takes effect the moment it is set.
   */
  dmxProtocols: () => Promise<DmxProtocols | null>;
  setDmxProtocols: (protocols: DmxProtocols) => Promise<DmxProtocols | null>;
}

/** Something in this show a fixture can be patched to. */
interface Patchable {
  /** How the chooser names it in its value. */
  key: string;
  label: string;
  target: FixtureTarget;
  /** The personalities that can drive it, from the catalogue. */
  personalities: Personality[];
}

/** The DMX-over-IP patch. */
export class PatchPanel {
  readonly #deps: PatchDeps;

  #selected: string | null = null;
  /**
   * What is being patched, by `Patchable.key`, and which of its personalities.
   *
   * Kept across a redraw so the left-hand list does not lose its place while
   * an operator is dragging out of it.
   */
  #target: string | null = null;
  #personality: string | null = null;
  #controlSeq = 0;

  readonly #ui = {
    documentFields: element('patch-document-fields'),
    documentNote: element('patch-document-note'),
    errors: element('patch-errors'),
    advisories: element('patch-advisories'),
    fixturePanel: element('patch-fixture-panel'),
    fixtureTitle: element<HTMLHeadingElement>('patch-fixture-title'),
    fixtureFields: element('patch-fixture-fields'),
    channels: element('patch-channels'),
    fixtures: element('patch-fixtures'),
    universeTabs: element('patch-universe-tabs'),
    universeTitle: element<HTMLHeadingElement>('patch-universe-title'),
    universeSays: element('patch-universe-says'),
    deleteFixture: element<HTMLButtonElement>('patch-delete'),
    channelGrid: element('patch-channels-grid'),
    artnet: element<HTMLButtonElement>('dmx-artnet'),
    sacn: element<HTMLButtonElement>('dmx-sacn'),
    protocolsNote: element('dmx-protocols-note'),
  };

  /** What reaches this show, or null before a server has said. */
  #protocols: DmxProtocols | null = null;
  /** Which universe the channel grid is showing. */
  #showing = 1;
  /**
   * The fixture being dragged onto a channel, while a drag is in progress.
   *
   * Held here as well as on the drag, because `getData` is deliberately
   * empty during `dragover` and a channel has to know whether it can take
   * what is being carried before the drop happens.
   */
  #dragging: Patchable | null = null;
  /**
   * The patched fixture being slid along its universe, while a drag is on.
   *
   * `id` is what is moving and `grabbed` is how far into its own footprint the
   * operator took hold of it, so the block lands where they let go rather than
   * jumping its first channel to the pointer.
   */
  #sliding: { id: string; footprint: number; grabbed: number } | null = null;

  constructor(deps: PatchDeps) {
    this.#deps = deps;
    // Top right of the universe, and only once something is selected (owner,
    // 2026-09-23).
    this.#ui.deleteFixture.addEventListener('click', () => {
      const show = this.#deps.show();
      if (show === null || this.#selected === null) {
        return;
      }
      this.#deps.apply(unpatchFixture(show, this.#selected));
      this.#selected = null;
    });
    this.#ui.artnet.addEventListener('click', () => void this.#toggleProtocol('artnet'));
    this.#ui.sacn.addEventListener('click', () => void this.#toggleProtocol('sacn'));
  }

  // --- what reaches this show -------------------------------------------------

  /**
   * The two protocol switches, and what they are for.
   *
   * A server setting rather than a show's: which protocols a building
   * speaks is a fact about the building, and carrying it in a show would
   * mean a show taken to another venue arriving with the wrong answer.
   */
  #drawProtocols(): void {
    const protocols = this.#protocols;
    for (const [node, on, name] of [
      [this.#ui.artnet, protocols?.artnet ?? false, 'Art-Net'],
      [this.#ui.sacn, protocols?.sacn ?? false, 'sACN'],
    ] as const) {
      node.disabled = protocols === null;
      node.classList.toggle('on', protocols !== null && on);
      node.classList.toggle('off', protocols !== null && !on);
      node.title =
        protocols === null
          ? 'Sign in to a server to see what reaches this show.'
          : on
            ? `${name} is driving this show. Press to take it off.`
            : `${name} is not driving this show. It is still listened for, and the status still says what is on the wire.`;
    }

    this.#ui.protocolsNote.textContent =
      protocols === null
        ? ''
        : protocols.artnet || protocols.sacn
          ? 'what reaches this show'
          : 'nothing on the network reaches this show';
  }

  /** Asks the server what reaches the show. */
  async loadProtocols(): Promise<void> {
    const answer = await this.#deps.dmxProtocols();
    this.#protocols = answer;
    this.#drawProtocols();
  }

  async #toggleProtocol(which: 'artnet' | 'sacn'): Promise<void> {
    const protocols = this.#protocols;
    if (protocols === null) {
      return;
    }
    const wanted = { ...protocols, [which]: !protocols[which] };
    // Shown at once, then corrected by what the server says: this is a switch
    // an operator reaches for when something has gone wrong out there, and a
    // switch that waits for a round trip feels broken.
    this.#protocols = wanted;
    this.#drawProtocols();
    this.#protocols = await this.#deps.setDmxProtocols(wanted);
    this.#drawProtocols();
  }

  /** Forgets what was selected, for a document that has just been opened. */
  reset(): void {
    this.#selected = null;
    this.#target = null;
    this.#personality = null;

    // The lowest universe this show already uses, because a rig is usually
    // looked at from the universe it is already on. A show that patches
    // nothing starts at the bottom of the range, which is a starting point
    // rather than a recommendation: Art-Net counts from zero and sACN from
    // one, and which of the two a console is speaking is not something this
    // client can know.
    const show = this.#deps.show();
    this.#showing = show === null ? MIN_UNIVERSE : universesOf(show)[0] ?? MIN_UNIVERSE;
  }

  /** Draws the whole page from the document in hand. */
  draw(): void {
    const show = this.#deps.show();
    this.#settleSelection(show);

    this.#drawDocument(show);
    this.#drawFixtures(show);
    this.#drawUniverseTabs(show);
    this.#drawChannelGrid(show);
    this.#drawFixture(show);
    this.#drawAdvisories(show);
  }

  #settleSelection(show: Show | null): void {
    if (!show || (this.#selected !== null && findFixture(show, this.#selected) === null)) {
      this.#selected = null;
    }
    if (!show) {
      return;
    }
    const patchable = this.#patchable(show);
    if (!patchable.some((each) => each.key === this.#target)) {
      // What was being patched has been patched, or the layer it pointed at has
      // gone. Either way the chooser stands somewhere that exists.
      this.#target = patchable[0]?.key ?? null;
      this.#personality = null;
    }
    const chosen = patchable.find((each) => each.key === this.#target);
    if (chosen && !chosen.personalities.some((each) => key(each) === this.#personality)) {
      this.#personality = chosen.personalities[0] ? key(chosen.personalities[0]) : null;
    }
  }

  // --- the document ----------------------------------------------------------

  /**
   * The same document the show editor holds, and the same Save.
   *
   * A patch is not saved on its own: there is no per-field endpoint, the server
   * takes the show whole, and a patch entry is refused or accepted along with
   * everything else in it. Every problem in the document is listed here, not
   * only the patch's, because a refusal about a layer is not a message to hide
   * from somebody standing on this page.
   */
  #drawDocument(show: Show | null): void {
    if (!show) {
      this.#ui.documentFields.replaceChildren(
        make('p', 'empty-note', 'Open a show in Settings to patch it.'),
      );
      this.#ui.documentNote.textContent = '';
      this.#ui.errors.replaceChildren();
      return;
    }

    this.#ui.documentFields.replaceChildren(
      make('p', 'patch-show-name', show.name),
    );

    const messages: string[] = [];
    if (this.#deps.dirty()) {
      messages.push('This document has changes the server has not got.');
    }
    if (show.id === this.#deps.liveShowId()) {
      messages.push('This is the show the server has open: saving it reloads it on the wall.');
    }
    this.#ui.documentNote.textContent = messages.join(' ');

    this.#ui.errors.replaceChildren(
      ...this.#deps.problemList().map((problem) => {
        const row = make('li');
        row.append(
          make('code', undefined, problem.path),
          document.createTextNode(` ${problem.reason}`),
        );
        return row;
      }),
    );
  }

  // --- patching something ----------------------------------------------------

  /**
   * Everything in this show that can still take a fixture.
   *
   * Exactly one master, a layer fixture per layer, and an effect instance for
   * which a module publishes a personality. None of the three is a
   * list written here: what a personality may drive comes from the catalogue,
   * and what there is to drive comes from the document.
   */
  #patchable(show: Show): Patchable[] {
    const catalogue = this.#deps.catalogue();
    const found: Patchable[] = [];

    const masters = masterPersonalities(catalogue);
    if (masterFixture(show) === null && masters.length > 0) {
      found.push({
        key: 'master',
        label: 'Master',
        target: { kind: 'master' },
        personalities: masters,
      });
    }

    const layers = layerPersonalities(catalogue);
    if (layers.length > 0) {
      for (const layer of show.layers) {
        if (fixtureForLayer(show, layer.id) !== null) {
          continue;
        }
        found.push({
          key: `layer:${layer.id}`,
          label: `Layer · ${layer.name}`,
          target: { kind: 'layer', layer_id: layer.id },
          personalities: layers,
        });
      }
    }

    // An effect's personality comes from that effect's own manifest, so the
    // match is by type and version rather than by anything named here. No
    // effect publishes one yet, which the panel says rather than leaving a gap.
    const modules = modulePersonalities(catalogue);
    for (const place of effectPlaces(show)) {
      if (fixtureForEffect(show, place.instance.id) !== null) {
        continue;
      }
      const personalities = modules.filter(
        (personality) =>
          personality.type_id === place.instance.type_id &&
          personality.version === place.instance.version,
      );
      if (personalities.length === 0) {
        continue;
      }
      const where =
        place.slotIndex === undefined
          ? place.layer.name
          : `${place.layer.name}, slot ${place.slotIndex}`;
      found.push({
        key: `effect:${place.instance.id}`,
        label: `Effect · ${place.instance.type_id} on ${where}`,
        target: { kind: 'effect', effect_instance_id: place.instance.id },
        personalities,
      });
    }

    return found;
  }

  // --- what is patched -------------------------------------------------------

  /**
   * One patched fixture, in numbers, and the channels it owns.
   *
   * The channel list is what somebody programming a desk writes down: the
   * absolute address of every channel, what it is called and what it controls.
   * All three come from the personality the server published.
   */
  #drawFixture(show: Show | null): void {
    const fixture = show && this.#selected ? findFixture(show, this.#selected) : null;
    this.#ui.fixturePanel.hidden = fixture === null;
    if (!show || !fixture) {
      return;
    }

    const catalogue = this.#deps.catalogue();
    const personality = findPersonality(catalogue, fixture.type_id, fixture.version);
    const base = fixturePath(show, fixture.id) ?? '';
    this.#ui.fixtureTitle.textContent = `Fixture · ${describeTarget(show, fixture.target)}`;

    // Where it is, said and not offered (owner, 2026-09-23). An address is
    // changed by dragging the block along its universe, and a universe by
    // taking the fixture out and dragging it into the one you want — which is
    // the gesture the grid is there for, and leaves one way of doing it rather
    // than two that have to agree.
    const range = channelRange(fixture);
    const where = make(
      'span',
      'reading',
      `Universe ${fixture.universe}, address ${fixture.start_address}`,
    );

    const fields: HTMLElement[] = [
      this.#field('Drives', make('span', 'reading', describeTarget(show, fixture.target)), `${base}.target`),
      this.#field('Patched at', where, `${base}.universe`),
      this.#field(
        'Channels',
        make(
          'span',
          'reading',
          range === null
            ? `${fixture.footprint} channels from ${fixture.start_address}, which does not fit in a ${MAX_DMX_ADDRESS}-channel universe`
            : `${range.first}–${range.last} of ${MAX_DMX_ADDRESS}`,
        ),
        `${base}.footprint`,
      ),
      this.#inline(
        'Patched. Unticked, this entry is parked: it keeps its address and holds no channels.',
        this.#toggle(fixture.enabled, (value) =>
          this.#deps.apply(setFixtureEnabled(show, fixture.id, value)),
        ),
      ),
    ];


    // Everything this fixture is overlapping, named on this fixture as well as
    // on the other one. Either of the two may be the one that should move.
    const meeting = readPatch(show).filter(
      (other) => other.id !== fixture.id && collides(fixture, other),
    );
    for (const other of meeting) {
      fields.push(
        make(
          'p',
          'collision',
          `Overlaps ${describeTarget(show, other.target)} at ${other.start_address}–${
            other.start_address + other.footprint - 1
          } in universe ${other.universe}. The server refuses a show with an overlap in it.`,
        ),
      );
    }

    this.#ui.fixtureFields.replaceChildren(...fields);
    this.#drawChannels(fixture, personality);
  }

  #drawChannels(fixture: FixtureInstance, personality: Personality | null): void {
    if (personality === null) {
      this.#ui.channels.replaceChildren();
      return;
    }

    const table = make('table', 'channels');
    const head = make('thead');
    const headings = make('tr');
    for (const caption of ['DMX', 'Channel', 'Control']) {
      headings.append(make('th', undefined, caption));
    }
    head.append(headings);

    const body = make('tbody');
    for (const channel of personality.channels) {
      const first = absoluteChannel(fixture, channel);
      const row = make('tr');
      row.append(
        make('td', 'dmx', channel.width > 1 ? `${first}–${first + channel.width - 1}` : String(first)),
        make('td', undefined, channel.name),
        make('td', 'control', `${channel.control}${channel.momentary ? ' · fires' : ''}`),
      );
      body.append(row);
    }

    table.append(head, body);
    this.#ui.channels.replaceChildren(table);
  }

  // --- the universes ---------------------------------------------------------

  // --- the desk layout --------------------------------------------------------

  /**
   * What there is to patch, down the left, as things to pick up.
   *
   * Everything in *this* show that a personality can drive: the master, each
   * layer, each module that publishes one. Not a catalogue of manufacturers'
   * products, because that is not what a fixture is on this server.
   *
   * One that is already patched is still listed, marked and not draggable: an
   * operator looking for a layer wants to find it and be told where it is,
   * rather than wonder why it is missing.
   */
  #drawFixtures(show: Show | null): void {
    if (!show) {
      this.#ui.fixtures.replaceChildren(
        make('p', 'empty-note', 'Open a show in Settings to patch it.'),
      );
      return;
    }

    const free = this.#patchable(show);
    const rows: HTMLElement[] = [];

    for (const patchable of free) {
      const personality = patchable.personalities[0];
      if (personality === undefined) {
        continue;
      }
      const tile = make('div', 'patch-fixture-tile');
      tile.draggable = true;
      tile.append(
        make('span', 'patch-fixture-name', patchable.label),
        make('span', 'patch-fixture-size', `${personality.footprint} ch`),
      );
      tile.title = `${patchable.label} \u2014 ${personality.name}, ${personality.footprint} channels. Drag it onto a channel to patch it there.`;

      tile.addEventListener('dragstart', (event) => {
        this.#dragging = patchable;
        if (event.dataTransfer !== null) {
          event.dataTransfer.effectAllowed = 'copy';
          event.dataTransfer.setData('text/plain', patchable.label);
        }
      });
      tile.addEventListener('dragend', () => {
        this.#dragging = null;
        this.#clearChannelHighlights();
      });
      rows.push(tile);
    }

    if (rows.length === 0) {
      rows.push(
        make(
          'p',
          'empty-note',
          readPatch(show).length === 0
            ? 'Nothing in this show publishes a personality yet.'
            : 'Everything this show can patch is patched.',
        ),
      );
    }

    // What is already patched, so the left-hand list is the whole picture.
    const patched = readPatch(show);
    if (patched.length > 0) {
      rows.push(make('h3', 'patch-fixtures-heading', 'PATCHED'));
      for (const fixture of patched) {
        const row = make('div', 'patch-fixture-tile patched');
        row.classList.toggle('selected', fixture.id === this.#selected);
        row.append(
          make('span', 'patch-fixture-name', describeTarget(show, fixture.target)),
          make('span', 'patch-fixture-size', `${fixture.universe}.${fixture.start_address}`),
        );
        row.title = `${describeTarget(show, fixture.target)} \u2014 universe ${fixture.universe}, channels ${fixture.start_address}\u2013${
          fixture.start_address + fixture.footprint - 1
        }`;
        row.addEventListener('click', () => {
          this.#selected = fixture.id;
          this.#showing = fixture.universe;
          this.draw();
        });
        rows.push(row);
      }
    }

    this.#ui.fixtures.replaceChildren(...rows);
  }

  /**
   * The universes, as tabs along the top.
   *
   * Every universe something is patched in, the one being looked at, and one
   * more — so there is always somewhere to put the next fixture without having
   * to find a control that makes a universe.
   */
  #drawUniverseTabs(show: Show | null): void {
    if (!show) {
      this.#ui.universeTabs.replaceChildren();
      return;
    }

    const used = universesOf(show);
    const highest = Math.max(this.#showing, ...used, 0);
    const offered: number[] = [];
    for (let universe = 1; universe <= Math.min(highest + 1, MAX_UNIVERSE); universe += 1) {
      offered.push(universe);
    }

    this.#ui.universeTabs.replaceChildren(
      ...offered.map((universe) => {
        const fixtures = fixturesIn(show, universe);
        const tab = button(
          `${universe}`,
          () => {
            this.#showing = universe;
            this.draw();
          },
          {
            className: universe === this.#showing ? 'settings-tab on' : 'settings-tab',
            title:
              fixtures.length === 0
                ? `Universe ${universe} \u2014 nothing patched here yet`
                : `Universe ${universe} \u2014 ${fixtures.length} ${fixtures.length === 1 ? 'fixture' : 'fixtures'}`,
          },
        );
        tab.classList.toggle('empty', fixtures.length === 0);
        return tab;
      }),
    );
  }

  /**
   * One universe's channels, and where a fixture can land.
   *
   * Five hundred and twelve cells. A cell a fixture holds carries its name and
   * is the fixture; an empty one is a place to drop something. Dropping is the
   * whole point of the layout the owner asked for (2026-09-23).
   */
  #drawChannelGrid(show: Show | null): void {
    if (!show) {
      this.#ui.channelGrid.replaceChildren();
      this.#ui.universeTitle.textContent = 'Universe';
      this.#ui.universeSays.textContent = '';
      return;
    }

    const fixtures = fixturesIn(show, this.#showing);
    const held = new Map<number, (typeof fixtures)[number]>();
    for (const fixture of fixtures) {
      const range = channelRange(fixture);
      if (range === null) {
        continue;
      }
      for (let address = range.first; address <= range.last; address += 1) {
        held.set(address, fixture);
      }
    }

    const used = held.size;
    // Only while something in this universe is selected: a Delete with nothing
    // to delete is a control an operator has to decide about every time they
    // look at it.
    const selected = this.#selected === null ? null : findFixture(show, this.#selected);
    this.#ui.deleteFixture.hidden = selected === null || selected.universe !== this.#showing;
    this.#ui.deleteFixture.textContent =
      selected === null ? 'Delete fixture' : `Delete ${describeTarget(show, selected.target)}`;

    this.#ui.universeTitle.textContent = `Universe ${this.#showing}`;
    this.#ui.universeSays.textContent = `${used} of ${MAX_DMX_ADDRESS} channels held \u00b7 ${fixtures.length} ${
      fixtures.length === 1 ? 'fixture' : 'fixtures'
    }`;

    const cells: HTMLElement[] = [];
    for (let address = 1; address <= MAX_DMX_ADDRESS; address += 1) {
      const fixture = held.get(address) ?? null;
      const cell = make('div', 'channel-cell');
      cell.dataset['address'] = String(address);
      cell.classList.toggle('held', fixture !== null);
      cell.classList.toggle('selected', fixture !== null && fixture.id === this.#selected);
      cell.classList.toggle('off', fixture !== null && !fixture.enabled);

      if (fixture === null) {
        cell.append(make('span', 'channel-number', String(address)));
        cell.title = `Channel ${address} \u2014 free`;
      } else {
        const range = channelRange(fixture);
        const first = range !== null && range.first === address;
        cell.classList.toggle('first', first);
        // A colour of its own, so two blocks side by side are two blocks
        // (owner, 2026-09-23). From the fixture's identity rather than from
        // its position, so it keeps its colour when it is moved and does not
        // take another fixture's when one is deleted.
        cell.style.setProperty('--fixture-hue', String(hueOf(fixture.id)));
        cell.append(make('span', 'channel-number', String(address)));
        cell.title = `Channel ${address} \u2014 ${describeTarget(show, fixture.target)}${
          fixture.enabled ? '' : ' (parked)'
        } \u2014 drag to move it along this universe`;
        cell.addEventListener('click', () => {
          this.#selected = fixture.id;
          this.draw();
        });

        // Slid along its universe (owner, 2026-09-23). Taking hold anywhere in
        // the block works, and the block keeps its shape under the pointer:
        // grabbing the fifth channel of a twelve-channel fixture and letting go
        // on channel 100 puts its *fifth* channel there.
        cell.draggable = true;
        cell.addEventListener('dragstart', (event) => {
          this.#selected = fixture.id;
          this.#sliding = {
            id: fixture.id,
            footprint: fixture.footprint,
            grabbed: range === null ? 0 : address - range.first,
          };
          if (event.dataTransfer !== null) {
            event.dataTransfer.effectAllowed = 'move';
            event.dataTransfer.setData('text/plain', describeTarget(show, fixture.target));
          }
        });
        cell.addEventListener('dragend', () => {
          this.#sliding = null;
          this.#clearChannelHighlights();
        });
      }

      cell.addEventListener('dragover', (event) => {
        const carried = this.#dragging;
        const sliding = this.#sliding;
        if (carried === null && sliding === null) {
          return;
        }
        event.preventDefault();
        if (event.dataTransfer !== null) {
          event.dataTransfer.dropEffect = carried === null ? 'move' : 'copy';
        }
        if (carried !== null) {
          this.#highlight(address, carried.personalities[0]?.footprint ?? 1);
        } else if (sliding !== null) {
          this.#highlight(address - sliding.grabbed, sliding.footprint);
        }
      });
      cell.addEventListener('drop', (event) => {
        event.preventDefault();
        this.#clearChannelHighlights();
        const carried = this.#dragging;
        const sliding = this.#sliding;
        if (carried !== null) {
          this.#patchAt(show, carried, address);
          return;
        }
        if (sliding !== null) {
          this.#slideTo(show, sliding, address);
        }
      });

      cells.push(cell);
    }

    // One name per fixture, centred over the block it holds, in a layer above
    // the grid (owner, 2026-09-23). Placed as a percentage of the grid rather
    // than put in a cell: a name in the first cell of a block sits at that
    // block's left edge and runs across its neighbours, which reads as though
    // it belonged to whichever one it happens to end on.
    const names = make('div', 'channel-names');
    const across = CHANNELS_ACROSS;
    for (const fixture of fixtures) {
      const range = channelRange(fixture);
      if (range === null) {
        continue;
      }
      // A block that wraps at the end of a row is drawn once per row it
      // occupies, because a name cannot span a line break and half a name on
      // each of two rows is worse than the same name twice.
      for (let start = range.first; start <= range.last; ) {
        const row = Math.floor((start - 1) / across);
        const column = (start - 1) % across;
        const end = Math.min(range.last, (row + 1) * across);
        const width = end - start + 1;

        const label = make('div', 'channel-name', describeTarget(show, fixture.target));
        label.style.left = `${(column / across) * 100}%`;
        label.style.width = `${(width / across) * 100}%`;
        // Which row it belongs to. Where that row *is* is measured after the
        // cells are in the page, because a guess at the row height is a name
        // sitting a few pixels above the middle of its block — which is what a
        // hardcoded seventeen was against a cell of twenty-six (owner,
        // 2026-09-23).
        label.dataset['row'] = String(row);
        names.append(label);

        start = end + 1;
      }
    }

    this.#ui.channelGrid.replaceChildren(...cells, names);
    this.#placeNames(cells, names);
  }

  /**
   * Puts each fixture's name over the row it belongs to, measured.
   *
   * The grid's rows are whatever the cells and the gaps come to, and that is
   * not a number this file can know: a cell has a minimum height, a font it did
   * not choose and a border. So the first cell of each row is asked where it is
   * and how tall, and the name is put exactly there.
   */
  #placeNames(cells: readonly HTMLElement[], names: HTMLElement): void {
    const grid = this.#ui.channelGrid.getBoundingClientRect();
    for (const label of names.children) {
      if (!(label instanceof HTMLElement)) {
        continue;
      }
      const row = Number(label.dataset['row'] ?? '0');
      const first = cells[row * CHANNELS_ACROSS];
      if (first === undefined) {
        continue;
      }
      const box = first.getBoundingClientRect();
      label.style.top = `${box.top - grid.top}px`;
      label.style.height = `${box.height}px`;
    }
  }

  /**
   * Lights the channels a fixture would take, and says whether it fits.
   *
   * The whole footprint rather than the one cell under the pointer: a twelve
   * channel fixture dropped at 505 does not fit, and the operator should see
   * that before they let go.
   */
  #highlight(address: number, footprint: number): void {
    const last = address + footprint - 1;
    const wrong = address < MIN_DMX_ADDRESS || last > MAX_DMX_ADDRESS;
    for (const node of this.#ui.channelGrid.children) {
      if (!(node instanceof HTMLElement)) {
        continue;
      }
      const at = Number(node.dataset['address']);
      const inside = at >= address && at <= last;
      node.classList.toggle('landing', inside && !wrong);
      node.classList.toggle('landing-wrong', inside && wrong);
    }
  }

  #clearChannelHighlights(): void {
    for (const node of this.#ui.channelGrid.children) {
      node.classList.remove('landing', 'landing-wrong');
    }
  }

  /**
   * Moves a patched fixture along the universe it is already in.
   *
   * The same edit the address field used to make, with the same refusals: an
   * address that does not fit, or one that would overlap something else, is
   * refused with the patch unchanged and the server's own reason shown.
   */
  #slideTo(
    show: Show,
    sliding: { id: string; footprint: number; grabbed: number },
    address: number,
  ): void {
    const first = address - sliding.grabbed;
    if (first < MIN_DMX_ADDRESS || first + sliding.footprint - 1 > MAX_DMX_ADDRESS) {
      this.#deps.note(
        `${sliding.footprint} channels from ${first} do not fit in a ${MAX_DMX_ADDRESS}-channel universe.`,
      );
      return;
    }
    this.#deps.apply(
      moveFixture(show, sliding.id, { universe: this.#showing, startAddress: first }),
    );
    this.#sliding = null;
  }

  /** Patches what is being dragged at the channel it was dropped on. */
  #patchAt(show: Show, patchable: Patchable, address: number): void {
    const personality = patchable.personalities[0];
    if (personality === undefined) {
      return;
    }
    // The same edit the address panel makes. A drop is a gesture for making
    // it, not a second way of making a patch entry: it is refused for the same
    // reasons and carries the same note about what it collides with.
    this.#deps.apply(
      patchFixture(show, this.#deps.catalogue(), personality, patchable.target, {
        universe: this.#showing,
        startAddress: address,
      }),
    );
    this.#dragging = null;
  }

  #drawAdvisories(show: Show | null): void {
    if (!show) {
      this.#ui.advisories.replaceChildren();
      return;
    }
    this.#ui.advisories.replaceChildren(
      ...patchAdvisories(show, this.#deps.catalogue()).map((advisory) => {
        const row = make('li');
        row.append(
          make('code', undefined, advisory.path),
          document.createTextNode(` ${advisory.message}`),
        );
        return row;
      }),
    );
  }

  // --- controls --------------------------------------------------------------
  //
  // The same shapes the rest of the editor uses, so a field here and a field on
  // a layer are the same control.

  #field(
    label: string,
    control: HTMLElement,
    path?: string,
    extra?: HTMLElement,
  ): HTMLElement {
    const wrap = make('div', 'field');
    const labelNode = make('label');
    labelNode.append(make('span', undefined, label));
    if (extra) {
      labelNode.append(extra);
    }

    if (control.id === '') {
      this.#controlSeq += 1;
      control.id = `patch-control-${this.#controlSeq}`;
    }
    labelNode.htmlFor = control.id;
    wrap.append(labelNode, control);

    if (path !== undefined) {
      wrap.dataset['path'] = path;
      for (const reason of this.#deps.problems(path)) {
        wrap.classList.add('wrong');
        wrap.append(make('span', 'why', reason));
      }
    }
    return wrap;
  }

  #inline(label: string, control: HTMLElement): HTMLElement {
    const wrap = make('div', 'inline');
    wrap.append(control, make('span', undefined, label));
    return wrap;
  }

  #toggle(value: boolean, onChange: (value: boolean) => void): HTMLInputElement {
    const input = make('input');
    input.type = 'checkbox';
    input.checked = value;
    input.addEventListener('change', () => onChange(input.checked));
    return input;
  }
}

/** How one personality is named in a chooser's value. */
function key(personality: Personality): string {
  return `${personality.type_id}:${personality.version}`;
}

/**
 * A hue for one fixture, from its identity.
 *
 * So that two blocks side by side in a universe are two blocks (owner,
 * 2026-09-23). From the identity rather than from the position, so a fixture
 * keeps its colour when it is moved along and does not inherit another one's
 * when something is deleted.
 *
 * The golden angle, which is what spreads a sequence of hues as far apart as a
 * sequence can be spread: any run of them lands evenly around the circle rather
 * than clumping the way a plain multiple does.
 */
function hueOf(fixtureId: string): number {
  let hash = 0;
  for (const character of fixtureId) {
    hash = (hash * 31 + character.charCodeAt(0)) % 100_000;
  }
  return Math.round((hash * 137.508) % 360);
}

/**
 * How many channels a row of the universe grid holds.
 *
 * Forty-eight, so 512 channels are eleven rows rather than thirty-two (owner,
 * 2026-09-23). It is here as well as in the stylesheet because the name layer
 * has to work out which row a block starts on, and a second opinion about the
 * number would put every name on the wrong line.
 */
const CHANNELS_ACROSS = 48;
