// The DMX-over-IP patch: what an edit does to it, and what the server would
// make of the result.
//
// These run against the patch model rather than the page, in the same spirit as
// the editor and the mapping tests: what matters about patching a fixture is
// whether the document it produces is one the server will take, and whether an
// operator is told about the one mistake this page exists to catch before the
// save rather than after it.
//
// The personalities below are ones this build has never heard of, on purpose.
// Every channel count, footprint and name has to come out of what the server
// published, so a test using `ovs.master` and its ten channels would prove
// less: it would pass just as well if the numbers were written into the client.

import { describe, expect, it } from 'vitest';

import type { EffectInstance, FixtureInstance, Personality, Show } from '@ovs/schema';

import { newId, removeLayer, validate } from '../src/shared/editor.js';
import {
  DEFAULT_BLEND_MODE,
  DEFAULT_OPACITY,
  DEFAULT_ROUTE,
  IDENTITY_TRANSFORM,
  MAX_DMX_ADDRESS,
  MAX_UNIVERSE,
} from '../src/shared/limits.js';
import { emptyCatalogue } from '../src/shared/modules.js';
import {
  channelRange,
  collides,
  collisions,
  effectPersonalities,
  effectPlaces,
  emptyFixtures,
  fixtureName,
  generatorPersonalities,
  generatorPlaces,
  moveFixture,
  patchAdvisories,
  patchFixture,
  patchProblems,
  readPatch,
  readSuggestion,
  setFixtureEnabled,
  unpatchFixture,
  type FixtureCatalogue,
} from '../src/shared/patch.js';

/** A master personality with a footprint nothing in this build has. */
const DESK_MASTER: Personality = {
  type_id: 'third.party.master',
  version: 3,
  name: 'Desk Master',
  footprint: 7,
  target: 'master',
  channels: [
    {
      offset: 1,
      width: 2,
      name: 'Grand Master',
      control: 'master_dimmer',
      momentary: false,
      encoding: { encoding: 'ranged16', min: 0, max: 1 },
      home: 0,
    },
    {
      offset: 3,
      width: 2,
      name: 'Crossfade',
      control: 'master_crossfader',
      momentary: false,
      encoding: { encoding: 'ranged16', min: 0, max: 1 },
      home: 0,
    },
    {
      offset: 5,
      width: 2,
      name: 'Tempo',
      control: 'master_bpm',
      momentary: false,
      encoding: { encoding: 'ranged16', min: 20, max: 999 },
      home: 0,
    },
    {
      offset: 7,
      width: 1,
      name: 'Tap',
      control: 'master_tap_tempo',
      momentary: true,
      encoding: { encoding: 'switch', threshold: 128 },
      home: 0,
    },
  ],
};

/** A layer personality, twenty channels wide, which is also nothing this build has. */
const DESK_LAYER: Personality = {
  type_id: 'third.party.layer',
  version: 2,
  name: 'Desk Layer',
  footprint: 20,
  target: 'layer',
  channels: [
    {
      offset: 1,
      width: 2,
      name: 'Level',
      control: 'layer_opacity',
      momentary: false,
      encoding: { encoding: 'ranged16', min: 0, max: 1 },
      home: 0,
    },
    {
      offset: 3,
      width: 1,
      name: 'Clip',
      control: 'layer_slot',
      momentary: false,
      encoding: { encoding: 'index8' },
      home: 0,
    },
  ],
};

/**
 * A module personality, which is what an effect's own manifest produces.
 *
 * `target: 'effect'` is the value the server sends for one; the client treats
 * anything that is not the master's or a layer's as a module's, so that a
 * personality for a kind this build has no name for still reaches the patch.
 */
const DESK_EFFECT: Personality = {
  type_id: 'third.party.wobble',
  version: 4,
  name: 'Desk Wobble',
  footprint: 2,
  target: 'effect',
  channels: [
    {
      offset: 1,
      width: 1,
      name: 'Amount',
      control: 'amount',
      momentary: false,
      encoding: { encoding: 'ranged8', min: 0, max: 1 },
      home: 0,
    },
    {
      offset: 2,
      width: 1,
      name: 'On',
      control: 'on',
      momentary: false,
      encoding: { encoding: 'switch', threshold: 128 },
      home: 0,
    },
  ],
};

function catalogue(): FixtureCatalogue {
  return { loaded: true, fixtures: [DESK_MASTER, DESK_LAYER] };
}

