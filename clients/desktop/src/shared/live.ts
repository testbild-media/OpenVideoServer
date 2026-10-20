// Live Mode's reasoning: the matrix, the layer status, the master, and honesty
// about which of the three the screen is currently able to tell the truth about.
//
// The screen itself is `ui/live.ts`. Everything that can be decided without a
// window is decided here, because the properties that matter during a show are
// the ones nobody can check by looking at a running client:
//
//   - **Live control is not editing.** Nothing this module produces writes to
//     the show document. `LiveCommand` is the whole of what the live screen
//     may send, and `isLiveCommand` is the runtime half of that promise.
//   - **A control that has been moved is not a control that has landed.** The
//     server owns the show; a fader moved here is a request until the server
//     answers, and `PendingControls` is what keeps the two apart. A
//     disconnected client therefore shows requests that never landed as exactly
//     that, instead of drawing them as though the wall had moved.
//   - **Never show a number the server did not send.** A value the mirror has
//     not delivered reads as unknown, not as zero. `Reading` has no fourth state
//     for "probably fine".
//
// The arithmetic in here is the server's own, cited where it lives: the layer
// beat rate is `MasterClock::beats_per_second` times the layer's ratio, and the
// slot rules are `OvsState::select_slot_at` and `switch_now`.

import {
  BLACK_SLOT_INDEX,
  MAX_BANKS,
  SLOTS_PER_BANK,
} from './limits.js';
import type { ParameterValue, Transform2D } from '@ovs/schema';

import type {
  BlendMode,
  Command,
  PlaybackDirection,
  PlaybackMode,
  Route,
  SlotSync,
} from './types.js';
import type { LayerView, MasterView, SlotView } from './view.js';

// --- what the live screen may send -------------------------------------------

/**
 * Every command Live Mode may issue.
 *
 * A narrowing of `Command` by construction: `open_show` and everything the
 * show editor sends are absent, so a control added to this screen cannot
 * quietly become an edit. The type says it at compile time and
 * [`isLiveCommand`] says it at run time, because the second is the one a test
 * can hold.
 *
 * `set_effect` and `set_generator` belong here and not in the editor's half for
 * the same reason `set_layer` does: what they move is live state that sits on
 * top of the show and is never written into it (`UpdateEffectRequest`, ADR
 * 0004). Adding an effect to a chain, taking one out, putting a generator in a
 * slot and changing what any of them *opens* at are still the document, and
 * still EDIT's.
 */
export type LiveCommand = Extract<
  Command,
  | { do: 'select_slot' }
  | { do: 'set_layer' }
  | { do: 'set_master' }
  | { do: 'set_effect' }
  | { do: 'set_generator' }
  | { do: 'tap' }
  | { do: 'reset_phase' }
>;

/** What a live command is allowed to be called. */
const LIVE_COMMANDS: ReadonlySet<string> = new Set([
  'select_slot',
  'set_layer',
  'set_master',
  'set_effect',
  'set_generator',
  'tap',
  'reset_phase',
]);

/**
 * Whether a command is one the live screen may send.
 *
 * The live screen funnels everything through this. A command that writes to the
 * show document — or loads one, which changes what is on the wall without an
 * operator having asked for it from here — does not pass.
 */
export function isLiveCommand(command: Command): command is LiveCommand {
  return LIVE_COMMANDS.has(command.do);
}

// --- what one control is doing -----------------------------------------------

/**
 * What one live control is doing, as the screen has to say it.
 *
 * There is no state for "assumed". A control is either something the server
 * reported, something this client asked for and has not been answered about, or
 * something nobody has said anything about at all.
 */
export type Reading<T> =
  /** The mirror has not delivered this. Not zero, not a default: unknown. */
  | { state: 'unknown' }
  /** What the server last said. */
  | { state: 'live'; value: T }
  /** Moved here, and the server has not answered the request yet. */
  | { state: 'pending'; value: T | null; wanted: T }
  /** The server refused it. What is on stage is still `value`. */
  | { state: 'refused'; value: T | null; wanted: T; reason: string };

