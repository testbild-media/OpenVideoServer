// Editing a show document: what an edit does, and what the server would
// make of the result.
//
// These run against the editing model rather than the page, in the same spirit
// as the mirror and the view: what matters about an edit is whether the
// document it produces is one the server will take.

import { describe, expect, it } from 'vitest';

import type { Effect, Generator, Layer, ParameterValue, Show } from '@ovs/schema';

import {
  addEffect,
  addLayer,
  assignSlot,
  clearSlot,
  isDirty,
  markSaved,
  moveEffect,
  moveLayer,
  newId,
  mergedForSave,
  openDraft,
  orderedLayers,
  removeLayer,
  renameLayer,
  setEffectParameter,
  setGeneratorParameter,
  setLayer,
  setSlot,
  validate,
} from '../src/shared/editor.js';
import {
  DEFAULT_BLEND_MODE,
  DEFAULT_OPACITY,
  DEFAULT_ROUTE,
  IDENTITY_TRANSFORM,
} from '../src/shared/limits.js';
import { emptyCatalogue, type Catalogue } from '../src/shared/modules.js';

// A catalogue of things this client has never heard of, on purpose: every
// control and every check has to come out of what the server published, so a
// test that used the effects this build happens to ship would prove less.
const KALEIDOSCOPE: Effect = {
  type_id: 'third.party.kaleidoscope',
  version: 2,
  name: 'Kaleidoscope',
  description: 'Mirrors the picture about its centre.',
  parameters: [
    { key: 'turns', name: 'Turns', parameter_type: { kind: 'number', min: 1, max: 8, default: 3 } },
    {
      key: 'tint',
      name: 'Tint',
      parameter_type: { kind: 'colour', default: [1, 1, 1, 1] },
    },
    { key: 'mirror', name: 'Mirror', parameter_type: { kind: 'toggle', default: true } },
  ],
};

const STARFIELD: Generator = {
  type_id: 'third.party.starfield',
  version: 1,
  name: 'Starfield',
  description: 'Stars.',
  parameters: [
    {
      key: 'density',
      name: 'Density',
      parameter_type: { kind: 'number', min: 0, max: 1, default: 0.25 },
    },
  ],
};

function catalogue(): Catalogue {
  return { loaded: true, effects: [KALEIDOSCOPE], generators: [STARFIELD] };
}

function show(overrides: Partial<Show> = {}): Show {
  return {
    id: newId(),
    schema_version: 1,
    name: 'Nachtfahrt',
    created_at: 1_700_000_000_000,
    updated_at: 1_700_000_000_000,
    composition: {
      size: { width: 1920, height: 1080 },
      render_rate: { numerator: 60, denominator: 1 },
    },
    master: {
      default_bpm: 120,
      beats_per_bar: 4,
      default_speed_scale: 1,
      default_dimmer: 1,
      default_crossfader: 0,
      crossfader_curve: 'linear',
    },
    layers: [],
    pieces: [],
    outputs: [],
    settings: { auto_save: true, notes: '' },
    ...overrides,
  };
}

/** Applies an edit that is expected to succeed. */
function must(edit: ReturnType<typeof addLayer>): Show {
  if (!edit.ok) {
    throw new Error(`the edit was refused: ${edit.reason}`);
  }
  return edit.show;
}

describe('a layer added to an empty show', () => {
  it('is valid to the same rules the server applies', () => {
    const added = must(addLayer(show()));
    expect(validate(added, catalogue())).toEqual([]);
  });

  it('starts from the values the server would have given it', () => {
    // `Layer::new` on the server and this must produce the same layer, or the
    // two disagree about a document neither of them changed.
    const layer = must(addLayer(show())).layers[0]!;
    expect(layer.enabled).toBe(true);
    expect(layer.route).toBe(DEFAULT_ROUTE);
    expect(layer.blend_mode).toBe(DEFAULT_BLEND_MODE);
    expect(layer.default_opacity).toBe(DEFAULT_OPACITY);
    expect(layer.default_transform).toEqual(IDENTITY_TRANSFORM);
    expect(layer.slots).toEqual([]);
    expect(layer.effects).toEqual([]);
  });

  it('carries an identifier the server will parse', () => {
    // The server refuses anything that is not a 26-character ULID, so a layer
    // this client adds cannot carry something that merely looks like one.
    const layer = must(addLayer(show())).layers[0]!;
    expect(layer.id).toMatch(/^[0-9A-HJKMNP-TV-Z]{26}$/);
  });

  it('goes on top of the stack', () => {
    const one = must(addLayer(show(), 'Bed'));
    const two = must(addLayer(one, 'Overlay'));
    expect(orderedLayers(two).map((layer) => layer.name)).toEqual(['Bed', 'Overlay']);
  });
});