/** The same, with a module personality in it. */
function catalogueWithEffect(): FixtureCatalogue {
  return { loaded: true, fixtures: [DESK_MASTER, DESK_LAYER, DESK_EFFECT] };
}

/** One effect instance of a type, with the version number its document was saved with. */
function effectInstance(typeId: string, version: number): EffectInstance {
  return { id: newId(), type_id: typeId, version, enabled: true };
}

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

/** A patch entry written by hand, for documents that arrive already broken. */
function entry(overrides: Partial<FixtureInstance> = {}): FixtureInstance {
  return {
    id: newId(),
    type_id: DESK_LAYER.type_id,
    version: DESK_LAYER.version,
    target: { kind: 'layer', layer_id: newId() },
    universe: 1,
    start_address: 1,
    footprint: DESK_LAYER.footprint,
    enabled: true,
    ...overrides,
  };
}

/** Applies an edit that is expected to succeed. */
function must(edit: ReturnType<typeof patchFixture>): Show {
  if (!edit.ok) {
    throw new Error(`the edit was refused: ${edit.reason}`);
  }
  return edit.show;
}

/** Why an edit was refused, or a failure saying it was not. */
function why(edit: ReturnType<typeof patchFixture>): string {
  if (edit.ok) {
    throw new Error('the edit was applied, and it should not have been');
  }
  return edit.reason;
}

/** Every problem reported at or under one path. */
function problemsAt(document: Show, path: string): string[] {
  return patchProblems(document, catalogue())
    .filter((problem) => problem.path === path || problem.path.startsWith(`${path}.`))
    .map((problem) => problem.message);
}

/** A show with a master and two layers, none of it patched. */
function rig(): Show {
  return show({ layers: [layerAt('Bed', 0), layerAt('Floor', 1)] });
}

describe('two fixtures whose channels meet in one universe', () => {
  it('are both told about it, because either of them may be the one to move', () => {
    // The server reports an overlap once, against the earlier of the two, which
    // is enough for a list and not enough for the row an operator is looking at.
    const layers = rig().layers;
    const document = show({
      layers,
      fixture_patch: [
        entry({ target: { kind: 'layer', layer_id: layers[0]!.id }, universe: 1, start_address: 1 }),
        entry({ target: { kind: 'layer', layer_id: layers[1]!.id }, universe: 1, start_address: 15 }),
      ],
    });

    expect(problemsAt(document, 'fixture_patch[0].start_address')).toEqual([
      'collides with fixture_patch[1] in universe 1',
    ]);
    expect(problemsAt(document, 'fixture_patch[1].start_address')).toEqual([
      'collides with fixture_patch[0] in universe 1',
    ]);
  });

  it('name the channels both of them claim', () => {
    const document = show({
      fixture_patch: [
        entry({ universe: 1, start_address: 1 }),
        entry({ universe: 1, start_address: 15 }),
      ],
    });
    const [overlap] = collisions(document);
    expect(overlap?.region).toEqual({ first: 15, last: 20 });
  });

  it('are left alone when they only meet end to end', () => {
    const first = entry({ universe: 1, start_address: 1 });
    const second = entry({ universe: 1, start_address: 21 });
    expect(collides(first, second)).toBe(false);
    expect(collides(second, first)).toBe(false);
  });

  it('never collide when they are in different universes', () => {
    expect(
      collides(entry({ universe: 1, start_address: 1 }), entry({ universe: 2, start_address: 1 })),
    ).toBe(false);
  });

  it('do not collide while one of them is parked, because a parked entry holds no channels', () => {
    const live = entry({ universe: 1, start_address: 1 });
    const parked = entry({ universe: 1, start_address: 1, enabled: false });
    expect(collides(live, parked)).toBe(false);
    expect(collides(parked, live)).toBe(false);
  });

  it('are reported by the show editor as well, so a refusal is not a surprise', () => {
    // The patch is part of the document, so the editor's own validation has to
    // know about it: an operator who never opens the patch page still has to be
    // told why their save was refused.
    const document = show({
      fixture_patch: [
        entry({ universe: 1, start_address: 1 }),
        entry({ universe: 1, start_address: 15 }),
      ],
    });
    const reported = validate(document, emptyCatalogue(), catalogue()).map(
      (problem) => problem.path,
    );
    expect(reported).toContain('fixture_patch[0].start_address');
    expect(reported).toContain('fixture_patch[1].start_address');
  });
});

