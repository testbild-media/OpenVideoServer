// What the show model allows, and what it starts from.
//
// The generated types carry the *shape* of the document but not its arithmetic:
// an opacity is a bare `number` in TypeScript, and the rule that it lies in
// 0..1 lives in `Layer::validate` rather than in the schema. So
// the bounds are written out once here and `test/limits.test.ts` reads the Rust
// and the generated schema and fails when they drift — the same arrangement
// `versions.ts` uses, and for the same reason: a client that guessed a range
// would offer an operator a control the server then refuses.
//
// Nothing here is a number chosen in this client. Every one of them has a line
// in `crates/ovs-types` that the drift test points at.

import type {
  BlendMode,
  ConnectorRole,
  CrossfaderCurve,
  FrameRate,
  MediaKind,
  PlaybackDirection,
  PlaybackMode,
  Route,
  SizePx,
  Transform2D,
} from '@ovs/schema';

// --- slots -------------------------------------------------------------------

/** The reserved index of the permanently black slot (`Layer::BLACK_SLOT_INDEX`). */
export const BLACK_SLOT_INDEX = 0;

/** Slots shown in one bank of the matrix (`Layer::SLOTS_PER_BANK`). */
export const SLOTS_PER_BANK = 12;

/**
 * Banks a layer may hold (`Layer::MAX_BANKS`).
 *
 * A layer starts with bank 1 alone and further banks are added deliberately,
 * up to this (owner, 2026-09-23).
 */
export const MAX_BANKS = 7;

/** The highest slot index a layer may hold (`Layer::MAX_SLOT_INDEX`). */
export const MAX_SLOT_INDEX = SLOTS_PER_BANK * MAX_BANKS;

// --- ranges ------------------------------------------------------------------

/**
 * The 0..1 range the model checks in several places.
 *
 * `Layer::validate` checks it for `default_opacity`; `MasterConfig::validate`
 * checks the same range for the dimmer and the crossfader.
 */
export const MIN_FRACTION = 0;
/** The top of that range. */
export const MAX_FRACTION = 1;


/** Lowest master BPM the clock accepts (`MIN_BPM`). */
export const MIN_BPM = 20;
/** Highest master BPM the clock accepts (`MAX_BPM`). */
export const MAX_BPM = 999;

/** Lowest master speed scale (`MIN_SPEED_SCALE`). */
export const MIN_SPEED_SCALE = 0.125;
/** Highest master speed scale (`MAX_SPEED_SCALE`). */
export const MAX_SPEED_SCALE = 4;

/**
 * Fewest beats in a bar.
 *
 * `MasterConfig::validate` refuses zero and says nothing else about it, so the
 * bottom of the range is one.
 */
export const MIN_BEATS_PER_BAR = 1;
/**
 * Most beats in a bar, which is the width of the field rather than a musical
 * limit: `MasterConfig::beats_per_bar` is a `u32`, and a larger number is one
 * the document could not carry back.
 */
export const MAX_BEATS_PER_BAR = 4_294_967_295;

// --- surfaces ----------------------------------------------------------------

/**
 * Smallest a composition, an output mode or a piece rectangle may be per axis.
 *
 * `SizePx::validate` and `RectPx::validate` both refuse a zero width or height,
 * which is the whole of what the model says about how small a surface may be.
 */
export const MIN_DIMENSION_PX = 1;
/**
 * Largest, which is again the width of the field: widths and heights are `u32`
 * throughout `geometry.rs`.
 */
export const MAX_DIMENSION_PX = 4_294_967_295;

/** Lowest draw order a piece can have (`Piece::order` is a `u32`). */
export const MIN_PIECE_ORDER = 0;
/** Highest, for the same reason. */
export const MAX_PIECE_ORDER = 4_294_967_295;

// --- the DMX patch -----------------------------------------------------------

/**
 * The lowest DMX start address.
 *
 * Addressing is one-based: `FixtureInstance::channel_range` answers `None` for
 * address zero, and the model says so in as many words — "address zero is
 * invalid because DMX addressing is one-based".
 */
export const MIN_DMX_ADDRESS = 1;
/**
 * The highest valid start address, given a one-channel footprint
 * (`MAX_DMX_ADDRESS`).
 *
 * A universe has this many channels. A fixture that would run off the end is a
 * fixture that cannot be patched there, and a show that wants more than this
 * needs another universe.
 */
export const MAX_DMX_ADDRESS = 512;

/**
 * The lowest universe number.
 *
 * The width of the field rather than a protocol limit: `FixtureInstance::universe`
 * is a `u16` and the model says nothing else about it. Art-Net counts from zero
 * and sACN from one, and a patch that disagreed with the console about which is
 * which is the operator's to settle — so nothing here narrows the range the
 * document can carry.
 */
export const MIN_UNIVERSE = 0;
/** The highest, for the same reason: `u16`. */
export const MAX_UNIVERSE = 65_535;