describe('slot 0', () => {
  it('cannot be assigned, because it is permanently black', () => {
    const withLayer = must(addLayer(show()));
    const layer = withLayer.layers[0]!;
    const refused = assignSlot(withLayer, layer.id, 0, {
      kind: 'media',
      media_item_id: newId(),
    });
    expect(refused.ok).toBe(false);
    expect(refused.ok === false && refused.reason).toContain('permanently Black');
  });

  it('is reported by validation in the words the server uses, if one arrives', () => {
    // A document written elsewhere can still contain one. The editor has to say
    // the same thing about it that the server will.
    const withLayer = must(addLayer(show()));
    const layer = withLayer.layers[0]!;
    const broken: Show = {
      ...withLayer,
      layers: [
        {
          ...layer,
          slots: [
            {
              index: 0,
              source: { kind: 'media', media_item_id: newId() },
              playback_mode: 'loop',
              direction: 'forward',
              transform: { ...IDENTITY_TRANSFORM },
              effects: [],
            },
          ],
        },
      ],
    };

    expect(validate(broken, catalogue())).toContainEqual({
      path: 'layers[0].slots[0].index',
      message: 'slot 0 is permanently Black and cannot be configured',
    });
  });
});

describe('slots', () => {
  it('keep the way they were set up when their source is swapped', () => {
    // An operator changing a clip has not asked to lose the playback mode, the
    // transform or the effect chain they built around it.
    let document = must(addLayer(show()));
    const layer = document.layers[0]!;
    document = must(assignSlot(document, layer.id, 3, { kind: 'media', media_item_id: newId() }));
    document = must(setSlot(document, layer.id, 3, { playback_mode: 'bounce' }));
    document = must(assignSlot(document, layer.id, 3, { kind: 'media', media_item_id: newId() }));

    expect(document.layers[0]!.slots[0]!.playback_mode).toBe('bounce');
  });

  it('run free until somebody says how many beats a pass should take', () => {
    // The owner's model of 2026-09-23. Free run is the default and stays the
    // default: a clip dropped into a slot plays at the speed it was made at.
    // It is also the only setting that is left off the wire, which is why the
    // field is optional and absent means free.
    let document = must(addLayer(show()));
    const layer = document.layers[0]!;
    document = must(assignSlot(document, layer.id, 1, { kind: 'media', media_item_id: newId() }));
    expect(document.layers[0]!.slots[0]!.sync ?? { kind: 'FREE_RUN' }).toEqual({
      kind: 'FREE_RUN',
    });

    document = must(setSlot(document, layer.id, 1, { sync: { kind: 'BEAT_SYNC', beats: 8 } }));
    expect(document.layers[0]!.slots[0]!.sync).toEqual({ kind: 'BEAT_SYNC', beats: 8 });

    document = must(setSlot(document, layer.id, 1, { sync: { kind: 'FREE_RUN' } }));
    expect(document.layers[0]!.slots[0]!.sync).toEqual({ kind: 'FREE_RUN' });
  });

  it('refuse a beat count that is not a whole number of at least one', () => {
    // Zero beats is not a length, and the renderer declines to build a pass out
    // of one — so a slot set this way would never advance, which is a layer
    // showing its placeholder for a reason nobody at the wall can see. Refused
    // where it is entered instead.
    let document = must(addLayer(show()));
    const layer = document.layers[0]!;
    document = must(assignSlot(document, layer.id, 1, { kind: 'media', media_item_id: newId() }));

    for (const beats of [0, -4, 1.5, Number.NaN]) {
      expect(setSlot(document, layer.id, 1, { sync: { kind: 'BEAT_SYNC', beats } }).ok).toBe(
        false,
      );
    }
    // One upwards, with no ceiling but the clip's own length.
    expect(setSlot(document, layer.id, 1, { sync: { kind: 'BEAT_SYNC', beats: 1 } }).ok).toBe(
      true,
    );
    expect(setSlot(document, layer.id, 1, { sync: { kind: 'BEAT_SYNC', beats: 1024 } }).ok).toBe(
      true,
    );
  });

  it('are kept in index order however they were filled', () => {
    let document = must(addLayer(show()));
    const layer = document.layers[0]!;
    for (const index of [7, 2, 13]) {
      document = must(
        assignSlot(document, layer.id, index, { kind: 'media', media_item_id: newId() }),
      );
    }
    expect(document.layers[0]!.slots.map((slot) => slot.index)).toEqual([2, 7, 13]);
  });

  it('can be emptied, and refuse to be emptied twice', () => {
    let document = must(addLayer(show()));
    const layer = document.layers[0]!;
    document = must(assignSlot(document, layer.id, 1, { kind: 'media', media_item_id: newId() }));
    document = must(clearSlot(document, layer.id, 1));
    expect(document.layers[0]!.slots).toEqual([]);
    expect(clearSlot(document, layer.id, 1).ok).toBe(false);
  });

  it('take a generator parameter only inside the range the catalogue declares', () => {
    let document = must(addLayer(show()));
    const layer = document.layers[0]!;
    document = must(
      assignSlot(document, layer.id, 1, {
        kind: 'generator',
        type_id: STARFIELD.type_id,
        version: STARFIELD.version,
        parameters: {},
      }),
    );

    const refused = setGeneratorParameter(document, catalogue(), layer.id, 1, 'density', {
      type: 'float',
      value: 4,
    });
    expect(refused.ok).toBe(false);

    document = must(
      setGeneratorParameter(document, catalogue(), layer.id, 1, 'density', {
        type: 'float',
        value: 0.5,
      }),
    );
    const source = document.layers[0]!.slots[0]!.source;
    expect(source.kind === 'generator' && source.parameters).toEqual({
      density: { type: 'float', value: 0.5 },
    });
  });
});

