// Live Mode's contextual work area: what each tab is, and what it refuses to be.
//
// Everything here is the pure half in `shared/work.ts`, driven the way the
// screen drives it. The properties being held are the ones nobody can check by
// looking at a running client during a show:
//
//   - choosing a tab changes nothing about the show;
//   - a tab named after show configuration shows it and does not write it;
//   - a tab whose endpoint does not exist says so rather than appearing to work;
//   - the tab an operator had open survives a reconnection, and restoring it
//     sends nothing.
//
// The rest is the reading each tab does, checked against the same answers the
// mapping editor and the renderer give, because a live screen that disagreed
// with either would be describing a wall the operator does not have.

import { describe, expect, it } from 'vitest';

import type { Effect, Generator, MediaItem, Show, ServerMessage } from '@ovs/schema';

import { isLiveCommand } from '../src/shared/live.js';
import { apply, emptyState } from '../src/shared/mirror.js';
import { emptyCatalogue, type Catalogue } from '../src/shared/modules.js';
import { COMPOSITION_SURFACE, MASKED_SURFACE } from '../src/shared/stream.js';
import { BLACK_SLOT_INDEX, MAX_SLOT_INDEX } from '../src/shared/limits.js';
import type { Command, SlotSource } from '../src/shared/types.js';
import { emptyView, toView, type ShowView } from '../src/shared/view.js';
import {
  BROWSER_TABS,
  WORK_TABS,
  matchingNdi,
  ndiTiles,
  afterReconnect,
  chooseTab,
  editTarget,
  dropOnPad,
  effectChains,
  generatorLines,
  dropEffectOnPad,
  effectTiles,
  generatorTiles,
  libraryLines,
  matching,
  matchingEffects,
  matchingGenerators,
  openWorkArea,
  overlapNotes,
  parameterControl,
  sliceLines,
  sliceOutlines,
  sliceSummary,
  wireSource,
} from '../src/shared/work.js';

// --- fixtures -----------------------------------------------------------------

const TRANSFORM = { x: 0, y: 0, scale_x: 1, scale_y: 1, rotation_deg: 0 };

/** A show with a mapping, a generator slot and both kinds of effect chain. */
function show(): Show {
  return {
    id: 'SHOW',
    name: 'Nachtfahrt',
    schema_version: 1,
    created_at: 0,
    updated_at: 0,
    composition: {
      size: { width: 1920, height: 1080 },
      render_rate: { numerator: 60, denominator: 1 },
    },
    layers: [
      {
        id: 'upper',
        name: 'Upper',
        enabled: true,
        order: 5,
        route: 'A',
        blend_mode: 'normal',
        default_opacity: 1,
        default_transform: TRANSFORM,
        effects: [
          {
            id: 'EFFECT1',
            type_id: 'ovs.blur',
            version: 1,
            enabled: true,
            parameters: { radius: { type: 'float', value: 12 } },
          },
        ],
        slots: [
          {
            index: 1,
            source: { kind: 'media', media_item_id: 'MEDIA1' },
            playback_mode: 'loop',
            direction: 'forward',
            transform: TRANSFORM,
            effects: [],
          },
          {
            index: 2,
            source: {
              kind: 'generator',
              instance_id: 'GENERATOR-1',
              type_id: 'ovs.solid_colour',
              version: 1,
              parameters: { colour: { type: 'color', value: [1, 0, 0, 1] } },
            },
            playback_mode: 'loop',
            direction: 'forward',
            transform: TRANSFORM,
            effects: [
              { id: 'EFFECT2', type_id: 'ovs.blur', version: 1, enabled: false, parameters: {} },
            ],
          },
        ],
      },
    ],
    master: {
      default_bpm: 120,
      default_crossfader: 0,
      default_dimmer: 1,
      default_speed_scale: 1,
      beats_per_bar: 4,
      crossfader_curve: 'linear',
    },
    outputs: [
      {
        id: 'OUT1',
        name: 'Wall Left',
        enabled: true,
        identity: {},
        mode: { size: { width: 1920, height: 1080 }, refresh: { numerator: 60, denominator: 1 } },
      },
    ],
    pieces: [
      {
        id: 'PIECE_UNDER',
        name: 'Backdrop',
        output_id: 'OUT1',
        order: 1,
        enabled: true,
        rotation_deg: 0,
        source_rect: { x: 0, y: 0, width: 960, height: 540 },
        dest_rect: { x: 0, y: 0, width: 960, height: 540 },
      },
      {
        id: 'PIECE_OVER',
        name: 'Centre',
        output_id: 'OUT1',
        order: 2,
        enabled: true,
        rotation_deg: 0,
        source_rect: { x: 960, y: 0, width: 960, height: 540 },
        dest_rect: { x: 480, y: 0, width: 960, height: 540 },
      },
      {
        id: 'PIECE_OFF',
        name: 'Spare',
        output_id: 'OUT1',
        order: 3,
        enabled: false,
        rotation_deg: 0,
        source_rect: { x: 0, y: 540, width: 480, height: 270 },
        dest_rect: { x: 0, y: 810, width: 480, height: 270 },
      },
    ],
    fixture_patch: [],
    settings: { auto_save: false },
  };
}

