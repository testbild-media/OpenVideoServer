// The mirror's one job: never show state it cannot account for.

import { describe, expect, it } from 'vitest';

import type { LayerLive, MasterLive, ParameterValue, Show, ServerMessage } from '@ovs/schema';
import { apply, emptyState, layersInOrder } from '../src/shared/mirror.js';

const MASTER: MasterLive = {
  bpm: 120,
  crossfader: 0,
  dimmer: 1,
  freeze: false,
  speed_scale: 1,
};

function layerLive(id: string, overrides: Partial<LayerLive> = {}): LayerLive {
  return {
    layer_id: id,
    active_slot: 0,
    opacity: 1,
    route: 'A',
    transform: { x: 0, y: 0, scale_x: 1, scale_y: 1, rotation_deg: 0 },
    trigger: 0,
    ...overrides,
  };
}

function show(layerOrder: Array<{ id: string; order: number }>): Show {
  return {
    id: 'SHOW',
    name: 'Show',
    schema_version: 1,
    created_at: 0,
    updated_at: 0,
    composition: {
      size: { width: 1920, height: 1080 },
      render_rate: { numerator: 60, denominator: 1 },
    },
    layers: layerOrder.map(({ id, order }) => ({
      id,
      name: id,
      enabled: true,
      order,
      route: 'A',
      blend_mode: 'normal',
      default_opacity: 1,
      default_transform: { x: 0, y: 0, scale_x: 1, scale_y: 1, rotation_deg: 0 },
      effects: [],
      slots: [],
    })),
    master: {
      default_bpm: 120,
      default_crossfader: 0,
      default_dimmer: 1,
      default_speed_scale: 1,
      beats_per_bar: 4,
      crossfader_curve: 'linear',
    },
    outputs: [],
    pieces: [],
    fixture_patch: [],
    settings: { auto_save: false },
  };
}

function snapshot(
  sequence: number,
  layers: LayerLive[] = [],
  effects?: Record<string, Record<string, ParameterValue>>,
): ServerMessage {
  return {
    type: 'snapshot',
    sequence,
    master: MASTER,
    layers,
    ...(effects === undefined ? {} : { effects }),
    renderer: { state: 'running' },
  };
}

const RADIUS_40: Record<string, ParameterValue> = { radius_px: { type: 'float', value: 40 } };