describe('reordering layers', () => {
  it('keeps their order field consistent', () => {
    // The numbers a document arrived with are the operator's, so moving one
    // layer exchanges two of them rather than renumbering the stack. What has
    // to hold either way is that they stay unique and that they say what the
    // composite sequence is.
    const base = show({
      layers: [
        layerAt('bottom', 1),
        layerAt('middle', 5),
        layerAt('top', 9),
      ],
    });
    expect(validate(base, catalogue())).toEqual([]);

    const moved = must(moveLayer(base, findByName(base, 'bottom'), 'up'));
    const orders = moved.layers.map((layer) => layer.order);
    expect(new Set(orders).size).toBe(orders.length);
    expect(orderedLayers(moved).map((layer) => layer.name)).toEqual(['middle', 'bottom', 'top']);
    expect(orderedLayers(moved).map((layer) => layer.order)).toEqual([1, 5, 9]);
    expect(validate(moved, catalogue())).toEqual([]);
  });

  it('refuses to move the top layer any higher', () => {
    const base = show({ layers: [layerAt('bottom', 0), layerAt('top', 1)] });
    const refused = moveLayer(base, findByName(base, 'top'), 'up');
    expect(refused.ok).toBe(false);
    expect(refused.ok === false && refused.reason).toContain('already on top');
  });

  it('is reported when two layers share a composite order', () => {
    const base = show({ layers: [layerAt('one', 3), layerAt('two', 3)] });
    expect(validate(base, catalogue())).toContainEqual({
      path: 'layers[1].order',
      message:
        'composite order 3 is used by more than one layer, which makes compositing non-deterministic',
    });
  });
});