/** The view the window draws, from a snapshot the way the client builds one. */
function loaded(activeSlot = 2): ShowView {
  const state = emptyState();
  apply(state, {
    type: 'snapshot',
    sequence: 1,
    show: show(),
    master: { bpm: 128, crossfader: 0, dimmer: 1, freeze: false, speed_scale: 1 },
    layers: [
      {
        layer_id: 'upper',
        active_slot: activeSlot,
        trigger: 1,
        route: 'A',
        opacity: 1,
        transform: TRANSFORM,
      },
    ],
    renderer: { state: 'running' },
  } satisfies ServerMessage);
  return toView(state);
}

/**
 * The view a client gets when it signs in halfway through a number.
 *
 * The snapshot carries `effects`, because a control source has already moved
 * the layer blur from the 12 the show configured to 40. Everything else is
 * exactly `loaded()`.
 */
function midShow(): ShowView {
  const state = emptyState();
  apply(state, {
    type: 'snapshot',
    sequence: 1,
    show: show(),
    master: { bpm: 128, crossfader: 0, dimmer: 1, freeze: false, speed_scale: 1 },
    layers: [
      {
        layer_id: 'upper',
        active_slot: 2,
        trigger: 1,
        route: 'A',
        opacity: 1,
        transform: TRANSFORM,
      },
    ],
    effects: { EFFECT1: { radius: { type: 'float', value: 40 } } },
    renderer: { state: 'running' },
  } satisfies ServerMessage);
  return toView(state);
}

const BLUR: Effect = {
  type_id: 'ovs.blur',
  version: 1,
  name: 'Blur',
  description: 'Softens the picture.',
  parameters: [
    { key: 'radius', name: 'Radius', parameter_type: { kind: 'number', min: 0, max: 64, default: 8 } },
    { key: 'amount', name: 'Amount', parameter_type: { kind: 'number', min: 0, max: 1, default: 1 } },
  ],
};

const SOLID: Generator = {
  type_id: 'ovs.solid_colour',
  version: 1,
  name: 'Solid colour',
  description: 'One colour, everywhere.',
  parameters: [
    { key: 'colour', name: 'Colour', parameter_type: { kind: 'colour', default: [0, 0, 0, 1] } },
  ],
};

function catalogue(): Catalogue {
  return { loaded: true, effects: [BLUR], generators: [SOLID] };
}

function item(overrides: Partial<MediaItem> = {}): MediaItem {
  return {
    id: 'MEDIA1',
    display_name: 'Kaleidoskop',
    original_filename: 'kaleidoskop.mov',
    kind: 'video',
    state: 'ready',
    has_alpha: false,
    created_at: 10,
    ...overrides,
  };
}

// --- choosing a tab is not doing anything -------------------------------------

