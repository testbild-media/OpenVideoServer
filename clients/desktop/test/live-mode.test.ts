// Live Mode: the matrix, the master, and what a control says while nobody has
// answered it.
//
// Everything here is the pure half in `shared/live.ts`, driven the way the
// screen drives it. The properties being held are the ones that cannot be
// checked by looking at a running client: that nothing on this screen can write
// to the show, that a control which has been moved is not a control that has
// landed, and that a value nobody sent is drawn as nothing rather than as zero.
//
// `test/live.test.ts` is a different suite: it runs the client against a real
// `ovsd` and is skipped unless one is named.

import { describe, expect, it } from 'vitest';

import type { LayerLive, ParameterValue, Show, ServerMessage } from '@ovs/schema';
import {
  PendingControls,
  bpmLine,
  controlKey,
  crossfaderLine,
  effectCommand,
  effectReset,
  generatorCommand,
  generatorReset,
  isLiveCommand,
  demandedFps,
  layerCommand,
  masterCommand,
  nearestSpeedScale,
  padsInBank,
  percentLine,
  rateNote,
  readingNote,
  reportedBy,
  slotBanks,
  speedLabel,
  syncLabel,
  SPEED_SCALE_STEPS,
  speedScaleLine,
  stepSlot,
  showTempo,
  valueOf,
  type ControlId,
} from '../src/shared/live.js';
import { MAX_SPEED_SCALE, MIN_SPEED_SCALE } from '../src/shared/limits.js';
import { apply, emptyState } from '../src/shared/mirror.js';
import type { Command } from '../src/shared/types.js';
import { emptyView, toView, type LayerView, type MasterView, type ShowView } from '../src/shared/view.js';

const TRANSFORM = { x: 0, y: 0, scale_x: 1, scale_y: 1, rotation_deg: 0 };

// --- what a server would have sent -------------------------------------------

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

function slot(index: number) {
  return {
    index,
    source: { kind: 'media' as const, media_item_id: `MEDIA${index}` },
    playback_mode: 'loop' as const,
    direction: 'forward' as const,
    transform: TRANSFORM,
    effects: [],
  };
}

