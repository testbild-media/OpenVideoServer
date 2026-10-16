// What the window is given to draw.
//
// The mirror holds the server's model; this is the same thing arranged the way
// Live Mode reads it — a layer with its slots beside it, in composite order,
// with the configuration and the live state already joined.
//
// It exists for two reasons. The window is sandboxed and talks to the main
// process through structured clone, which does not carry a `Map`; and a view
// that is computed once, here, is a view the tests can check without a browser.

import type { OutputConfig, ParameterValue, Piece, Transform2D } from '@ovs/schema';

import type {
  BlendMode,
  PlaybackDirection,
  PlaybackMode,
  Route,
  SlotSource,
  SlotSync,
  Transport,
} from './types.js';
import { IDENTITY_TRANSFORM } from './limits.js';
import { layersInOrder, type ShowState } from './mirror.js';

/**
 * One effect in a chain, as the work area reads it: configured and live at once.
 *
 * Both halves travel, because an operator has to be able to see which of the two
 * they are looking at. `parameters` is the document — what the effect opens with
 * and what a reset puts it back to — and `live` is what a control source has
 * done to it since. The chain itself, and the
 * configured values, are still edited in Settings; the live values are changed from
 * here, through `PUT /live/effects/{id}`.
 */
export interface EffectView {
  id: string;
  typeId: string;
  version: number;
  /** Whether the chain runs it at all. */
  enabled: boolean;
  /** The values the show set, keyed by the manifest's own names. */
  parameters: Record<string, ParameterValue>;
  /**
   * The values a control source has moved since the show opened.
   *
   * Only the parameters somebody has touched. One that is absent is not at its
   * configured value *here*; it has no live value at all, and what it is doing
   * is whatever the show configured. Empty for an instance nobody has moved,
   * which is where every effect starts and where a restart returns it to.
   */
  live: Record<string, ParameterValue>;
}

/**
 * The mapping as the show has it: composition → pieces → outputs.
 *
 * Deliberately the schema's own `Piece` and `OutputConfig` rather than a reduced
 * copy, so that the reading in `shared/mapping.ts` — draw order, what wins where
 * two pieces meet, whether a piece resamples — is the same reading the mapping
 * editor does rather than a second implementation of it.
 */
export interface MappingView {
  outputs: OutputConfig[];
  pieces: Piece[];
}

/** One slot as the matrix shows it. */
export interface SlotView {
  index: number;
  /** What it plays, in a form the window can label. */
  source: SlotSource;
  /**
   * What the operator calls this pad, where they have called it anything.
   *
   * `null` is the ordinary case: the pad is named after what it plays. A name
   * here is on the pad alone — the library and the generator keep theirs — and
   * it is what two pads of one clip with different effects are told apart by
   * (owner, 2026-10-15).
   */
  name: string | null;
  /**
   * The slot's own effect chain.
   *
   * The renderer does not run slot chains yet. The server
   * still holds a live value put on one, so the EFFECTS tab says that rather
   * than leaving an operator to infer it from a wall that did not change.
   */
  effects: EffectView[];
  /** Whether this is the slot the layer is playing. */
  active: boolean;
  /** Whether this is a slot the layer is waiting on a grid boundary to reach. */
  /**
   * What the slot does at the end of its timeline, and which way it runs.
   *
   * Configuration rather than live state, and shown rather than offered: a pad
   * that said "Bounce" and could be changed here would be an edit to the
   * show made from the live screen.
   */
  playbackMode: PlaybackMode;
  direction: PlaybackDirection;
  /**
   * Whether this slot runs free or is locked to a number of beats.
   *
   * On the slot because it is a property of the clip: how many beats *this*
   * piece of footage should take is a fact about the footage, and a layer
   * that carried it would apply one answer to every slot on it (owner,
   * 2026-09-23). The layer strip reads the active slot's.
   *
   * Configuration rather than live state, and shown in LIVE rather than
   * offered there, for the same reason the playback mode above is.
   */
  sync: SlotSync;
  /**
   * The grid this slot waits for when it is triggered, or null for the layer's.
   *
   * On the slot because the owner asked for it there (2026-09-23), and for the
   * same reason `sync` is: a stab that has to land on the beat and a four-bar
   * wash sit on the same layer and want different answers. Null is "follow the
   * layer", which is what every document written before the field existed says
   * and what the DMX channel driving a layer's grid goes on driving.
   */
  /**
   * How fast this slot runs through one pass, as a ratio.
   *
   * One is the clip's own length, or exactly the beats it was given. A slot
   * written before this existed has none, and one is what it was doing.
   */
  speed: number;
  /**
   * Where this source sits, before the layer's own transform.
   *
   * The slot's rather than the layer's: two clips in one layer are rarely
   * framed the same way, and this is the one an operator puts right without
   * moving the other (owner, 2026-09-23). Carried here so OPTIONS can offer
   * it — it is the document, changed through `PUT …/slots/{n}/settings` with
   * the show running.
   */
  transform: Transform2D;
}