/** Which control an entry is about. */
export type ControlId =
  | {
      of: 'master';
      field: 'bpm' | 'crossfader' | 'dimmer' | 'speedScale' | 'freeze' | 'dmxEnabled';
    }
  | {
      of: 'layer';
      layerId: string;
      field: 'route' | 'opacity' | 'transform' | 'blendMode' | 'transport' | 'enabled';
    }
  /** A slot tap, which is a trigger rather than a value. */
  | { of: 'slot'; layerId: string }
  /**
   * One parameter of one effect instance, or the whole instance.
   *
   * `parameter` is null for the control that puts the instance back to what the
   * show configured, which is one request about all of them (`reset`).
   */
  | { of: 'effect'; effectInstanceId: string; parameter: string | null }
  /** The same, for one generator instance. */
  | { of: 'generator'; generatorInstanceId: string; parameter: string | null };

/**
 * A stable key for one control, for the map and for an element's identity.
 *
 * An effect parameter is keyed the way the parameter registry names it,
 * `effect.<instance>.parameter.<name>`, because that is already
 * the agreed name for exactly this thing.
 */
export function controlKey(id: ControlId): string {
  switch (id.of) {
    case 'master':
      return `master.${id.field}`;
    case 'layer':
      return `layer.${id.layerId}.${id.field}`;
    case 'slot':
      return `layer.${id.layerId}.slot`;
    case 'effect':
      return id.parameter === null
        ? `effect.${id.effectInstanceId}`
        : `effect.${id.effectInstanceId}.parameter.${id.parameter}`;
    case 'generator':
      return id.parameter === null
        ? `generator.${id.generatorInstanceId}`
        : `generator.${id.generatorInstanceId}.parameter.${id.parameter}`;
  }
}

/**
 * How close two unit values have to be to count as the same one.
 *
 * Opacity, the dimmer and the crossfader are `f32` on the server and are sent
 * from a slider in thousandths, so a value that made the round trip differs in
 * the seventh decimal place. A tenth of one slider step is far enough below
 * that to be exact and far enough below one step not to settle the neighbour.
 */
const UNIT_TOLERANCE = 1e-5;

/** One control the operator has moved and the server has not answered. */
interface Entry {
  id: ControlId;
  wanted: unknown;
  /** Why the server refused it, or null while it is merely waiting. */
  refusal: string | null;
}

/**
 * The controls this client has moved and the server has not answered.
 *
 * Deliberately small and deliberately not a queue. This does not retry, reorder
 * or resend anything: a client that queued live commands would go on moving a
 * show that had carried on without it. It remembers what was asked so the screen
 * can say "asked for, not answered" rather than drawing it as done.
 *
 * **What answers a request is the request's own reply, not the update stream.**
 * A live endpoint answers 204 when it has accepted the change *and* when the
 * change moved nothing — a value the instance was already at, a fader held
 * still, a bus a layer is already on — and it publishes an update only when
 * something actually moved. A control that waited for the update would
 * therefore wait for ever on exactly those requests, which are the ones an
 * operator makes most: dragging a fader sends the position it passed through
 * again and again.
 *
 * So the acknowledgement is what clears a control, and the update stream is what
 * *corrects* it. If the server accepted something other than what was asked — a
 * clamp, or another source holding the same value — it publishes, and the value
 * beside the control becomes what the server published. If it accepted exactly
 * what was asked, there is nothing to correct. Nothing here ever shows a value
 * on the strength of this client having asked for it: once a request is
 * answered, what the control reads is the mirror's, and until it is answered the
 * control says so.
 */
export class PendingControls {
  readonly #entries = new Map<string, Entry>();

  /** How many controls are waiting. */
  get size(): number {
    return this.#entries.size;
  }