/**
 * A show whose layer is saved routed to M, half open, on a four-beat grid and
 * at half speed.
 *
 * Deliberately different from everything the live state below says, because the
 * whole point of the layer status area is that those are two different numbers.
 */
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
        route: 'M',
        blend_mode: 'normal',
        default_opacity: 0.5,
        default_transform: TRANSFORM,
        effects: [],
        slots: [slot(3), slot(1), slot(2)],
      },
      {
        id: 'lower',
        name: 'Lower',
        enabled: false,
        order: 1,
        route: 'A',
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

/** The view the window draws, from a snapshot the way the client builds one. */
function loaded(overrides: Partial<LayerLive> = {}): ShowView {
  const state = emptyState();
  apply(state, {
    type: 'snapshot',
    sequence: 1,
    show: show(),
    master: { bpm: 128, crossfader: 0.25, dimmer: 0.8, freeze: false, speed_scale: 1 },
    layers: [
      live('upper', { active_slot: 2, opacity: 0.9, route: 'B', ...overrides }),
      live('lower'),
    ],
    renderer: { state: 'running' },
  } satisfies ServerMessage);
  return toView(state);
}

function upperOf(view: ShowView): LayerView {
  const layer = view.layers.find((candidate) => candidate.id === 'upper');
  if (layer === undefined) {
    throw new Error('the fixture lost its layer');
  }
  return layer;
}

function master(overrides: Partial<MasterView> = {}): MasterView {
  return {
    bpm: 128,
    crossfader: 0.25,
    dimmer: 0.8,
    speedScale: 1,
    freeze: false,
    dmxEnabled: true,
    ...overrides,
  };
}

/** A view carrying one layer, for reading a layer control against. */
function withLayer(overrides: Partial<LayerView>): ShowView {
  const view = loaded();
  return { ...view, layers: view.layers.map((layer) => (layer.id === 'upper' ? { ...layer, ...overrides } : layer)) };
}

const DIMMER: ControlId = { of: 'master', field: 'dimmer' };
const SLOT: ControlId = { of: 'slot', layerId: 'upper' };
const OPACITY: ControlId = { of: 'layer', layerId: 'upper', field: 'opacity' };
/** One parameter of one effect instance, and the offer to put the instance back. */
const RADIUS: ControlId = { of: 'effect', effectInstanceId: 'EFFECT1', parameter: 'radius_px' };
const AMOUNT: ControlId = { of: 'effect', effectInstanceId: 'EFFECT1', parameter: 'amount' };
const PUT_BACK: ControlId = { of: 'effect', effectInstanceId: 'EFFECT1', parameter: null };

const number = (value: number): ParameterValue => ({ type: 'float', value });

// --- live control is not editing ---------------------------------------------

describe('what the live screen may send', () => {
  it('sends a slot tap as a trigger, and nothing that writes to the show', () => {
    const tap: Command = { do: 'select_slot', layerId: 'upper', slotIndex: 3 };
    expect(isLiveCommand(tap)).toBe(true);

    // The whole of `Command`. Only the five that change live state pass; the
    // one that would change what document the server has open does not, and
    // there is no command on this screen that saves one at all.
    const everything: Command[] = [
      { do: 'select_slot', layerId: 'upper', slotIndex: 1 },
      { do: 'set_layer', layerId: 'upper', opacity: 0.5 },
      { do: 'set_master', dimmer: 0.5 },
      { do: 'tap' },
      { do: 'reset_phase' },
      { do: 'open_show', showId: 'SHOW' },
    ];
    expect(everything.filter(isLiveCommand).map((command) => command.do)).toEqual([
      'select_slot',
      'set_layer',
      'set_master',
      'tap',
      'reset_phase',
    ]);
  });

  it('never produces a command the live screen is not allowed to send', () => {
    const layer = upperOf(loaded());
    const produced = [
      masterCommand(master(), { dimmer: 0.1 }),
      layerCommand(layer, { opacity: 0.1 }),
    ].filter((command) => command !== null);

    expect(produced).toHaveLength(2);
    for (const command of produced) {
      expect(isLiveCommand(command)).toBe(true);
    }
  });
});

// --- the matrix ---------------------------------------------------------------

describe('the matrix', () => {
  it('marks the slot the server says is playing, not the one the show was saved with', () => {
    // A show carries slot assignments and no selection at all, so the only
    // thing that can say which pad is lit is the live state.
    const pads = padsInBank(upperOf(loaded()), slotBanks(loaded().layers)[0]!);
    expect(pads.filter((pad) => pad.active).map((pad) => pad.index)).toEqual([2]);

    const moved = padsInBank(upperOf(loaded({ active_slot: 3 })), slotBanks(loaded().layers)[0]!);
    expect(moved.filter((pad) => pad.active).map((pad) => pad.index)).toEqual([3]);
  });

  it('shows the live route and opacity rather than the show’s defaults', () => {
    const layer = upperOf(loaded());
    expect([layer.route, layer.opacity]).toEqual(['B', 0.9]);
    // The show's own values travel beside them, so the difference is legible
    // without the live screen loading the document.
    expect(layer.defaults).toEqual({
      enabled: true,
      route: 'M',
      opacity: 0.5,
    });
  });

  it('has no black pad, because an empty one does that job now', () => {
    // The owner, 2026-09-23: "Black Existiert in der GUI nicht mehr und wird
    // von MUTE abgelöst… jeder nicht belegte Slot ist ein Black/Full
    // Tranzparent für den Layer."
    //
    // It was a thirteenth pad on every bank, there to take a layer off stage.
    // MUTE does that now, and an empty pad does it for a different reason:
    // there is nothing in it. Slot 0 is still in the model and still a number a
    // DMX console sends — it is only gone from the matrix.
    const layer = upperOf(loaded());
    for (const bank of [
      { first: 1, last: 12, label: '1–12' },
      { first: 13, last: 24, label: '13–24' },
    ]) {
      const pads = padsInBank(layer, bank);
      expect(pads).toHaveLength(12);
      expect(pads[0]?.index).toBe(bank.first);
      expect(pads.some((pad) => pad.black)).toBe(false);
      expect(pads.some((pad) => pad.index === 0)).toBe(false);
    }
  });

  it('draws a pad for a slot nothing is assigned to, and says it is empty', () => {
    const pads = padsInBank(upperOf(loaded()), { first: 1, last: 12, label: '1–12' });
    expect(pads.filter((pad) => !pad.empty).map((pad) => pad.index)).toEqual([1, 2, 3]);
    expect(pads.find((pad) => pad.index === 7)?.empty).toBe(true);
  });

  it('marks the slot that is playing, and only that one', () => {
    // There used to be a second state here, for a slot waiting on a grid
    // boundary. The owner removed the trigger grid on 2026-09-23 — "Trigger
    // Grid weg." — so a pad is on or it is not.
    const pads = padsInBank(upperOf(loaded({ active_slot: 1 })), {
      first: 1,
      last: 12,
      label: '1–12',
    });
    expect(pads.filter((pad) => pad.active).map((pad) => pad.index)).toEqual([1]);
  });

  it('offers as many banks as the show has slots for, and never none', () => {
    expect(slotBanks([]).map((bank) => bank.label)).toEqual(['1–12']);
    expect(slotBanks(loaded().layers).map((bank) => bank.label)).toEqual(['1–12']);

    const far = withLayer({ slots: [{ ...upperOf(loaded()).slots[0]!, index: 30 }] });
    expect(slotBanks(far.layers).map((bank) => bank.label)).toEqual(['1–12', '13–24', '25–36']);
  });

  it('keeps a bank an operator asked for even though nothing is in it yet', () => {
    // The whole reason a layer carries a bank count. Somebody right-clicks for
    // the next group in order to fill it, and a count derived from the slots
    // would take it away again on the next sample (owner, 2026-09-23).
    const asked = loaded();
    const layers = asked.layers.map((layer) => ({ ...layer, banks: 3 }));
    expect(slotBanks(layers).map((bank) => bank.label)).toEqual(['1–12', '13–24', '25–36']);
  });

  it('still shows every slot of a document written before layers had a bank count', () => {
    const far = withLayer({ slots: [{ ...upperOf(loaded()).slots[0]!, index: 30 }] });
    const old = far.layers.map((layer) => ({ ...layer, banks: 1 }));
    expect(slotBanks(old).map((bank) => bank.label)).toEqual(['1–12', '13–24', '25–36']);
  });

  it('steps to the next slot that holds something, and back to black before the first', () => {
    const layer = upperOf(loaded({ active_slot: 1 }));
    expect(stepSlot(layer, 1)).toBe(2);
    expect(stepSlot(layer, -1)).toBe(0);
    expect(stepSlot(upperOf(loaded({ active_slot: 3 })), 1)).toBeNull();
    expect(stepSlot(upperOf(loaded({ active_slot: 0 })), -1)).toBeNull();
  });
});

// --- the layer status area ----------------------------------------------------

describe('the layer status area', () => {
  it('works the show’s tempo out of the master clock and nothing else', () => {
    // 128 BPM at unity is 128, and at 2x it is 256. **There is nothing per
    // layer in it** (owner, 2026-09-23): a layer has no rate, so this is the
    // tempo the whole show counts at and the only number beside a beat-locked
    // slot.
    expect(showTempo(master())).toBe(128);
    expect(showTempo(master({ speedScale: 2 }))).toBe(256);
    expect(showTempo(master({ speedScale: 0.5 }))).toBe(64);
  });

  it('shows no tempo at all when the server has not sent the master clock', () => {
    expect(showTempo(null)).toBeNull();
  });

  it('reads a slot that runs free as FREE and a locked one as its beats', () => {
    expect(syncLabel({ kind: 'FREE_RUN' })).toBe('FREE');
    expect(syncLabel({ kind: 'BEAT_SYNC', beats: 8 })).toBe('8');
    expect(syncLabel({ kind: 'BEAT_SYNC', beats: 1 })).toBe('1');
    // A layer with nothing on stage. Not FREE, which would be a claim about a
    // slot that is not playing, and not 1.
    expect(syncLabel(null)).toBe('—');
  });

  it('names each step of the master scale, and says plainly when it is off them', () => {
    expect(speedLabel(0.5)).toBe('/2');
    expect(speedLabel(1)).toBe('1');
    expect(speedLabel(4)).toBe('x4');
    // A console can put the master between the steps. Snapping it for display
    // would report a speed the show is not running at.
    expect(speedLabel(0.75)).toBe('×0.75');
  });

  it('says what a slot is asking of the decoder, and warns when it is too much', () => {
    // The fault of 2026-09-23, in one assertion: slot 2 of layer 1 held a
    // 74.88-second 4K clip at 25 fps, set to 7 beats, at 120 BPM. That is a
    // pass of 3.5 seconds — twenty-one times the clip's own rate, 535 frames a
    // second — and the wall stood still with nothing anywhere saying why.
    const clip = { durationMs: 74_880, fps: 25 };
    const beatSync = { kind: 'BEAT_SYNC', beats: 7 } as const;

    expect(Math.round(demandedFps(clip, beatSync, 1, 120) ?? 0)).toBe(535);

    const note = rateNote(clip, beatSync, 1, 120);
    expect(note?.strained).toBe(true);
    expect(note?.text).toContain('21x the clip’s own rate'.replace('’', "'"));

    // The same clip on free run is exactly its own rate, which is not a demand
    // and gets no sentence at all.
    expect(rateNote(clip, { kind: 'FREE_RUN' }, 1, 120)).toBeNull();

    // And a slot's own scale is what makes free run faster, since the master's
    // does not reach it: "Free Run ist richtig, da soll die Master Scale nicht
    // greifen, weil 'Free' ist."
    const doubled = rateNote(clip, { kind: 'FREE_RUN' }, 2, 120);
    expect(doubled?.strained).toBe(false);
    expect(doubled?.text).toContain('2x');
  });
});

// --- the master ---------------------------------------------------------------

describe('the master’s controls', () => {
  it('send only what changed', () => {
    const now = master({ dimmer: 0.8, crossfader: 0.25 });
    // The dimmer moved and the crossfader did not, so only the dimmer goes. A
    // client that sent the crossfader too would be fighting whatever is holding
    // it, which during a show is the lighting console.
    expect(masterCommand(now, { dimmer: 0.5, crossfader: 0.25 })).toEqual({
      do: 'set_master',
      dimmer: 0.5,
    });
  });

  it('carry freeze both ways, which is a boolean and not a number', () => {
    // The owner, 2026-09-24: "Freeze funktioniert immer noch nicht." The only
    // test that ever pressed this button lives in the integration suite, which
    // is skipped unless a real server is given — so the whole path could break
    // and nothing here would say so.
    expect(masterCommand(master({ freeze: false }), { freeze: true })).toEqual({
      do: 'set_master',
      freeze: true,
    });
    // And off again.  is a value, not an absence: a command that dropped
    // it would freeze a show that could never be unfrozen.
    expect(masterCommand(master({ freeze: true }), { freeze: false })).toEqual({
      do: 'set_master',
      freeze: false,
    });
  });

  it('send nothing at all when nothing would move', () => {
    expect(masterCommand(master(), { dimmer: 0.8, crossfader: 0.25, freeze: false })).toBeNull();
  });

  it('send what was asked for when the server has not said where anything is', () => {
    // Nothing to compare against is not a reason to send nothing: the operator
    // asked for something and there is no evidence it is already there.
    expect(masterCommand(null, { dimmer: 0.5 })).toEqual({ do: 'set_master', dimmer: 0.5 });
  });

  it('are not fooled into resending by a value that made a round trip through the server', () => {
    // Dimmer and crossfader are f32 on the server and are sent in thousandths,
    // so a value that came back differs in the seventh decimal place.
    expect(masterCommand(master({ dimmer: 0.30000001192092896 }), { dimmer: 0.3 })).toBeNull();
    // One step of the fader is still a change.
    expect(masterCommand(master({ dimmer: 0.3 }), { dimmer: 0.301 })).not.toBeNull();
  });
});

describe('a layer’s controls', () => {
  it('send only what changed', () => {
    const layer = upperOf(loaded());
    expect(layerCommand(layer, { route: 'B', opacity: 0.4 })).toEqual({
      do: 'set_layer',
      layerId: 'upper',
      opacity: 0.4,
    });
    expect(layerCommand(layer, { route: 'B' })).toBeNull();
  });
});

// --- a control that has been moved and not answered ---------------------------

describe('a control that has been moved', () => {
  it('is shown as pending rather than as done until the server answers', () => {
    const pending = new PendingControls();
    pending.asked(DIMMER, 0.5);

    const reading = pending.read<number>(DIMMER, 0.8);
    expect(reading).toEqual({ state: 'pending', value: 0.8, wanted: 0.5 });
    // What the fader shows is where the operator put it; what the line beside it
    // shows is what the server still says.
    expect(valueOf(reading)).toBe(0.5);
    expect(reportedBy(reading)).toBe(0.8);
    expect(percentLine(reading)).toBe('50%');
  });

  it('clears its pending state on the acknowledgement rather than on an echo that may never come', () => {
    // The case this rule exists for: a value the server was already at, or a
    // fader held still. The request is accepted and answered, and nothing is
    // published, because nothing moved. A control that waited for the
    // publication would wait for ever.
    const pending = new PendingControls();
    pending.asked(DIMMER, 0.8);
    expect(pending.read<number>(DIMMER, 0.8).state).toBe('pending');

    pending.acknowledged(DIMMER);

    expect(pending.size).toBe(0);
    expect(pending.read<number>(DIMMER, 0.8)).toEqual({ state: 'live', value: 0.8 });
  });

  it('shows what the server says once its request has been answered, not what was asked', () => {
    const pending = new PendingControls();
    pending.asked(DIMMER, 0.5);
    pending.acknowledged(DIMMER);

    // The mirror has not caught up yet. What is drawn is still the server's own
    // last word rather than this client's request, because a value shown on the
    // strength of having asked for it is a value nobody reported.
    expect(pending.read<number>(DIMMER, 0.8)).toEqual({ state: 'live', value: 0.8 });
  });

  it('is corrected by an echo that carries a value other than the one asked for', () => {
    // A console holding the same fader, or a value the server clamped. The
    // request was accepted, so the control is not waiting; what it shows is
    // whatever the update stream then delivered.
    const pending = new PendingControls();
    pending.asked(DIMMER, 0.5);
    pending.acknowledged(DIMMER);

    expect(pending.read<number>(DIMMER, 0.2)).toEqual({ state: 'live', value: 0.2 });
  });

  it('keeps showing a refusal until the control is moved again', () => {
    const pending = new PendingControls();
    pending.asked(DIMMER, 0.5);
    pending.refused(DIMMER, 'no show is open');

    expect(pending.read<number>(DIMMER, 0.8)).toEqual({
      state: 'refused',
      value: 0.8,
      wanted: 0.5,
      reason: 'no show is open',
    });
    // The value beside it goes on being the server's, so an update about
    // something else is not the server changing its mind.
    expect(pending.read<number>(DIMMER, 0.2).state).toBe('refused');
  });

  it('stays pending for ever against a client that is talking to nothing', () => {
    // The disconnect case, which is the other half of what this mechanism is
    // for: a fader moved while the socket is down must not read as a fader that
    // moved the wall. Nothing answers, so nothing clears it.
    const pending = new PendingControls();
    pending.asked(DIMMER, 0.5);

    expect(pending.read<number>(DIMMER, null).state).toBe('pending');
    expect(pending.size).toBe(1);
  });

  it('is forgotten when the connection is', () => {
    const pending = new PendingControls();
    pending.asked(DIMMER, 0.5);
    pending.asked(OPACITY, 0.2);
    expect(pending.keys()).toHaveLength(2);

    pending.clear();
    expect(pending.size).toBe(0);
  });
});

describe('a slot that has been tapped', () => {
  it('stops being this client’s request when the server accepts the trigger', () => {
    // Between this client asking for a pad and the server saying what happened,
    // the request is this client's own and is drawn as such. Once the server has
    // published the slot, that statement is the server's and this client has
    // nothing left of its own to draw.
    const pending = new PendingControls();
    pending.asked(SLOT, 3);
    expect(pending.read<number>(SLOT, 2).state).toBe('pending');

    pending.acknowledged(SLOT);

    expect(pending.size).toBe(0);
    expect(pending.read<number>(SLOT, 2)).toEqual({ state: 'live', value: 2 });
  });

  it('keeps a refused trigger against the layer it was meant for', () => {
    const pending = new PendingControls();
    pending.asked(SLOT, 3);
    pending.refused(SLOT, 'this layer has no slot 3');

    expect(pending.read<number>(SLOT, 2)).toEqual({
      state: 'refused',
      value: 2,
      wanted: 3,
      reason: 'this layer has no slot 3',
    });
  });
});

// --- an effect's live values --------------------------------------------------

describe('a live effect value', () => {
  it('is sent as a live change and never as a show edit', () => {
    const command = effectCommand('EFFECT1', 'radius_px', number(40));

    expect(isLiveCommand(command)).toBe(true);
    expect(command).toEqual({
      do: 'set_effect',
      effectInstanceId: 'EFFECT1',
      overrides: { radius_px: { type: 'float', value: 40 } },
    });
    // Nothing in it names a show, and the one command that would change what
    // document the server has open is still not one this screen may send. There
    // is no command on this screen that saves one at all.
    expect(isLiveCommand({ do: 'open_show', showId: 'SHOW' })).toBe(false);
  });

  it('names the parameter the operator moved and no other', () => {
    // A request carries only what was touched. One that sent the others would
    // move them on stage, and what it would fight over is whichever one a
    // console is holding.
    const command = effectCommand('EFFECT1', 'amount', number(0.25));
    const overrides = command.do === 'set_effect' ? (command.overrides ?? {}) : {};
    expect(Object.keys(overrides)).toEqual(['amount']);
  });

  it('puts an effect back with a reset rather than with the configured values', () => {
    // Sending the configured values would leave a live value on every parameter
    // that happened to equal one, and the two would part company the moment
    // somebody edited the show. There is no value that means "stop
    // overriding this", because no override is an absence.
    const command = effectReset('EFFECT1');

    expect(isLiveCommand(command)).toBe(true);
    expect(command).toEqual({ do: 'set_effect', effectInstanceId: 'EFFECT1', reset: true });
    expect(command.do === 'set_effect' && command.overrides).toBeUndefined();
  });

  it('is named the way the parameter registry names it', () => {
    // `effect.<instance>.parameter.<name>`. One name for one
    // thing, so a control and its reading are found by the same key.
    expect(controlKey(RADIUS)).toBe('effect.EFFECT1.parameter.radius_px');
    expect(controlKey(PUT_BACK)).toBe('effect.EFFECT1');
  });

  it('clears its pending state on the acknowledgement rather than waiting for an echo', () => {
    // An effect request that changes nothing — a radius the effect is already
    // at — is answered and never published, which is most of what dragging a
    // fader produces.
    const pending = new PendingControls();
    pending.asked(RADIUS, number(40));
    expect(pending.read<ParameterValue>(RADIUS, number(8)).state).toBe('pending');

    pending.acknowledged(RADIUS);

    expect(pending.size).toBe(0);
    expect(pending.read<ParameterValue>(RADIUS, number(40))).toEqual({
      state: 'live',
      value: number(40),
    });
  });

  it('is corrected by an echo carrying the value the server settled on', () => {
    const pending = new PendingControls();
    pending.asked(RADIUS, number(40));
    pending.acknowledged(RADIUS);

    // Another source moved the same parameter, so what the wall is doing is 12
    // and 12 is what the control reads.
    expect(pending.read<ParameterValue>(RADIUS, number(12))).toEqual({
      state: 'live',
      value: number(12),
    });
  });

  it('shows a refusal against the parameter it names, in the server’s own words', () => {
    // `invalid_parameter` quotes the manifest. Paraphrasing it would be this
    // client stating a range the server never gave.
    const quoted = 'radius_px must be a number between 0 and 64';
    const pending = new PendingControls();
    pending.asked(RADIUS, number(4_000));
    pending.asked(AMOUNT, number(0.25));
    pending.refused(RADIUS, quoted);

    expect(pending.read<ParameterValue>(RADIUS, number(8))).toEqual({
      state: 'refused',
      value: number(8),
      wanted: number(4_000),
      reason: quoted,
    });
    expect(readingNote(pending.read<ParameterValue>(RADIUS, number(8)), 'the radius')).toContain(
      quoted,
    );
    // And only against that one. The other parameter of the same instance is
    // still waiting on its own answer.
    expect(pending.read<ParameterValue>(AMOUNT, number(1)).state).toBe('pending');
  });

  it('shows a refused reset against the effect rather than against a parameter', () => {
    const pending = new PendingControls();
    pending.asked(PUT_BACK, true);
    pending.refused(PUT_BACK, 'this server does not publish ovs.blur version 2');

    expect(pending.read<boolean>(PUT_BACK, null)).toEqual({
      state: 'refused',
      value: null,
      wanted: true,
      reason: 'this server does not publish ovs.blur version 2',
    });
    expect(pending.read<ParameterValue>(RADIUS, number(8))).toEqual({
      state: 'live',
      value: number(8),
    });
  });
});

// --- never a number the server did not send -----------------------------------

describe('a value the mirror has not delivered', () => {
  it('reads as unknown rather than as zero', () => {
    const pending = new PendingControls();
    expect(pending.read<number>(DIMMER, undefined)).toEqual({ state: 'unknown' });
    expect(pending.read<number>(DIMMER, null)).toEqual({ state: 'unknown' });
  });

  it('is drawn as an em dash rather than as nought per cent or nought beats', () => {
    const nothing = { state: 'unknown' } as const;
    expect(percentLine(nothing)).toBe('—');
    expect(bpmLine(nothing)).toBe('—');
    expect(crossfaderLine(nothing)).toBe('—');
    expect(speedScaleLine(nothing)).toBe('—');
    expect(valueOf(nothing)).toBeNull();
  });

  it('is not what an empty view says about a master that is simply off', () => {
    // An empty view is a client connected to nothing, which is exactly the case
    // where a zero would be read as a dark stage.
    const pending = new PendingControls();
    expect(pending.read<number>(DIMMER, emptyView().master?.dimmer)).toEqual({ state: 'unknown' });
  });

  it('offers exactly the speeds the master fixture has, and nothing between', () => {
    // Buttons, not a fader (owner, 2026-09-23). A fader across this range is a
    // control an operator can move away from 1x and never quite put back, and
    // with six settings there is nothing in between worth reaching for.
    expect([...SPEED_SCALE_STEPS]).toEqual([0.125, 0.25, 0.5, 1, 2, 4]);
    expect(SPEED_SCALE_STEPS[0]).toBe(MIN_SPEED_SCALE);
    expect(SPEED_SCALE_STEPS[SPEED_SCALE_STEPS.length - 1]).toBe(MAX_SPEED_SCALE);

    // What the server reports lights the button it belongs to. Nearest by
    // ratio, because the server stores an f64 and a desk sends a band: a value
    // that arrives as 0.9999999 is still 1x.
    for (const step of SPEED_SCALE_STEPS) {
      expect(nearestSpeedScale(step)).toBe(step);
      expect(nearestSpeedScale(step * 1.000001)).toBe(step);
    }
    expect(nearestSpeedScale(3)).toBe(4);
    expect(nearestSpeedScale(0.2)).toBe(0.25);

    // And nothing off either end, or off the scale entirely, escapes.
    expect(nearestSpeedScale(1000)).toBe(MAX_SPEED_SCALE);
    expect(nearestSpeedScale(0)).toBe(1);
    expect(nearestSpeedScale(Number.NaN)).toBe(1);
  });

  it('still draws real numbers as themselves', () => {
    expect(percentLine({ state: 'live', value: 0.8 })).toBe('80%');
    expect(bpmLine({ state: 'live', value: 128 })).toBe('128.0');
    expect(crossfaderLine({ state: 'live', value: 0 })).toBe('A');
    expect(crossfaderLine({ state: 'live', value: 1 })).toBe('B');
    expect(crossfaderLine({ state: 'live', value: 0.25 })).toBe('75/25');
    expect(speedScaleLine({ state: 'live', value: 2 })).toBe('2.00×');
  });
});

// --- a generator's live values ------------------------------------------------

describe('a live generator value', () => {
  it('is the same command an effect gets, aimed at the other identity', () => {
    // The owner's instruction of 2026-09-23: generators get no separate
    // special solution. Two commands because the identities are two types on
    // the server, one shape because it is one parameter model.
    const command = generatorCommand('GENERATOR1', 'angle_deg', number(180));

    expect(isLiveCommand(command)).toBe(true);
    expect(command).toEqual({
      do: 'set_generator',
      generatorInstanceId: 'GENERATOR1',
      overrides: { angle_deg: { type: 'float', value: 180 } },
    });
  });

  it('names the parameter the operator moved and no other', () => {
    const command = generatorCommand('GENERATOR1', 'colour_a', number(0.25));
    const overrides = command.do === 'set_generator' ? (command.overrides ?? {}) : {};
    expect(Object.keys(overrides)).toEqual(['colour_a']);
  });

  it('puts one back with a reset rather than with the configured values', () => {
    const command = generatorReset('GENERATOR1');

    expect(isLiveCommand(command)).toBe(true);
    expect(command).toEqual({ do: 'set_generator', generatorInstanceId: 'GENERATOR1', reset: true });
  });

  it('is keyed the way the parameter registry names it', () => {
    // The protocol calls it generator.<instance>.parameter.<name>, and the
    // key a control is held under is that name rather than one this client
    // invented — so a console and a client are talking about the same thing.
    expect(
      controlKey({ of: 'generator', generatorInstanceId: 'GENERATOR1', parameter: 'angle_deg' }),
    ).toBe('generator.GENERATOR1.parameter.angle_deg');
    // And the whole instance, which is what a reset is about.
    expect(controlKey({ of: 'generator', generatorInstanceId: 'GENERATOR1', parameter: null })).toBe(
      'generator.GENERATOR1',
    );
    // Never the same key as an effect with the same identifier, which is the
    // collision the two maps exist to prevent.
    expect(
      controlKey({ of: 'effect', effectInstanceId: 'GENERATOR1', parameter: 'angle_deg' }),
    ).not.toBe(
      controlKey({ of: 'generator', generatorInstanceId: 'GENERATOR1', parameter: 'angle_deg' }),
    );
  });
});