describe('choosing a tab', () => {
  it('changes nothing about the show', () => {
    const area = openWorkArea();
    for (const tab of WORK_TABS) {
      expect(chooseTab(area, tab.id).commands).toEqual([]);
    }
  });

  it('opens the tab that was pressed and nothing else', () => {
    // There was a second rail beside the preview until the owner took it out
    // (2026-09-23): between it and the row of surface buttons above the
    // picture, the same question was being asked twice.
    const area = chooseTab(openWorkArea(), 'effects').area;
    expect(area).toEqual({ tab: 'effects' });
  });

  it('never gives a tab a command the live screen is not allowed to send', () => {
    for (const tab of WORK_TABS) {
      for (const name of tab.sends) {
        // A `do` this screen may send is one `isLiveCommand` recognises, which
        // is the run-time half of the promise that nothing here writes to the
        // show document.
        expect(isLiveCommand({ do: name } as Command)).toBe(true);
      }
    }
  });

  it('gives every tab but the live one nothing at all to send', () => {
    const sending = WORK_TABS.filter((tab) => tab.sends.length > 0);
    expect(sending.map((tab) => tab.id)).toEqual(['options']);
    expect(sending.every((tab) => tab.kind === 'live')).toBe(true);
  });

  it('says of each tab which of the kinds it is', () => {
    // The whole point of the catalogue: an operator must be able to read
    // "does this write to my show?" off the tab rather than find out.
    expect(WORK_TABS.map((tab) => [tab.id, tab.kind])).toEqual([
      // Four browsers, each holding one kind of thing and all of them the
      // same gesture: find it, drag it onto a pad (owner, 2026-09-23). That
      // gesture writes to the show, and each says so.
      ['inputs', 'assigns'],
      ['media', 'assigns'],
      ['generators', 'assigns'],
      ['effects', 'assigns'],
      // And one that edits whatever is selected.
      ['options', 'live'],
    ]);
    expect(WORK_TABS.every((tab) => tab.says.length > 0)).toBe(true);
  });

  it('keeps the four browsers apart from the editor', () => {
    // The rail draws them as two groups and they are two groups: a browser
    // offers something to drag, and OPTIONS offers controls for what is
    // already chosen.
    expect(BROWSER_TABS).toEqual(['inputs', 'media', 'generators', 'effects']);
    expect(BROWSER_TABS.every((id) => WORK_TABS.some((tab) => tab.id === id))).toBe(true);
    expect(BROWSER_TABS).not.toContain('options');
  });
});

describe('the tab an operator had open', () => {
  it('survives a reconnection, and restoring it sends nothing', () => {
    const before = chooseTab(openWorkArea(), 'effects').area;
    const after = afterReconnect(before);
    expect(after.area).toEqual(before);
    expect(after.commands).toEqual([]);
  });
});

// --- a tab that shows configuration -------------------------------------------

describe('a tab that shows configuration', () => {
  it('does not write to the show: the most it produces is a request to look at it', () => {
    const view = loaded();
    const target = editTarget(view, { layerId: 'upper', slotIndex: 2, what: 'Upper, slot 2' });
    expect(target).toEqual({
      showId: 'SHOW',
      layerId: 'upper',
      slotIndex: 2,
      what: 'Upper, slot 2',
    });
    // Nothing in it is a value: it names where to look and carries no change.
    expect(Object.keys(target ?? {}).sort()).toEqual(['layerId', 'showId', 'slotIndex', 'what']);
  });

  it('offers no way in at all when the server has no show open', () => {
    expect(editTarget(emptyView(), { pieceId: 'PIECE_OVER', what: 'Centre' })).toBeNull();
  });

  it('leaves the document exactly as it found it, whatever it was asked', () => {
    // Every reader the four tabs use, run over one view, with the document it
    // came from compared before and after. The rule these tabs exist to keep
    // is that reading configuration on the live screen is all they do.
    const before = structuredClone(show());
    const view = loaded();

    sliceLines(view.mapping);
    sliceSummary(view.mapping);
    overlapNotes(view.mapping);
    sliceOutlines(view.mapping, view.composition, COMPOSITION_SURFACE);
    sliceOutlines(view.mapping, view.composition, 'OUT1');
    generatorLines(view.layers, catalogue());
    effectChains(view.layers, catalogue());
    libraryLines([item()], view.layers);
    editTarget(view, { layerId: 'upper', what: 'Upper' });

    expect(show()).toEqual(before);
    // And the view they were handed is untouched too, which is what makes the
    // mirror's copy safe to hand to four tabs at once.
    expect(toView(apply2(show()))).toEqual(loaded());
  });
});

/** The mirror with one show in it, for comparing a view against itself. */
function apply2(open: Show) {
  const state = emptyState();
  apply(state, {
    type: 'snapshot',
    sequence: 1,
    show: open,
    master: { bpm: 128, crossfader: 0, dimmer: 1, freeze: false, speed_scale: 1 },
    layers: [
      {
        layer_id: 'upper',
        active_slot: 2,
        trigger: 1,
        route: 'A',
        opacity: 1,
        transform: TRANSFORM,
      },
    ],
    renderer: { state: 'running' },
  } satisfies ServerMessage);
  return state;
}

// --- which exposed parameters this server publishes a route for ---------------