describe('an effect chain', () => {
  it('takes only effects this server publishes', () => {
    const document = must(addLayer(show()));
    const layer = document.layers[0]!;
    const refused = addEffect(document, catalogue(), { layerId: layer.id }, 'ovs.nothing');
    expect(refused.ok).toBe(false);
    expect(refused.ok === false && refused.reason).toBe('this server does not publish ovs.nothing');
  });

  it('writes the catalogue’s own version number into the instance it adds', () => {
    // The document has the field, so something has to go in it, and the only
    // number that is not invented here is the one the catalogue carries. It is
    // data: the effect is found by its type.
    const document = must(addLayer(show()));
    const layer = document.layers[0]!;
    const withEffect = must(
      addEffect(document, catalogue(), { layerId: layer.id }, KALEIDOSCOPE.type_id),
    );
    const effect = withEffect.layers[0]!.effects[0]!;
    expect(effect.type_id).toBe(KALEIDOSCOPE.type_id);
    expect(effect.version).toBe(KALEIDOSCOPE.version);
  });

  it('adds an effect with no parameter values, so adding one changes nothing', () => {
    // Every published effect is neutral at its defaults, so an effect that has
    // just been added and not touched cannot alter a show that was already
    // right.
    const document = must(addLayer(show()));
    const layer = document.layers[0]!;
    const withEffect = must(
      addEffect(document, catalogue(), { layerId: layer.id }, KALEIDOSCOPE.type_id),
    );
    const effect = withEffect.layers[0]!.effects[0]!;
    expect(effect.enabled).toBe(true);
    expect(effect.parameters).toEqual({});
    expect(validate(withEffect, catalogue())).toEqual([]);
  });

  it('keeps its order when an effect is moved along it', () => {
    let document = must(addLayer(show()));
    const layer = document.layers[0]!;
    const chain = { layerId: layer.id };
    document = must(addEffect(document, catalogue(), chain, KALEIDOSCOPE.type_id));
    document = must(addEffect(document, catalogue(), chain, KALEIDOSCOPE.type_id));
    const [first, second] = document.layers[0]!.effects;

    const moved = must(moveEffect(document, chain, second!.id, 'earlier'));
    expect(moved.layers[0]!.effects.map((effect) => effect.id)).toEqual([second!.id, first!.id]);
    expect(moveEffect(moved, chain, second!.id, 'earlier').ok).toBe(false);
  });

  it('lives on a slot as well as on a layer', () => {
    let document = must(addLayer(show()));
    const layer = document.layers[0]!;
    document = must(assignSlot(document, layer.id, 4, { kind: 'media', media_item_id: newId() }));
    document = must(
      addEffect(document, catalogue(), { layerId: layer.id, slotIndex: 4 }, KALEIDOSCOPE.type_id),
    );
    expect(document.layers[0]!.slots[0]!.effects).toHaveLength(1);
    expect(document.layers[0]!.effects).toHaveLength(0);
  });
});