describe('a fixture that will not fit where it is put', () => {
  it('is refused when its footprint would run past the last channel of a universe', () => {
    const document = rig();
    const refusal = why(
      patchFixture(document, catalogue(), DESK_LAYER, {
        kind: 'layer',
        layer_id: document.layers[0]!.id,
      }, { universe: 1, startAddress: 500 }),
    );
    // 500 + 20 - 1 is 519, and a universe has 512 channels.
    expect(refusal).toBe(
      'address 500 with a footprint of 20 does not fit in a 512-channel universe',
    );
  });

  it('fits exactly when its last channel is the last of the universe', () => {
    const document = rig();
    const patched = must(
      patchFixture(document, catalogue(), DESK_LAYER, {
        kind: 'layer',
        layer_id: document.layers[0]!.id,
      }, { universe: 1, startAddress: MAX_DMX_ADDRESS - DESK_LAYER.footprint + 1 }),
    );
    expect(channelRange(readPatch(patched)[0]!)).toEqual({ first: 493, last: 512 });
  });

  it('is refused below the first channel, because DMX addressing is one-based', () => {
    const document = rig();
    expect(
      why(
        patchFixture(document, catalogue(), DESK_MASTER, { kind: 'master' }, {
          universe: 1,
          startAddress: 0,
        }),
      ),
    ).toContain('at least 1');
  });

  it('is reported as a fault when a document arrives holding one', () => {
    const document = show({ fixture_patch: [entry({ start_address: 500 })] });
    expect(problemsAt(document, 'fixture_patch[0].start_address')).toEqual([
      'address 500 with a footprint of 20 does not fit in a 512-channel universe',
    ]);
  });
});

describe('a universe number', () => {
  it('is refused at the control when the field could not carry it', () => {
    const document = rig();
    for (const universe of [-1, MAX_UNIVERSE + 1, 1.5]) {
      expect(
        why(
          patchFixture(document, catalogue(), DESK_MASTER, { kind: 'master' }, {
            universe,
            startAddress: 1,
          }),
        ),
      ).toBe('a universe is a whole number between 0 and 65535');
    }
  });

  it('is taken at the ends of the range the document can hold', () => {
    const document = rig();
    for (const universe of [0, MAX_UNIVERSE]) {
      const patched = must(
        patchFixture(document, catalogue(), DESK_MASTER, { kind: 'master' }, {
          universe,
          startAddress: 1,
        }),
      );
      expect(readPatch(patched)[0]?.universe).toBe(universe);
    }
  });
});

describe('the master fixture', () => {
  it('is refused a second time, because a show has exactly one', () => {
    const document = must(
      patchFixture(rig(), catalogue(), DESK_MASTER, { kind: 'master' }, {
        universe: 1,
        startAddress: 1,
      }),
    );
    expect(
      why(
        patchFixture(document, catalogue(), DESK_MASTER, { kind: 'master' }, {
          universe: 2,
          startAddress: 1,
        }),
      ),
    ).toContain('already has a master fixture');
  });

  it("is reported in the server's own words when a document arrives with two", () => {
    const document = show({
      fixture_patch: [
        entry({ type_id: DESK_MASTER.type_id, version: DESK_MASTER.version, target: { kind: 'master' }, footprint: DESK_MASTER.footprint, universe: 1, start_address: 1 }),
        entry({ type_id: DESK_MASTER.type_id, version: DESK_MASTER.version, target: { kind: 'master' }, footprint: DESK_MASTER.footprint, universe: 2, start_address: 1 }),
      ],
    });
    expect(problemsAt(document, 'fixture_patch')).toContain(
      'a show has exactly one master fixture, found 2',
    );
  });
});