describe('an exposed generator parameter', () => {
  it('is live state this server publishes a route for, and names the route', () => {
    const control = parameterControl('generator');
    expect(control.live).toBe(true);
    expect(control.endpoint).toBe('PUT /api/v1/live/generators/{generator_instance_id}');
    expect(control.why).toContain('generator.<instance>.parameter.<name>');
    // And says the thing that stops this being a way into the show editor.
    expect(control.why).toContain('never written into it');
  });

  it('reads exactly as an effect parameter does, because it is the same model', () => {
    // The owner's instruction of 2026-09-23: generators get no separate
    // special solution. What that means here is that the line carries the
    // declaration, the configured value, the value in force and whether
    // anybody moved it — the same six fields an effect's does, built by the
    // same function.
    const lines = generatorLines(loaded().layers, catalogue());
    expect(lines).toHaveLength(1);
    expect(lines[0]?.name).toBe('Solid colour');
    expect(lines[0]?.instanceId).toBe('GENERATOR-1');
    expect(lines[0]?.moved).toBe(false);
    expect(
      lines[0]?.parameters.map((parameter) => [
        parameter.key,
        parameter.name,
        parameter.text,
        parameter.configuredText,
        parameter.set,
        parameter.moved,
      ]),
    ).toEqual([['colour', 'Colour', '1, 0, 0, 1', '1, 0, 0, 1', true, false]]);
    // And the declaration travels, because it is what draws the control.
    expect(lines[0]?.parameters[0]?.declared.parameter_type.kind).toBe('colour');
  });

  it('shows a live value over the configured one, and says it was moved', () => {
    const view = loaded();
    const slot = view.layers
      .flatMap((layer) => layer.slots)
      .find((candidate) => candidate.source.kind === 'generator');
    if (slot === undefined || slot.source.kind !== 'generator') {
      throw new Error('the fixture has a generator slot');
    }
    slot.source.live = { colour: { type: 'color', value: [0, 0, 1, 1] } };

    const lines = generatorLines(view.layers, catalogue());
    expect(lines[0]?.moved).toBe(true);
    expect(lines[0]?.parameters[0]?.text).toBe('0, 0, 1, 1');
    // The show's own value is still there beside it, which is what makes
    // the difference legible mid-show and what a reset puts it back to.
    expect(lines[0]?.parameters[0]?.configuredText).toBe('1, 0, 0, 1');
    expect(lines[0]?.parameters[0]?.moved).toBe(true);
  });
});

describe('an exposed effect parameter', () => {
  it('is live state this server publishes a route for, and names the route', () => {
    const control = parameterControl('effect');
    expect(control.live).toBe(true);
    expect(control.endpoint).toBe('PUT /api/v1/live/effects/{effect_instance_id}');
    expect(control.why).toContain('effect.<instance>.parameter.<name>');
    // And says the thing that stops this being a way into the show editor.
    expect(control.why).toContain('never written into it');
  });

  it('falls back to what the module was published with, and marks it as that', () => {
    const chains = effectChains(loaded().layers, catalogue());
    const slotChain = chains.find((chain) => chain.where === 'slot');
    // The slot's blur sets nothing and nobody has moved it, so both parameters
    // are the manifest's own, configured and in force alike.
    expect(
      slotChain?.effects[0]?.parameters.map((parameter) => [
        parameter.key,
        parameter.text,
        parameter.configuredText,
        parameter.set,
        parameter.moved,
      ]),
    ).toEqual([
      ['radius', '8 (0–64)', '8 (0–64)', false, false],
      ['amount', '1 (0–1)', '1 (0–1)', false, false],
    ]);
    expect(slotChain?.effects[0]?.moved).toBe(false);
  });

  it('carries the manifest’s own declaration, which is what draws the control', () => {
    // Not a range this client knows: an effect this build has never heard of
    // arrives with a working control because the catalogue said what it takes.
    const chains = effectChains(loaded().layers, catalogue());
    expect(chains[0]?.effects[0]?.parameters[0]?.declared).toEqual(BLUR.parameters[0]);
  });

  it('is left unread rather than guessed at when this server publishes no manifest for it', () => {
    const chains = effectChains(loaded().layers, emptyCatalogue());
    const layerChain = chains.find((chain) => chain.where === 'layer');
    expect(layerChain?.effects[0]?.published).toBe(false);
    expect(layerChain?.effects[0]?.name).toBe('ovs.blur');
    // No parameters rather than the raw keys: a value without the definition
    // that declared it has no name, no range and no meaning.
    expect(layerChain?.effects[0]?.parameters).toEqual([]);
  });
});

// --- a client that joined in the middle ---------------------------------------

