// Composition → pieces → outputs: the geometry an operator patches a wall with.
//
// A piece takes a rectangle of the composition and puts it on a rectangle of an
// output. That is the whole of the model, and everything here is
// arithmetic on those two rectangles — no document is edited in this file, so
// each of these can be asked a question without anything changing.
//
// Three things it is careful about.
//
// **It answers the questions the renderer asks.** `drawOrder` is the sort in
// `OutputMapping::build`, `isOffScreen` is the renderer's own test written in
// pixels rather than in normalised coordinates, and `isPixelExact` is
// `Piece::is_pixel_exact`. An editor that disagreed with the renderer about
// which piece is on top would be drawing a mapping the wall does not have.
//
// **It reports rather than refuses.** Two pieces overlapping on one output is
// not an error — the server takes that show, and the draw order decides what
// is visible where they meet. So the editor says which piece wins rather than
// pretending the document is broken.
//
// **It does not invent a rate.** An output's mode comes from what the connector
// reported, in millihertz, and turning that back into the exact rational rate
// the model stores is done against the rates the model names. What that cannot
// do exactly, it says so about rather than rounding quietly.

import type {
  ConnectorInfo,
  ConnectorModeInfo,
  FrameRate,
  OutputConfig,
  Piece,
  PieceSource,
  Show,
  RectPx,
  SizePx,
} from '@ovs/schema';

import { FRAME_RATES, MIN_DIMENSION_PX } from './limits.js';

// --- rectangles ---------------------------------------------------------------

/** The exclusive right edge (`RectPx::right`). */
export function rectRight(rect: RectPx): number {
  return rect.x + rect.width;
}

/** The exclusive bottom edge (`RectPx::bottom`). */
export function rectBottom(rect: RectPx): number {
  return rect.y + rect.height;
}

/** Whether a rectangle lies entirely within a surface (`RectPx::fits_within`). */
export function fitsWithin(rect: RectPx, size: SizePx): boolean {
  return (
    rect.x >= 0 && rect.y >= 0 && rectRight(rect) <= size.width && rectBottom(rect) <= size.height
  );
}

/** Whether a rectangle would draw nothing (`RectPx::validate`). */
export function isEmpty(rect: RectPx): boolean {
  return rect.width === 0 || rect.height === 0;
}

/**
 * Whether a rectangle lies entirely outside a surface.
 *
 * `MappedPiece::is_off_screen`, in pixels. The renderer works in normalised
 * coordinates, where the test reads `dest_right <= -1.0 || dest_left >= 1.0`;
 * `normalise_dest` makes that exactly these four comparisons. A piece this is
 * true of is skipped by `OutputMapping::build`, so nothing of it reaches the
 * wall.
 */
export function isOffScreen(rect: RectPx, size: SizePx): boolean {
  return (
    rectRight(rect) <= 0 || rect.x >= size.width || rectBottom(rect) <= 0 || rect.y >= size.height
  );
}

/** The region two rectangles share, or null when they do not touch. */
export function intersection(a: RectPx, b: RectPx): RectPx | null {
  const x = Math.max(a.x, b.x);
  const y = Math.max(a.y, b.y);
  const right = Math.min(rectRight(a), rectRight(b));
  const bottom = Math.min(rectBottom(a), rectBottom(b));
  if (right <= x || bottom <= y) {
    return null;
  }
  return { x, y, width: right - x, height: bottom - y };
}

/** How much of a rectangle falls outside a surface, in pixels. */
export function spillOf(rect: RectPx, size: SizePx): number {
  const inside = intersection(rect, { x: 0, y: 0, width: size.width, height: size.height });
  const area = rect.width * rect.height;
  return area - (inside === null ? 0 : inside.width * inside.height);
}

/** Whether a piece maps its source without resampling (`Piece::is_pixel_exact`). */
export function isPixelExact(piece: Piece): boolean {
  return (
    piece.source_rect.width === piece.dest_rect.width &&
    piece.source_rect.height === piece.dest_rect.height &&
    piece.rotation_deg === 0
  );
}

// --- reading the mapping ------------------------------------------------------