describe('a layer fixture', () => {
  it('is refused a second time on one layer', () => {
    // The server would take this; there is one per patched layer, and two of
    // them is two stretches of a console quietly driving the same layer.
    const start = rig();
    const layer = start.layers[0]!;
    const document = must(
      patchFixture(start, catalogue(), DESK_LAYER, { kind: 'layer', layer_id: layer.id }, {
        universe: 1,
        startAddress: 1,
      }),
    );
    expect(
      why(
        patchFixture(document, catalogue(), DESK_LAYER, { kind: 'layer', layer_id: layer.id }, {
          universe: 1,
          startAddress: 101,
        }),
      ),
    ).toContain('Bed already has a fixture');
  });

  it('is refused for a layer this show does not have', () => {
    expect(
      why(
        patchFixture(rig(), catalogue(), DESK_LAYER, { kind: 'layer', layer_id: newId() }, {
          universe: 1,
          startAddress: 1,
        }),
      ),
    ).toBe('that layer is not in this show');
  });

  it('goes with its layer when the layer is removed, and the patch says nothing else', () => {
    // The show editor removes the entry and tells the operator. This page has
    // one fewer row afterwards and no second story to tell about it.
    const start = rig();
    const layer = start.layers[0]!;
    const document = must(
      patchFixture(start, catalogue(), DESK_LAYER, { kind: 'layer', layer_id: layer.id }, {
        universe: 1,
        startAddress: 1,
      }),
    );

    const removed = removeLayer(document, layer.id);
    expect(removed.ok).toBe(true);
    if (!removed.ok) {
      return;
    }
    expect(readPatch(removed.show)).toEqual([]);
    expect(removed.note).toContain('DMX patch');
    expect(patchProblems(removed.show, catalogue())).toEqual([]);
  });

  it('is reported at the path the server names when its layer is not in the document', () => {
    const missing = newId();
    const document = show({
      layers: [],
      fixture_patch: [entry({ target: { kind: 'layer', layer_id: missing } })],
    });
    expect(problemsAt(document, 'fixture_patch[0].target.layer_id')).toEqual([
      `references layer ${missing} which is not in this show`,
    ]);
  });
});

describe('an address suggestion', () => {
  it('is taken from the server as given rather than recomputed', () => {
    // The endpoint answers from the show the server has on disk and knows
    // what is already patched there. A client that worked the address out
    // itself would be a second opinion about the one question the endpoint
    // exists to settle — so this takes 97 even though nothing in the document
    // in hand would suggest it.
    const suggestion = readSuggestion({ universe: 3, start_address: 97 }, DESK_LAYER.footprint);
    expect(suggestion).toEqual({ ok: true, universe: 3, startAddress: 97 });
    if (!suggestion.ok) {
      return;
    }

    const start = rig();
    const patched = must(
      patchFixture(start, catalogue(), DESK_LAYER, {
        kind: 'layer',
        layer_id: start.layers[0]!.id,
      }, { universe: suggestion.universe, startAddress: suggestion.startAddress }),
    );
    expect(readPatch(patched)[0]?.start_address).toBe(97);
    expect(readPatch(patched)[0]?.universe).toBe(3);
  });

  it('is the universe the server answered about, not the one that was asked', () => {
    expect(readSuggestion({ universe: 9, start_address: 1 }, 7)).toEqual({
      ok: true,
      universe: 9,
      startAddress: 1,
    });
  });

  it('says there is no room when the server says there is none', () => {
    // No room is a real answer: a universe has 512 channels and a show can want
    // more than that. Nothing here invents an address instead.
    const answered = readSuggestion({ universe: 1 }, 20);
    expect(answered.ok).toBe(false);
    expect(answered.ok === false && answered.reason).toBe(
      'universe 1 has no room for a 20-channel fixture',
    );
  });
});

describe('moving a fixture', () => {
  it('leaves its channel patch otherwise unchanged', () => {
    const start = rig();
    const patched = must(
      patchFixture(start, catalogue(), DESK_LAYER, {
        kind: 'layer',
        layer_id: start.layers[0]!.id,
      }, { universe: 1, startAddress: 1 }),
    );
    // A field written by a server this client is not the same age as. Moving a
    // fixture is a change to two numbers, and nothing else about the entry —
    // including something nothing here understands — may go with them.
    const before = { ...readPatch(patched)[0]!, park_note: 'spare' } as FixtureInstance;
    const document: Show = { ...patched, fixture_patch: [before] };

    const moved = moveFixture(document, before.id, { universe: 4, startAddress: 200 });
    expect(moved.ok).toBe(true);
    if (!moved.ok) {
      return;
    }
    const after = readPatch(moved.show)[0]!;
    expect(after).toEqual({ ...before, universe: 4, start_address: 200 });
    expect(after.id).toBe(before.id);
    expect(after.footprint).toBe(before.footprint);
    expect(after.target).toEqual(before.target);
    expect(after.enabled).toBe(before.enabled);
  });

  it('is refused when it would run off the end of the universe it is moved to', () => {
    const start = rig();
    const patched = must(
      patchFixture(start, catalogue(), DESK_MASTER, { kind: 'master' }, {
        universe: 1,
        startAddress: 1,
      }),
    );
    const fixture = readPatch(patched)[0]!;
    expect(why(moveFixture(patched, fixture.id, { universe: 1, startAddress: 510 }))).toContain(
      'does not fit in a 512-channel universe',
    );
    // And it did not move a little of the way there.
    expect(readPatch(patched)[0]?.start_address).toBe(1);
  });

  it('says so when it lands on top of something', () => {
    const start = rig();
    const one = must(
      patchFixture(start, catalogue(), DESK_MASTER, { kind: 'master' }, {
        universe: 1,
        startAddress: 1,
      }),
    );
    const two = must(
      patchFixture(one, catalogue(), DESK_LAYER, {
        kind: 'layer',
        layer_id: start.layers[0]!.id,
      }, { universe: 1, startAddress: 100 }),
    );
    const moving = readPatch(two)[1]!;

    const moved = moveFixture(two, moving.id, { universe: 1, startAddress: 5 });
    expect(moved.ok).toBe(true);
    expect(moved.ok === true && moved.note).toContain('overlaps Master');
  });
});