describe('a client joining mid-show', () => {
  it('draws the overrides already in effect, not the values the show configured', () => {
    // The whole reason the snapshot carries them. Without this the operator
    // would be looking at a radius of 12 while the wall showed 40.
    const chains = effectChains(midShow().layers, catalogue());
    const radius = chains[0]?.effects[0]?.parameters[0];

    expect(radius?.text).toBe('40 (0–64)');
    expect(radius?.configuredText).toBe('12 (0–64)');
    expect(radius?.moved).toBe(true);
    expect(chains[0]?.effects[0]?.moved).toBe(true);
  });

  it('leaves a parameter nobody moved where the show put it', () => {
    // An override on one parameter must not quietly return the others to their
    // published defaults, and it must not make them look moved either.
    const amount = effectChains(midShow().layers, catalogue())[0]?.effects[0]?.parameters[1];

    expect(amount?.text).toBe('1 (0–1)');
    expect(amount?.moved).toBe(false);
  });

  it('shows an instance nobody has touched at exactly what the show configured', () => {
    // An instance absent from the map is not an instance with no values; it is
    // one running at what the show configured, which is where they all start.
    const chains = effectChains(midShow().layers, catalogue());
    const slotChain = chains.find((chain) => chain.where === 'slot');

    expect(slotChain?.effects[0]?.moved).toBe(false);
    expect(slotChain?.effects[0]?.parameters.every((parameter) => !parameter.moved)).toBe(true);
  });
});

// --- EFFECTS ------------------------------------------------------------------

describe('the effect chains', () => {
  it('leaves out a layer with nothing on it, and lists a slot chain beside its layer', () => {
    const chains = effectChains(loaded().layers, catalogue());
    expect(chains.map((chain) => [chain.where, chain.slotIndex])).toEqual([
      ['layer', null],
      ['slot', 2],
    ]);
  });

  it('says that a slot chain is configured and that the renderer does not run it', () => {
    const chains = effectChains(loaded().layers, catalogue());
    expect(chains.find((chain) => chain.where === 'layer')?.rendered).toBe(true);
    // Slot chains are persisted, validated and not implemented.
    expect(chains.find((chain) => chain.where === 'slot')?.rendered).toBe(false);
  });

  it('marks an effect the chain skips as off rather than leaving it out', () => {
    const chains = effectChains(loaded().layers, catalogue());
    expect(chains.find((chain) => chain.where === 'slot')?.effects[0]?.enabled).toBe(false);
  });
});

// --- GENERATOR ----------------------------------------------------------------

describe('the generator slots', () => {
  it('says which one the layer is actually playing', () => {
    expect(generatorLines(loaded(2).layers, catalogue())[0]?.onAir).toBe(true);
    expect(generatorLines(loaded(1).layers, catalogue())[0]?.onAir).toBe(false);
  });

  it('reports a type this server does not publish rather than hiding the slot', () => {
    const line = generatorLines(loaded().layers, { loaded: true, effects: [], generators: [] })[0];
    expect(line?.published).toBe(false);
    expect(line?.name).toBe('ovs.solid_colour');
  });
});

// --- BROWSER ------------------------------------------------------------------

describe('the browser', () => {
  it('says where the open show uses an item, and whether it is on air', () => {
    const lines = libraryLines([item()], loaded(1).layers);
    expect(lines[0]?.usedBy).toEqual([
      { layerId: 'upper', layerName: 'Upper', slotIndex: 1, onAir: true },
    ]);
    expect(lines[0]?.onAir).toBe(true);
  });

  it('shows an item no slot plays rather than only what is in tonight’s show', () => {
    const lines = libraryLines([item({ id: 'MEDIA9', display_name: 'Unused' })], loaded().layers);
    expect(lines[0]?.usedBy).toEqual([]);
    expect(lines[0]?.onAir).toBe(false);
  });

  it('puts the newest first, which is the order somebody who has just imported looks in', () => {
    const lines = libraryLines(
      [item({ id: 'OLD', created_at: 1 }), item({ id: 'NEW', created_at: 9 })],
      [],
    );
    expect(lines.map((line) => line.item.id)).toEqual(['NEW', 'OLD']);
  });

  it('narrows to what the operator typed, over the name and the filename', () => {
    // Both, because somebody looking for a clip half-remembers one or the
    // other: what the library calls it, or what the file was called.
    const lines = libraryLines(
      [
        item({ id: 'A', display_name: 'Kaleidoskop', original_filename: 'kal.mov' }),
        item({ id: 'B', display_name: 'Nebel', original_filename: 'fog-loop.mov' }),
      ],
      [],
    );
    expect(matching(lines, 'kaleido').map((line) => line.item.id)).toEqual(['A']);
    expect(matching(lines, 'FOG').map((line) => line.item.id)).toEqual(['B']);
    expect(matching(lines, '  nebel ').map((line) => line.item.id)).toEqual(['B']);
  });

  it('is every line when nothing was typed, in the order it already had', () => {
    const lines = libraryLines([item({ id: 'OLD', created_at: 1 }), item({ id: 'NEW', created_at: 9 })], []);
    expect(matching(lines, '').map((line) => line.item.id)).toEqual(['NEW', 'OLD']);
    expect(matching(lines, '   ')).toEqual(lines);
  });
});