/**
 * The lowest fixture definition version.
 *
 * `FixtureInstance::validate` refuses zero and says "fixture definition versions
 * start at 1", which is the whole of what the model says about it.
 */
export const MIN_FIXTURE_VERSION = 1;

// --- what a new thing starts as ----------------------------------------------
//
// These are the values `Layer::new` and the `#[default]` variants give a layer
// and a slot on the server. A layer this client adds has to be the same layer
// the server would have made, or the two disagree about a document neither of
// them changed.

/** `Route::default()`. */
export const DEFAULT_ROUTE: Route = 'M';
/** `BlendMode::default()`. */
export const DEFAULT_BLEND_MODE: BlendMode = 'normal';
/**
 * The largest picture this client will offer to prepare, on a side.
 *
 * Matched to the server's own `MAX_MEDIA_SIDE` so that an operator is not
 * told “preparing…” for several minutes and then refused by the far end. A
 * file larger than this is refused before a frame is read.
 */
export const MAX_SIDE = 8192;

/** The longest name an operator may give a pad (`MAX_SLOT_NAME_CHARS`). */
export const MAX_SLOT_NAME_CHARS = 64;

/** The fewest beats a slot may be locked to (`MIN_SLOT_BEATS`). */
export const MIN_SLOT_BEATS = 1;
/** What a slot locked to the beat is offered when it is switched on. */
export const DEFAULT_SLOT_BEATS = 8;

/** `PlaybackMode::default()`. */
export const DEFAULT_PLAYBACK_MODE: PlaybackMode = 'loop';
/** `PlaybackDirection::default()`. */
export const DEFAULT_PLAYBACK_DIRECTION: PlaybackDirection = 'forward';
/** The opacity `Layer::new` starts a layer at. */
export const DEFAULT_OPACITY = 1;

/** `Transform2D::IDENTITY`. */
export const IDENTITY_TRANSFORM: Transform2D = {
  x: 0,
  y: 0,
  scale_x: 1,
  scale_y: 1,
  rotation_deg: 0,
};

// --- what the master starts from ---------------------------------------------
//
// `MasterConfig::default()`. The first three are what the clock starts at; the
// last three are what the master is *restored* to, because live state is not
// kept across a restart and these are the values the show sits at until a
// console sends one.

/** BPM the master clock starts at. */
export const DEFAULT_BPM = 120;
/** Beats in a bar, which is what the clock counts in. */
export const DEFAULT_BEATS_PER_BAR = 4;
/** Master speed scale a show starts at. */
export const DEFAULT_SPEED_SCALE = 1;
/** Master dimmer a restart returns to. */
export const DEFAULT_DIMMER = 1;
/** Crossfader position a restart returns to: 0 is fully A, 1 fully B. */
export const DEFAULT_CROSSFADER = 0;
/** `CrossfaderCurve::default()`. */
export const DEFAULT_CROSSFADER_CURVE: CrossfaderCurve = 'equal_power';

// --- what the composition starts from ----------------------------------------

/** `Composition::default().size`. */
export const DEFAULT_COMPOSITION_SIZE: SizePx = { width: 1920, height: 1080 };
/** `Composition::default().render_rate`, which is `FrameRate::FPS_60`. */
export const DEFAULT_RENDER_RATE: FrameRate = { numerator: 60, denominator: 1 };

// --- what a new piece and a new output start from ----------------------------
//
// `Piece::new` and `OutputConfig::new`. The rectangles are not here: a piece is
// created against a composition and an output that the document already holds
// the size of, so they come out of the document rather than out of this file.

/** `Piece::new` draws the piece it creates. */
export const DEFAULT_PIECE_ENABLED = true;
/** The draw order `Piece::new` gives a piece. */
export const DEFAULT_PIECE_ORDER = 0;
/** The rotation `Piece::new` gives a piece. */
export const DEFAULT_PIECE_ROTATION_DEG = 0;
/** `OutputConfig::new` creates an output that is driven. */
export const DEFAULT_OUTPUT_ENABLED = true;

// --- the menus ---------------------------------------------------------------
//
// Order matters: it is the order the Rust declares the variants in, which is
// the order the DMX range mapping uses and the order an operator reading the
// Live Mode reference expects. The drift test checks each list against the
// generated schema, which preserves declaration order.

/** Every bus a layer can feed, in declaration order. */
export const ROUTES: readonly Route[] = ['A', 'B', 'M'];

/** Every blend mode, in declaration order. */
export const BLEND_MODES: readonly BlendMode[] = [
  'normal',
  'add',
  'multiply',
  'screen',
  'overlay',
  'lighten',
  'darken',
  'difference',
];

/** `Slot`'s own rate, which is the clip's.
 *
 * The steps it can be set to are `SPEED_SCALE_STEPS` in `live.ts` — the same six
 * the master runs on, not a second copy of them. The owner's ladder
 * (2026-09-23): "pro Slot Scale sollt aber verfügbar sein für Free Run und BPM
 * Sync… 0,125x 1x 4x usw.", and its two ends are `MIN_SPEED_SCALE` and
 * `MAX_SPEED_SCALE`, which is the range the model validates a slot against. */