/**
 * What a layer opens on, as opposed to what it is doing.
 *
 * Carried beside the live values so the difference is legible without the live
 * screen loading the show document: `default_opacity` is not the opacity, and
 * an operator halfway through a show has to be able to see which of the two they
 * are looking at.
 */
export interface LayerDefaults {
  /** Whether the show opens this layer switched on. */
  enabled: boolean;
  route: Route;
  opacity: number;
}

/** One layer as the matrix shows it. */
export interface LayerView {
  id: string;
  name: string;
  /**
   * Whether the layer contributes to its bus at all, right now.
   *
   * Live. The show's own value is where it starts and is in `defaults`;
   * changing this during a show never writes back (owner decision Q16), so a
   * layer switched off keeps its slots, its transform and its chain.
   */
  enabled: boolean;
  /**
   * What the layer's playhead is doing.
   *
   * Not the same as being on slot 0, which is Black: pausing holds the frame
   * on screen and resuming carries on from it, which coming back from black
   * cannot do.
   */
  transport: Transport;
  route: Route;
  blendMode: BlendMode;
  opacity: number;
  /**
   * Where the layer's picture sits.
   *
   * Live, and usually driven by the Layer Fixture. The show's own value is
   * in `defaults`, which is where a layer starts and what it comes back to.
   */
  transform: Transform2D;
  activeSlot: number;
  /**
   * How many times a slot has been selected on this layer.
   *
   * The number means nothing; that it moved means a trigger reached the server.
   * Re-selecting the running slot is a retrigger and moves nothing else, so this
   * is the only acknowledgement a retrigger has.
   */
  trigger: number;
  /** What the show says this layer starts from. */
  defaults: LayerDefaults;
  /**
   * The layer's effect chain, in chain order.
   *
   * Which effects, in what order, switched on or off is the document and is
   * edited in Settings, for the same reason the blend mode is. What
   * each one's parameters are doing right now is live state, and is changed
   * from the EFFECTS tab.
   */
  effects: EffectView[];
  slots: SlotView[];
  /**
   * How many banks of twelve this layer's matrix shows.
   *
   * What the document asks for, not what its slots need. An operator adds
   * a bank in order to put something in it, so a count derived from the
   * slots would lose the empty one they just asked for (owner, 2026-09-23).
   * A document written before the field existed says one, and the matrix
   * still shows every slot it has — see [`slotBanks`].
   *
   * [`slotBanks`]: ../live.ts
   */
  banks: number;
}

/** The master strip, as the server reports it. */
export interface MasterView {
  bpm: number;
  crossfader: number;
  dimmer: number;
  speedScale: number;
  freeze: boolean;
  /**
   * Whether incoming DMX reaches the show at all.
   *
   * On for a show being driven from the desk, which is nearly all of them.
   * Off is somebody having taken it by hand.
   */
  dmxEnabled: boolean;
}

/**
 * What the master returns to, and what the clock counts in.
 *
 * From the show rather than from the live state, and null when no show is
 * open. `beatsPerBar` is here because a bar-length trigger grid counts in it and
 * nothing in the live state carries it.
 */
export interface MasterDefaults {
  bpm: number;
  crossfader: number;
  dimmer: number;
  speedScale: number;
  beatsPerBar: number;
}

/** Everything the window draws. */
export interface ShowView {
  sequence: number;
  showId: string | null;
  showName: string | null;
  /**
   * When the server last stored the open show.
   *
   * Here so that a panel editing the document can tell "the same show" from
   * "the same show, changed". The owner, 2026-10-16: a generator put into a
   * slot did not appear in the patch until the client was restarted, because
   * the editor's draft is fetched when a show is opened and never again — and
   * the mirror beside it had the new document all along.
   */
  showUpdatedAt: number | null;
  /** How wide and tall the composition is, for the preview's aspect. */
  composition: { width: number; height: number } | null;
  master: MasterView | null;
  /** What the master returns to, from the open show. */
  masterDefaults: MasterDefaults | null;
  layers: LayerView[];
  /** The slices and the outputs they land on, for the work area's SLICES tab. */
  mapping: MappingView;
  renderer: { state: string; reason: string | null };
}

/** An empty view: connected to nothing. */
export function emptyView(): ShowView {
  return {
    sequence: 0,
    showId: null,
    showName: null,
    showUpdatedAt: null,
    composition: null,
    master: null,
    masterDefaults: null,
    layers: [],
    mapping: { outputs: [], pieces: [] },
    renderer: { state: 'stopped', reason: null },
  };
}