describe('an effect parameter', () => {
  function chainWithEffect(): { document: Show; chain: { layerId: string }; effectId: string } {
    let document = must(addLayer(show()));
    const layer = document.layers[0]!;
    const chain = { layerId: layer.id };
    document = must(addEffect(document, catalogue(), chain, KALEIDOSCOPE.type_id));
    return { document, chain, effectId: document.layers[0]!.effects[0]!.id };
  }

  it('cannot be set outside the range the catalogue declares', () => {
    const { document, chain, effectId } = chainWithEffect();
    for (const value of [0.5, 9, Number.NaN]) {
      const refused = setEffectParameter(document, catalogue(), chain, effectId, 'turns', {
        type: 'float',
        value,
      });
      expect(refused.ok, `${value} is outside 1..8 and must be refused`).toBe(false);
      expect(refused.ok === false && refused.reason).toContain('between 1 and 8');
    }

    const accepted = setEffectParameter(document, catalogue(), chain, effectId, 'turns', {
      type: 'float',
      value: 6,
    });
    expect(accepted.ok).toBe(true);
  });

  it('cannot be set to a value of the wrong kind', () => {
    const { document, chain, effectId } = chainWithEffect();
    const refused = setEffectParameter(document, catalogue(), chain, effectId, 'mirror', {
      type: 'float',
      value: 1,
    });
    expect(refused.ok).toBe(false);
    expect(refused.ok === false && refused.reason).toContain('on or off');
  });

  it('cannot be set at all if the catalogue never declared it', () => {
    const { document, chain, effectId } = chainWithEffect();
    const refused = setEffectParameter(document, catalogue(), chain, effectId, 'radius_px', {
      type: 'float',
      value: 1,
    });
    expect(refused.ok).toBe(false);
    expect(refused.ok === false && refused.reason).toContain('no parameter radius_px');
  });

  it('goes back to the published default by being removed rather than written', () => {
    // A show that does not mention a parameter is a show that wants what
    // the effect was published with. Writing the default in would be a show
    // pinning a value nobody chose.
    const { document, chain, effectId } = chainWithEffect();
    const set = must(
      setEffectParameter(document, catalogue(), chain, effectId, 'turns', {
        type: 'float',
        value: 6,
      }),
    );
    expect(set.layers[0]!.effects[0]!.parameters).toEqual({ turns: { type: 'float', value: 6 } });

    const cleared = must(setEffectParameter(set, catalogue(), chain, effectId, 'turns', null));
    expect(cleared.layers[0]!.effects[0]!.parameters).toEqual({});
  });

  it('is reported by validation when a document holds one the catalogue refuses', () => {
    const { document } = chainWithEffect();
    const broken: Show = {
      ...document,
      layers: [
        {
          ...document.layers[0]!,
          effects: [
            {
              ...document.layers[0]!.effects[0]!,
              parameters: { turns: { type: 'float', value: 40 } },
            },
          ],
        },
      ],
    };

    expect(validate(broken, catalogue())).toContainEqual({
      path: 'layers[0].effects[0].parameters.turns',
      message: 'must be a number between 1 and 8',
    });
  });

  it('is not judged at all while the catalogues have not been read', () => {
    // An editor that has not been told what this server publishes would
    // otherwise report its own ignorance as a fault in the operator's document.
    const { document } = chainWithEffect();
    expect(validate(document, emptyCatalogue())).toEqual([]);
  });
});

describe('a module saved with another version number', () => {
  // There is one module per type and it has no versions: "TV Glitch V2 ist TV
  // Glitch. Invert V2 ist Invert" (owner, 2026-10-18). A show saved before that
  // can still say `version: 7` where the catalogue says 2, and it is the same
  // module. Every number below is one the catalogue does not carry, so a lookup
  // that still compared versions would find nothing and fail each of these.

  function withVersions(effectVersion: number, generatorVersion: number): Show {
    let document = must(addLayer(show()));
    const layer = document.layers[0]!;
    document = must(
      assignSlot(document, layer.id, 1, {
        kind: 'generator',
        type_id: STARFIELD.type_id,
        version: generatorVersion,
        parameters: {},
      }),
    );
    document = must(addEffect(document, catalogue(), { layerId: layer.id }, KALEIDOSCOPE.type_id));
    const added = document.layers[0]!;
    return {
      ...document,
      layers: [
        { ...added, effects: [{ ...added.effects[0]!, version: effectVersion }] },
      ],
    };
  }

  it('is the module the catalogue publishes, and the document is valid', () => {
    expect(KALEIDOSCOPE.version).not.toBe(7);
    expect(STARFIELD.version).not.toBe(9);
    expect(validate(withVersions(7, 9), catalogue())).toEqual([]);
  });

  it('takes its parameters from that module, effect and generator alike', () => {
    const document = withVersions(7, 9);
    const layer = document.layers[0]!;

    const six: ParameterValue = { type: 'float', value: 6 };
    const effect = must(
      setEffectParameter(
        document,
        catalogue(),
        { layerId: layer.id },
        layer.effects[0]!.id,
        'turns',
        six,
      ),
    );
    expect(effect.layers[0]!.effects[0]!.parameters).toEqual({ turns: six });
    // The number the document arrived with is its own, and an edit leaves it.
    expect(effect.layers[0]!.effects[0]!.version).toBe(7);

    const generator = must(
      setGeneratorParameter(document, catalogue(), layer.id, 1, 'density', {
        type: 'float',
        value: 0.5,
      }),
    );
    const source = generator.layers[0]!.slots[0]!.source;
    expect(source.kind === 'generator' && source.parameters).toEqual({
      density: { type: 'float', value: 0.5 },
    });
  });

  it('is checked against that module, in words that name no version', () => {
    const document = withVersions(7, 9);
    const layer = document.layers[0]!;
    const broken: Show = {
      ...document,
      layers: [
        {
          ...layer,
          effects: [
            { ...layer.effects[0]!, parameters: { radius_px: { type: 'float', value: 1 } } },
          ],
        },
      ],
    };
    expect(validate(broken, catalogue())).toEqual([
      {
        path: 'layers[0].effects[0].parameters.radius_px',
        message: 'third.party.kaleidoscope has no such parameter',
      },
    ]);
  });
});