/**
 * The half of a show a mapping is: its outputs and its pieces.
 *
 * The readers below take this rather than a whole `Show` so that Live Mode
 * can ask them the same questions the mapping editor asks without carrying a
 * document it has no business holding — the live screen is handed the mapping
 * to read, not the show to edit. A `Show` is one of
 * these, so every existing caller passes one unchanged.
 */
export type Mapping = Pick<Show, 'outputs' | 'pieces'>;

/** Returns an output by identifier, or null. */
export function findOutput(show: Mapping, outputId: string): OutputConfig | null {
  return show.outputs.find((output) => output.id === outputId) ?? null;
}

/** Returns a piece by identifier, or null. */
export function findPiece(show: Mapping, pieceId: string): Piece | null {
  return show.pieces.find((piece) => piece.id === pieceId) ?? null;
}

/** Every piece belonging to one output, in the order the show lists them. */
export function piecesOf(show: Mapping, outputId: string): Piece[] {
  return show.pieces.filter((piece) => piece.output_id === outputId);
}

/**
 * Every piece of one output in the sequence the renderer would draw them in.
 *
 * The sort `OutputMapping::build` does: by order, stably, so two pieces at the
 * same order keep the order the show lists them in rather than swapping
 * between runs. Last in this list is drawn last, which is to say on top.
 *
 * Disabled ones included, unlike `drawOrder`. A piece that is switched off
 * still has a place, and an operator arranging the list is deciding where it
 * lands when it is switched back on.
 */
export function sequenceOf(show: Mapping, outputId: string): Piece[] {
  return show.pieces
    .map((piece, position) => ({ piece, position }))
    .filter(({ piece }) => piece.output_id === outputId)
    .sort((a, b) => a.piece.order - b.piece.order || a.position - b.position)
    .map(({ piece }) => piece);
}

/**
 * The pieces one output actually draws, in the order it draws them.
 *
 * The sequence without the ones `OutputMapping::build` skips for being
 * disabled. What it does *not* drop is a piece that would draw nothing because
 * it is off the panel: that is reported rather than hidden, because a piece
 * missing from this list without explanation is a wall an operator cannot
 * account for.
 */
export function drawOrder(show: Mapping, outputId: string): Piece[] {
  return sequenceOf(show, outputId).filter((piece) => piece.enabled);
}

/** Two pieces that cover the same part of one output, and which one wins. */
export interface Overlap {
  /** The output they meet on. */
  outputId: string;
  /** The piece drawn first, and therefore covered where they meet. */
  under: Piece;
  /** The piece drawn over it. */
  over: Piece;
  /** The region of the output both cover, in output pixels. */
  region: RectPx;
}

/**
 * Every place two pieces cover the same part of one output.
 *
 * Not a fault: the server takes a show with overlapping pieces, and the
 * renderer resolves them by draw order. What an operator needs to be told is
 * which of the two they will actually see, which is what `over` is.
 *
 * A rotated piece is left out of this, because its rectangle is not what it
 * covers: the quad is turned about the centre of `dest_rect`, so two rectangles
 * that do not meet may still overlap once one of them is turned. Rotation is
 * reported on its own instead, in `advisories`, rather than answered here with
 * a number that would be wrong.
 */
export function overlaps(show: Mapping): Overlap[] {
  const found: Overlap[] = [];
  for (const output of show.outputs) {
    const drawn = drawOrder(show, output.id).filter((piece) => piece.rotation_deg === 0);
    for (const [position, under] of drawn.entries()) {
      for (const over of drawn.slice(position + 1)) {
        const region = intersection(under.dest_rect, over.dest_rect);
        if (region !== null) {
          found.push({ outputId: output.id, under, over, region });
        }
      }
    }
  }
  return found;
}

/** What an overlap means, in words an operator can act on. */
export function describeOverlap(overlap: Overlap): string {
  const { region } = overlap;
  return (
    `${overlap.over.name} is drawn over ${overlap.under.name} across ` +
    `${region.width}×${region.height} px at ${region.x}, ${region.y}. ` +
    `Draw order decides, so ${overlap.over.name} is what reaches the wall there.`
  );
}

// --- changing the composition -------------------------------------------------

