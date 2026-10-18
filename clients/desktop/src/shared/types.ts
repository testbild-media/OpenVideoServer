// Types this client owns, as opposed to the ones the server defines.
//
// Everything the server sends comes from `@ovs/schema`, generated from the same
// Rust types the server is built from, so it cannot drift. What lives here is
// the handful of shapes that exist only between this client's own halves — the
// window and the main process — and nothing here is ever sent to a server.

import type {
  BlendMode,
  ParameterValue,
  PlaybackDirection,
  PlaybackMode,
  Route,
  SlotSync,
  Transform2D,
  Transport,
  ValidationError,
} from '@ovs/schema';

export type { BlendMode, PlaybackDirection, PlaybackMode, Route, SlotSync, Transport };

/**
 * What a slot plays, reduced to what a pad has to label.
 *
 * A generator carries the parameters the show set, because the work area's
 * GENERATOR tab reads them: a value is only meaningful beside the manifest that
 * declared it, and that manifest is found by the generator's type alone. They
 * are carried to be read. Nothing on the live screen writes one back — there is
 * no endpoint that would, which is what that tab says. An *effect* instance is a
 * different case now, and `set_effect` below is why.
 */
export type SlotSource =
  | { kind: 'media'; mediaItemId: string }
  | {
      kind: 'ndi';
      /**
       * The full NDI source name, as the network announces it.
       *
       * `MACHINE (Source Name)`, carried whole. It is the only handle NDI
       * offers that survives a sender restarting, so it is what a show stores
       * and what a slot is matched back to when one appears again.
       */
      name: string;
    }
  | {
      kind: 'generator';
      /**
       * This generator instance's own identity, or null when it has none yet.
       *
       * What a live value is keyed by and what a patched fixture points at,
       * the same way an effect instance's is. A show written before
       * generators had one carries none, and the server mints one when it
       * loads that show — so a slot in the *live* view always has one.
       *
       * Null rather than an empty string, and null rather than assuming: a
       * control drawn against an identity this client invented would send a
       * request naming a generator the server does not have, and the refusal
       * would be about the wrong thing.
       */
      instanceId: string | null;
      typeId: string;
      /**
       * The number the document stores beside the type.
       *
       * Carried only because a `SourceRef` has the field, so a generator
       * dropped from the browser goes onto the wire with the catalogue's own
       * number in it. It names nothing: there is one module per type and no
       * versions of it (owner, 2026-10-18), so a generator is never looked up
       * or shown by this.
       */
      version: number;
      /** The values the show set, keyed by the manifest's own names. */
      parameters: Record<string, ParameterValue>;
      /**
       * The values a control source has moved since the show opened.
       *
       * The generator half of `EffectView.live`, and the same rule: only what
       * somebody moved is in here, and a parameter that is absent is one
       * running at what the show configured rather than one sitting at that
       * value.
       */
      live: Record<string, ParameterValue>;
    };

/**
 * A command the window asks the main process to send.
 *
 * Named commands rather than a path and a body: the window is sandboxed and
 * everything it sends crosses a bridge, and a bridge that forwarded arbitrary
 * requests would be a way to reach any endpoint from a page. Each of these is
 * something the operator can actually do.
 */
export type Command =
  | { do: 'select_slot'; layerId: string; slotIndex: number }
  | {
      do: 'set_layer';
      layerId: string;
      route?: Route;
      opacity?: number;
      /**
       * Where the layer's picture sits.
       *
       * Live, and usually driven by the Layer Fixture. It is here because the
       * owner asked for the placement in OPTIONS (2026-09-23), and because it
       * is what a console drives: a value set here is one a console can move
       * again a moment later, which is the definition of live state.
       */
      transform?: Transform2D;
      /** How the layer composites onto its bus. */
      blendMode?: BlendMode;
      /** Playing, paused or stopped. Not the same as selecting slot 0. */
      transport?: Transport;
      /** Whether the layer is in the composition. Never written to the show. */
      enabled?: boolean;
    }
  | {
      do: 'set_master';
      crossfader?: number;
      dimmer?: number;
      bpm?: number;
      speedScale?: number;
      freeze?: boolean;
      /** Whether incoming DMX reaches the show at all. */
      dmxEnabled?: boolean;
    }
  /**
   * Moves the live values on one effect instance, or puts them all back.
   *
   * A live value sits on top of what the show configured for that instance
   * and is never written back to it (`UpdateEffectRequest`), so this
   * is live control and not a shortcut into the show editor.
   *
   * `reset` has to be its own field rather than a value: "nobody has moved this"
   * is an *absence* in `EffectOverrides`, so there is no value that means "stop
   * overriding it", and sending the configured value instead would only look the
   * same until somebody edited the show.
   */
  | {
      do: 'set_effect';
      effectInstanceId: string;
      /** Only the parameters the operator moved, keyed as the manifest names them. */
      overrides?: Record<string, ParameterValue>;
      reset?: boolean;
    }
  | {
      do: 'set_generator';
      generatorInstanceId: string;
      /** Only the parameters the operator moved, keyed as the manifest names them. */
      overrides?: Record<string, ParameterValue>;
      reset?: boolean;
    }
  | { do: 'tap' }
  | { do: 'reset_phase' }
  | { do: 'open_show'; showId: string };

/**
 * What a tap did to the tempo, as the server answered.
 *
 * Carried back because the first tap of a run never changes anything — one tap
 * is not an interval — and `TapTempoResponse` says so. A screen that showed the
 * new tempo after the first tap would be showing the old one and calling it new.
 */
export interface TapResult {
  bpm: number;
  changed: boolean;
}

/**
 * What the main process answers a command with.
 *
 * `tap` is the one command that answers with more than "carried out", and it is
 * here rather than in a second bridge call because it is the same command.
 */
export type CommandResult =
  | { ok: true; tap?: TapResult }
  | { ok: false; code: string; message: string };

/**
 * What the main process answers a request for something with.
 *
 * The editor asks for documents rather than only pressing buttons — the show
 * list, a show, the effect and generator catalogues, the media library — so
 * it needs a result that can carry an answer as well as a refusal. `details`
 * carries the server's field-level problems, which is how a refused save is put
 * back beside the fields it names rather than shown as one sentence.
 */
export type Answer<T> =
  | { ok: true; value: T }
  | { ok: false; code: string; message: string; details: ValidationError[] };