// --- putting something in a slot ----------------------------------------------

describe('dragging a clip onto a pad', () => {
  const clip: SlotSource = { kind: 'media', mediaItemId: 'MEDIA1' };
  const other: SlotSource = { kind: 'media', mediaItemId: 'MEDIA2' };

  it('goes straight in when the pad is empty', () => {
    expect(
      dropOnPad({ slotIndex: 5, holding: null, dropping: clip, holdingName: '' }),
    ).toEqual({ do: 'assign' });
  });

  it('asks first when the pad is taken, and names what is in it', () => {
    // The owner's rule (2026-09-23). A pad in a running show may be the one
    // the operator is about to press.
    expect(
      dropOnPad({ slotIndex: 5, holding: other, dropping: clip, holdingName: 'Nebel' }),
    ).toEqual({ do: 'ask', replacing: 'Nebel' });
  });

  it('does nothing when the pad already plays exactly that', () => {
    // Neither a question nor a request: the operator would be answering a
    // question about replacing something with itself.
    expect(
      dropOnPad({ slotIndex: 5, holding: clip, dropping: clip, holdingName: 'Kaleidoskop' }),
    ).toEqual({ do: 'nothing' });
  });

  it('refuses slot 0, which is not storage', () => {
    const verdict = dropOnPad({
      slotIndex: BLACK_SLOT_INDEX,
      holding: null,
      dropping: clip,
      holdingName: '',
    });
    expect(verdict.do).toBe('refuse');
  });

  it('refuses a slot past the last bank, because the server would', () => {
    const verdict = dropOnPad({
      slotIndex: MAX_SLOT_INDEX + 1,
      holding: null,
      dropping: clip,
      holdingName: '',
    });
    expect(verdict.do).toBe('refuse');
    expect(
      dropOnPad({ slotIndex: MAX_SLOT_INDEX, holding: null, dropping: clip, holdingName: '' }),
    ).toEqual({ do: 'assign' });
  });


  it('offers every generator this server publishes, by name, as something to drag', () => {
    // “Generators are treated like videos: they stand as an icon and drag onto
    // a slot” (owner, 2026-09-23).
    const tiles = generatorTiles({
      loaded: true,
      effects: [],
      generators: [
        { ...SOLID, type_id: 'ovs.noise', name: 'Noise' },
        { ...SOLID, name: 'Gradient', type_id: 'ovs.gradient' },
      ],
    });
    expect(tiles.map((tile) => tile.name)).toEqual(['Gradient', 'Noise']);
  });

  it('carries no instance identity and no copied defaults', () => {
    // The server mints the identity, and every parameter has a default in its
    // own manifest. A client that wrote either into the document would be
    // answering a question the document already settles.
    const tile = generatorTiles(catalogue())[0]!;
    expect(tile.source).toEqual({
      kind: 'generator',
      instanceId: null,
      typeId: 'ovs.solid_colour',
      version: 1,
      parameters: {},
      live: {},
    });
    expect(wireSource(tile.source)).toEqual({
      kind: 'generator',
      type_id: 'ovs.solid_colour',
      version: 1,
    });
  });

  it('searches over the name and the type, and is empty for neither', () => {
    const tiles = generatorTiles(catalogue());
    expect(matchingGenerators(tiles, 'solid')).toHaveLength(1);
    expect(matchingGenerators(tiles, 'ovs.solid')).toHaveLength(1);
    expect(matchingGenerators(tiles, 'kaleido')).toHaveLength(0);
    expect(matchingGenerators(tiles, '')).toEqual(tiles);
  });

  it('offers nothing at all before the catalogue has been fetched', () => {
    // Reporting this client's own ignorance as an empty server.
    expect(generatorTiles(emptyCatalogue())).toEqual([]);
  });


  it('offers every effect this server publishes, by name', () => {
    const tiles = effectTiles(catalogue());
    expect(tiles.map((tile) => [tile.typeId, tile.version])).toEqual([['ovs.blur', 1]]);
    expect(matchingEffects(tiles, 'blur')).toHaveLength(1);
    expect(matchingEffects(tiles, 'ovs.bl')).toHaveLength(1);
    expect(matchingEffects(tiles, 'noise')).toHaveLength(0);
  });

  it('lets an effect land on a pad that has something in it, and nowhere else', () => {
    // An effect belongs to a source. An empty pad has none, and slot 0 is
    // black on every layer and has no chain at all.
    expect(dropEffectOnPad(3, true)).toEqual({ do: 'add' });
    expect(dropEffectOnPad(3, false).do).toBe('refuse');
    expect(dropEffectOnPad(BLACK_SLOT_INDEX, true).do).toBe('refuse');
  });

  it('sends a media source as the document names one, and nothing else', () => {
    expect(wireSource(clip)).toEqual({ kind: 'media', media_item_id: 'MEDIA1' });
  });

  it('leaves a generator’s instance identity to the server', () => {
    // An identity invented here would be a second answer to a question the
    // document already settles: the server mints one for a slot that arrives
    // without it, and a patched fixture points at that one.
    const wire = wireSource({
      kind: 'generator',
      instanceId: null,
      typeId: 'ovs.solid_colour',
      version: 1,
      parameters: {},
      live: {},
    });
    expect(wire).toEqual({ kind: 'generator', type_id: 'ovs.solid_colour', version: 1 });
  });
});