/** What resizing the composition would do to the pieces already in the show. */
export interface ResizeReport {
  /** The size being moved to. */
  size: SizePx;
  /** Pieces whose source rectangle a canvas that size does not contain. */
  outside: Piece[];
  /** Pieces that still fit. */
  fitting: Piece[];
}

/** Works out what a resize would mean without performing one. */
export function resizeReport(show: Show, size: SizePx): ResizeReport {
  const outside: Piece[] = [];
  const fitting: Piece[] = [];
  for (const piece of show.pieces) {
    (fitsWithin(piece.source_rect, size) ? fitting : outside).push(piece);
  }
  return { size, outside, fitting };
}

// --- several rectangles at once ------------------------------------------------

/**
 * The smallest rectangle containing all of these, or null for none.
 *
 * What a group of selected pieces is dragged and scaled as: one box, so their
 * arrangement relative to each other survives the gesture. Four panels an
 * operator lined up do not stop being lined up because they were made bigger
 * together (owner, 2026-09-23).
 */
export function boundsOf(rects: readonly RectPx[]): RectPx | null {
  if (rects.length === 0) {
    return null;
  }
  let left = Number.POSITIVE_INFINITY;
  let top = Number.POSITIVE_INFINITY;
  let right = Number.NEGATIVE_INFINITY;
  let bottom = Number.NEGATIVE_INFINITY;
  for (const rect of rects) {
    left = Math.min(left, rect.x);
    top = Math.min(top, rect.y);
    right = Math.max(right, rect.x + rect.width);
    bottom = Math.max(bottom, rect.y + rect.height);
  }
  return { x: left, y: top, width: right - left, height: bottom - top };
}

/**
 * How far a corner drag has scaled a box, as one number for both axes.
 *
 * **One number is the whole point.** A corner that took the horizontal
 * movement for the width and the vertical for the height would let an operator
 * change the shape of a panel by a pixel every time they tried to make it
 * bigger, and an LED wall's shape is not theirs to change (owner,
 * 2026-09-23). The pointer is followed by showing where it went onto the
 * arm from the anchor to the corner, which is the scale that best matches the
 * gesture in both directions at once.
 *
 * Returns 1 when there is nothing to scale about — a zero-size box, or a
 * pointer exactly on the anchor — because no movement is the honest answer to
 * a gesture with no reference.
 */
export function proportionalScale(
  corner: { x: number; y: number },
  anchor: { x: number; y: number },
  dx: number,
  dy: number,
): number {
  const armX = corner.x - anchor.x;
  const armY = corner.y - anchor.y;
  const length = armX * armX + armY * armY;
  if (length === 0) {
    return 1;
  }
  // The new arm showed onto the old one, over the old one's length.
  const scale = ((armX + dx) * armX + (armY + dy) * armY) / length;
  return Number.isFinite(scale) ? scale : 1;
}

/**
 * One rectangle scaled about a point, in whole pixels.
 *
 * Never smaller than the model's minimum in either direction: a rectangle of
 * nothing is one `Piece::validate` refuses, and a gesture that ends in a
 * refusal every time is not a control.
 */
export function scaledAbout(
  rect: RectPx,
  anchor: { x: number; y: number },
  scale: number,
): RectPx {
  const width = Math.max(MIN_DIMENSION_PX, Math.round(rect.width * scale));
  const height = Math.max(MIN_DIMENSION_PX, Math.round(rect.height * scale));
  return {
    x: Math.round(anchor.x + (rect.x - anchor.x) * scale),
    y: Math.round(anchor.y + (rect.y - anchor.y) * scale),
    width,
    height,
  };
}

/** One rectangle moved, in whole pixels. */
export function nudged(rect: RectPx, dx: number, dy: number): RectPx {
  return { ...rect, x: rect.x + dx, y: rect.y + dy };
}

/**
 * One rectangle grown from its top-left corner, in whole pixels.
 *
 * What an arrow key does with a modifier held: the corner an operator is
 * reading the numbers from stays where it is, so the position they typed does
 * not move while they adjust the size.
 */
export function grown(rect: RectPx, dx: number, dy: number): RectPx {
  return {
    ...rect,
    width: Math.max(MIN_DIMENSION_PX, rect.width + dx),
    height: Math.max(MIN_DIMENSION_PX, rect.height + dy),
  };
}