  /** The keys of every control that is waiting or was refused. */
  keys(): string[] {
    return [...this.#entries.keys()];
  }

  /** Records that a command has gone out for one control. */
  asked(id: ControlId, wanted: unknown): void {
    this.#entries.set(controlKey(id), { id, wanted, refusal: null });
  }

  /**
   * The server carried the request out. The control stops waiting.
   *
   * Here rather than when the update stream echoes the change, because a
   * request that moved nothing is answered and never echoed, and a control that
   * waited for the echo would hang on it for ever. What the control shows from
   * this moment on is the mirror's value — which is either already what was
   * asked for, or is what the server did instead and is the thing worth
   * showing.
   */
  acknowledged(id: ControlId): void {
    this.#entries.delete(controlKey(id));
  }

  /** The server refused the request. It stays on screen until it is moved again. */
  refused(id: ControlId, reason: string): void {
    const key = controlKey(id);
    const entry = this.#entries.get(key);
    if (entry === undefined) {
      return;
    }
    this.#entries.set(key, { ...entry, refusal: reason });
  }

  /** Forgets all of them: a fresh connection is a fresh set of facts. */
  clear(): void {
    this.#entries.clear();
  }

  /** What one control is currently saying, given what the server reports. */
  read<T>(id: ControlId, actual: T | null | undefined): Reading<T> {
    const entry = this.#entries.get(controlKey(id));
    const value = actual === undefined || actual === null ? null : actual;

    if (entry === undefined) {
      return value === null ? { state: 'unknown' } : { state: 'live', value };
    }
    if (entry.refusal !== null) {
      return { state: 'refused', value, wanted: entry.wanted as T, reason: entry.refusal };
    }
    return { state: 'pending', value, wanted: entry.wanted as T };
  }
}

/**
 * Whether two values are the same one, within a tolerance for numbers.
 *
 * What the master and layer commands below use to decide whether a field would
 * actually move anything. Non-numbers compare exactly, because a route or a
 * trigger grid has no near miss.
 */
function same(a: unknown, b: unknown, tolerance: number): boolean {
  if (typeof a === 'number' && typeof b === 'number') {
    return Math.abs(a - b) <= tolerance;
  }
  return a === b;
}

// --- the master --------------------------------------------------------------

/** A change to the master, in the terms an operator moved it in. */
export interface MasterWish {
  bpm?: number;
  crossfader?: number;
  dimmer?: number;
  speedScale?: number;
  freeze?: boolean;
  /**
   * Whether incoming DMX reaches the show at all.
   *
   * The operator taking the show off the console by hand, which is what
   * Live Mode is for when something out there has gone wrong.
   */
  dmxEnabled?: boolean;
}

/**
 * The command for a wish, carrying only what would actually change.
 *
 * `UpdateMasterRequest` treats an absent field as "leave this alone", and the
 * point of that is not economy: a client that sent a field it was not thinking
 * about would move it on stage, and the field it would fight over is whichever
 * one the lighting console is holding. So a wish that matches what the server
 * reports produces no command at all.
 *
 * A master the mirror has not delivered is not an argument for sending nothing:
 * with nothing to compare against, what the operator asked for is sent.
 */
export function masterCommand(current: MasterView | null, wish: MasterWish): LiveCommand | null {
  const change: MasterWish = {};
  let anything = false;

  /** Keeps a field only when it would actually move something. */
  const keep = <K extends keyof MasterWish>(field: K, tolerance: number): void => {
    const wanted = wish[field];
    if (wanted === undefined) {
      return;
    }
    if (current !== null && same(current[field], wanted, tolerance)) {
      return;
    }
    change[field] = wanted;
    anything = true;
  };

  keep('bpm', 0);
  keep('crossfader', UNIT_TOLERANCE);
  keep('dimmer', UNIT_TOLERANCE);
  keep('speedScale', 0);
  keep('freeze', 0);
  keep('dmxEnabled', 0);

  return anything ? { do: 'set_master', ...change } : null;
}

/** Whether two placements are the same, to the tolerance a fader can send. */
function samePlace(left: Transform2D, right: Transform2D): boolean {
  return (
    same(left.x, right.x, 0.5) &&
    same(left.y, right.y, 0.5) &&
    same(left.scale_x, right.scale_x, 0.0005) &&
    same(left.scale_y, right.scale_y, 0.0005) &&
    same(left.rotation_deg, right.rotation_deg, 0.05)
  );
}

/** A change to one layer's live state. */
export interface LayerWish {
  route?: Route;
  opacity?: number;
  /** Where the layer's picture sits. Live, and driven by the Layer Fixture. */
  transform?: Transform2D;
  /**
   * How the layer composites onto its bus.
   *
   * Live since 2026-09-23: a blend mode is something an operator changes
   * while looking at the wall, and it was in the show editor.
   */
  blendMode?: BlendMode;
}

/**
 * The command for a layer wish, carrying only what would actually change.
 *
 * The same rule as the master, for the same reason: `UpdateLayerRequest` treats
 * an absent field as "leave this alone", and the Layer Fixture drives several of
 * these from a console at the same time.
 */
export function layerCommand(layer: LayerView, wish: LayerWish): LiveCommand | null {
  const change: LayerWish = {};
  let anything = false;

  const keep = <K extends keyof LayerWish>(field: K, tolerance: number): void => {
    const wanted = wish[field];
    if (wanted === undefined) {
      return;
    }
    if (same(layer[field], wanted, tolerance)) {
      return;
    }
    change[field] = wanted;
    anything = true;
  };

  keep('route', 0);
  keep('opacity', UNIT_TOLERANCE);
  keep('blendMode', 0);
  // Compared whole rather than field by field: a placement is one thing, and a
  // transform that differs in one number is a different placement.
  if (wish.transform !== undefined && !samePlace(layer.transform, wish.transform)) {
    change.transform = wish.transform;
    anything = true;
  }

  return anything ? { do: 'set_layer', layerId: layer.id, ...change } : null;
}

// --- an effect's live values --------------------------------------------------

/**
 * A live value for one parameter of one effect instance.
 *
 * One key per request, and always sent, unlike the master and the layer above.
 * Those two compare against what the server reports first, because their
 * requests carry fields that mean "leave this alone" and a client that sent one
 * it was not thinking about would fight whatever is holding it — during a show,
 * the lighting console. An effect request carries only the parameter the
 * operator moved, so there is no untouched field to fight over.
 *
 * That leaves the one case a comparison would have caught: asking for a value
 * the effect is already at. The server answers that 204 and publishes nothing,
 * and the control it belongs to clears on the answer rather than on the
 * publication, so it costs a request and settles correctly rather than hanging.
 */
export function effectCommand(
  effectInstanceId: string,
  key: string,
  value: ParameterValue,
): LiveCommand {
  return { do: 'set_effect', effectInstanceId, overrides: { [key]: value } };
}

/**
 * Putting one instance back to the values the show configured.
 *
 * `reset` rather than the configured values themselves. Sending those would
 * leave a live value on every parameter that happened to equal what the show
 * set, and the two would part company the moment somebody edited the show —
 * the effect would go on running the number this client sent, which is not what
 * "put it back" means. There is no value that means "stop overriding this",
 * because no override is an *absence* (`EffectOverrides`).
 */
export function effectReset(effectInstanceId: string): LiveCommand {
  return { do: 'set_effect', effectInstanceId, reset: true };
}

/**
 * Moving one parameter of one generator instance.
 *
 * Everything [`effectCommand`] is, for the other kind of module. One route
 * each on the server because the identities are different types, and one
 * parameter model behind them because an effect and a generator are both
 * plugins with declared parameters — which is the owner's instruction of
 * 2026-09-23, and the reason a console moving a generator's colour and this
 * moving it end up in the same place.
 */
export function generatorCommand(
  generatorInstanceId: string,
  key: string,
  value: ParameterValue,
): LiveCommand {
  return { do: 'set_generator', generatorInstanceId, overrides: { [key]: value } };
}

/** Putting one generator instance back to the values the show configured. */
export function generatorReset(generatorInstanceId: string): LiveCommand {
  return { do: 'set_generator', generatorInstanceId, reset: true };
}

// --- the matrix --------------------------------------------------------------

/** One bank of slots, as the bank buttons offer them. */
export interface SlotBank {
  /** The lowest slot in it, counting from 1. */
  first: number;
  /** The highest. */
  last: number;
  /** `1–12`. */
  label: string;
}

/**
 * The banks there are, given what the show has in its slots.
 *
 * Derived rather than fixed. The reference drawing shows seven banks up to slot
 * 84, but a bank of slots nothing is assigned to is a page of pads that cannot
 * do anything, and an operator who pages to it has been sent somewhere for no
 * reason. There is always at least the first bank, because slot 0 lives in it
 * and every layer has one.
 */
export function slotBanks(layers: readonly LayerView[]): SlotBank[] {
  let highest = 1;
  // What any layer *asks for*, as well as what any layer's slots need. The
  // first is the one an operator added on purpose and has not filled yet;
  // the second is what a document written before layers had a bank count
  // still has in it (owner, 2026-09-23).
  let asked = 1;
  for (const layer of layers) {
    asked = Math.max(asked, layer.banks);
    for (const slot of layer.slots) {
      highest = Math.max(highest, slot.index);
    }
  }

  const count = Math.max(asked, Math.ceil(highest / SLOTS_PER_BANK), 1);
  const banks: SlotBank[] = [];
  for (let bank = 0; bank < count; bank += 1) {
    const first = bank * SLOTS_PER_BANK + 1;
    const last = first + SLOTS_PER_BANK - 1;
    banks.push({ first, last, label: `${first}–${last}` });
  }
  return banks;
}

/**
 * Every bank there can be, up to slot 84, whether the show has opened it yet.
 *
 * **The bar offers all seven** (owner, 2026-10-21): "Slots 1-12 sind immer
 * sichtbar als Tab, aber alle anderen bis 84 sind nicht sichtbar. Da komme ich
 * nicht hin." A bank was opened by right-clicking a pad, which nothing on the
 * screen said, so the matrix looked as though it ended at twelve. Which of
 * these are open is still `slotBanks`'s answer: a closed one is shown so that
 * it can be reached, and opening it is still a deliberate act.
 */
export function allBanks(): SlotBank[] {
  const banks: SlotBank[] = [];
  for (let bank = 0; bank < MAX_BANKS; bank += 1) {
    const first = bank * SLOTS_PER_BANK + 1;
    const last = first + SLOTS_PER_BANK - 1;
    banks.push({ first, last, label: `${first}–${last}` });
  }
  return banks;
}

/** One pad of the matrix. */
export interface PadView {
  index: number;
  /** Slot 0: permanently black, on every layer, and never configured. */
  /**
   * Whether this pad is the permanent Black.
   *
   * Never, since the owner took it out of the matrix (2026-09-23): MUTE takes
   * a layer off stage and an empty pad shows nothing, so the thirteenth pad
   * had two replacements and no job. Kept on the view because slot 0 is still
   * in the model and still a number a DMX console sends, and a layer sitting
   * on it has to be describable.
   */
  black: boolean;
  /** Nothing is assigned to this index on this layer. */
  empty: boolean;
  /** What it plays, when something does. */
  slot: SlotView | null;
  /** The layer is playing it. */
  active: boolean;
}

/**
 * The pads of one layer for one bank, slot 0 first.
 *
 * Slot 0 is in every bank rather than only the first. It is the one pad an
 * operator reaches for when something is wrong, and a black button that is on
 * another page is a black button that is not there.
 */
export function padsInBank(layer: LayerView, bank: SlotBank): PadView[] {
  const configured = new Map(layer.slots.map((slot) => [slot.index, slot]));
  // **No Black pad** (owner, 2026-09-23): "Black Existiert in der GUI nicht
  // mehr und wird von MUTE abgelöst… jeder nicht belegte Slot ist ein
  // Black/Full Tranzparent für den Layer."
  //
  // It was a thirteenth pad that existed to take a layer off stage, which MUTE
  // does now — and an empty pad does the same thing for a different reason:
  // there is nothing in it, so the layer shows nothing. Slot 0 is still in the
  // model and still a number a DMX console can send; it is only gone from the
  // matrix.
  const pads: PadView[] = [];

  for (let index = bank.first; index <= bank.last; index += 1) {
    const slot = configured.get(index) ?? null;
    pads.push({
      index,
      black: false,
      empty: slot === null,
      slot,
      active: layer.activeSlot === index,
    });
  }
  return pads;
}

/**
 * The slot to move to from the one that is playing, in the given direction.
 *
 * Only slots that hold something, and only within what the show has: a next
 * that landed on an empty pad would be a trigger the server refuses. Returns
 * null when there is nowhere to go, which is a layer with no slots or one
 * already at the end. Slot 0 is the starting point when nothing is playing.
 */
export function stepSlot(layer: LayerView, direction: 1 | -1): number | null {
  const filled = layer.slots.map((slot) => slot.index).sort((a, b) => a - b);
  if (filled.length === 0) {
    return null;
  }
  const from = layer.activeSlot;

  if (direction === 1) {
    return filled.find((index) => index > from) ?? null;
  }
  // Backwards from the first configured slot is Black, which is a real place to
  // be and the one a layer starts in.
  const earlier = filled.filter((index) => index < from);
  if (earlier.length === 0) {
    return from === BLACK_SLOT_INDEX ? null : BLACK_SLOT_INDEX;
  }
  return earlier[earlier.length - 1] ?? null;
}

// --- the layer status area ---------------------------------------------------

/**
 * The tempo the show is counting at, in beats per minute.
 *
 * `MasterClock::beats_per_second` is `bpm / 60 * speed_scale`, and this is
 * the same multiplication in beats per minute. **Nothing per layer is in it** 
 * any more (owner, 2026-09-23): a layer has no rate, a slot either runs free
 * at 1x or is locked to a number of beats, and those beats are counted
 * against this one tempo. At 120 BPM with the master on 2x this reads 240,
 * which is what the strip shows beside a beat-locked slot.
 *
 * Null when the server has not sent the master yet, rather than falling back
 * to a tempo nobody set.
 */
export function showTempo(master: MasterView | null): number | null {
  if (master === null) {
    return null;
  }
  return master.bpm * master.speedScale;
}

/**
 * What the layer strip reads beside the active slot.
 *
 * The slot's own setting, shown rather than offered: FREE for a slot that
 * ignores the tempo, and the beat count for one locked to it. A layer with
 * nothing on stage has nothing to say, which is an em dash and not a 1.
 */
export function syncLabel(sync: SlotSync | null): string {
  if (sync === null) {
    return '—';
  }
  return sync.kind === 'BEAT_SYNC' ? String(sync.beats) : 'FREE';
}

/**
 * How many of a clip's own frames a pass asks for each second.
 *
 * **The number that was missing when the owner found slot 2 of layer 1 standing
 * still** (2026-09-23: "Freerun alles gut mit BPM Sync läuft null"). Seven beats
 * on a 74.9-second clip at 120 BPM is a pass of 3.5 seconds — twenty-one times
 * the clip's own rate, 535 frames a second — and a decoder answers a frame it
 * has not reached by repeating the last one it gave. The picture does not play
 * fast, it stops.
 *
 * Worked out here rather than read back from the server, because the point is to
 * say it *while the operator is typing the number*, not after the wall has
 * stopped. It matches `demanded_fps` in `media/timeline.rs`.
 *
 * `null` when it cannot be known: no clip, no length, or a beat-locked slot on a
 * server that has not reported a tempo.
 */
export function demandedFps(
  clip: { durationMs: number; fps: number } | null,
  sync: SlotSync,
  speed: number,
  tempo: number | null,
): number | null {
  if (clip === null || clip.durationMs <= 0 || clip.fps <= 0) {
    return null;
  }
  if (!Number.isFinite(speed) || speed <= 0) {
    return null;
  }

  let passSeconds: number;
  if (sync.kind === 'BEAT_SYNC') {
    if (tempo === null || tempo <= 0 || sync.beats <= 0) {
      return null;
    }
    passSeconds = sync.beats / (tempo / 60) / speed;
  } else {
    passSeconds = clip.durationMs / 1000 / speed;
  }
  if (!Number.isFinite(passSeconds) || passSeconds <= 0) {
    return null;
  }

  const frames = (clip.durationMs * clip.fps) / 1000;
  return frames / passSeconds;
}

/**
 * Frames a second past which a clip is unlikely to keep up.
 *
 * `FPS_WORTH_WARNING_ABOUT` in `media/timeline.rs`, and the same measurement is
 * written down there: the reference server held nothing at 180 frames a second
 * of 4K and a fifth of its passes at 240. A number to warn at and never one to
 * enforce — a small clip goes far past it.
 */
export const FPS_WORTH_WARNING_ABOUT = 200;

/**
 * What a slot's rate and sync are asking for, in one sentence.
 *
 * `null` when there is nothing to say, which is the ordinary case: a clip at its
 * own rate is not a demand and does not need a sentence about it.
 */
export function rateNote(
  clip: { durationMs: number; fps: number } | null,
  sync: SlotSync,
  speed: number,
  tempo: number | null,
): { text: string; strained: boolean } | null {
  const fps = demandedFps(clip, sync, speed, tempo);
  if (fps === null || clip === null) {
    return null;
  }
  const times = fps / clip.fps;
  if (times > 0.995 && times < 1.005) {
    return null;
  }

  const rounded = times >= 10 ? times.toFixed(0) : times.toFixed(2).replace(/0+$/, '').replace(/\.$/, '');
  const text = `${rounded}x the clip's own rate — ${Math.round(fps)} frames a second`;
  if (fps <= FPS_WORTH_WARNING_ABOUT) {
    return { text, strained: false };
  }
  return {
    text: `${text}. That is more than a decoder is likely to give: the picture will hold frames rather than play faster.`,
    strained: true,
  };
}

/** What the end of a slot's timeline does, in the words the editor uses. */
export function playbackLabel(mode: PlaybackMode, direction: PlaybackDirection): string {
  const ends = { loop: 'Loop', once_freeze: 'Once, freeze', once_black: 'Once, black', bounce: 'Bounce' }[
    mode
  ];
  return direction === 'reverse' ? `${ends}, reverse` : ends;
}

// --- putting numbers in front of an operator ---------------------------------

/**
 * A reading as a percentage, or an em dash when the server has not said.
 *
 * The dash is the point of this function. A dimmer the mirror has not delivered
 * drawn as `0%` says the stage is dark, and the stage may be anything at all.
 */
export function percentLine(reading: Reading<number>): string {
  switch (reading.state) {
    case 'unknown':
      return '—';
    case 'live':
      return `${Math.round(reading.value * 100)}%`;
    case 'pending':
    case 'refused':
      return `${Math.round(reading.wanted * 100)}%`;
  }
}

/** A tempo to one decimal, as the reference drawing shows it, or an em dash. */
export function bpmLine(reading: Reading<number>): string {
  switch (reading.state) {
    case 'unknown':
      return '—';
    case 'live':
      return reading.value.toFixed(1);
    case 'pending':
    case 'refused':
      return reading.wanted.toFixed(1);
  }
}

/**
 * Where the crossfader is, in the terms an operator reads it in.
 *
 * `A` and `B` at the ends because those are positions with names, and the split
 * in between because a number is what says how far across it is.
 */
export function crossfaderLine(reading: Reading<number>): string {
  const position = valueOf(reading);
  if (position === null) {
    return '—';
  }
  if (position <= 0.001) {
    return 'A';
  }
  if (position >= 0.999) {
    return 'B';
  }
  return `${Math.round((1 - position) * 100)}/${Math.round(position * 100)}`;
}

/** The master speed scale, as `1.00×`, or an em dash. */
export function speedScaleLine(reading: Reading<number>): string {
  const scale = valueOf(reading);
  return scale === null ? '—' : `${scale.toFixed(2)}×`;
}

/**
 * Every master speed the show has, in the order they are offered.
 *
 * The same six the master fixture's own channel has (`MASTER_SCALE_STEPS`), so
 * that a value set here and a value set from a desk are the same set of values
 * rather than two that nearly agree.
 *
 * Buttons, not a fader, and that is the owner's instruction of 2026-09-23. A
 * fader across this range is a control an operator can move away from 1× and
 * never quite put back — and with only six settings there is nothing in
 * between worth reaching for. Unity is the fourth of them, which is also where
 * the DMX channel's home value sits.
 */
export const SPEED_SCALE_STEPS: readonly number[] = [0.125, 0.25, 0.5, 1, 2, 4];

/**
 * What one master speed step is called.
 *
 * The same spelling a lighting desk's own scale channel uses: a division below
 * unity, a multiplication above it, and a bare 1 at the show's own tempo. Six
 * steps, so this is a lookup rather than arithmetic on a float.
 *
 * The master's ladder and a slot's are the same six steps and read the same
 * way. That is deliberate: they are one idea in two places — how fast a thing
 * runs against what it would do on its own — and two spellings would make an
 * operator learn it twice.
 */
export function speedLabel(scale: number): string {
  const known = new Map([
    [0.125, '/8'],
    [0.25, '/4'],
    [0.5, '/2'],
    [1, '1'],
    [2, 'x2'],
    [4, 'x4'],
  ]);
  const found = known.get(scale);
  if (found !== undefined) {
    return found;
  }
  // A scale a console put there. Shown as what it is rather than snapped to
  // the nearest step, which would be this client reporting a speed the show
  // is not running at.
  return `×${round(scale, 3)}`;
}

/** The offered speed nearest to what the server reports. */
export function nearestSpeedScale(scale: number): number {
  const usable = Number.isFinite(scale) && scale > 0 ? scale : 1;
  let best = SPEED_SCALE_STEPS[0] ?? 1;
  for (const step of SPEED_SCALE_STEPS) {
    // Nearest in ratio rather than in difference: the steps are doublings, so
    // 3 belongs to 4 and not to 2.
    if (Math.abs(Math.log2(step / usable)) < Math.abs(Math.log2(best / usable))) {
      best = step;
    }
  }
  return best;
}

/** Whatever the reading is about, or null when nothing is known. */
export function valueOf<T>(reading: Reading<T>): T | null {
  switch (reading.state) {
    case 'unknown':
      return null;
    case 'live':
      return reading.value;
    case 'pending':
    case 'refused':
      return reading.wanted;
  }
}

/**
 * What the server itself last said, ignoring anything this client asked for.
 *
 * The other half of `valueOf`: a control being dragged shows where the operator
 * has put it, and the line beside it shows what is on stage.
 */
export function reportedBy<T>(reading: Reading<T>): T | null {
  switch (reading.state) {
    case 'unknown':
      return null;
    case 'live':
      return reading.value;
    case 'pending':
    case 'refused':
      return reading.value;
  }
}

/**
 * The word for a reading's state, for a class name and for a tooltip.
 *
 * `pending` is the one that earns its place: it is the difference between a
 * client whose command reached the server and a client that is talking to
 * nothing, and it must not look like either "done" or "broken".
 */
export function readingNote<T>(reading: Reading<T>, what: string): string {
  switch (reading.state) {
    case 'unknown':
      return `The server has not reported ${what} yet.`;
    case 'live':
      return `${capitalise(what)} as the server reports it.`;
    case 'pending':
      return `Asked for. The server has not answered about ${what} yet.`;
    case 'refused':
      return `The server refused this: ${reading.reason}`;
  }
}

/** Rounds for display without turning 0.1 + 0.2 into a paragraph. */
function round(value: number, places: number): number {
  const scale = 10 ** places;
  return Math.round(value * scale) / scale;
}

function capitalise(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}