describe('parking a fixture', () => {
  it('frees its channels and leaves it where it is', () => {
    const start = rig();
    const patched = must(
      patchFixture(start, catalogue(), DESK_MASTER, { kind: 'master' }, {
        universe: 2,
        startAddress: 40,
      }),
    );
    const fixture = readPatch(patched)[0]!;

    const parked = setFixtureEnabled(patched, fixture.id, false);
    expect(parked.ok).toBe(true);
    if (!parked.ok) {
      return;
    }
    const after = readPatch(parked.show)[0]!;
    expect(after.enabled).toBe(false);
    expect(after.universe).toBe(2);
    expect(after.start_address).toBe(40);
    expect(collides(after, { ...after, id: newId() })).toBe(false);
  });
});

describe('unpatching a fixture', () => {
  it('takes that entry and nothing else', () => {
    const start = rig();
    const one = must(
      patchFixture(start, catalogue(), DESK_MASTER, { kind: 'master' }, {
        universe: 1,
        startAddress: 1,
      }),
    );
    const two = must(
      patchFixture(one, catalogue(), DESK_LAYER, {
        kind: 'layer',
        layer_id: start.layers[0]!.id,
      }, { universe: 1, startAddress: 100 }),
    );

    const removed = unpatchFixture(two, readPatch(two)[0]!.id);
    expect(removed.ok).toBe(true);
    if (!removed.ok) {
      return;
    }
    expect(readPatch(removed.show)).toHaveLength(1);
    expect(readPatch(removed.show)[0]?.target).toEqual({
      kind: 'layer',
      layer_id: start.layers[0]!.id,
    });
  });
});