/**
 * What a resize means, said plainly, or null when it means nothing.
 *
 * Two different consequences, and an operator needs both. A piece whose source
 * rectangle no longer fits makes the whole show unsaveable — `Piece::validate`
 * refuses it — so those are named. Every other piece keeps the rectangle it
 * had, which sounds like nothing happening and is not: the rectangle is in
 * composition pixels, so the same rectangle on a different canvas is a
 * different part of the picture.
 */
export function describeResize(report: ResizeReport): string | null {
  const { width, height } = report.size;
  const parts: string[] = [];

  if (report.outside.length > 0) {
    const names = report.outside.map((piece) => piece.name).join(', ');
    parts.push(
      `${report.outside.length} ${report.outside.length === 1 ? 'piece takes' : 'pieces take'} ` +
        `a region a ${width}×${height} composition does not contain (${names}). ` +
        `The server refuses a show with ${report.outside.length === 1 ? 'it' : 'them'} in it, ` +
        `so each must be moved or resized before this show can be saved.`,
    );
  }
  if (report.fitting.length > 0) {
    const one = report.fitting.length === 1;
    // "The other" only when there was a group before it to be other than.
    const subject =
      report.outside.length > 0
        ? `The other ${report.fitting.length} ${one ? 'piece keeps' : 'pieces keep'}`
        : one
          ? 'The piece keeps'
          : `All ${report.fitting.length} pieces keep`;
    parts.push(
      `${subject} the rectangle ${one ? 'it takes' : 'they take'}, in pixels, ` +
        `so ${one ? 'it' : 'each'} now takes a different part of the picture.`,
    );
  }

  return parts.length === 0 ? null : parts.join(' ');
}

// --- frame rates --------------------------------------------------------------

/**
 * A rate in millihertz, rounded to nearest (`FrameRate::millihertz`).
 *
 * The unit display hardware reports its modes in, so this is how an exact
 * rational rate is matched against a panel's mode list: 60000/1001 becomes
 * 59 940, which is what a driver calls that mode.
 */
export function millihertzOf(rate: FrameRate): number | null {
  if (rate.numerator === 0 || rate.denominator === 0) {
    return null;
  }
  return Math.round((rate.numerator * 1000) / rate.denominator);
}

/** A rate as a number to read, which is display only. */
export function rateText(rate: FrameRate): string {
  if (rate.denominator === 0) {
    return '—';
  }
  const named = FRAME_RATES.find(
    (each) => each.rate.numerator === rate.numerator && each.rate.denominator === rate.denominator,
  );
  if (named) {
    return named.label;
  }
  const value = rate.numerator / rate.denominator;
  return rate.denominator === 1 ? String(rate.numerator) : value.toFixed(3).replace(/0+$/, '');
}

/** Greatest common divisor, for reducing a rate to its smallest exact form. */
function gcd(a: number, b: number): number {
  let left = Math.abs(a);
  let right = Math.abs(b);
  while (right !== 0) {
    [left, right] = [right, left % right];
  }
  return left === 0 ? 1 : left;
}

/**
 * The exact rational rate a connector's mode is, or null when it has none.
 *
 * A driver reports millihertz, and the model stores a ratio, and the conversion
 * is not free: 59 940 mHz is 60000/1001, not 59940/1000, and the two are
 * different rates a scheduler would drift between. So the rates the model names
 * are tried first, by the same arithmetic the Rust rounds them with. Everything
 * else is an integer or near-integer rate a panel reports honestly — 60 000,
 * 144 000, 23 976 — and is reduced to its smallest exact form.
 *
 * The `/1001` family is the whole of the inexactness in practice, and it is
 * covered by the named rates, so what comes back is either one of those or a
 * ratio that is exactly the millihertz reported.
 */
export function rateFromMillihertz(millihertz: number): FrameRate | null {
  if (!Number.isFinite(millihertz) || millihertz <= 0) {
    return null;
  }
  const rounded = Math.round(millihertz);
  const named = FRAME_RATES.find((each) => millihertzOf(each.rate) === rounded);
  if (named) {
    return { ...named.rate };
  }
  const divisor = gcd(rounded, 1000);
  return { numerator: rounded / divisor, denominator: 1000 / divisor };
}

