// Editing a show document.
//
// A show is one document. The server has no per-field endpoint: it hands the
// whole thing over, takes the whole thing back, and refuses the save outright
// if any part of it breaks an invariant (`show_routes::save`). So this
// module is the document in memory and every edit an operator can make to it,
// as plain functions with no window and no network — which is how they can be
// tested against the same rules the server applies.
//
// Two things it is careful about.
//
// **It never rebuilds what it did not change.** Every edit copies along the
// path it touches and spreads everything else through, so a field this build
// has never heard of — written by a newer server, or by a part of the model no
// editor covers yet — is still in the document when it is saved. Dropping one
// silently would be a show quietly losing a piece of a show.
//
// **It edits configuration, never the show.** `default_opacity` and
// are what a layer *starts* from. What the layer is doing right now is live state, belongs to the console
// and to Live Mode, and is never written back here.

import type {
  AnchorPoint,
  Composition,
  ConnectorInfo,
  ConnectorModeInfo,
  DisplayMode,
  EffectInstance,
  Layer,
  MasterConfig,
  OutputConfig,
  ParameterValue,
  Piece,
  PieceSource,
  Show,
  RectPx,
  SizePx,
  Slot,
  SlotSync,
  SourceRef,
  Transform2D,
  ValidationError,
} from '@ovs/schema';

import {
  BLACK_SLOT_INDEX,
  DEFAULT_BLEND_MODE,
  DEFAULT_OPACITY,
  DEFAULT_OUTPUT_ENABLED,
  DEFAULT_PIECE_ENABLED,
  DEFAULT_PIECE_ORDER,
  DEFAULT_PIECE_ROTATION_DEG,
  DEFAULT_PLAYBACK_DIRECTION,
  DEFAULT_PLAYBACK_MODE,
  DEFAULT_ROUTE,
  IDENTITY_TRANSFORM,
  MAX_BEATS_PER_BAR,
  MAX_BPM,
  MAX_DIMENSION_PX,
  MAX_FRACTION,
  MAX_PIECE_ORDER,
  MAX_SPEED_SCALE,
  MIN_BEATS_PER_BAR,
  MIN_BPM,
  MIN_DIMENSION_PX,
  MIN_FRACTION,
  MIN_PIECE_ORDER,
  MIN_SLOT_BEATS,
  MIN_SPEED_SCALE,
  SLOTS_PER_BANK,
} from './limits.js';
import { newId } from './ids.js';
import {
  connectorKey,
  connectorOf,
  describeResize,
  findOutput,
  findPiece,
  fitsWithin,
  isEmpty,
  isOffScreen,
  piecesOf,
  rateFromMillihertz,
  resizeReport,
  sequenceOf,
  spillOf,
} from './mapping.js';
import {
  findEffect,
  findGenerator,
  parameterProblems,
  refuse as refuseParameter,
  type Catalogue,
  type ParameterMap,
} from './modules.js';
import { emptyFixtures, validatePatch, type FixtureCatalogue } from './patch.js';

// Minted in one place for the whole client, and re-exported here because this
// is where an editor looks for it.
export { newId } from './ids.js';

// --- what an edit answers ----------------------------------------------------

/** An edit that was made. */
export interface Applied {
  ok: true;
  /** The document with the edit in it. The original is untouched. */
  show: Show;
  /**
   * Something the operator has to be told about the edit.
   *
   * Used where an edit has a consequence elsewhere in the document. Removing a
   * layer takes its DMX patch entry with it, and an operator who is not told
   * that would find out from a console.
   */
  note?: string;
}

/** An edit that was not made, and why. */
export interface Refused {
  ok: false;
  reason: string;
}

/** What every edit answers. One shape, so a caller has one thing to handle. */
export type Edit = Applied | Refused;

function applied(show: Show, note?: string): Applied {
  return note === undefined ? { ok: true, show } : { ok: true, show, note };
}

function refused(reason: string): Refused {
  return { ok: false, reason };
}

// --- the document in hand ----------------------------------------------------

/** A show being edited, and the version the server last confirmed. */
export interface Draft {
  /** The document as the server gave it back, for knowing what has changed. */
  saved: Show;
  /** The document being edited. */
  show: Show;
}

/** Opens a document for editing. */
export function openDraft(show: Show): Draft {
  return { saved: show, show };
}

/** Whether the document differs from what the server last confirmed. */
export function isDirty(draft: Draft): boolean {
  return canonical(draft.saved) !== canonical(draft.show);
}

/**
 * A document as one string, with every object's keys in a settled order.
 *
 * `JSON.stringify` writes keys in insertion order, so two documents that are
 * the same document compare as different when one of them was built by
 * spreading an object and adding a field. That is not a hypothetical: it is
 * exactly what happened the day a generator gained an instance identity — the
 * client put it last, `serde` puts it where the struct declares it, and the
 * editor showed unsaved work on a document it had just saved and read back.
 *
 * Sorting the keys makes the comparison about the document. Nothing else here
 * depends on key order, and no operator has ever cared what order a saved file
 * writes its fields in.
 */
function canonical(value: unknown): string {
  return JSON.stringify(value, (_key, held: unknown) => {
    if (held === null || typeof held !== 'object' || Array.isArray(held)) {
      return held;
    }
    const sorted: Record<string, unknown> = {};
    for (const key of Object.keys(held as Record<string, unknown>).sort()) {
      sorted[key] = (held as Record<string, unknown>)[key];
    }
    return sorted;
  });
}

/**
 * Records that the server took the document, at the moment it says it did.
 *
 * `updated_at` comes from the save response rather than from a clock here: the
 * server decides when it wrote the show, and a client inventing a timestamp
 * would put a number in the document that nothing measured.
 */
export function markSaved(draft: Draft, updatedAt: number): Draft {
  const show: Show = { ...draft.show, updated_at: updatedAt };
  return { saved: show, show };
}

/**
 * The document to send, given what the server has now.
 *
 * **A show is edited in two places at once, and always has been.** Slots are
 * filled, effects are added and banks are opened on the live screen, and each
 * one edits the document the server holds (`show_edits`). Meanwhile somebody
 * may have the same show open in Settings, changing the mapping or the
 * master defaults. That editor read the document when it opened it, and a save
 * that sent its copy back whole would put the document back to the moment it
 * was read — which is what happened: "Projekt Speichern killt immer noch alle
 * nicht gespeicherten Slots" (owner, 2026-09-23). An evening's slot assignments
 * gone for a mapping change.
 *
 * So a save is a merge rather than a replacement, under one rule: **you keep
 * what you changed, and everything else comes from the server as it is now.**
 * What the editor changed is knowable exactly — `Draft` carries the document as
 * it was read — so no guessing is involved and nothing is asked of the
 * operator.
 *
 * ## Slots, effects and banks are never the editor's
 *
 * Even when the layer list *has* been edited — a layer renamed, one added —
 * each surviving layer takes its slots, its effect chain and its bank count
 * from the server. Settings has no control for any of the three since the slot
 * grid moved to LIVE, so the editor's copy of them is only ever a stale reading
 * and can never be an intention.
 */
export function mergedForSave(draft: Draft, server: Show): Show {
  const changed = <K extends keyof Show>(key: K): boolean =>
    canonical(draft.saved[key]) !== canonical(draft.show[key]);

  // Every section the editor touched, and the server's for the rest. `id` and
  // `created_at` are the server's by definition; `updated_at` is whatever the
  // save stamps on it.
  const merged: Show = {
    ...server,
    name: changed('name') ? draft.show.name : server.name,
    settings: changed('settings') ? draft.show.settings : server.settings,
    composition: changed('composition') ? draft.show.composition : server.composition,
    master: changed('master') ? draft.show.master : server.master,
    pieces: changed('pieces') ? draft.show.pieces : server.pieces,
    outputs: changed('outputs') ? draft.show.outputs : server.outputs,
    fixture_patch: changed('fixture_patch')
      ? draft.show.fixture_patch
      : server.fixture_patch,
    layers: changed('layers') ? draft.show.layers : server.layers,
  };

  // And whatever the layer list came out as, what is *in* each layer is the
  // server's. A layer the editor made has none of the three yet, so it keeps
  // its own empty ones.
  merged.layers = merged.layers.map((layer) => {
    const live = server.layers.find((each) => each.id === layer.id);
    return live === undefined
      ? layer
      : { ...layer, slots: live.slots, effects: live.effects, banks: live.banks };
  });

  return merged;
}