describe('layer configuration', () => {
  it('refuses an opacity the server would refuse', () => {
    const document = must(addLayer(show()));
    const layer = document.layers[0]!;
    expect(setLayer(document, layer.id, { default_opacity: 1.5 }).ok).toBe(false);
    expect(setLayer(document, layer.id, { default_opacity: -0.1 }).ok).toBe(false);
    expect(setLayer(document, layer.id, { default_opacity: 0.25 }).ok).toBe(true);
  });

  it('refuses a transform that is not a number', () => {
    const document = must(addLayer(show()));
    const layer = document.layers[0]!;
    const refused = setLayer(document, layer.id, {
      default_transform: { ...IDENTITY_TRANSFORM, scale_x: Number.NaN },
    });
    expect(refused.ok).toBe(false);
  });

  it('refuses an empty name, which is what the server says about one', () => {
    const document = must(addLayer(show()));
    const layer = document.layers[0]!;
    expect(renameLayer(document, layer.id, '   ').ok).toBe(false);
  });
});

describe('removing a layer', () => {
  it('takes the patch entries that pointed at it, and says so', () => {
    // A fixture instance whose layer has gone makes the whole show
    // unsaveable, and a patch that quietly lost an entry is a console whose
    // faders have moved on to something else.
    const document = must(addLayer(show()));
    const layer = document.layers[0]!;
    const patched: Show = {
      ...document,
      fixture_patch: [
        {
          id: newId(),
          type_id: 'ovs.layer',
          version: 1,
          target: { kind: 'layer', layer_id: layer.id },
          universe: 0,
          start_address: 1,
          footprint: 12,
          enabled: true,
        },
      ],
    };

    const removed = removeLayer(patched, layer.id);
    expect(removed.ok).toBe(true);
    expect(removed.ok === true && removed.show.fixture_patch).toEqual([]);
    expect(removed.ok === true && removed.note).toContain('DMX patch');
  });
});

