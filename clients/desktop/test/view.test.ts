// The view the window draws: the show joined to the live state.

import { describe, expect, it } from 'vitest';

import type { LayerLive, Show, ServerMessage } from '@ovs/schema';
import { apply, emptyState } from '../src/shared/mirror.js';
import { toView } from '../src/shared/view.js';

const TRANSFORM = { x: 0, y: 0, scale_x: 1, scale_y: 1, rotation_deg: 0 };

function live(id: string, overrides: Partial<LayerLive> = {}): LayerLive {
  return {
    layer_id: id,
    active_slot: 0,
    opacity: 1,
    route: 'A',
    transform: TRANSFORM,
    trigger: 0,
    ...overrides,
  };
}

function show(): Show {
  return {
    id: 'SHOW',
    name: 'Nachtfahrt',
    schema_version: 1,
    created_at: 0,
    updated_at: 0,
    composition: {
      size: { width: 3840, height: 1080 },
      render_rate: { numerator: 60_000, denominator: 1001 },
    },
    layers: [
      {
        id: 'upper',
        name: 'Upper',
        enabled: true,
        order: 5,
        route: 'A',
        blend_mode: 'add',
        default_opacity: 1,
        default_transform: TRANSFORM,
        effects: [],
        slots: [
          {
            index: 2,
            source: { kind: 'generator', type_id: 'ovs.test_pattern', version: 2, parameters: {} },
            playback_mode: 'loop',
            direction: 'forward',
            transform: TRANSFORM,
            effects: [],
          },
          {
            index: 1,
            source: { kind: 'media', media_item_id: 'MEDIA1' },
            playback_mode: 'loop',
            direction: 'forward',
            transform: TRANSFORM,
            effects: [],
          },
        ],
      },
      {
        id: 'lower',
        name: 'Lower',
        enabled: false,
        order: 1,
        route: 'M',
        blend_mode: 'normal',
        default_opacity: 1,
        default_transform: TRANSFORM,
        effects: [],
        slots: [],
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
    outputs: [],
    pieces: [],
    fixture_patch: [],
    settings: { auto_save: false },
  };
}

function loaded(): ReturnType<typeof emptyState> {
  const state = emptyState();
  apply(state, {
    type: 'snapshot',
    sequence: 1,
    show: show(),
    master: { bpm: 128, crossfader: 0.25, dimmer: 0.8, freeze: false, speed_scale: 1 },
    layers: [
      live('upper', {
        active_slot: 1,
        route: 'B',
        opacity: 0.6,
        trigger: 4,
      }),
      live('lower'),
    ],
    renderer: { state: 'degraded', reason: 'one output is missing' },
  } satisfies ServerMessage);
  return state;
}

describe('the view', () => {
  it('lists layers bottom first, as they composite', () => {
    expect(toView(loaded()).layers.map((layer) => layer.id)).toEqual(['lower', 'upper']);
  });

  it('shows the route the layer is on now, not the one it was saved with', () => {
    // Route is a live control: an operator moves a layer to B during a show and
    // the show keeps what it will open on next time.
    const view = toView(loaded());
    expect(view.layers.find((layer) => layer.id === 'upper')?.route).toBe('B');
  });

  it('marks the slot that is playing, and only that one', () => {
    const upper = toView(loaded()).layers.find((layer) => layer.id === 'upper');
    expect(upper?.slots.map((slot) => [slot.index, slot.active])).toEqual([
      [1, true],
      [2, false],
    ]);
  });

  it('carries what the show opens a layer on beside what the layer is doing', () => {
    // Live Mode has to show both and say which is which: `default_opacity` is
    // not the opacity, and an operator halfway through a show must be able to
    // tell them apart without loading the document.
    const upper = toView(loaded()).layers.find((layer) => layer.id === 'upper');
    expect([upper?.route, upper?.opacity]).toEqual(['B', 0.6]);
    expect(upper?.defaults).toEqual({
      enabled: true,
      route: 'A',
      opacity: 1,
    });
  });

  it('carries the trigger count, because a retrigger moves nothing else', () => {
    // Re-selecting the running slot restarts the clip and the slot index does
    // not change, so this counter is the only thing that says it happened.
    expect(toView(loaded()).layers.find((layer) => layer.id === 'upper')?.trigger).toBe(4);
  });

  it('carries what a slot does at the end of its timeline, to be read and not set', () => {
    const upper = toView(loaded()).layers.find((layer) => layer.id === 'upper');
    expect(upper?.slots.map((slot) => [slot.playbackMode, slot.direction])).toEqual([
      ['loop', 'forward'],
      ['loop', 'forward'],
    ]);
  });

  it('carries what the master returns to, which is not what it is at', () => {
    const view = toView(loaded());
    expect(view.master?.crossfader).toBe(0.25);
    expect(view.masterDefaults).toEqual({
      bpm: 120,
      crossfader: 0,
      dimmer: 1,
      speedScale: 1,
      beatsPerBar: 4,
    });
  });

  it('has no master defaults before a show is open, rather than inventing some', () => {
    const state = emptyState();
    apply(state, {
      type: 'snapshot',
      sequence: 2,
      master: { bpm: 120, crossfader: 0, dimmer: 1, freeze: false, speed_scale: 1 },
      layers: [],
      renderer: { state: 'running' },
    } satisfies ServerMessage);
    expect(toView(state).masterDefaults).toBeNull();
  });

  it('carries the composition size, for a preview that has to keep its shape', () => {
    expect(toView(loaded()).composition).toEqual({ width: 3840, height: 1080 });
  });

  it('carries why the renderer is unhappy', () => {
    expect(toView(loaded()).renderer).toEqual({
      state: 'degraded',
      reason: 'one output is missing',
    });
  });

  it('is empty, not broken, before a show is open', () => {
    const state = emptyState();
    apply(state, {
      type: 'snapshot',
      sequence: 3,
      master: { bpm: 120, crossfader: 0, dimmer: 1, freeze: false, speed_scale: 1 },
      layers: [],
      renderer: { state: 'running' },
    } satisfies ServerMessage);

    const view = toView(state);
    expect(view.showName).toBeNull();
    expect(view.layers).toEqual([]);
    expect(view.master?.bpm).toBe(120);
  });

  it('leaves out live state for a layer the show does not have', () => {
    const state = loaded();
    state.layers.set('ghost', live('ghost'));
    expect(toView(state).layers.map((layer) => layer.id)).toEqual(['lower', 'upper']);
  });
});

describe("who may choose a layer's slot", () => {
  // The owner, 2026-10-16: "Layer Slotauswahl, pro Layer von DMX oder GUI
  // abschaltbar, also das nur eins schalten kann." The client half of that is
  // one flag on the view, and the matrix refuses a press when it is false.

  it('lets the client choose when the show says nothing', () => {
    // Which is every show written before the setting existed. Adding it must
    // not take a slot button away from anybody.
    const view = toView(loaded());
    for (const layer of view.layers) {
      expect(layer.mayChooseSlot).toBe(true);
    }
  });

  it('lets the client choose when the show says either or client only', () => {
    for (const control of ['either', 'client_only'] as const) {
      const state = loaded();
      state.show!.layers[0]!.slot_control = control;
      expect(toView(state).layers.find((each) => each.id === 'upper')?.mayChooseSlot).toBe(true);
    }
  });

  it("does not when the layer is a console's", () => {
    const state = loaded();
    state.show!.layers[0]!.slot_control = 'desk_only';
    const view = toView(state);
    expect(view.layers.find((each) => each.id === 'upper')?.mayChooseSlot).toBe(false);
    // And only that layer: the setting is per layer, which is the whole point.
    expect(view.layers.find((each) => each.id === 'lower')?.mayChooseSlot).toBe(true);
  });
});