export const DEFAULT_SLOT_SPEED = 1;

/** Every end-of-timeline behaviour, in declaration order. */
export const PLAYBACK_MODES: readonly PlaybackMode[] = [
  'loop',
  'once_freeze',
  'once_black',
  'bounce',
];

/** Both timeline directions. */
export const PLAYBACK_DIRECTIONS: readonly PlaybackDirection[] = ['forward', 'reverse'];

/** Both crossfader responses, in declaration order. */
export const CROSSFADER_CURVES: readonly CrossfaderCurve[] = ['linear', 'equal_power'];

/**
 * Every role a GPU connector can have, in declaration order.
 *
 * Read only here. What a socket is for describes the machine rather than the
 * show, is set on the server, and must never be inferred from what happens
 * to be plugged in — so this client shows what the server says
 * and offers no control that would change it.
 */
export const CONNECTOR_ROLES: readonly ConnectorRole[] = [
  'SERVICE_DISPLAY',
  'MEDIA_OUTPUT',
  'UNUSED',
];

/**
 * Every internal playback profile, in declaration order.
 *
 * Offered in exactly one place and only then: a machine with no `ffprobe`
 * cannot say what a file holds, and the server now refuses an upload whose
 * codec disagrees with what was declared. Somebody has to say, and the operator
 * is the only one left who can.
 *
 * Nowhere else does this client show a profile, ask for one or store one. The
 * normal path never reaches this list: the file is probed and declared from
 * what is in it, and the library shows import, processing, ready.
 */
export const MEDIA_KINDS: readonly MediaKind[] = ['video', 'image'];

// The label maps are `Record`s rather than arrays so that adding a variant to
// the generated type is a compile error here rather than a menu with a gap in
// it.

/** What to show beside each route. */
export const ROUTE_LABELS: Record<Route, string> = {
  A: 'A',
  B: 'B',
  M: 'M',
};

/** What to show beside each blend mode. */
export const BLEND_MODE_LABELS: Record<BlendMode, string> = {
  normal: 'Normal',
  add: 'Add',
  multiply: 'Multiply',
  screen: 'Screen',
  overlay: 'Overlay',
  lighten: 'Lighten',
  darken: 'Darken',
  difference: 'Difference',
};

/** What to show beside each playback mode. */
export const PLAYBACK_MODE_LABELS: Record<PlaybackMode, string> = {
  loop: 'Loop',
  once_freeze: 'Once, then freeze',
  once_black: 'Once, then black',
  bounce: 'Bounce',
};

/** What to show beside each direction. */
export const PLAYBACK_DIRECTION_LABELS: Record<PlaybackDirection, string> = {
  forward: 'Forward',
  reverse: 'Reverse',
};

/** What to show beside each crossfader response. */
export const CROSSFADER_CURVE_LABELS: Record<CrossfaderCurve, string> = {
  linear: 'Linear',
  equal_power: 'Equal power',
};

/** What to show beside each connector role. */
export const CONNECTOR_ROLE_LABELS: Record<ConnectorRole, string> = {
  SERVICE_DISPLAY: 'Service display',
  MEDIA_OUTPUT: 'Media output',
  UNUSED: 'Unused',
};

/**
 * What to show beside each profile, in the operator's words.
 *
 * The codec name is in there because this is the one screen where an operator
 * has to recognise their own file, and "ProRes 4444" is what the person who
 * exported it was looking at. What each one *means* comes first, because that
 * is the part they can answer from memory.
 */
export const MEDIA_KIND_LABELS: Record<MediaKind, string> = {
  video: 'Video',
  image: 'Still image',
};

/** One rate the model names, and what it is commonly written as. */
export interface NamedRate {
  /** The label the Rust constant is named after: `FPS_29_97` is `29.97`. */
  label: string;
  /** The exact rational rate. */
  rate: FrameRate;
}

/**
 * The frame rates the model names, in declaration order.
 *
 * Rational rather than decimal so 60000/1001 stays exact: 59.94 and
 * 60000/1001 are different rates, and a scheduler given the first would drift
 *. These four are the ones `FrameRate` declares constants for,
 * and the drift test reads them out of `geometry.rs` — a composition rendered
 * at a rate this client made up would be a number nothing measured.
 *
 * An output's rate is not chosen from here: it comes from the mode list the
 * connector reported.
 */
export const FRAME_RATES: readonly NamedRate[] = [
  { label: '60', rate: { numerator: 60, denominator: 1 } },
  { label: '50', rate: { numerator: 50, denominator: 1 } },
  { label: '29.97', rate: { numerator: 30000, denominator: 1001 } },
  { label: '59.94', rate: { numerator: 60000, denominator: 1001 } },
];