describe('a document', () => {
  // A show written by a server this client is not the same age as. The two
  // extra fields are the point: nothing here understands them, and nothing here
  // may lose them.
  const raw = JSON.stringify({
    id: '01HQ0000000000000000000001',
    schema_version: 1,
    name: 'Nachtfahrt',
    created_at: 1_700_000_000_000,
    updated_at: 1_700_000_000_000,
    composition: {
      size: { width: 1920, height: 1080 },
      render_rate: { numerator: 60, denominator: 1 },
      colour_space: 'rec709',
    },
    master: {
      default_bpm: 120,
      beats_per_bar: 4,
      default_speed_scale: 1,
      default_dimmer: 1,
      default_crossfader: 0,
      crossfader_curve: 'linear',
    },
    layers: [
      {
        id: '01HQ0000000000000000000002',
        name: 'Bed',
        order: 0,
        enabled: true,
        route: 'M',
        default_opacity: 1,
        default_transform: { x: 0, y: 0, scale_x: 1, scale_y: 1, rotation_deg: 0 },
        blend_mode: 'normal',
        slots: [],
        effects: [],
        mask: { kind: 'none' },
      },
    ],
    pieces: [],
    outputs: [],
    settings: { auto_save: true, notes: '' },
  });

  it('loaded and saved unchanged is byte-identical', () => {
    const draft = openDraft(JSON.parse(raw) as Show);
    expect(JSON.stringify(draft.show)).toBe(raw);
  });

  it('keeps fields this build does not understand through an edit', () => {
    const draft = openDraft(JSON.parse(raw) as Show);
    const renamed = must(renameLayer(draft.show, draft.show.layers[0]!.id, 'Floor'));

    const asRead = JSON.parse(JSON.stringify(renamed)) as Record<string, unknown>;
    const composition = asRead['composition'] as Record<string, unknown>;
    const layer = (asRead['layers'] as Array<Record<string, unknown>>)[0]!;
    expect(composition['colour_space']).toBe('rec709');
    expect(layer['mask']).toEqual({ kind: 'none' });
    expect(layer['name']).toBe('Floor');
    // The fields are also still in the order they arrived in, which is what
    // makes an unchanged save byte-identical in the first place.
    expect(Object.keys(layer)).toEqual(Object.keys(JSON.parse(raw).layers[0]));
  });

  it('is only dirty once something has changed', () => {
    const draft = openDraft(JSON.parse(raw) as Show);
    expect(isDirty(draft)).toBe(false);

    const edited = { saved: draft.saved, show: must(addLayer(draft.show)) };
    expect(isDirty(edited)).toBe(true);
  });

  it('takes the time of a save from the server rather than from a clock here', () => {
    const draft = openDraft(JSON.parse(raw) as Show);
    const edited = { saved: draft.saved, show: must(addLayer(draft.show)) };

    const saved = markSaved(edited, 1_800_000_000_000);
    expect(saved.show.updated_at).toBe(1_800_000_000_000);
    expect(isDirty(saved)).toBe(false);
  });
});

describe('validation', () => {
  it('reports every problem at once, at the paths the server names', () => {
    const broken = show({
      name: '   ',
      master: {
        default_bpm: 5,
        beats_per_bar: 0,
        default_speed_scale: 100,
        default_dimmer: 2,
        default_crossfader: -1,
        crossfader_curve: 'linear',
      },
      layers: [{ ...layerAt('', 0), default_opacity: 2 }],
    });

    const problems = validate(broken, catalogue());
    const paths = problems.map((problem) => problem.path);
    expect(paths).toContain('name');
    expect(paths).toContain('master.default_bpm');
    expect(paths).toContain('master.beats_per_bar');
    expect(paths).toContain('master.default_speed_scale');
    expect(paths).toContain('master.default_dimmer');
    expect(paths).toContain('master.default_crossfader');
    expect(paths).toContain('layers[0].name');
    expect(paths).toContain('layers[0].default_opacity');
  });
});

describe('an identifier this client mints', () => {
  it('sorts by the time it was minted, as a ULID does', () => {
    const early = newId(1_700_000_000_000);
    const late = newId(1_800_000_000_000);
    expect(early < late).toBe(true);
    expect(early.slice(0, 10) < late.slice(0, 10)).toBe(true);
  });

  it('is not the same one twice', () => {
    const minted = new Set(Array.from({ length: 500 }, () => newId()));
    expect(minted.size).toBe(500);
  });
});

// --- fixtures ----------------------------------------------------------------

function layerAt(name: string, order: number): Show['layers'][number] {
  return {
    id: newId(),
    name,
    order,
    enabled: true,
    route: DEFAULT_ROUTE,
    default_opacity: DEFAULT_OPACITY,
    default_transform: { ...IDENTITY_TRANSFORM },
    blend_mode: DEFAULT_BLEND_MODE,
    slots: [],
    effects: [],
  };
}

function findByName(document: Show, name: string): string {
  const layer = document.layers.find((each) => each.name === name);
  if (!layer) {
    throw new Error(`no layer called ${name}`);
  }
  return layer.id;
}