// --- SLICES -------------------------------------------------------------------

describe('the slices', () => {
  it('lists them in the order the renderer draws them, and numbers only the drawn ones', () => {
    const lines = sliceLines(loaded().mapping);
    expect(lines.map((line) => [line.name, line.drawnAt])).toEqual([
      ['Backdrop', 1],
      ['Centre', 2],
      ['Spare', null],
    ]);
  });

  it('says which piece wins where two meet, rather than calling the mapping wrong', () => {
    const notes = overlapNotes(loaded().mapping);
    expect(notes).toHaveLength(1);
    expect(notes[0]).toContain('Centre is drawn over Backdrop');
  });

  it('says whether a piece reaches the wall pixel for pixel', () => {
    const lines = sliceLines(loaded().mapping);
    expect(lines.find((line) => line.name === 'Backdrop')?.pixelExact).toBe(true);
  });

  it('counts what is mapped without inventing a number when nothing is', () => {
    expect(sliceSummary(loaded().mapping)).toBe('3 pieces on 1 output · 2 drawn · 1 overlap');
    expect(sliceSummary(emptyView().mapping)).toBe('This show maps no pieces yet.');
  });

  it('outlines what a piece takes over the composition and what it fills over an output', () => {
    const view = loaded();
    const over = sliceOutlines(view.mapping, view.composition, COMPOSITION_SURFACE);
    // `Centre` takes the right-hand half of a 1920×1080 composition.
    expect(over?.find((outline) => outline.name === 'Centre')).toMatchObject({
      left: 0.5,
      top: 0,
      width: 0.5,
      height: 0.5,
    });

    const onto = sliceOutlines(view.mapping, view.composition, 'OUT1');
    // And fills a rectangle a quarter of the way across that output.
    expect(onto?.find((outline) => outline.name === 'Centre')).toMatchObject({
      left: 0.25,
      top: 0,
      width: 0.5,
      height: 0.5,
    });
  });

  it('outlines the turn each end of a piece is taken and placed at', () => {
    // The owner's report, 2026-09-23: the masked preview was drawing horizontal
    // bars where the mapping has diagonal ones. The overlay over it was doing
    // the same, because the composition side was built with a rotation of zero
    // written into it — the input end of a piece could not turn when that was
    // written, and now it can.
    const view = loaded();
    const piece = view.mapping.pieces.find((each) => each.name === 'Centre');
    if (piece === undefined) {
      throw new Error('the fixture has no piece called Centre');
    }
    piece.source_rotation_deg = 30;
    piece.source_anchor = { x: 0, y: 1 };
    piece.rotation_deg = 12;
    piece.anchor = { x: 1, y: 0 };

    const over = sliceOutlines(view.mapping, view.composition, COMPOSITION_SURFACE);
    expect(over?.find((outline) => outline.name === 'Centre')).toMatchObject({
      rotationDeg: 30,
      anchor: { x: 0, y: 1 },
    });

    // And the output end keeps its own, which is a different turn about a
    // different point: one is where the rectangle is taken from, the other
    // where it lands.
    const onto = sliceOutlines(view.mapping, view.composition, 'OUT1');
    expect(onto?.find((outline) => outline.name === 'Centre')).toMatchObject({
      rotationDeg: 12,
      anchor: { x: 1, y: 0 },
    });
  });

  it('gives a piece that says nothing about turning the centre to turn about', () => {
    // Every document written before either field existed, which must read as
    // "upright, about the middle" rather than as a missing value.
    const view = loaded();
    const over = sliceOutlines(view.mapping, view.composition, COMPOSITION_SURFACE);
    expect(over?.every((outline) => outline.rotationDeg === 0)).toBe(true);
    expect(over?.every((outline) => outline.anchor.x === 0.5 && outline.anchor.y === 0.5)).toBe(
      true,
    );
  });

  it('outlines the same rectangles over the masked composition', () => {
    // The masked surface is the composition with pixels blacked out, so a
    // piece over it is still the region it takes — and the outlines then land
    // exactly on the edges of what survived, which is how an operator sees
    // that the client and the renderer agree about the mapping.
    const view = loaded();
    expect(sliceOutlines(view.mapping, view.composition, MASKED_SURFACE)).toEqual(
      sliceOutlines(view.mapping, view.composition, COMPOSITION_SURFACE),
    );
  });

  it('says it cannot place them rather than drawing an empty overlay', () => {
    const view = loaded();
    // A surface the open show configures nothing on. Null, not an empty
    // list: "nothing is mapped here" and "this client cannot say" are different
    // things and only one of them is about the show.
    expect(sliceOutlines(view.mapping, view.composition, 'OUT_ELSEWHERE')).toBeNull();
    expect(sliceOutlines(view.mapping, null, COMPOSITION_SURFACE)).toBeNull();
    expect(sliceOutlines(view.mapping, null, MASKED_SURFACE)).toBeNull();
  });

  it('marks a switched-off piece as one the renderer does not draw', () => {
    const view = loaded();
    const outlines = sliceOutlines(view.mapping, view.composition, 'OUT1');
    expect(outlines?.find((outline) => outline.name === 'Spare')?.drawn).toBe(false);
    expect(outlines?.find((outline) => outline.name === 'Centre')?.drawn).toBe(true);
  });
});