describe('what the personality decides', () => {
  it('gives the fixture its footprint, rather than anything written in this client', () => {
    const start = rig();
    const patched = must(
      patchFixture(start, catalogue(), DESK_LAYER, {
        kind: 'layer',
        layer_id: start.layers[0]!.id,
      }, { universe: 0, startAddress: 1 }),
    );
    const fixture = readPatch(patched)[0]!;
    expect(fixture.footprint).toBe(DESK_LAYER.footprint);
    expect(fixture.type_id).toBe(DESK_LAYER.type_id);
    expect(fixture.version).toBe(DESK_LAYER.version);
    expect(channelRange(fixture)).toEqual({ first: 1, last: 20 });
  });

  it('refuses one this server does not publish', () => {
    const invented: Personality = { ...DESK_LAYER, type_id: 'nobody.publishes.this' };
    expect(
      why(
        patchFixture(rig(), catalogue(), invented, {
          kind: 'layer',
          layer_id: rig().layers[0]!.id,
        }, { universe: 1, startAddress: 1 }),
      ),
    ).toBe('this server does not publish nobody.publishes.this');
  });

  it('refuses one that cannot drive what it is pointed at', () => {
    // A layer personality aimed at the master would give one layer's level
    // fader the grand master.
    expect(
      why(
        patchFixture(rig(), catalogue(), DESK_LAYER, { kind: 'master' }, {
          universe: 1,
          startAddress: 1,
        }),
      ),
    ).toBe('third.party.layer cannot drive what this patch entry points at');
  });

  it('reports a footprint that disagrees with what the server published', () => {
    // A patch that disagrees with the personality means every fixture after it
    // is offset, and the first anyone knows of it is a fader doing the wrong
    // thing during a show.
    const document = show({ fixture_patch: [entry({ footprint: 14 })] });
    expect(problemsAt(document, 'fixture_patch[0].footprint')).toEqual([
      'third.party.layer occupies 20 channels, not 14',
    ]);
  });

  it('reads an entry saved with another version number as the one personality of its type', () => {
    // There is one module per type and no versions of it (owner, 2026-10-18).
    // An entry saved as `version: 9` where the catalogue says 2 is that same
    // personality: named by it and checked against its footprint, not reported
    // as something this build does not publish.
    expect(DESK_LAYER.version).not.toBe(9);
    const saved = entry({ version: 9, footprint: 14 });
    const document = show({ fixture_patch: [saved] });
    expect(problemsAt(document, 'fixture_patch[0].type_id')).toEqual([]);
    expect(problemsAt(document, 'fixture_patch[0].footprint')).toEqual([
      'third.party.layer occupies 20 channels, not 14',
    ]);
    expect(fixtureName(saved, catalogue())).toBe(DESK_LAYER.name);
  });

  it('reports a personality this build does not have at all', () => {
    const document = show({
      fixture_patch: [entry({ type_id: 'gone.away', version: 9, footprint: 4 })],
    });
    expect(problemsAt(document, 'fixture_patch[0].type_id')).toEqual([
      'this build does not publish gone.away',
    ]);
  });

  it('says nothing at all before the catalogue has arrived', () => {
    // Reporting that a personality is unpublished before the server has said
    // what it publishes would be reporting this client's ignorance as a fault in
    // the operator's document.
    const document = show({
      fixture_patch: [
        entry({ type_id: 'gone.away', version: 9, target: { kind: 'master' } }),
      ],
    });
    expect(patchProblems(document, emptyFixtures())).toEqual([]);
    expect(problemsAt(document, 'fixture_patch[0].type_id')).toEqual([
      'this build does not publish gone.away',
    ]);
  });
});

describe('what is true about a patch and is not a fault', () => {
  it('says a parked fixture holds no channels', () => {
    const start = rig();
    const patched = must(
      patchFixture(start, catalogue(), DESK_MASTER, { kind: 'master' }, {
        universe: 1,
        startAddress: 1,
      }),
    );
    const parked = setFixtureEnabled(patched, readPatch(patched)[0]!.id, false);
    expect(parked.ok).toBe(true);
    if (!parked.ok) {
      return;
    }
    const said = patchAdvisories(parked.show, catalogue()).map(
      (advisory) => advisory.message,
    );
    expect(said.some((message) => message.includes('parked'))).toBe(true);
  });

  it('says which layers a console cannot reach, and that nothing reaches the master', () => {
    const start = rig();
    const patched = must(
      patchFixture(start, catalogue(), DESK_LAYER, {
        kind: 'layer',
        layer_id: start.layers[0]!.id,
      }, { universe: 1, startAddress: 1 }),
    );
    const said = patchAdvisories(patched, catalogue()).map((advisory) => advisory.message);
    expect(said.some((message) => message.includes('Floor has no fixture'))).toBe(true);
    expect(said.some((message) => message.includes('nothing is patched to the master'))).toBe(
      true,
    );
    expect(said.some((message) => message.includes('Bed has no fixture'))).toBe(false);
  });

  it('says nothing at all about a show that patches nothing', () => {
    // Plenty of shows never see a console, and telling one of them that none of
    // its layers is patched would be a page of complaints about a decision
    // nobody made.
    expect(patchAdvisories(rig(), catalogue())).toEqual([]);
  });
});