/** What a connector's mode reads as in a chooser. */
export function modeText(mode: ConnectorModeInfo): string {
  const rate = rateFromMillihertz(mode.refresh_millihertz);
  // Millihertz, exactly as reported, when nothing can be made of it: an
  // operator reading a mode list must see what the driver said.
  const hertz = rate === null ? `${mode.refresh_millihertz} mHz` : `${rateText(rate)} Hz`;
  return `${mode.size.width}×${mode.size.height} @ ${hertz}`;
}

/** The connector an output is forced onto, written the way a rack labels it. */
export function connectorKey(gpuPciAddress: string, connector: string): string {
  // `ovs_db::connector_key`, so a key built here is the key the server settled
  // that connector's role under.
  return `${gpuPciAddress}/${connector}`;
}

/** The connector one output is forced onto, or null when it names none. */
export function connectorOf(output: OutputConfig): string | null {
  const { gpu_pci_address: gpu, connector } = output.identity;
  return gpu && connector ? connectorKey(gpu, connector) : null;
}

/** The connector information for one output, or null when the server has none. */
export function connectorFor(
  output: OutputConfig,
  connectors: ConnectorInfo[],
): ConnectorInfo | null {
  const key = connectorOf(output);
  if (key === null) {
    return null;
  }
  return (
    connectors.find((each) => connectorKey(each.gpu_pci_address, each.connector) === key) ?? null
  );
}

// --- what the operator should be told, but is not an error --------------------

/** Something worth saying about the mapping that the server will still accept. */
export interface Advisory {
  /** The path into the document it is about, so it can be put beside the field. */
  path: string;
  /** What to say. */
  message: string;
}

/**
 * Everything true about this mapping that an operator would want to know and
 * that a save will not mention.
 *
 * Deliberately separate from `validate`. Validation says what the server would
 * refuse; this says what the server would accept and the wall would still get
 * wrong — a piece placed where nothing draws it, two pieces fighting over the
 * same panels, an output on a socket the server says is the service display.
 * Mixing the two would teach an operator to read a red mark as advice.
 *
 * `connectors` is what `/server/capabilities` reported. When it is empty —
 * because no renderer has connected yet — nothing here guesses at the machine,
 * and the connector advisories are simply not made.
 */