describe('saving a document somebody else has been editing', () => {
  // The owner, twice on 2026-09-23: "Ein Speichern unter Projekt Killt alles.
  // Slot Belegungen sind auf einmal weg" and then "Projekt Speichern killt
  // immer noch alle nicht gespeicherten Slots".
  //
  // A show is edited in two places at once and always has been: slots are
  // filled on the live screen while the mapping is changed in Settings. The
  // editor read the document when it opened it, and a save that sent that copy
  // back put the document back to the moment it was read.

  /** A layer with a clip in slot 1, as the live screen would have left it. */
  function layerWithSlots(name: string, id: string): Layer {
    return {
      ...must(addLayer(show(), name)).layers[0]!,
      id,
      slots: [
        {
          index: 1,
          source: { kind: 'media', media_item_id: 'CLIP' },
          playback_mode: 'loop',
          direction: 'forward',
          transform: { x: 0, y: 0, scale_x: 1, scale_y: 1, rotation_deg: 0 },
          effects: [],
        },
      ],
      banks: 2,
    };
  }

  it('keeps the slots the live screen filled while the mapping was being changed', () => {
    const opened = show({ layers: [{ ...layerWithSlots('Layer 1', 'L1'), slots: [], banks: 1 }] });
    const draft = openDraft(opened);

    // In Settings: the composition is resized. Nothing about the layers.
    const editing = {
      ...draft,
      show: {
        ...opened,
        composition: { ...opened.composition, size: { width: 1280, height: 720 } },
      },
    };

    // Meanwhile, on the live screen: a clip dropped on a pad and a bank opened.
    const onTheServer = show({
      id: opened.id,
      created_at: opened.created_at,
      layers: [layerWithSlots('Layer 1', 'L1')],
    });

    const sending = mergedForSave(editing, onTheServer);

    expect(sending.composition.size).toEqual({ width: 1280, height: 720 });
    expect(sending.layers[0]?.slots).toHaveLength(1);
    expect(sending.layers[0]?.banks).toBe(2);
  });

  it('keeps them even when the layer list itself was edited', () => {
    // The narrower hole, and the one a section-by-section merge alone would
    // leave: renaming a layer in Settings makes the whole layer list the
    // editor's, stale slots and all. Slots, effects and banks are never the
    // editor's — Settings has no control for any of them.
    const opened = show({ layers: [{ ...layerWithSlots('Layer 1', 'L1'), slots: [], banks: 1 }] });
    const draft = openDraft(opened);

    const renamed = {
      ...draft,
      show: {
        ...opened,
        layers: [{ ...opened.layers[0]!, name: 'Upstage wall' }],
      },
    };

    const onTheServer = show({
      id: opened.id,
      created_at: opened.created_at,
      layers: [layerWithSlots('Layer 1', 'L1')],
    });

    const sending = mergedForSave(renamed, onTheServer);

    expect(sending.layers[0]?.name).toBe('Upstage wall');
    expect(sending.layers[0]?.slots).toHaveLength(1);
    expect(sending.layers[0]?.banks).toBe(2);
  });

  it('takes a section the editor never touched from the server', () => {
    // Somebody patched a fixture from another window. A save from here is not
    // an instruction to undo it.
    const opened = show();
    const draft = openDraft(opened);
    const editing = { ...draft, show: { ...opened, name: 'Nachtfahrt II' } };

    const onTheServer = show({
      id: opened.id,
      created_at: opened.created_at,
      pieces: [],
      settings: { auto_save: false, notes: 'patched from the desk' },
    });

    const sending = mergedForSave(editing, onTheServer);

    expect(sending.name).toBe('Nachtfahrt II');
    expect(sending.settings.notes).toBe('patched from the desk');
  });

  it('a layer the editor made keeps its own empty slots', () => {
    // There is nothing on the server to take them from, and an undefined here
    // would be a document that does not validate.
    const opened = show();
    const draft = openDraft(opened);
    const added = must(addLayer(opened, 'Second wall'));
    const editing = { ...draft, show: added };

    const sending = mergedForSave(editing, show({ id: opened.id, created_at: opened.created_at }));

    expect(sending.layers).toHaveLength(1);
    expect(sending.layers[0]?.slots).toEqual([]);
  });
});