/** Arranges the mirror for the window. */
export function toView(state: ShowState): ShowView {
  const show = state.show;
  const configured = new Map((show?.layers ?? []).map((layer) => [layer.id, layer]));

  const layers: LayerView[] = [];
  for (const live of layersInOrder(state)) {
    const layer = configured.get(live.layer_id);
    if (!layer) {
      // Live state for a layer the show does not have. The server does not
      // send that, and if it ever did, drawing a row with no name and no slots
      // would be worse than leaving it out.
      continue;
    }
    layers.push({
      id: layer.id,
      name: layer.name,
      // Both default here as they do on the server: a state message that does
      // not carry them is one from a build before they existed, and a layer
      // nobody mentioned is on and playing rather than missing from the wall.
      enabled: live.enabled ?? true,
      transport: live.transport ?? 'playing',
      route: live.route,
      opacity: live.opacity,
      // A server older than this sends none, and a layer that has never been
      // placed is where the document put it. A document written before layers
      // had one at all falls through to the identity, because this field is
      // not optional and a window that read `undefined.x` would take the panel
      // down rather than draw a layer with no placement.
      transform: live.transform ?? layer.default_transform ?? IDENTITY_TRANSFORM,
      // The live one. A server older than this sends none, and what it was
      // doing then is what the document says.
      blendMode: live.blend_mode ?? layer.blend_mode,
      activeSlot: live.active_slot,
      trigger: live.trigger,
      defaults: {
        enabled: layer.enabled,
        route: layer.route,
        opacity: layer.default_opacity,
      },
      // One when the document does not say, which is what the model's own
      // `serde` default is. A show written before layers had a bank count
      // reads as one bank and still shows every slot it has.
      banks: layer.banks ?? 1,
      effects: layer.effects.map((effect) => toEffect(effect, state.effects)),
      slots: layer.slots
        .slice()
        .sort((a, b) => a.index - b.index)
        .map((slot) => ({
          index: slot.index,
          // A document written before pads had names of their own has none,
          // which is the same as a pad nobody has named.
          name: slot.name ?? null,
          source:
            slot.source.kind === 'media'
              ? { kind: 'media', mediaItemId: slot.source.media_item_id }
              : slot.source.kind === 'ndi'
              ? { kind: 'ndi', name: slot.source.name }
              : {
                  kind: 'generator',
                  instanceId: slot.source.instance_id ?? null,
                  typeId: slot.source.type_id,
                  version: slot.source.version,
                  parameters: slot.source.parameters ?? {},
                  live:
                    slot.source.instance_id === undefined
                      ? {}
                      : (state.generators.get(slot.source.instance_id) ?? {}),
                },
          effects: slot.effects.map((effect) => toEffect(effect, state.effects)),
          active: slot.index === live.active_slot,
          playbackMode: slot.playback_mode,
          speed: slot.speed ?? 1,
          direction: slot.direction,
          // A slot written before sync existed has none, and free run is
          // what it was doing: a clip plays at the speed it was made at
          // until somebody decides otherwise.
          sync: slot.sync ?? { kind: 'FREE_RUN' },
          transform: slot.transform,
        })),
    });
  }

  const renderer = state.renderer;
  return {
    sequence: state.sequence,
    showId: show?.id ?? null,
    showName: show?.name ?? null,
    showUpdatedAt: show?.updated_at ?? null,
    composition: show
      ? { width: show.composition.size.width, height: show.composition.size.height }
      : null,
    master: state.master
      ? {
          bpm: state.master.bpm,
          crossfader: state.master.crossfader,
          dimmer: state.master.dimmer,
          speedScale: state.master.speed_scale,
          // Absent means a server older than the switch, and every one of
          // those was driving from the desk.
          dmxEnabled: state.master.dmx_enabled ?? true,
          freeze: state.master.freeze,
        }
      : null,
    masterDefaults: show
      ? {
          bpm: show.master.default_bpm,
          crossfader: show.master.default_crossfader,
          dimmer: show.master.default_dimmer,
          speedScale: show.master.default_speed_scale,
          beatsPerBar: show.master.beats_per_bar,
        }
      : null,
    layers,
    mapping: { outputs: show?.outputs ?? [], pieces: show?.pieces ?? [] },
    renderer: {
      state: renderer?.state ?? 'stopped',
      reason: renderer && 'reason' in renderer ? renderer.reason : null,
    },
  };
}

/**
 * One effect instance: what the show configured, and what has been moved.
 *
 * `parameters` is absent in a show that took every published default, which
 * is not an incomplete show: an empty map and a missing one mean the same
 * thing, and `modules.parameterValue` resolves either against the manifest.
 *
 * The live values are looked up by instance id, which is the only identity a
 * layer chain and a slot chain have in common — and the identity the parameter
 * registry names, `effect.<instance>.parameter.<name>`.
 */
function toEffect(
  effect: {
    id: string;
    type_id: string;
    version: number;
    enabled: boolean;
    parameters?: Record<string, ParameterValue>;
  },
  live: ReadonlyMap<string, Record<string, ParameterValue>>,
): EffectView {
  return {
    id: effect.id,
    typeId: effect.type_id,
    version: effect.version,
    enabled: effect.enabled,
    parameters: effect.parameters ?? {},
    live: live.get(effect.id) ?? {},
  };
}