export function advisories(show: Show, connectors: ConnectorInfo[] = []): Advisory[] {
  const found: Advisory[] = [];

  for (const [index, output] of show.outputs.entries()) {
    const path = `outputs[${index}]`;
    const key = connectorOf(output);

    if (!output.enabled) {
      const count = piecesOf(show, output.id).length;
      if (count > 0) {
        found.push({
          path: `${path}.enabled`,
          message: `this output is switched off, so the ${count} ${count === 1 ? 'piece' : 'pieces'} on it ${count === 1 ? 'is' : 'are'} not drawn`,
        });
      }
    }

    if (key === null) {
      found.push({
        path: `${path}.identity`,
        message:
          'this output names no connector, so nothing drives it: an output is forced onto a socket by GPU and connector',
      });
      continue;
    }

    const twin = show.outputs.find(
      (other) => other.id !== output.id && connectorOf(other) === key,
    );
    if (twin) {
      found.push({
        path: `${path}.identity`,
        message: `${twin.name} is forced onto ${key} as well, and one socket drives one output`,
      });
    }

    // Nothing at all while the server has reported no connectors. An empty list
    // means no renderer has said what the machine has, which is a different
    // thing from a machine with no sockets, and neither is something to guess.
    if (connectors.length === 0) {
      continue;
    }
    const socket = connectors.find(
      (each) => connectorKey(each.gpu_pci_address, each.connector) === key,
    );
    if (!socket) {
      found.push({
        path: `${path}.identity`,
        message: `this server reports no connector ${key}; its pieces stay dark until one is there`,
      });
    } else if (socket.role === 'SERVICE_DISPLAY') {
      found.push({
        path: `${path}.identity`,
        message: `${key} is set as the service display on this server, not a media output`,
      });
    } else if (socket.role === 'UNUSED') {
      found.push({
        path: `${path}.identity`,
        message: `${key} is not marked as a media output on this server. What a socket is for is set on the server rather than in a show.`,
      });
    }
  }

  for (const [index, piece] of show.pieces.entries()) {
    const path = `pieces[${index}]`;
    const output = findOutput(show, piece.output_id);
    if (!output) {
      // Already a fault the server refuses; validation says so.
      continue;
    }

    if (piece.rotation_deg !== 0) {
      found.push({
        path: `${path}.rotation_deg`,
        message:
          'this piece is turned, so what it covers on the output is not the rectangle typed here and is not checked against the other pieces',
      });
    } else if (isOffScreen(piece.dest_rect, output.mode.size)) {
      found.push({
        path: `${path}.dest_rect`,
        message: `this piece lies entirely outside ${output.name}, so the renderer draws none of it`,
      });
    } else {
      const spill = spillOf(piece.dest_rect, output.mode.size);
      if (spill > 0) {
        found.push({
          path: `${path}.dest_rect`,
          message: `${spill} px of this piece falls outside ${output.name} and is not drawn`,
        });
      }
    }

    // **Resampling is not news** (owner, 2026-09-23): "Das ist zu viel Info."
    // Any piece whose source and destination are different sizes is resampled,
    // which is most of them and is what mapping a wall *is*. The panel already
    // says "resampled" beside the piece it is about; a paragraph of arithmetic
    // under the canvas for every one of them is a list nobody reads, and a list
    // nobody reads is where a real warning goes to hide.
  }

  for (const overlap of overlaps(show)) {
    const index = show.pieces.findIndex((piece) => piece.id === overlap.under.id);
    found.push({ path: `pieces[${index}].dest_rect`, message: describeOverlap(overlap) });
  }

  return found;
}

/**
 * What a piece can take its rectangle out of, in the operator's words.
 *
 * The owner's names (2026-09-23). The composition is the show as it leaves the
 * server; the three buses are what it is made of, and a wall fed from one is a
 * monitor of the look coming up rather than the one going out.
 *
 * One layer on its own is not here: a layer composites straight into its bus, so
 * there is no picture of one to sample, and offering it would be a control that
 * does nothing.
 */
export const PIECE_SOURCES: readonly { source: PieceSource; name: string }[] = [
  { source: { kind: 'composition' }, name: 'Composition' },
  { source: { kind: 'bus', route: 'A' }, name: 'Crossfade A' },
  { source: { kind: 'bus', route: 'B' }, name: 'Crossfade B' },
  { source: { kind: 'bus', route: 'M' }, name: 'Main' },
];

/** The same source as a string, so a `select` can hold one. */
export function sourceKeyOf(source: PieceSource | null | undefined): string {
  if (source === null || source === undefined || source.kind === 'composition') {
    return 'composition';
  }
  return `bus:${source.route}`;
}

/** And back again, falling to the composition for anything unrecognised. */
export function sourceFromKey(key: string): PieceSource {
  return (
    PIECE_SOURCES.find((entry) => sourceKeyOf(entry.source) === key)?.source ?? {
      kind: 'composition',
    }
  );
}

// --- snapping -----------------------------------------------------------------

/** A point on a surface, in that surface's own pixels. */
export interface SnapPoint {
  x: number;
  y: number;
}

/** How far apart two points may be and still be treated as one, by default. */
export const SNAP_WITHIN_PX = 5;

/**
 * The four corners of a rectangle, turned about its anchor.
 *
 * In document order — top-left, top-right, bottom-right, bottom-left — before
 * the turn, which is the order the edges between them run in.
 *
 * The anchor is a fraction of the rectangle, so a piece hinged at a corner
 * turns about that corner and one hung square turns about its middle. It is the
 * same pivot the renderer uses, which is what makes a snap on this screen a
 * snap on the wall.
 */