/**
 * Puts a whole mapping back, as one edit.
 *
 * What Put back does in the Mapping tab (owner, 2026-09-23). The mapping is
 * live — every rectangle reaches the wall as it is moved — so the way out of a
 * session of moving things is a rectangle-for-rectangle restore rather than an
 * undo of each gesture. Validated like every other edit, because the mapping
 * being put back is one the document already held and must still hold.
 */
export function restoreMapping(
  show: Show,
  outputs: OutputConfig[],
  pieces: Piece[],
): Edit {
  // Applied rather than checked: what is being put back is a mapping this
  // document already held, so the only way it could be refused is if something
  // else had changed under it — and the server validates it whole anyway, on
  // the way to the wall.
  return applied({ ...show, outputs, pieces });
}

/** Throws the edits away and goes back to what the server last confirmed. */
export function revert(draft: Draft): Draft {
  return { saved: draft.saved, show: draft.saved };
}

// --- reading the document ----------------------------------------------------

/**
 * Layers in composite order, bottom first.
 *
 * Sorted by order then by identifier, exactly as `layers_in_composite_order`
 * does, so that a document with two layers on the same order — which validation
 * refuses, but which can be on screen while it is being fixed — still draws in
 * a settled sequence rather than jumping about.
 */
export function orderedLayers(show: Show): Layer[] {
  return [...show.layers].sort((a, b) => a.order - b.order || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
}

/** Returns a layer by identifier, or null. */
export function findLayer(show: Show, layerId: string): Layer | null {
  return show.layers.find((layer) => layer.id === layerId) ?? null;
}

/** Returns a configured slot by index, or null. Index 0 is never configured. */
export function findSlot(layer: Layer, index: number): Slot | null {
  return layer.slots.find((slot) => slot.index === index) ?? null;
}

/**
 * The bank a slot index falls in, counting from zero, or null for black.
 *
 * Mirrors `Layer::bank_of`: indices are one-based, so 1..12 is bank 0.
 */
export function bankOf(index: number): number | null {
  return index === BLACK_SLOT_INDEX ? null : Math.floor((index - 1) / SLOTS_PER_BANK);
}

/** How many banks it takes to show every configured slot. At least one. */
export function bankCount(layer: Layer): number {
  let highest = 0;
  for (const slot of layer.slots) {
    const bank = bankOf(slot.index);
    if (bank !== null && bank + 1 > highest) {
      highest = bank + 1;
    }
  }
  return Math.max(highest, 1);
}

/** The slot indices one bank covers, in order. */
export function bankIndices(bank: number): number[] {
  const first = bank * SLOTS_PER_BANK + 1;
  return Array.from({ length: SLOTS_PER_BANK }, (_unused, offset) => first + offset);
}

// --- where a thing lives, in the server's words ------------------------------
//
// The server reports a refused save as a list of dotted paths into the model
// (`layers[2].slots[0].index`). These build the same paths from this side, so a
// message about a field can be put beside that field rather than in a list at
// the bottom of the screen.

/** The path of a layer, or null when it is not in this document. */
export function layerPath(show: Show, layerId: string): string | null {
  const index = show.layers.findIndex((layer) => layer.id === layerId);
  return index < 0 ? null : `layers[${index}]`;
}

/** The path of a configured slot, or null. */
export function slotPath(show: Show, layerId: string, slotIndex: number): string | null {
  const layerIndex = show.layers.findIndex((layer) => layer.id === layerId);
  const layer = show.layers[layerIndex];
  if (!layer) {
    return null;
  }
  const position = layer.slots.findIndex((slot) => slot.index === slotIndex);
  return position < 0 ? null : `layers[${layerIndex}].slots[${position}]`;
}

/** The path of an output, or null when it is not in this document. */
export function outputPath(show: Show, outputId: string): string | null {
  const index = show.outputs.findIndex((output) => output.id === outputId);
  return index < 0 ? null : `outputs[${index}]`;
}

/** The path of a piece, or null when it is not in this document. */
export function piecePath(show: Show, pieceId: string): string | null {
  const index = show.pieces.findIndex((piece) => piece.id === pieceId);
  return index < 0 ? null : `pieces[${index}]`;
}

/** Which chain an effect belongs to: a whole layer, or one slot on it. */
export interface ChainRef {
  layerId: string;
  /** Left out for a layer's own chain. */
  slotIndex?: number;
}

/** The path of an effect instance, or null. */
export function effectPath(show: Show, chain: ChainRef, effectId: string): string | null {
  const base =
    chain.slotIndex === undefined
      ? layerPath(show, chain.layerId)
      : slotPath(show, chain.layerId, chain.slotIndex);
  if (base === null) {
    return null;
  }
  const effects = readChain(show, chain);
  const position = effects?.findIndex((effect) => effect.id === effectId) ?? -1;
  return position < 0 ? null : `${base}.effects[${position}]`;
}

/** The effect chain a reference points at, or null. */
export function readChain(show: Show, chain: ChainRef): EffectInstance[] | null {
  const layer = findLayer(show, chain.layerId);
  if (!layer) {
    return null;
  }
  if (chain.slotIndex === undefined) {
    return layer.effects;
  }
  return findSlot(layer, chain.slotIndex)?.effects ?? null;
}

// --- the show itself ------------------------------------------------------

/** Renames the show. */
export function renameShow(show: Show, name: string): Edit {
  if (name.trim() === '') {
    return refused('a show name must not be empty');
  }
  return applied({ ...show, name });
}

/** Turns auto-save on or off. */
export function setAutoSave(show: Show, autoSave: boolean): Edit {
  return applied({ ...show, settings: { ...show.settings, auto_save: autoSave } });
}

/** Replaces the operator's notes. */
export function setNotes(show: Show, notes: string): Edit {
  return applied({ ...show, settings: { ...show.settings, notes } });
}

// --- the master --------------------------------------------------------------

/**
 * What can be changed about the master. All of it is configuration.
 *
 * There is one master clock and one crossfader, and these are what they are
 * *restored* to — at load, and after a restart, because live state is not kept
 * across one. What the master is doing right now belongs to the
 * console and to Live Mode and is never written here.
 */
export interface MasterChange {
  default_bpm?: number;
  beats_per_bar?: number;
  default_speed_scale?: number;
  default_dimmer?: number;
  default_crossfader?: number;
  crossfader_curve?: MasterConfig['crossfader_curve'];
}

/**
 * Changes the master settings.
 *
 * Every bound is `MasterConfig::validate`'s, in the same words, and a value
 * outside one is refused rather than clamped: this is a document, and the
 * useful answer to a number that cannot be stored is to say so. Refusing here
 * rather than at the save is the whole point — until now the editor reported a
 * bad BPM and offered no way to correct it.
 */
export function setMaster(show: Show, change: MasterChange): Edit {
  if (change.default_bpm !== undefined) {
    const bpm = change.default_bpm;
    if (!Number.isFinite(bpm) || bpm < MIN_BPM || bpm > MAX_BPM) {
      return refused(`BPM must be between ${MIN_BPM} and ${MAX_BPM}`);
    }
  }
  if (change.beats_per_bar !== undefined) {
    const beats = change.beats_per_bar;
    if (!Number.isInteger(beats) || beats < MIN_BEATS_PER_BAR || beats > MAX_BEATS_PER_BAR) {
      return refused('beats per bar is a whole number greater than zero');
    }
  }
  if (change.default_speed_scale !== undefined) {
    const scale = change.default_speed_scale;
    if (!Number.isFinite(scale) || scale < MIN_SPEED_SCALE || scale > MAX_SPEED_SCALE) {
      return refused(`master speed must be between ${MIN_SPEED_SCALE} and ${MAX_SPEED_SCALE}`);
    }
  }
  for (const [name, value] of [
    ['dimmer', change.default_dimmer],
    ['crossfader', change.default_crossfader],
  ] as const) {
    if (value !== undefined && (!Number.isFinite(value) || value < MIN_FRACTION || value > MAX_FRACTION)) {
      return refused(`the ${name} must be between ${MIN_FRACTION} and ${MAX_FRACTION}`);
    }
  }

  return applied({ ...show, master: { ...show.master, ...change } });
}

// --- the composition ---------------------------------------------------------

/** What can be changed about the composition canvas. */
export interface CompositionChange {
  size?: SizePx;
  render_rate?: Composition['render_rate'];
}

/**
 * Changes the composition canvas, and says what that did to the pieces.
 *
 * The composition is what every piece is measured against: a piece's source
 * rectangle is in composition pixels, so resizing the canvas is not a change to
 * one thing. Two consequences, and the operator is told both — a piece whose
 * region the new canvas does not contain makes the whole show unsaveable,
 * and every other piece keeps its rectangle and therefore takes a different
 * part of the picture. Neither is guessed at: nothing here moves a piece to
 * make it fit.
 */
export function setComposition(show: Show, change: CompositionChange): Edit {
  if (change.size !== undefined) {
    const reason = refuseSize(change.size, 'the composition');
    if (reason !== null) {
      return refused(reason);
    }
  }
  if (change.render_rate !== undefined) {
    const reason = refuseRate(change.render_rate);
    if (reason !== null) {
      return refused(reason);
    }
  }

  const next: Show = {
    ...show,
    composition: { ...show.composition, ...change },
  };

  const resized =
    change.size !== undefined &&
    (change.size.width !== show.composition.size.width ||
      change.size.height !== show.composition.size.height);
  const note = resized ? describeResize(resizeReport(show, change.size!)) : null;

  return applied(next, note ?? undefined);
}

// --- layers ------------------------------------------------------------------

/**
 * Adds an empty layer on top of the stack.
 *
 * Its properties are the ones `Layer::new` gives a layer on the server, so a
 * layer added here and a layer added there are the same layer.
 */
export function addLayer(show: Show, name?: string): Edit {
  const order = show.layers.reduce((highest, layer) => Math.max(highest, layer.order + 1), 0);
  const chosen = (name ?? `Layer ${show.layers.length + 1}`).trim();
  if (chosen === '') {
    return refused('a layer name must not be empty');
  }

  const layer: Layer = {
    id: newId(),
    name: chosen,
    order,
    enabled: true,
    route: DEFAULT_ROUTE,
    default_opacity: DEFAULT_OPACITY,
    default_transform: { ...IDENTITY_TRANSFORM },
    blend_mode: DEFAULT_BLEND_MODE,
    slots: [],
    effects: [],
  };

  return applied({ ...show, layers: [...show.layers, layer] });
}

/**
 * Removes a layer, and any patch entry that was pointing at it.
 *
 * A fixture instance whose layer has gone makes the whole show unsaveable
 * (`validate_patch`), so it goes with the layer — and the operator is told,
 * because a patch that quietly lost an entry is a console whose faders have
 * moved on to something else.
 */
export function removeLayer(show: Show, layerId: string): Edit {
  if (!findLayer(show, layerId)) {
    return refused('that layer is not in this show');
  }

  const patch = show.fixture_patch ?? [];
  const kept = patch.filter(
    (fixture) => !(fixture.target.kind === 'layer' && fixture.target.layer_id === layerId),
  );
  const unpatched = patch.length - kept.length;

  const next: Show = {
    ...show,
    layers: show.layers.filter((layer) => layer.id !== layerId),
  };
  if (show.fixture_patch !== undefined) {
    next.fixture_patch = kept;
  }

  return applied(
    next,
    unpatched > 0
      ? `${unpatched} DMX patch ${unpatched === 1 ? 'entry' : 'entries'} for that layer ${
          unpatched === 1 ? 'was' : 'were'
        } removed with it`
      : undefined,
  );
}

/** Renames a layer. */
export function renameLayer(show: Show, layerId: string, name: string): Edit {
  if (name.trim() === '') {
    return refused('a layer name must not be empty');
  }
  return withLayer(show, layerId, (layer) => ({ ...layer, name }));
}

/**
 * Moves a layer one place through the composite.
 *
 * `up` is towards the front of the composite, which is the layer drawn over the
 * others. The two layers exchange their `order` values rather than the whole
 * stack being renumbered: the numbers a document arrived with are the
 * operator's, and an edit that rewrites every one of them to move one layer is
 * a bigger change than the one that was asked for.
 */
export function moveLayer(show: Show, layerId: string, direction: 'up' | 'down'): Edit {
  const ordered = orderedLayers(show);
  const position = ordered.findIndex((layer) => layer.id === layerId);
  if (position < 0) {
    return refused('that layer is not in this show');
  }

  const neighbour = ordered[direction === 'up' ? position + 1 : position - 1];
  const moving = ordered[position];
  if (!neighbour || !moving) {
    return refused(
      direction === 'up' ? 'that layer is already on top' : 'that layer is already at the bottom',
    );
  }

  const layers = show.layers.map((layer) => {
    if (layer.id === moving.id) {
      return { ...layer, order: neighbour.order };
    }
    if (layer.id === neighbour.id) {
      return { ...layer, order: moving.order };
    }
    return layer;
  });

  return applied({ ...show, layers });
}

/** What can be changed about a layer. All of it is configuration. */
export interface LayerChange {
  enabled?: boolean;
  route?: Layer['route'];
  blend_mode?: Layer['blend_mode'];
  default_opacity?: number;
  default_transform?: Transform2D;
}

/**
 * Changes a layer's configuration.
 *
 * Ranges are refused rather than clamped, for the reason a parameter is: this
 * is a document, and the useful answer to a value that cannot be stored is to
 * say so.
 */
export function setLayer(show: Show, layerId: string, change: LayerChange): Edit {
  if (change.default_opacity !== undefined) {
    const opacity = change.default_opacity;
    if (!Number.isFinite(opacity) || opacity < MIN_FRACTION || opacity > MAX_FRACTION) {
      return refused(`opacity must be between ${MIN_FRACTION} and ${MAX_FRACTION}`);
    }
  }
  if (change.default_transform !== undefined) {
    const reason = refuseTransform(change.default_transform);
    if (reason !== null) {
      return refused(reason);
    }
  }

  return withLayer(show, layerId, (layer) => ({ ...layer, ...change }));
}

// --- slots -------------------------------------------------------------------

/**
 * Puts a source in a slot, or replaces what is in one.
 *
 * Slot 0 is permanently Black on every layer and is not stored at all
 * (`Layer::BLACK_SLOT_INDEX`). It is refused here rather than
 * at the save, because there is nothing an operator could do about it later:
 * there is no separate layer-black mechanism to offer them instead.
 *
 * A generator is given a fresh instance identity on the way in, for the two
 * reasons [`identified`] gives.
 */
export function assignSlot(
  show: Show,
  layerId: string,
  index: number,
  source: SourceRef,
): Edit {
  if (index === BLACK_SLOT_INDEX) {
    return refused('slot 0 is permanently Black on every layer and cannot be assigned');
  }
  if (!Number.isInteger(index) || index < 0) {
    return refused('a slot index is a whole number');
  }

  return withLayer(show, layerId, (layer) => {
    const existing = findSlot(layer, index);
    if (existing) {
      // Replacing the source keeps the playback settings, the transform and the
      // effect chain: an operator swapping a clip for another one has not asked
      // to lose the way the slot was set up.
      return {
        ...layer,
        slots: layer.slots.map((slot) =>
          slot.index === index ? { ...slot, source: identified(source) } : slot,
        ),
      };
    }
    const slot: Slot = {
      index,
      source: identified(source),
      playback_mode: DEFAULT_PLAYBACK_MODE,
      direction: DEFAULT_PLAYBACK_DIRECTION,
      transform: { ...IDENTITY_TRANSFORM },
      effects: [],
    };
    return { ...layer, slots: [...layer.slots, slot].sort((a, b) => a.index - b.index) };
  });
}

/**
 * A source with its own identity, minting one for a generator.
 *
 * Always a fresh one, and never the one the source arrived with.
 *
 * **Fresh, because two slots are two instances.** The panel hands the same
 * source object to every slot an operator drops a generator into, and two slots
 * sharing one identity would be two slots sharing one set of live values — a
 * console moving the colour of the one on air would move the one waiting in the
 * next bank as well.
 *
 * **Here, because the alternative is phantom unsaved work.** The server gives
 * any generator without an identity one when it stores the show, so a
 * document sent without them comes back different from the one that was sent,
 * and the editor reads a document that came back different as unsaved work
 * (`isDirty`). Minting client-side makes the round trip exact and leaves the
 * server's own pass as the backstop it is meant to be: for documents written
 * before generators had identities, and for clients that are not this one.
 */
function identified(source: SourceRef): SourceRef {
  if (source.kind !== 'generator') {
    return source;
  }
  return { ...source, instance_id: newId() };
}

/** Empties a slot. The layer keeps playing whatever it is playing. */
export function clearSlot(show: Show, layerId: string, index: number): Edit {
  const layer = findLayer(show, layerId);
  if (!layer) {
    return refused('that layer is not in this show');
  }
  if (!findSlot(layer, index)) {
    return refused('that slot is already empty');
  }
  return withLayer(show, layerId, (current) => ({
    ...current,
    slots: current.slots.filter((slot) => slot.index !== index),
  }));
}

/** What can be changed about a slot. */
export interface SlotChange {
  playback_mode?: Slot['playback_mode'];
  direction?: Slot['direction'];
  transform?: Transform2D;
  /**
   * Whether this slot runs free or is locked to a number of beats.
   *
   * On the slot because how long a pass takes is a fact about the footage,
   * and a layer that carried it would apply one answer to every slot on it
   * (owner, 2026-09-23).
   */
  sync?: SlotSync;
}

/** Changes a slot's playback settings, its sync or its transform. */
export function setSlot(
  show: Show,
  layerId: string,
  index: number,
  change: SlotChange,
): Edit {
  if (change.transform !== undefined) {
    const reason = refuseTransform(change.transform);
    if (reason !== null) {
      return refused(reason);
    }
  }
  if (change.sync !== undefined && change.sync.kind === 'BEAT_SYNC') {
    const beats = change.sync.beats;
    // One and up, whole. `Slot::validate` says the same thing, and the useful
    // answer to half a beat is that it is not a number of beats rather than a
    // silent round to the nearest one.
    if (!Number.isInteger(beats) || beats < MIN_SLOT_BEATS) {
      return refused(`beats must be a whole number of at least ${MIN_SLOT_BEATS}`);
    }
  }
  return withSlot(show, layerId, index, (slot) => ({ ...slot, ...change }));
}

/**
 * Sets one parameter of a slot's generator, or puts it back to its default.
 *
 * `null` removes the value, which is not the same as setting it to zero: a
 * parameter a show does not mention is the one the generator was published
 * with, and leaving it out is how a show says "whatever this was".
 */
export function setGeneratorParameter(
  show: Show,
  catalogue: Catalogue,
  layerId: string,
  index: number,
  key: string,
  value: ParameterValue | null,
): Edit {
  const layer = findLayer(show, layerId);
  const slot = layer ? findSlot(layer, index) : null;
  if (!slot) {
    return refused('that slot is not configured');
  }
  if (slot.source.kind !== 'generator') {
    return refused('that slot does not play a generator');
  }

  const source = slot.source;
  const generator = findGenerator(catalogue, source.type_id, source.version);
  if (!generator) {
    return refused(`this server does not publish ${source.type_id} version ${source.version}`);
  }
  const declared = generator.parameters.find((parameter) => parameter.key === key);
  if (!declared) {
    return refused(`${source.type_id} version ${source.version} has no parameter ${key}`);
  }
  if (value !== null) {
    const reason = refuseParameter(declared, value);
    if (reason !== null) {
      return refused(`${declared.name} ${reason}`);
    }
  }

  return withSlot(show, layerId, index, (current) => {
    if (current.source.kind !== 'generator') {
      return current;
    }
    return {
      ...current,
      source: {
        ...current.source,
        parameters: writeParameter(current.source.parameters, key, value),
      },
    };
  });
}

// --- effect chains -----------------------------------------------------------

/**
 * Adds an effect to the end of a chain.
 *
 * A chain is ordered rather than mixed and there is no effect master
 *, so where an effect sits is the whole of what it means for it to be
 * "after" another one. New ones go at the end, which is where an operator
 * building a look adds them.
 *
 * The effect must be one this server publishes. Adding one it does not would
 * make a document it will refuse, and the refusal would arrive at the save
 * rather than at the button.
 */
export function addEffect(
  show: Show,
  catalogue: Catalogue,
  chain: ChainRef,
  typeId: string,
  version: number,
): Edit {
  if (!findEffect(catalogue, typeId, version)) {
    return refused(`this server does not publish ${typeId} version ${version}`);
  }
  // No parameter values: an effect that has just been added is the effect as it
  // was published, and every published effect is neutral at its defaults.
  const instance: EffectInstance = {
    id: newId(),
    type_id: typeId,
    version,
    enabled: true,
    parameters: {},
  };
  return withChain(show, chain, (effects) => [...effects, instance]);
}

/** Removes an effect from a chain. */
export function removeEffect(show: Show, chain: ChainRef, effectId: string): Edit {
  const effects = readChain(show, chain);
  if (!effects?.some((effect) => effect.id === effectId)) {
    return refused('that effect is not in this chain');
  }
  return withChain(show, chain, (current) =>
    current.filter((effect) => effect.id !== effectId),
  );
}

/** Moves an effect one place along its chain. */
export function moveEffect(
  show: Show,
  chain: ChainRef,
  effectId: string,
  direction: 'earlier' | 'later',
): Edit {
  const effects = readChain(show, chain);
  if (!effects) {
    return refused('that chain is not in this show');
  }
  const position = effects.findIndex((effect) => effect.id === effectId);
  if (position < 0) {
    return refused('that effect is not in this chain');
  }
  const target = direction === 'earlier' ? position - 1 : position + 1;
  if (target < 0 || target >= effects.length) {
    return refused(
      direction === 'earlier'
        ? 'that effect is already first in the chain'
        : 'that effect is already last in the chain',
    );
  }

  return withChain(show, chain, (current) => {
    const moved = [...current];
    const [effect] = moved.splice(position, 1);
    if (effect) {
      moved.splice(target, 0, effect);
    }
    return moved;
  });
}

/** Switches an effect in or out of its chain without removing it. */
export function setEffectEnabled(
  show: Show,
  chain: ChainRef,
  effectId: string,
  enabled: boolean,
): Edit {
  return withEffect(show, chain, effectId, (effect) => ({ ...effect, enabled }));
}

/**
 * Sets one parameter of an effect, or puts it back to its default.
 *
 * The range comes from the catalogue the server published, never from anything
 * written here, and a value outside it is refused rather than clamped.
 */
export function setEffectParameter(
  show: Show,
  catalogue: Catalogue,
  chain: ChainRef,
  effectId: string,
  key: string,
  value: ParameterValue | null,
): Edit {
  const effects = readChain(show, chain);
  const instance = effects?.find((effect) => effect.id === effectId);
  if (!instance) {
    return refused('that effect is not in this chain');
  }

  const published = findEffect(catalogue, instance.type_id, instance.version);
  if (!published) {
    return refused(
      `this server does not publish ${instance.type_id} version ${instance.version}`,
    );
  }
  const declared = published.parameters.find((parameter) => parameter.key === key);
  if (!declared) {
    return refused(`${instance.type_id} version ${instance.version} has no parameter ${key}`);
  }
  if (value !== null) {
    const reason = refuseParameter(declared, value);
    if (reason !== null) {
      return refused(`${declared.name} ${reason}`);
    }
  }

  return withEffect(show, chain, effectId, (effect) => ({
    ...effect,
    parameters: writeParameter(effect.parameters, key, value),
  }));
}

// --- outputs -----------------------------------------------------------------

/**
 * Adds an output on one of the connectors this server reported.
 *
 * Never on a connector of this client's own invention. An output is forced onto
 * a socket by GPU and connector and stays there whatever is plugged in
 *, so both halves come out of `/server/capabilities`, and so
 * does the mode: an operator choosing a mode for a wall needs the list the
 * driver gave, and a mode made up here is one the panel may not have.
 *
 * The EDID fields are deliberately not copied into the document. They are what
 * was discovered on the socket, they decide nothing, and a show carrying the
 * serial of the processor that happened to be in the rack that night would
 * invite somebody to match on it later.
 */
export function addOutput(
  show: Show,
  connector: ConnectorInfo,
  mode: ConnectorModeInfo,
  name?: string,
): Edit {
  const key = connectorKey(connector.gpu_pci_address, connector.connector);

  if (connector.role === 'SERVICE_DISPLAY') {
    return refused(
      `${key} drives the service console on this server. Show output never shares it.`,
    );
  }
  if (show.outputs.some((output) => connectorOf(output) === key)) {
    return refused(`${key} already drives an output in this show, and one socket drives one`);
  }

  const rate = rateFromMillihertz(mode.refresh_millihertz);
  if (rate === null) {
    return refused(`this server reported no usable refresh rate for that mode`);
  }
  const sizeReason = refuseSize(mode.size, 'an output mode');
  if (sizeReason !== null) {
    return refused(sizeReason);
  }

  // The model name when the panel gave one, the socket when it did not. Both
  // are things that were reported; neither is a name this client thought of.
  const chosen = (name ?? connector.model ?? connector.connector).trim();
  if (chosen === '') {
    return refused('an output name must not be empty');
  }

  const output: OutputConfig = {
    id: newId(),
    name: chosen,
    enabled: DEFAULT_OUTPUT_ENABLED,
    identity: {
      gpu_pci_address: connector.gpu_pci_address,
      connector: connector.connector,
    },
    mode: { size: { ...mode.size }, refresh: rate },
  };

  return applied(
    { ...show, outputs: [...show.outputs, output] },
    connector.role === 'UNUSED'
      ? `${key} is not marked as a media output on this server. What a socket is for is set on the server, not in a show.`
      : undefined,
  );
}

/**
 * Whether another output in this show already publishes under this name.
 *
 * The one thing two network outputs may not share. Everything else about a
 * duplicate name is untidy; this one is two senders announcing the same thing,
 * and somebody picking it from a list in another room has no way to tell which
 * of the two they got.
 */
function alreadyPublished(show: Show, name: string, except: string | null): boolean {
  const wanted = name.trim();
  return show.outputs.some(
    (each) => each.id !== except && each.on_the_network === true && each.name.trim() === wanted,
  );
}

/**
 * Adds an output that publishes on the network instead of driving a socket.
 *
 * One name. The stream carries the output's own, and the server puts its machine
 * in front of it — `Wall Left` on a server called `OVS-01` is announced as
 * `OVS-01 (Wall Left)`. What is stored is what was typed, so moving a show to
 * another machine does not rename every stream in it.
 *
 * The mode is typed rather than chosen. A stream has no EDID and no driver to
 * offer a list: the size is whatever the operator wants sampled out of the
 * composition, and the rate is what the frames are stamped with.
 */
export function addNetworkOutput(show: Show, name: string, mode: DisplayMode): Edit {
  const wanted = name.trim();
  if (wanted === '') {
    return refused('an output name must not be empty');
  }
  if (alreadyPublished(show, wanted, null)) {
    return refused(
      `${wanted} is already published by an output in this show. ` +
        'Two senders with one name cannot be told apart from another machine.',
    );
  }
  const sizeReason = refuseSize(mode.size, 'a stream size');
  if (sizeReason !== null) {
    return refused(sizeReason);
  }

  const output: OutputConfig = {
    id: newId(),
    name: wanted,
    enabled: DEFAULT_OUTPUT_ENABLED,
    // Empty, and the server refuses it as anything else: an output cannot be a
    // socket and a stream at once.
    identity: {},
    mode: { size: { ...mode.size }, refresh: { ...mode.refresh } },
    on_the_network: true,
  };

  return applied({ ...show, outputs: [...show.outputs, output] });
}

/**
 * Moves an output onto the network, off whatever socket it was on.
 *
 * The other direction is [`setOutputConnector`]. Neither is decided once: a wall
 * unplugged for an evening has a feed that still has to go somewhere, and a
 * stream that turns out to need a cable should not have to be rebuilt with its
 * pieces.
 *
 * It publishes under the output's own name, so anything watching a stream by
 * that name on this server finds it.
 */
export function setOutputOnNetwork(show: Show, outputId: string): Edit {
  const output = findOutput(show, outputId);
  if (!output) {
    return refused('that output is not in this show');
  }
  if (alreadyPublished(show, output.name, outputId)) {
    return refused(
      `${output.name.trim()} is already published by another output in this show. ` +
        'Two senders with one name cannot be told apart from another machine.',
    );
  }

  return applied({
    ...show,
    outputs: show.outputs.map((each) =>
      each.id === outputId ? { ...each, identity: {}, on_the_network: true } : each,
    ),
  });
}

/**
 * Removes an output, and the pieces that were drawing to it.
 *
 * A piece whose output has gone makes the whole show unsaveable
 * (`validate_pieces`), so it goes with the output — and the operator is told,
 * because a mapping that quietly lost a piece is a wall that goes dark.
 */
export function removeOutput(show: Show, outputId: string): Edit {
  const output = findOutput(show, outputId);
  if (!output) {
    return refused('that output is not in this show');
  }

  const orphaned = piecesOf(show, outputId).length;
  const next: Show = {
    ...show,
    outputs: show.outputs.filter((each) => each.id !== outputId),
    pieces: show.pieces.filter((piece) => piece.output_id !== outputId),
  };

  return applied(
    next,
    orphaned > 0
      ? `${orphaned} ${orphaned === 1 ? 'piece was' : 'pieces were'} drawing to ${output.name} and ${orphaned === 1 ? 'was' : 'were'} removed with it`
      : undefined,
  );
}

/** Renames an output. */
export function renameOutput(show: Show, outputId: string, name: string): Edit {
  if (name.trim() === '') {
    return refused('an output name must not be empty');
  }
  // A network output's name is its name on the network, so renaming one is a
  // rename out there too — and two senders with one name cannot be told apart.
  const output = findOutput(show, outputId);
  if (output?.on_the_network === true && alreadyPublished(show, name, outputId)) {
    return refused(
      `${name.trim()} is already published by another output in this show. ` +
        'Two senders with one name cannot be told apart from another machine.',
    );
  }
  return withOutput(show, outputId, (each) => ({ ...each, name }));
}

/** Switches an output on or off. Its pieces stay in the document either way. */
export function setOutputEnabled(show: Show, outputId: string, enabled: boolean): Edit {
  return withOutput(show, outputId, (output) => ({ ...output, enabled }));
}

/**
 * Puts an output on a different mode.
 *
 * The mode comes from the connector's own list, so this takes a
 * [`DisplayMode`] that has already been read out of one rather than a size and
 * a rate typed here.
 */
/**
 * Moves an output to another socket.
 *
 * An output is forced onto a connector and stays there whatever is plugged in,
 * but which connector that is is an operator's decision: a wall moved to
 * another card on a show day is ordinary, and rebuilding the output to say so
 * would take its pieces with it.
 *
 * The same two refusals as making one. A socket drives one output, and the
 * System screen's is not a wall.
 */
export function setOutputConnector(
  show: Show,
  outputId: string,
  gpuPciAddress: string,
  connector: string,
): Edit {
  const output = findOutput(show, outputId);
  if (!output) {
    return refused('that output is not in this show');
  }

  const key = connectorKey(gpuPciAddress, connector);
  if (show.outputs.some((each) => each.id !== outputId && connectorOf(each) === key)) {
    return refused(`${key} already drives an output in this show, and one socket drives one`);
  }

  return applied({
    ...show,
    outputs: show.outputs.map((each) =>
      each.id === outputId
        ? {
            ...each,
            identity: { gpu_pci_address: gpuPciAddress, connector },
            // Off the network, because an output cannot be both and a document
            // claiming both is one the server refuses.
            on_the_network: false,
          }
        : each,
    ),
  });
}

export function setOutputMode(show: Show, outputId: string, mode: DisplayMode): Edit {
  const sizeReason = refuseSize(mode.size, 'an output mode');
  if (sizeReason !== null) {
    return refused(sizeReason);
  }
  const rateReason = refuseRate(mode.refresh);
  if (rateReason !== null) {
    return refused(rateReason);
  }

  const output = findOutput(show, outputId);
  if (!output) {
    return refused('that output is not in this show');
  }

  // A smaller panel can leave a piece drawing to nothing. Said rather than
  // fixed: where a piece belongs on a wall is the operator's decision.
  const lost = piecesOf(show, outputId).filter(
    (piece) => piece.rotation_deg === 0 && isOffScreen(piece.dest_rect, mode.size),
  );

  return applied(
    { ...show, outputs: show.outputs.map((each) => (each.id === outputId ? { ...each, mode } : each)) },
    lost.length === 0
      ? undefined
      : `${lost.length} ${lost.length === 1 ? 'piece now lies' : 'pieces now lie'} entirely outside ${output.name} (${lost
          .map((piece) => piece.name)
          .join(', ')}) and would not be drawn.`,
  );
}

// --- pieces ------------------------------------------------------------------

/**
 * Adds a piece taking the whole composition onto the whole of an output.
 *
 * The rectangles are the two surfaces the document already holds the size of,
 * so a new piece starts as the one mapping that is certainly meaningful — the
 * whole picture on the whole wall — and nothing about it is a number this
 * client chose.
 */
export function addPiece(show: Show, outputId: string, name?: string): Edit {
  const output = findOutput(show, outputId);
  if (!output) {
    return refused('that output is not in this show');
  }

  const chosen = (name ?? `Piece ${show.pieces.length + 1}`).trim();
  if (chosen === '') {
    return refused('a piece name must not be empty');
  }

  const composition = show.composition.size;
  const piece: Piece = {
    id: newId(),
    name: chosen,
    enabled: DEFAULT_PIECE_ENABLED,
    order: DEFAULT_PIECE_ORDER,
    output_id: outputId,
    source_rect: { x: 0, y: 0, width: composition.width, height: composition.height },
    dest_rect: { x: 0, y: 0, width: output.mode.size.width, height: output.mode.size.height },
    rotation_deg: DEFAULT_PIECE_ROTATION_DEG,
  };

  return applied({ ...show, pieces: [...show.pieces, piece] });
}

/**
 * Copies a piece, on the same output and over the top of it.
 *
 * Duplicate exists because a wall is usually several pieces of the
 * same shape. The copy is deliberately in the same place: an operator moves it
 * where it belongs, and a copy that appeared somewhere chosen here would be a
 * position nobody decided.
 */
export function duplicatePiece(show: Show, pieceId: string): Edit {
  const piece = findPiece(show, pieceId);
  if (!piece) {
    return refused('that piece is not in this show');
  }
  const copy: Piece = {
    ...piece,
    id: newId(),
    name: `${piece.name} copy`,
    source_rect: { ...piece.source_rect },
    dest_rect: { ...piece.dest_rect },
  };
  return applied({ ...show, pieces: [...show.pieces, copy] });
}

/** Removes a piece. */
export function removePiece(show: Show, pieceId: string): Edit {
  if (!findPiece(show, pieceId)) {
    return refused('that piece is not in this show');
  }
  return applied({ ...show, pieces: show.pieces.filter((piece) => piece.id !== pieceId) });
}

/** Renames a piece. */
export function renamePiece(show: Show, pieceId: string, name: string): Edit {
  if (name.trim() === '') {
    return refused('a piece name must not be empty');
  }
  return withPiece(show, pieceId, (piece) => ({ ...piece, name }));
}

/** What can be changed about a piece other than its two rectangles. */
export interface PieceChange {
  enabled?: boolean;
  order?: number;
  rotation_deg?: number;
  output_id?: string;
  /**
   * Which picture this piece takes its rectangle out of.
   *
   * The composition, or one of the three buses (owner, 2026-09-23). A wall fed
   * from a bus is a monitor of the look coming up rather than the one going
   * out, which is the thing a crossfade is judged against.
   */
  source?: PieceSource;
  /**
   * The point the piece turns about, as a fraction of its own size.
   *
   * Fractions rather than pixels so that resizing the piece does not move
   * the point the operator picked (`AnchorPoint`).
   */
  anchor?: AnchorPoint;
  /**
   * How far the rectangle *taken from the composition* is turned.
   *
   * Both ends of a piece turn (owner, 2026-09-23). The rotation above places
   * the rectangle on a wall at an angle; this one takes it out of the
   * composition at an angle, which is what a wall hung on the diagonal has to
   * be fed.
   */
  source_rotation_deg?: number;
  /** The point in the source rectangle that turn happens about. */
  source_anchor?: AnchorPoint;
}

/**
 * Changes a piece's draw order, either rotation or anchor, output or whether it
 * is drawn.
 */
export function setPiece(show: Show, pieceId: string, change: PieceChange): Edit {
  if (change.order !== undefined) {
    const order = change.order;
    if (!Number.isInteger(order) || order < MIN_PIECE_ORDER || order > MAX_PIECE_ORDER) {
      return refused(`a draw order is a whole number between ${MIN_PIECE_ORDER} and ${MAX_PIECE_ORDER}`);
    }
  }
  for (const angle of [change.rotation_deg, change.source_rotation_deg]) {
    if (angle !== undefined && !Number.isFinite(angle)) {
      return refused('a rotation must be a finite number');
    }
  }
  for (const point of [change.anchor, change.source_anchor]) {
    if (point === undefined) {
      continue;
    }
    const { x, y } = point;
    if (![x, y].every((value) => Number.isFinite(value) && value >= 0 && value <= 1)) {
      return refused('an anchor point is a fraction of the piece, between 0 and 1');
    }
  }
  if (change.output_id !== undefined && !findOutput(show, change.output_id)) {
    return refused('that output is not in this show');
  }

  const moved =
    change.output_id !== undefined ? findOutput(show, change.output_id) : null;
  const piece = findPiece(show, pieceId);
  const note =
    moved !== null && piece !== null && piece.rotation_deg === 0 && isOffScreen(piece.dest_rect, moved.mode.size)
      ? `that rectangle lies entirely outside ${moved.name}, so nothing of this piece is drawn there`
      : undefined;

  const edit = withPiece(show, pieceId, (current) => ({ ...current, ...change }));
  return edit.ok && note !== undefined ? applied(edit.show, note) : edit;
}

/**
 * Sets the region of the composition a piece takes.
 *
 * Refused outside the composition, in the server's own words: `Piece::validate`
 * will not have it, and an operator stopped at the field can put it right
 * without a round trip. The destination side has no such rule — a piece may
 * legitimately hang off the edge of its output — which is why the two are
 * separate functions rather than one.
 */
export function setPieceSource(show: Show, pieceId: string, rect: RectPx): Edit {
  const reason = refuseRect(rect);
  if (reason !== null) {
    return refused(reason);
  }
  const composition = show.composition.size;
  if (!fitsWithin(rect, composition)) {
    return refused(
      `must lie within the ${composition.width}x${composition.height} composition`,
    );
  }
  return withPiece(show, pieceId, (piece) => ({ ...piece, source_rect: { ...rect } }));
}

/**
 * Sets the region of the output a piece fills.
 *
 * Hanging off an edge is allowed, because it is a real mapping choice and the
 * model says so — but the operator is told how much of the piece that costs.
 * Lying *entirely* outside is refused: `OutputMapping::build` skips a piece the
 * renderer would draw nothing of, so it is not a mapping at all, and an
 * operator would be looking at a rectangle in this editor that has no
 * counterpart on the wall.
 *
 * A turned piece is exempt from both, exactly as the renderer is: its quad is
 * the rectangle rotated about its own centre, so one placed just past an edge
 * can turn back into view.
 */
export function setPieceDest(show: Show, pieceId: string, rect: RectPx): Edit {
  const reason = refuseRect(rect);
  if (reason !== null) {
    return refused(reason);
  }

  const piece = findPiece(show, pieceId);
  if (!piece) {
    return refused('that piece is not in this show');
  }
  const output = findOutput(show, piece.output_id);
  if (!output) {
    return refused('this piece draws to an output that is not in this show');
  }

  const size = output.mode.size;
  let note: string | undefined;
  if (piece.rotation_deg === 0) {
    if (isOffScreen(rect, size)) {
      return refused(
        `that rectangle lies entirely outside ${output.name}, which is ${size.width}×${size.height}, so nothing of this piece would be drawn`,
      );
    }
    const spill = spillOf(rect, size);
    if (spill > 0) {
      note = `${spill} px of this piece falls outside ${output.name} and is not drawn.`;
    }
  }

  const edit = withPiece(show, pieceId, (current) => ({ ...current, dest_rect: { ...rect } }));
  return edit.ok && note !== undefined ? applied(edit.show, note) : edit;
}

/**
 * Moves a piece one place through its output's draw order.
 *
 * `later` is towards the front, which is the piece drawn over the others where
 * they meet. Two pieces exchange their `order` values rather than the whole
 * output being renumbered, for the reason `moveLayer` does it: the numbers a
 * document arrived with are the operator's.
 *
 * Two pieces may share an order, which the model allows and the renderer
 * settles by keeping the order the show lists them in. When that is what
 * separates them, this exchanges their places in the document instead — which
 * is the thing that actually decides between them.
 */
export function movePiece(show: Show, pieceId: string, direction: 'earlier' | 'later'): Edit {
  const piece = findPiece(show, pieceId);
  if (!piece) {
    return refused('that piece is not in this show');
  }

  const sequence = sequenceOf(show, piece.output_id);
  const position = sequence.findIndex((each) => each.id === pieceId);
  const neighbour = sequence[direction === 'later' ? position + 1 : position - 1];
  if (!neighbour) {
    return refused(
      direction === 'later'
        ? 'that piece is already drawn last on its output'
        : 'that piece is already drawn first on its output',
    );
  }

  if (neighbour.order !== piece.order) {
    return applied({
      ...show,
      pieces: show.pieces.map((each) => {
        if (each.id === piece.id) {
          return { ...each, order: neighbour.order };
        }
        if (each.id === neighbour.id) {
          return { ...each, order: piece.order };
        }
        return each;
      }),
    });
  }

  const pieces = [...show.pieces];
  const here = pieces.findIndex((each) => each.id === piece.id);
  const there = pieces.findIndex((each) => each.id === neighbour.id);
  const mine = pieces[here];
  const theirs = pieces[there];
  if (!mine || !theirs) {
    return refused('that piece is not in this show');
  }
  pieces[here] = theirs;
  pieces[there] = mine;
  return applied({ ...show, pieces });
}

// --- validation --------------------------------------------------------------

/**
 * Every rule the server applies that this editor can check, with its paths.
 *
 * Not a second opinion: the server decides, and its answer to a save is the one
 * that counts. This exists so an operator is stopped at the control that is
 * wrong rather than at the save button, and so the editor can say what a
 * refused save meant without waiting to be told.
 *
 * It covers the parts of the document this editor edits — the name, the master
 * settings, the composition, layers, slots, effect chains, pieces, outputs and
 * the DMX patch. `fixtures` is the personality catalogue the server published;
 * without it the patch's personalities are not checked, because reporting that
 * a personality is unpublished before the catalogue has arrived would be
 * reporting this client's ignorance as a fault in the operator's document.
 *
 * What is *not* here is anything the server would accept. Two pieces
 * overlapping on one output is a legitimate mapping the renderer settles by
 * draw order, and calling it an error would teach an operator to ignore the
 * red marks. That lives in `mapping.advisories` instead, and the patch's
 * equivalent in `patch.patchAdvisories`.
 */
export function validate(
  show: Show,
  catalogue: Catalogue,
  fixtures: FixtureCatalogue = emptyFixtures(),
): ValidationError[] {
  const errors: ValidationError[] = [];
  const fault = (path: string, message: string) => errors.push({ path, message });

  if (show.name.trim() === '') {
    fault('name', 'must not be empty');
  }
  if (show.schema_version === 0) {
    fault('schema_version', 'must be at least 1');
  }

  validateComposition(show, fault);
  validateMaster(show, fault);
  validateOutputs(show, fault);
  validatePieces(show, fault);
  validatePatch(show, fixtures, fault);

  const seenIds = new Set<string>();
  const seenOrders = new Set<number>();
  for (const [index, layer] of show.layers.entries()) {
    const path = `layers[${index}]`;
    if (seenIds.has(layer.id)) {
      fault(`${path}.id`, `duplicate layer id ${layer.id}`);
    }
    seenIds.add(layer.id);
    if (seenOrders.has(layer.order)) {
      fault(
        `${path}.order`,
        `composite order ${layer.order} is used by more than one layer, which makes compositing non-deterministic`,
      );
    }
    seenOrders.add(layer.order);
    validateLayer(layer, path, catalogue, fault);
  }

  return errors;
}

type Fault = (path: string, message: string) => void;

function validateComposition(show: Show, fault: Fault): void {
  validateSize(show.composition.size, 'composition.size', fault);
  validateRate(show.composition.render_rate, 'composition.render_rate', fault);
}

/**
 * Outputs, exactly as `Show::validate_outputs` checks them.
 *
 * Which connector an output is forced onto is not checked, because nothing in
 * the show model checks it: an output that names a socket this machine does
 * not have is a show moved between servers, not a broken document, and the
 * renderer faults it rather than the save refusing it.
 */
function validateOutputs(show: Show, fault: Fault): void {
  const seen = new Set<string>();
  for (const [index, output] of show.outputs.entries()) {
    const path = `outputs[${index}]`;
    if (seen.has(output.id)) {
      fault(`${path}.id`, `duplicate output id ${output.id}`);
    }
    seen.add(output.id);
    if (output.name.trim() === '') {
      fault(`${path}.name`, 'must not be empty');
    }
    validateSize(output.mode.size, `${path}.mode.size`, fault);
    validateRate(output.mode.refresh, `${path}.mode.refresh`, fault);
  }
}

/** Pieces, exactly as `Piece::validate` checks them. */
function validatePieces(show: Show, fault: Fault): void {
  const composition = show.composition.size;
  const outputs = new Set(show.outputs.map((output) => output.id));
  const seen = new Set<string>();

  for (const [index, piece] of show.pieces.entries()) {
    const path = `pieces[${index}]`;
    if (seen.has(piece.id)) {
      fault(`${path}.id`, `duplicate piece id ${piece.id}`);
    }
    seen.add(piece.id);

    if (piece.name.trim() === '') {
      fault(`${path}.name`, 'must not be empty');
    }
    for (const [name, rect] of [
      ['source_rect', piece.source_rect],
      ['dest_rect', piece.dest_rect],
    ] as const) {
      if (isEmpty(rect)) {
        fault(`${path}.${name}`, 'width and height must both be greater than zero');
      }
    }
    if (!Number.isFinite(piece.rotation_deg)) {
      fault(`${path}.rotation_deg`, 'must be a finite number');
    }
    if (!fitsWithin(piece.source_rect, composition)) {
      fault(
        `${path}.source_rect`,
        `must lie within the ${composition.width}x${composition.height} composition`,
      );
    }
    if (!outputs.has(piece.output_id)) {
      fault(
        `${path}.output_id`,
        `references output ${piece.output_id} which is not configured`,
      );
    }
  }
}

function validateSize(size: SizePx, path: string, fault: Fault): void {
  if (size.width === 0 || size.height === 0) {
    fault(path, 'width and height must both be greater than zero');
  }
}

function validateRate(rate: Composition['render_rate'], path: string, fault: Fault): void {
  if (rate.numerator === 0 || rate.denominator === 0) {
    fault(path, 'numerator and denominator must both be greater than zero');
  }
}

function validateMaster(show: Show, fault: Fault): void {
  const master = show.master;
  if (!Number.isFinite(master.default_bpm) || master.default_bpm < MIN_BPM || master.default_bpm > MAX_BPM) {
    fault('master.default_bpm', `must be between ${MIN_BPM} and ${MAX_BPM}`);
  }
  if (master.beats_per_bar === 0) {
    fault('master.beats_per_bar', 'must be greater than zero');
  }
  if (
    !Number.isFinite(master.default_speed_scale) ||
    master.default_speed_scale < MIN_SPEED_SCALE ||
    master.default_speed_scale > MAX_SPEED_SCALE
  ) {
    fault('master.default_speed_scale', `must be between ${MIN_SPEED_SCALE} and ${MAX_SPEED_SCALE}`);
  }
  for (const [name, value] of [
    ['default_dimmer', master.default_dimmer],
    ['default_crossfader', master.default_crossfader],
  ] as const) {
    if (!Number.isFinite(value) || value < MIN_FRACTION || value > MAX_FRACTION) {
      fault(`master.${name}`, `must be between ${MIN_FRACTION} and ${MAX_FRACTION}`);
    }
  }
}

function validateLayer(layer: Layer, path: string, catalogue: Catalogue, fault: Fault): void {
  if (layer.name.trim() === '') {
    fault(`${path}.name`, 'must not be empty');
  }
  if (
    !Number.isFinite(layer.default_opacity) ||
    layer.default_opacity < MIN_FRACTION ||
    layer.default_opacity > MAX_FRACTION
  ) {
    fault(`${path}.default_opacity`, `must be between ${MIN_FRACTION} and ${MAX_FRACTION}`);
  }
  validateTransform(layer.default_transform, `${path}.default_transform`, fault);

  const seen = new Set<number>();
  for (const [position, slot] of layer.slots.entries()) {
    const slotPath = `${path}.slots[${position}]`;
    if (slot.index === BLACK_SLOT_INDEX) {
      fault(`${slotPath}.index`, 'slot 0 is permanently Black and cannot be configured');
    }
    if (seen.has(slot.index)) {
      fault(`${slotPath}.index`, `duplicate slot index ${slot.index}`);
    }
    seen.add(slot.index);
    validateSlot(slot, slotPath, catalogue, fault);
  }

  validateChain(layer.effects, path, catalogue, fault);
}

function validateSlot(slot: Slot, path: string, catalogue: Catalogue, fault: Fault): void {
  validateTransform(slot.transform, `${path}.transform`, fault);

  if (slot.source.kind === 'generator') {
    const source = slot.source;
    if (source.type_id.trim() === '') {
      fault(`${path}.source.type_id`, 'must not be empty');
    } else if (catalogue.loaded) {
      const generator = findGenerator(catalogue, source.type_id, source.version);
      if (!generator) {
        fault(
          `${path}.source.type_id`,
          `this build does not publish ${source.type_id} version ${source.version}`,
        );
      } else {
        for (const problem of parameterProblems(generator, source.parameters)) {
          fault(`${path}.source.parameters.${problem.key}`, problem.reason);
        }
      }
    }
  }

  validateChain(slot.effects, path, catalogue, fault);
}

function validateChain(
  effects: EffectInstance[],
  path: string,
  catalogue: Catalogue,
  fault: Fault,
): void {
  for (const [position, effect] of effects.entries()) {
    const effectPath = `${path}.effects[${position}]`;
    if (effect.type_id.trim() === '') {
      fault(`${effectPath}.type_id`, 'must not be empty');
      continue;
    }
    if (effect.version === 0) {
      fault(`${effectPath}.version`, 'manifest versions start at 1');
      continue;
    }
    if (!catalogue.loaded) {
      continue;
    }
    const published = findEffect(catalogue, effect.type_id, effect.version);
    if (!published) {
      fault(
        `${effectPath}.type_id`,
        `this build does not publish ${effect.type_id} version ${effect.version}`,
      );
      continue;
    }
    for (const problem of parameterProblems(published, effect.parameters)) {
      fault(`${effectPath}.parameters.${problem.key}`, problem.reason);
    }
  }
}

function validateTransform(transform: Transform2D, path: string, fault: Fault): void {
  for (const name of ['x', 'y', 'scale_x', 'scale_y', 'rotation_deg'] as const) {
    if (!Number.isFinite(transform[name])) {
      fault(`${path}.${name}`, 'must be a finite number');
    }
  }
}

function refuseTransform(transform: Transform2D): string | null {
  for (const name of ['x', 'y', 'scale_x', 'scale_y', 'rotation_deg'] as const) {
    if (!Number.isFinite(transform[name])) {
      return `${name} must be a finite number`;
    }
  }
  return null;
}

/** Why a surface size cannot be stored, or null. */
function refuseSize(size: SizePx, what: string): string | null {
  for (const axis of ['width', 'height'] as const) {
    const value = size[axis];
    if (!Number.isInteger(value) || value < MIN_DIMENSION_PX || value > MAX_DIMENSION_PX) {
      return `the ${axis} of ${what} is a whole number of pixels, at least ${MIN_DIMENSION_PX}`;
    }
  }
  return null;
}

/**
 * Why a rectangle cannot be stored, or null.
 *
 * Offsets are signed and may be negative — `RectPx` says so, because a piece
 * positioned partly outside its output is a legitimate mapping choice — so only
 * the extent is bounded here.
 */
function refuseRect(rect: RectPx): string | null {
  for (const axis of ['x', 'y'] as const) {
    if (!Number.isInteger(rect[axis])) {
      return `${axis} is a whole number of pixels`;
    }
  }
  for (const axis of ['width', 'height'] as const) {
    const value = rect[axis];
    if (!Number.isInteger(value) || value < MIN_DIMENSION_PX || value > MAX_DIMENSION_PX) {
      return `${axis} is a whole number of pixels, at least ${MIN_DIMENSION_PX}`;
    }
  }
  return null;
}

/** Why a frame rate cannot be stored, or null. */
function refuseRate(rate: Composition['render_rate']): string | null {
  for (const part of ['numerator', 'denominator'] as const) {
    const value = rate[part];
    if (!Number.isInteger(value) || value < 1 || value > MAX_DIMENSION_PX) {
      return 'numerator and denominator must both be whole numbers greater than zero';
    }
  }
  return null;
}

// --- copying along the path being changed ------------------------------------
//
// Every one of these spreads the thing it is changing, so whatever else was in
// it — including anything this build does not know about — is still there
// afterwards.

function withLayer(show: Show, layerId: string, change: (layer: Layer) => Layer): Edit {
  const index = show.layers.findIndex((layer) => layer.id === layerId);
  const layer = show.layers[index];
  if (!layer) {
    return refused('that layer is not in this show');
  }
  const layers = [...show.layers];
  layers[index] = change(layer);
  return applied({ ...show, layers });
}

function withOutput(
  show: Show,
  outputId: string,
  change: (output: OutputConfig) => OutputConfig,
): Edit {
  const index = show.outputs.findIndex((output) => output.id === outputId);
  const output = show.outputs[index];
  if (!output) {
    return refused('that output is not in this show');
  }
  const outputs = [...show.outputs];
  outputs[index] = change(output);
  return applied({ ...show, outputs });
}

function withPiece(show: Show, pieceId: string, change: (piece: Piece) => Piece): Edit {
  const index = show.pieces.findIndex((piece) => piece.id === pieceId);
  const piece = show.pieces[index];
  if (!piece) {
    return refused('that piece is not in this show');
  }
  const pieces = [...show.pieces];
  pieces[index] = change(piece);
  return applied({ ...show, pieces });
}

function withSlot(
  show: Show,
  layerId: string,
  index: number,
  change: (slot: Slot) => Slot,
): Edit {
  const layer = findLayer(show, layerId);
  if (!layer) {
    return refused('that layer is not in this show');
  }
  if (!findSlot(layer, index)) {
    return refused('that slot is not configured');
  }
  return withLayer(show, layerId, (current) => ({
    ...current,
    slots: current.slots.map((slot) => (slot.index === index ? change(slot) : slot)),
  }));
}

function withChain(
  show: Show,
  chain: ChainRef,
  change: (effects: EffectInstance[]) => EffectInstance[],
): Edit {
  if (chain.slotIndex === undefined) {
    return withLayer(show, chain.layerId, (layer) => ({
      ...layer,
      effects: change(layer.effects),
    }));
  }
  return withSlot(show, chain.layerId, chain.slotIndex, (slot) => ({
    ...slot,
    effects: change(slot.effects),
  }));
}

function withEffect(
  show: Show,
  chain: ChainRef,
  effectId: string,
  change: (effect: EffectInstance) => EffectInstance,
): Edit {
  const effects = readChain(show, chain);
  if (!effects?.some((effect) => effect.id === effectId)) {
    return refused('that effect is not in this chain');
  }
  return withChain(show, chain, (current) =>
    current.map((effect) => (effect.id === effectId ? change(effect) : effect)),
  );
}

/**
 * Writes or removes one parameter value, leaving the rest of the map alone.
 *
 * Handed back in key order, which is the order the server writes one in: a
 * `ParameterMap` is a `BTreeMap` on that side, "so that serialised shows are
 * byte-stable" (`config/slot.rs`). Written in the order the manifest declares
 * them, as this used to be, a map the client built and the same map read back
 * off the server were the same document and a different string — which is all
 * `isDirty` looks at, so a document that had just been loaded could read as
 * having unsaved work in it. Every manifest key is ASCII, so sorting UTF-16 code
 * units here is the same ordering as sorting bytes there.
 */
function writeParameter(
  values: ParameterMap | undefined,
  key: string,
  value: ParameterValue | null,
): ParameterMap {
  const next: ParameterMap = { ...(values ?? {}) };
  if (value === null) {
    delete next[key];
  } else {
    next[key] = value;
  }

  const ordered: ParameterMap = {};
  for (const name of Object.keys(next).sort()) {
    ordered[name] = next[name]!;
  }
  return ordered;
}
