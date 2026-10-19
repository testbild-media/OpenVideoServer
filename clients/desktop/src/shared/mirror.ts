// The client's copy of what the server says is happening.
//
// A mirror, never a source. The server owns the show (house rule §4); everything
// here is derived from what it sent, and the one thing this module exists to
// enforce is that it is derived *completely* — an update that does not follow
// the last one is a gap, and a gap means asking for a fresh snapshot rather
// than applying it and hoping.
//
// The Rust half of this rule is `ovs_proto::SequenceTracker`, which a Rust peer
// drives for the same purpose. This is not a port of it: a client also has to
// hold the state, and a tracker that said "applicable" while the caller kept
// its own copy would be two places to get the same thing wrong.

import type {
  DeskControls,
  LayerLive,
  MasterLive,
  ParameterValue,
  Show,
  RendererStatus,
  ServerMessage,
  StateChange,
} from '@ovs/schema';

/** What the client believes the server is doing. */
export interface ShowState {
  /** The position in the update stream this reflects. */
  sequence: number;
  /** The loaded show, or null when the server has none open. */
  show: Show | null;
  /** Live master state. */
  master: MasterLive | null;
  /** Live state of each layer, by layer id. */
  layers: Map<string, LayerLive>;
  /**
   * What a control source has done to the effects, by effect instance id.
   *
   * `EffectOverrides`, held the way the layers are. An instance with no entry is
   * one running at exactly what the show configured, and a parameter absent
   * from an instance's map is one nobody has moved — *not* one sitting at its
   * configured value. Writing the configured value in here would be a second
   * copy of it, and the two would disagree the moment somebody edited the
   * show (`crates/ovs-types/src/live.rs`).
   */
  effects: Map<string, Record<string, ParameterValue>>;
  /**
   * The same, for the generators, by generator instance id.
   *
   * A separate map rather than one keyed by both kinds of identity, because
   * the two identities are different types on the server and a client that
   * merged them would be the one place in the system where an effect and a
   * generator could collide.
   */
  generators: Map<string, Record<string, ParameterValue>>;
  /**
   * What the open show's patch gives a lighting console.
   *
   * Held only while the master's DMX switch is on. The server's own reading
   * of the patch, sent with the show, so that what this client greys out is
   * exactly what the server would refuse (owner, 2026-10-19).
   */
  desk: DeskControls;
  /** What the renderer is doing. */
  renderer: RendererStatus | null;
}

/** What applying a message did. */
export type Applied =
  | { kind: 'applied' }
  /** The message arrived out of order and nothing was changed. */
  | { kind: 'gap'; expected: number; received: number }
  /** Not a message this mirror acts on. */
  | { kind: 'ignored' };

/** An empty mirror: connected to nothing, believing nothing. */
export function emptyState(): ShowState {
  return {
    sequence: 0,
    show: null,
    master: null,
    layers: new Map(),
    effects: new Map(),
    generators: new Map(),
    desk: {},
    renderer: null,
  };
}

/**
 * Applies one server message to the mirror, in place.
 *
 * Returns what happened. A `gap` leaves the mirror untouched and is the
 * caller's signal to ask for a fresh snapshot, which the socket answers with
 * one. Nothing is applied in the meantime: state the client cannot account for
 * is worse than none, because it looks right.
 */
export function apply(state: ShowState, message: ServerMessage): Applied {
  switch (message.type) {
    case 'snapshot': {
      // A snapshot replaces everything. It is the only message that may move
      // the sequence by more than one, because it *is* the whole state.
      state.sequence = message.sequence;
      state.show = message.show ?? null;
      state.master = message.master;
      state.renderer = message.renderer;
      state.layers = new Map(message.layers.map((layer) => [layer.layer_id, layer]));
      // The whole reason the snapshot carries these: a client that joins
      // mid-show would otherwise draw every effect control at its configured
      // value while the wall showed something else. Absent means nobody has
      // moved anything, which is where every effect starts.
      state.effects = new Map(Object.entries(message.effects ?? {}));
      state.generators = new Map(Object.entries(message.generators ?? {}));
      // Absent from a server older than the rule, which held nothing.
      state.desk = message.desk ?? {};
      return { kind: 'applied' };
    }

    case 'update': {
      const expected = state.sequence + 1;
      if (message.sequence !== expected) {
        return { kind: 'gap', expected, received: message.sequence };
      }
      applyChange(state, message.change);
      state.sequence = message.sequence;
      return { kind: 'applied' };
    }

    default:
      // Telemetry, previews, faults: real messages, but not this mirror's.
      return { kind: 'ignored' };
  }
}

/** Applies one change to the mirror. */
function applyChange(state: ShowState, change: StateChange): void {
  switch (change.change) {
    case 'show_loaded': {
      // Carries the live state as well as the show, because loading one
      // replaces both. A client that derived the live state instead would be a
      // second implementation of the restart policy.
      state.show = change.show ?? null;
      state.master = change.master;
      state.layers = new Map(change.layers.map((layer) => [layer.layer_id, layer]));
      // What they were when the show was last open: a show comes back the way
      // it was left (owner, 2026-10-19). Taken from the message rather than
      // worked out here, for the same reason the master and the layers are: the
      // restart policy lives in the core, and a client with its own copy would
      // be a second implementation of it that has to agree with the server's
      // for ever.
      state.effects = new Map(Object.entries(change.effects ?? {}));
      state.generators = new Map(Object.entries(change.generators ?? {}));
      // A patch edit is a document edit, so this is where a fixture starts or
      // stops holding what it drives.
      state.desk = change.desk ?? {};
      break;
    }

    case 'master_changed':
      state.master = change.master;
      break;

    case 'layer_changed':
      state.layers.set(change.layer.layer_id, change.layer);
      break;

    case 'effect_changed': {
      // Whole rather than a difference, so applying one needs no history. An
      // empty map is how "this instance is back at what the show configured"
      // is said, and the entry goes rather than being kept as an empty one: an
      // instance nobody has moved has no entry at all.
      const overrides = change.overrides ?? {};
      if (Object.keys(overrides).length === 0) {
        state.effects.delete(change.effect_instance_id);
      } else {
        state.effects.set(change.effect_instance_id, overrides);
      }
      break;
    }

    case 'generator_changed': {
      // Everything `effect_changed` is, for the other kind of module.
      const overrides = change.overrides ?? {};
      if (Object.keys(overrides).length === 0) {
        state.generators.delete(change.generator_instance_id);
      } else {
        state.generators.set(change.generator_instance_id, overrides);
      }
      break;
    }

    case 'renderer_changed':
      state.renderer = change.renderer;
      break;

    default:
      break;
  }
}

/** Returns the layers in composite order, bottom first. */
export function layersInOrder(state: ShowState): LayerLive[] {
  const order = new Map<string, number>();
  for (const layer of state.show?.layers ?? []) {
    order.set(layer.id, layer.order);
  }
  return [...state.layers.values()].sort(
    (a, b) => (order.get(a.layer_id) ?? 0) - (order.get(b.layer_id) ?? 0),
  );
}