export function cornersOf(rect: RectPx, rotationDeg = 0, anchor?: SnapPoint): SnapPoint[] {
  const about = anchor ?? { x: 0.5, y: 0.5 };
  const pivot = {
    x: rect.x + about.x * rect.width,
    y: rect.y + about.y * rect.height,
  };
  const radians = (rotationDeg * Math.PI) / 180;
  const cos = Math.cos(radians);
  const sin = Math.sin(radians);

  return [
    { x: rect.x, y: rect.y },
    { x: rect.x + rect.width, y: rect.y },
    { x: rect.x + rect.width, y: rect.y + rect.height },
    { x: rect.x, y: rect.y + rect.height },
  ].map((point) => {
    const armX = point.x - pivot.x;
    const armY = point.y - pivot.y;
    return {
      x: pivot.x + armX * cos - armY * sin,
      y: pivot.y + armX * sin + armY * cos,
    };
  });
}

/**
 * Every point of a rectangle worth snapping to.
 *
 * Nine of them: the four corners, the middle of each edge, and the centre
 * (owner, 2026-09-23 — "auch Mittelpunkte an jeder Kante sollte ein
 * Einrastpunkt sein"). They are computed from the *turned* corners, so a piece
 * at an angle offers the corners it actually has rather than the ones its
 * rectangle would have had lying flat.
 */
export function snapPointsOf(rect: RectPx, rotationDeg = 0, anchor?: SnapPoint): SnapPoint[] {
  const corners = cornersOf(rect, rotationDeg, anchor);
  const middles = corners.map((corner, index) => {
    const next = corners[(index + 1) % corners.length] ?? corner;
    return { x: (corner.x + next.x) / 2, y: (corner.y + next.y) / 2 };
  });
  const first = corners[0] ?? { x: rect.x, y: rect.y };
  const third = corners[2] ?? first;
  const centre = { x: (first.x + third.x) / 2, y: (first.y + third.y) / 2 };
  return [...corners, ...middles, centre];
}

/**
 * The nudge that brings a moving set of points onto a fixed one.
 *
 * **The axes are decided separately**, which is what makes one function do both
 * of the things the owner asked for: when a corner is near a corner both answer
 * and the two meet exactly; when only an edge lines up, one answers and the
 * piece slides along that edge. Nothing is forced — an axis with no fixed point
 * within reach contributes nothing.
 *
 * Whole pixels, because a wall is patched to them and a snap that left a piece
 * at 419.6 would be a snap that did not snap.
 */
export function snapTranslation(
  moving: readonly SnapPoint[],
  fixed: readonly SnapPoint[],
  within = SNAP_WITHIN_PX,
): { dx: number; dy: number } {
  const { dx, dy } = snapGuides(moving, fixed, within);
  return { dx, dy };
}

/** A snap, and where it caught, so the screen can show it. */
export interface SnapGuides {
  /** The nudge, in the surface's own pixels. */
  dx: number;
  dy: number;
  /** The line it caught on, or null for an axis that did not catch. */
  atX: number | null;
  atY: number | null;
  /**
   * The point both axes caught on, when they caught on the same one.
   *
   * **Corner to corner**, which is a different thing from two edges that happen
   * to line up, and the owner asked for the two to look different (2026-09-23):
   * "eine Magenta Linie für Linie an Linie oder Punkt für Ecke an Ecke".
   */
  corner: SnapPoint | null;
}

/**
 * The same snap, with what it caught on.
 *
 * Separate from [`snapTranslation`] only because most callers want the nudge
 * and one wants to draw it. The arithmetic is here and there is one of it.
 */
export function snapGuides(
  moving: readonly SnapPoint[],
  fixed: readonly SnapPoint[],
  within = SNAP_WITHIN_PX,
): SnapGuides {
  let bestX: number | null = null;
  let bestY: number | null = null;
  let atX: number | null = null;
  let atY: number | null = null;

  for (const from of moving) {
    for (const to of fixed) {
      const dx = to.x - from.x;
      if (Math.abs(dx) <= within && (bestX === null || Math.abs(dx) < Math.abs(bestX))) {
        bestX = dx;
        atX = to.x;
      }
      const dy = to.y - from.y;
      if (Math.abs(dy) <= within && (bestY === null || Math.abs(dy) < Math.abs(bestY))) {
        bestY = dy;
        atY = to.y;
      }
    }
  }

  // Corner to corner is one fixed point answering both axes. Two edges that
  // happen to cross are two points, and saying so with a dot would claim a
  // meeting that is not there.
  const corner =
    atX !== null && atY !== null && fixed.some((point) => point.x === atX && point.y === atY)
      ? { x: atX, y: atY }
      : null;

  return { dx: Math.round(bestX ?? 0), dy: Math.round(bestY ?? 0), atX, atY, corner };
}