describe('a document with a patch in it', () => {
  // A show written by a server this client is not the same age as. The extra
  // field inside the patch entry is the point: nothing here understands it, and
  // nothing here may lose it.
  const raw = JSON.stringify({
    id: '01HQ0000000000000000000001',
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
    fixture_patch: [
      {
        id: '01HQ0000000000000000000003',
        type_id: 'ovs.master',
        version: 1,
        target: { kind: 'master' },
        universe: 0,
        start_address: 1,
        footprint: 10,
        enabled: true,
        park_note: 'spare',
      },
    ],
    settings: { auto_save: true, notes: '' },
  });

  it('loaded and saved unchanged is still byte-identical', () => {
    const document = JSON.parse(raw) as Show;
    expect(JSON.stringify(document)).toBe(raw);
  });

  it('keeps every field of a patch entry through a move', () => {
    const document = JSON.parse(raw) as Show;
    const moved = moveFixture(document, readPatch(document)[0]!.id, {
      universe: 2,
      startAddress: 33,
    });
    expect(moved.ok).toBe(true);
    if (!moved.ok) {
      return;
    }
    const written = JSON.parse(JSON.stringify(moved.show)) as {
      fixture_patch: Array<Record<string, unknown>>;
    };
    expect(written.fixture_patch[0]?.['park_note']).toBe('spare');
    expect(written.fixture_patch[0]?.['universe']).toBe(2);
    expect(written.fixture_patch[0]?.['start_address']).toBe(33);
    // And in the order they arrived in, which is what makes an unchanged save
    // byte-identical in the first place.
    expect(Object.keys(written.fixture_patch[0]!)).toEqual(
      Object.keys(JSON.parse(raw).fixture_patch[0]),
    );
  });
});

describe('what a console can reach on an effect', () => {
  // None of this had a test. The owner, 2026-10-16: "Ich kann nicht einen
  // Effekt mehr per DMX steuern. Die Effekte tauchen im DMX Patch nicht auf.
  // Nur Master Fixture und Layer." The matching rule below is what decides
  // that, and it was only ever exercised by running the client.

  it('offers an effect the personality its own manifest published', () => {
    const found = effectPersonalities(catalogueWithEffect(), 'third.party.wobble');
    expect(found).toEqual([DESK_EFFECT]);
    // By its type: another effect's personality is not this one's.
    expect(effectPersonalities(catalogueWithEffect(), 'third.party.other')).toEqual([]);
  });

  it('drives an instance saved with another version number with the one personality of its type', () => {
    // There is one module per type and no versions of it: "Invert V2 ist
    // Invert" (owner, 2026-10-18). An instance saved as `version: 5` where the
    // personality says 4 is that effect, so it is offered that personality and
    // a patch entry on it is checked against that footprint.
    expect(DESK_EFFECT.version).not.toBe(5);
    const instance = effectInstance('third.party.wobble', 5);
    const document = show({ layers: [{ ...layerAt('Wall', 0), effects: [instance] }] });

    const place = effectPlaces(document)[0]!;
    expect(effectPersonalities(catalogueWithEffect(), place.instance.type_id)).toEqual([
      DESK_EFFECT,
    ]);

    const patched: Show = {
      ...document,
      fixture_patch: [
        entry({
          type_id: 'third.party.wobble',
          version: 5,
          target: { kind: 'effect', effect_instance_id: instance.id },
          footprint: 3,
        }),
      ],
    };
    expect(patchProblems(patched, catalogueWithEffect())).toEqual([
      {
        path: 'fixture_patch[0].footprint',
        message: 'third.party.wobble occupies 2 channels, not 3',
      },
    ]);
  });

  it('never offers the master or a layer personality to an effect', () => {
    // The catalogue is one list. Without the target filter a desk's Grand
    // Master would appear as something to patch onto an effect.
    expect(effectPersonalities(catalogue(), 'third.party.layer')).toEqual([]);
    expect(effectPersonalities(catalogue(), 'third.party.master')).toEqual([]);
  });

  it('has nothing to offer when the catalogue carries no module personalities', () => {
    // Which is what a server that publishes none looks like, and is a
    // different thing from the show having no effects in it.
    expect(effectPersonalities(catalogue(), 'third.party.wobble')).toEqual([]);
    // And a catalogue that has not been fetched at all, which is the state a
    // client is in before it has signed in.
    expect(effectPersonalities(emptyFixtures(), 'third.party.wobble')).toEqual([]);
  });

  it('finds an effect whether it sits on the layer or in a slot', () => {
    // Both chains, because an operator who put theirs on a slot and found
    // nothing in the patch would reasonably conclude slots cannot be patched.
    const onLayer = effectInstance('third.party.wobble', 4);
    const onSlot = effectInstance('third.party.wobble', 4);
    const layer = { ...layerAt('Wall', 0), effects: [onLayer] };
    layer.slots = [
      {
        index: 3,
        effects: [onSlot],
        source: { kind: 'generator', type_id: 'ovs.noise', version: 1, instance_id: newId() },
      } as unknown as Show['layers'][number]['slots'][number],
    ];

    const places = effectPlaces(show({ layers: [layer] }));
    expect(places.map((place) => place.instance.id)).toEqual([onLayer.id, onSlot.id]);
    expect(places[0]?.slotIndex).toBeUndefined();
    expect(places[1]?.slotIndex).toBe(3);
  });

  it('says nothing is patchable only when the show has no effects at all', () => {
    // The answer to the owner's report: the master and the layers are in every
    // document, an effect is in one only once somebody puts it there. An empty
    // list of places is a show without effects, not a broken catalogue.
    expect(effectPlaces(show({ layers: [layerAt('Wall', 0)] }))).toEqual([]);
  });
});