describe('the mirror', () => {
  it('takes a snapshot whatever sequence it is at', () => {
    // A snapshot is the whole state, so it is the one message that may move the
    // sequence by more than one.
    const state = emptyState();
    expect(apply(state, snapshot(9_000)).kind).toBe('applied');
    expect(state.sequence).toBe(9_000);
    expect(state.master).toEqual(MASTER);
  });

  it('applies an update that follows the last one', () => {
    const state = emptyState();
    apply(state, snapshot(4));

    const applied = apply(state, {
      type: 'update',
      sequence: 5,
      change: { change: 'master_changed', master: { ...MASTER, dimmer: 0.5 } },
    });

    expect(applied.kind).toBe('applied');
    expect(state.sequence).toBe(5);
    expect(state.master?.dimmer).toBe(0.5);
  });

  it('refuses an update that does not follow, and changes nothing', () => {
    // The whole point. A client that applied this would be showing the operator
    // a dimmer the server never set.
    const state = emptyState();
    apply(state, snapshot(4));

    const applied = apply(state, {
      type: 'update',
      sequence: 6,
      change: { change: 'master_changed', master: { ...MASTER, dimmer: 0.25 } },
    });

    expect(applied).toEqual({ kind: 'gap', expected: 5, received: 6 });
    expect(state.sequence).toBe(4);
    expect(state.master?.dimmer).toBe(1);
  });

  it('refuses an update that arrives before any snapshot', () => {
    const state = emptyState();
    const applied = apply(state, {
      type: 'update',
      sequence: 77,
      change: { change: 'renderer_changed', renderer: { state: 'running' } },
    });

    expect(applied.kind).toBe('gap');
    expect(state.renderer).toBeNull();
  });

  it('replaces both the show and the live state when a show is loaded', () => {
    // Loading a show resets the live state to what the restart policy says;
    // a client that kept the old master would be showing yesterday's dimmer.
    const state = emptyState();
    apply(state, snapshot(1, [layerLive('A', { opacity: 0.2 })]));

    apply(state, {
      type: 'update',
      sequence: 2,
      change: {
        change: 'show_loaded',
        show: show([{ id: 'B', order: 0 }]),
        master: { ...MASTER, dimmer: 1 },
        layers: [layerLive('B')],
        effects: {},
        generators: {},
      },
    });

    expect(state.show?.id).toBe('SHOW');
    expect([...state.layers.keys()]).toEqual(['B']);
  });

  it('empties the show when one is closed', () => {
    const state = emptyState();
    apply(state, snapshot(1, [layerLive('A')]));

    apply(state, {
      type: 'update',
      sequence: 2,
      change: { change: 'show_loaded', master: MASTER, layers: [], effects: {}, generators: {} },
    });

    expect(state.show).toBeNull();
    expect(state.layers.size).toBe(0);
  });

  it('ignores messages that are not state', () => {
    const state = emptyState();
    apply(state, snapshot(3));

    const applied = apply(state, { type: 'pong', nonce: 1 });

    expect(applied.kind).toBe('ignored');
    expect(state.sequence).toBe(3);
  });

  it('takes the effect values a client joining mid-show has to see', () => {
    // Without these a late client would draw every effect control at the value
    // the show configured while the wall showed something else.
    const state = emptyState();
    apply(state, snapshot(3, [], { EFFECT1: RADIUS_40 }));

    expect(state.effects.get('EFFECT1')).toEqual(RADIUS_40);
    // An instance nobody has moved has no entry at all, which is not the same
    // as an instance with an empty one.
    expect(state.effects.has('EFFECT2')).toBe(false);
  });

  it('replaces an instance’s values whole rather than merging them', () => {
    // `EffectChanged` carries every live value on that instance, so applying one
    // needs no history and a client cannot end up holding a value nobody sent.
    const state = emptyState();
    apply(state, snapshot(1, [], { EFFECT1: { ...RADIUS_40, amount: { type: 'float', value: 0.5 } } }));

    apply(state, {
      type: 'update',
      sequence: 2,
      change: { change: 'effect_changed', effect_instance_id: 'EFFECT1', overrides: RADIUS_40 },
    });

    expect(state.effects.get('EFFECT1')).toEqual(RADIUS_40);
  });

  it('drops an instance that is back at what the show configured', () => {
    // An empty map is how the server says "no live values here any more", and
    // no live values is an absence rather than an empty entry.
    const state = emptyState();
    apply(state, snapshot(1, [], { EFFECT1: RADIUS_40 }));

    apply(state, {
      type: 'update',
      sequence: 2,
      change: { change: 'effect_changed', effect_instance_id: 'EFFECT1' },
    });

    expect(state.effects.has('EFFECT1')).toBe(false);
  });

  it('holds a generator’s live values in their own map, not the effects’', () => {
    // Two identities of two different types on the server. One map would be
    // the only place in the system where an effect and a generator could
    // collide, and the collision would be silent.
    const state = emptyState();
    apply(state, {
      type: 'update',
      sequence: 1,
      change: {
        change: 'generator_changed',
        generator_instance_id: 'GENERATOR1',
        overrides: RADIUS_40,
      },
    });

    expect(state.generators.get('GENERATOR1')).toEqual(RADIUS_40);
    expect(state.effects.has('GENERATOR1')).toBe(false);
  });

  it('drops a generator that is back at what the show configured', () => {
    const state = emptyState();
    apply(state, {
      type: 'update',
      sequence: 1,
      change: {
        change: 'generator_changed',
        generator_instance_id: 'GENERATOR1',
        overrides: RADIUS_40,
      },
    });

    apply(state, {
      type: 'update',
      sequence: 2,
      change: { change: 'generator_changed', generator_instance_id: 'GENERATOR1' },
    });

    expect(state.generators.has('GENERATOR1')).toBe(false);
  });

  it('takes the effect values from a show load rather than emptying its own', () => {
    // Opening a show returns every effect to what it was configured with
    //. The server says so; a client that cleared its own copy would
    // be a second implementation of that policy.
    const state = emptyState();
    apply(state, snapshot(1, [layerLive('A')], { EFFECT1: RADIUS_40 }));

    apply(state, {
      type: 'update',
      sequence: 2,
      change: {
        change: 'show_loaded',
        show: show([{ id: 'A', order: 0 }]),
        master: MASTER,
        layers: [layerLive('A')],
        effects: {},
        generators: {},
      },
    });

    expect(state.effects.size).toBe(0);
  });

  it('orders layers the way the show composites them', () => {
    const state = emptyState();
    apply(state, snapshot(1, [layerLive('top'), layerLive('bottom')]));
    apply(state, {
      type: 'update',
      sequence: 2,
      change: {
        change: 'show_loaded',
        show: show([
          { id: 'top', order: 10 },
          { id: 'bottom', order: 1 },
        ]),
        master: MASTER,
        layers: [layerLive('top'), layerLive('bottom')],
        effects: {},
        generators: {},
      },
    });

    expect(layersInOrder(state).map((layer) => layer.layer_id)).toEqual(['bottom', 'top']);
  });
});