describe('the network senders an operator can choose from', () => {
  // NDI announces a source as `MACHINE (Source Name)`, and that whole string is
  // the only handle there is: nothing survives a sender restarting except this.
  // So the two halves on a tile are for reading and the show gets the name
  // whole. A client that rebuilt the name from the halves would work until the
  // first source name with a bracket in it.
  it('splits a name for reading and carries it whole into the show', () => {
    const [tile] = ndiTiles(['JK-GAMING-02 (Test Pattern)']);
    expect(tile?.machine).toBe('JK-GAMING-02');
    expect(tile?.stream).toBe('Test Pattern');
    expect(tile?.source).toEqual({ kind: 'ndi', name: 'JK-GAMING-02 (Test Pattern)' });
  });

  it('keeps brackets that belong to the source name', () => {
    const [tile] = ndiTiles(['STUDIO-1 (Camera 2 (wide))']);
    expect(tile?.machine).toBe('STUDIO-1');
    expect(tile?.stream).toBe('Camera 2 (wide)');
    expect(tile?.source).toEqual({ kind: 'ndi', name: 'STUDIO-1 (Camera 2 (wide))' });
  });

  it('still offers a name that is not shaped the usual way', () => {
    // Not a shape any sender has produced. Refusing to show it would mean an
    // operator can see a source in Studio Monitor and not in this client, with
    // nothing anywhere saying why.
    const [tile] = ndiTiles(['just-a-name']);
    expect(tile?.machine).toBe('just-a-name');
    expect(tile?.stream).toBe('just-a-name');
    expect(tile?.source).toEqual({ kind: 'ndi', name: 'just-a-name' });
  });

  it('sorts by name, so the list does not reshuffle between two looks at it', () => {
    const tiles = ndiTiles(['B-MACHINE (Program)', 'A-MACHINE (Program)']);
    expect(tiles.map((tile) => tile.machine)).toEqual(['A-MACHINE', 'B-MACHINE']);
  });

  it('finds a sender by its machine or by its stream', () => {
    // Three machines each sending "Program" is an ordinary rig, and both halves
    // have to be searchable: the machine to see everything one box sends, the
    // stream to see one feed across every box.
    const tiles = ndiTiles([
      'STUDIO-1 (Program)',
      'STUDIO-2 (Program)',
      'STUDIO-1 (Camera 2)',
    ]);
    expect(matchingNdi(tiles, 'studio-1').map((tile) => tile.stream)).toEqual([
      'Camera 2',
      'Program',
    ]);
    expect(matchingNdi(tiles, 'program').map((tile) => tile.machine)).toEqual([
      'STUDIO-1',
      'STUDIO-2',
    ]);
    expect(matchingNdi(tiles, '')).toHaveLength(3);
    expect(matchingNdi(tiles, 'nothing here')).toHaveLength(0);
  });
});