describe('what a console can reach on a generator in a slot', () => {
  // The gap the owner found, 2026-10-16: "jetzt gerade sehe ich nur layer 2
  // slot 1 im dmx patch obwohl ich ja auf layer 3 den slots auch ganz viel
  // habe". Nine generators publish a personality and the patch page offered
  // none of them. Everything else for them already worked, which is why
  // nothing failed -- there was simply never an entry to click.

  const WOBBLE: Personality = { ...DESK_EFFECT, target: 'generator' };

  function slotWith(index: number, source: Record<string, unknown>) {
    return { index, effects: [], source } as unknown as Show['layers'][number]['slots'][number];
  }

  it('offers a generator the personality its own manifest published', () => {
    const catalogue: FixtureCatalogue = { loaded: true, fixtures: [DESK_MASTER, WOBBLE] };
    expect(generatorPersonalities(catalogue, 'third.party.wobble')).toEqual([WOBBLE]);
    expect(generatorPersonalities(catalogue, 'third.party.other')).toEqual([]);
    // The master is in the same list and is never a generator's.
    expect(generatorPersonalities(catalogue, 'third.party.master')).toEqual([]);
  });

  it('finds every generator in every slot', () => {
    const layer = layerAt('Wall', 0);
    layer.slots = [
      slotWith(1, { kind: 'generator', type_id: 'ovs.noise', version: 1, instance_id: 'A' }),
      slotWith(2, { kind: 'generator', type_id: 'ovs.strobe', version: 2, instance_id: 'B' }),
      slotWith(3, { kind: 'media', media_item_id: 'C' }),
    ];

    const places = generatorPlaces(show({ layers: [layer] }));
    expect(places.map((place) => [place.typeId, place.instanceId, place.slotIndex])).toEqual([
      ['ovs.noise', 'A', 1],
      ['ovs.strobe', 'B', 2],
    ]);
    // A clip is not a module and has no personality of its own.
    expect(places).toHaveLength(2);
  });

  it('has nothing to point at until the server has minted an identity', () => {
    // A show written before generators had one. The server mints it on the
    // next save; until then a patch entry would name something that is not in
    // the document.
    const layer = layerAt('Wall', 0);
    layer.slots = [slotWith(1, { kind: 'generator', type_id: 'ovs.noise', version: 1 })];
    expect(generatorPlaces(show({ layers: [layer] }))[0]?.instanceId).toBe('');
  });

  it('patches a strobe saved as version 2 with the one Strobe the catalogue publishes', () => {
    // There is no Strobe v2 beside a Strobe v1: one module per type, and no
    // versions of it (owner, 2026-10-18). A show saved with `version: 2` on the
    // slot and on its patch entry is driven by the personality the catalogue
    // publishes for `ovs.strobe`, whatever number that one carries.
    const strobe: Personality = {
      ...WOBBLE,
      type_id: 'ovs.strobe',
      version: 1,
      name: 'Strobe',
      footprint: 6,
    };
    const catalogue: FixtureCatalogue = { loaded: true, fixtures: [DESK_MASTER, strobe] };

    const layer = layerAt('Wall', 0);
    layer.slots = [
      slotWith(1, { kind: 'generator', type_id: 'ovs.strobe', version: 2, instance_id: 'B' }),
    ];
    const saved = entry({
      type_id: 'ovs.strobe',
      version: 2,
      target: { kind: 'generator', generator_instance_id: 'B' },
      footprint: strobe.footprint,
    });
    const document = show({ layers: [layer], fixture_patch: [saved] });

    const place = generatorPlaces(document)[0]!;
    expect(generatorPersonalities(catalogue, place.typeId)).toEqual([strobe]);
    // Found, so neither "not published" nor anything about its channels.
    expect(patchProblems(document, catalogue)).toEqual([]);
    expect(fixtureName(saved, catalogue)).toBe('Strobe');
  });
});