/**
 * The points a surface itself offers: its own corners, edges and middle.
 *
 * A wall's edge is the thing pieces are lined up against more often than they
 * are lined up against each other, and its middle is where a single piece goes.
 */
export function surfacePoints(surface: SizePx): SnapPoint[] {
  return snapPointsOf({ x: 0, y: 0, width: surface.width, height: surface.height });
}

// --- the anchor as the origin -------------------------------------------------
//
// The owner, 2026-09-23: "Der Anchor soll genau das machen wofür er da ist den
// wirklichen Nullpunkt/Anchor setzen im Piece… Anchor gilt für alles, auch für
// ziehen mit der Maus."
//
// The model keeps a rectangle, because that is what a renderer draws and what a
// wall is built from. The anchor is a fraction of it, and everything an operator
// does to a piece is expressed about that point: where it is, what it grows
// from, and what it turns about. A piece anchored at its centre and made twice
// as wide grows both ways; the same piece anchored top-left grows to the right.

/** The default anchor, which is the middle: `AnchorPoint::CENTRE`. */
export const CENTRE_ANCHOR: SnapPoint = { x: 0.5, y: 0.5 };

/** Where a rectangle's anchor sits, in the surface's own pixels. */
export function anchorOf(rect: RectPx, anchor: SnapPoint = CENTRE_ANCHOR): SnapPoint {
  return {
    x: rect.x + anchor.x * rect.width,
    y: rect.y + anchor.y * rect.height,
  };
}

/** The same rectangle, with its anchor put at a point. */
export function placedAt(rect: RectPx, anchor: SnapPoint, at: SnapPoint): RectPx {
  return {
    ...rect,
    x: Math.round(at.x - anchor.x * rect.width),
    y: Math.round(at.y - anchor.y * rect.height),
  };
}

/**
 * The same rectangle at a new size, with its anchor where it was.
 *
 * This is what makes a typed width mean what it says: a piece anchored at its
 * centre and told to be 400 wide is 400 wide about the same centre, not 400
 * wide hanging off the same left edge.
 */
export function sizedAbout(
  rect: RectPx,
  anchor: SnapPoint,
  width: number,
  height: number,
): RectPx {
  const at = anchorOf(rect, anchor);
  return placedAt({ ...rect, width, height }, anchor, at);
}

/**
 * The rectangle a dragged edge or corner produces, about the anchor.
 *
 * The anchor stays where it is and the *opposite* side moves to match, which is
 * what "gilt für alles" means: a corner dragged out from a piece anchored at its
 * centre grows the piece in both directions, and the same corner on a piece
 * anchored top-left grows it in one.
 *
 * An edge the anchor is standing on cannot move — a piece anchored at its right
 * edge has nothing to the right of the anchor to lengthen — so that drag does
 * nothing rather than dividing by nought.
 */
export function resizedAbout(
  rect: RectPx,
  anchor: SnapPoint,
  grip: string,
  dx: number,
  dy: number,
): RectPx {
  const pinned = 0.0001;
  let width = rect.width;
  let height = rect.height;

  if (grip.includes('e') && 1 - anchor.x > pinned) {
    width = rect.width + dx / (1 - anchor.x);
  } else if (grip.includes('w') && anchor.x > pinned) {
    width = rect.width - dx / anchor.x;
  }
  if (grip.includes('s') && 1 - anchor.y > pinned) {
    height = rect.height + dy / (1 - anchor.y);
  } else if (grip.includes('n') && anchor.y > pinned) {
    height = rect.height - dy / anchor.y;
  }

  return sizedAbout(
    rect,
    anchor,
    Math.max(MIN_DIMENSION_PX, Math.round(width)),
    Math.max(MIN_DIMENSION_PX, Math.round(height)),
  );
}
