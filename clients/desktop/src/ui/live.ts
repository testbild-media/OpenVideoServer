// Live Mode: the screen an operator stands in front of while the show runs.
//
// The reference drawing, in three
// bands — the global area, the dominant layer × slot matrix, and the preview/work
// area under it — and the whole of what it does is live control. There is no
// control on this screen for layer order, blend mode, slot assignment or any
// `default_*` value: those are the show document and the document is edited in
// Settings. Everything this panel sends goes through
// `send`, which takes a `LiveCommand` and checks it again at run time.
//
// Three things it is careful about, and all three are about honesty rather than
// appearance.
//
//   - **A pad lights because the server said so.** Nothing here sets its own
//     state from a click. What a click does is send a command; what the screen
//     shows is the mirror.
//   - **A control that has been moved and not confirmed says so.** `pending` is
//     a different colour from live and from refused, because during a show the
//     difference between "the wall moved" and "this client is talking to
//     nothing" is the only thing an operator needs from the screen.
//   - **Nothing draws a number the server did not send.** A value the mirror has
//     not delivered is an em dash. The tempo field is empty rather than zero,
//     the faders are disabled rather than sitting at their minimum, and the
//     layer rate reads `—` rather than `0.0`.
//
// Two things on the screen move, and both are carried on between samples rather
// than waited for: the beat strip under the tempo, and the progress bar on each
// layer that is playing a clip. That is what both of them are — `ClockTelemetry`
// and `LayerPlayhead` say as much — and the rules they keep are in
// `shared/clock.ts` and `shared/playhead.ts`: they predict only from a sample the
// server sent, at the rate the server sent with it, they are corrected by every
// sample, and they say they do not know rather than running on alone. One frame
// loop drives both, and it costs nothing when this is not the mode on screen or
// the window is not one anybody can see.
//
// The DOM is built once per shape and updated in place afterwards. That is not
// an optimisation: rebuilding a row while an operator has its opacity fader
// under their finger takes the fader out from under them.
//
// The bottom band's contextual tools are the drawing's two tab rails —
// COMPOSITION and SLICES on the preview box, BROWSER, GENERATOR, OPTIONS and
// EFFECTS on the work panel. Four of those six names are things that are show
// configuration, so each tab declares which of three things it is — a view, live
// control, or a way into Settings — and says so on screen. `shared/work.ts` makes
// that decision and does the reading; nothing in this band writes to the show.
// EFFECTS is live control because this server publishes a route for an effect's
// exposed parameters; GENERATOR says in full that there is none for a
// generator's, instead of offering a control with nowhere to send anything.

import type {
  ClockTelemetry,
  LayerPlayhead,
  MediaItem,
  ModuleParameter,
  ParameterValue,
  PlaybackDirection,
  PlaybackMode,
  SetLockRequest,
  SetSlotRequest,
} from '@ovs/schema';

import type { OvsBridge } from '../preload.js';
import type { BlendMode, SlotSource } from '../shared/types.js';
import type { ConnectionStatus } from '../shared/connection.js';
import { beatAt, beatNote, sampleClock, stripView, type ClockSample } from '../shared/clock.js';
import {
  isStale,
  playheadAt,
  playheadLine,
  playheadNote,
  samplePlayheads,
  type PlayheadSample,
} from '../shared/playhead.js';
import {
  controlKey,
  crossfaderLine,
  effectCommand,
  effectReset,
  generatorCommand,
  generatorReset,
  nearestSpeedScale,
  isLiveCommand,
  layerCommand,
  masterCommand,
  padsInBank,
  percentLine,
  playbackLabel,
  rateNote,
  readingNote,
  reportedBy,
  slotBanks,
  showTempo,
  speedLabel,
  SPEED_SCALE_STEPS,
  speedScaleLine,
  valueOf,
  PendingControls,
  type ControlId,
  type LiveCommand,
  type PadView,
  type Reading,
  type SlotBank,
} from '../shared/live.js';
import {
  BLACK_SLOT_INDEX,
  BLEND_MODES,
  BLEND_MODE_LABELS,
  MAX_BPM,
  MAX_BANKS,
  MAX_SLOT_INDEX,
  MAX_SLOT_NAME_CHARS,
  MIN_BPM,
  MIN_SLOT_BEATS,
  DEFAULT_SLOT_BEATS,
  PLAYBACK_DIRECTIONS,
  PLAYBACK_DIRECTION_LABELS,
  PLAYBACK_MODES,
  PLAYBACK_MODE_LABELS,
  IDENTITY_TRANSFORM,
  ROUTES,
  SLOTS_PER_BANK,
} from '../shared/limits.js';
import {
  COMPOSITION_SURFACE,
  MASKED_SURFACE,
  StreamPlayer,
  openStream,
  statusLine,
  streamUrl,
  type StreamHandle,
  type StreamStatus,
} from '../shared/stream.js';
import type { Route } from '../shared/types.js';
import type { Transform2D } from '@ovs/schema';
import { hexFromLinear, linearFromHex } from '../shared/colour.js';
import { emptyCatalogue, type Catalogue } from '../shared/modules.js';
import { durationLabel, facts, stateLabel } from '../shared/media.js';
import { browse, whereLabel, type Crumb, type FolderTile } from '../shared/browse.js';
import { ROOT_FOLDER } from '../shared/folders.js';
import {
  BROWSER_TABS,
  WORK_TABS,
  afterReconnect,
  chooseTab,
  editTarget,
  dropEffectOnPad,
  dropOnPad,
  effectChains,
  effectTiles,
  generatorLines,
  generatorTiles,
  libraryLines,
  matchingEffects,
  matchingGenerators,
  matchingNdi,
  ndiTiles,
  openWorkArea,
  parameterControl,
  parameterText,
  wireSource,
  type ChainLine,
  type Dragged,
  type EditTarget,
  type EffectTile,
  type EffectLine,
  type GeneratorLine,
  type GeneratorTile,
  type EffectParameter,
  type LibraryLine,
  type NdiTile,
  type ParameterLine,
  type TabDefinition,
  type WorkArea,
  type WorkTab,
} from '../shared/work.js';
import type { LayerView, ShowView, SlotView } from '../shared/view.js';
import { emptyView } from '../shared/view.js';
import { button, element, make } from './dom.js';

/** How often the still preview is refreshed while it is the one on screen. */
const PREVIEW_INTERVAL_MS = 500;

/**
 * The drag type a library icon carries.
 *
 * Its own type rather than `text/plain`, so that a pad lights up for a clip
 * dragged out of the browser and not for a word dragged out of a text field.
 */
const SOURCE_DRAG_TYPE = 'application/x-ovs-source';

/** A line break inside a `title`, which is the only markup a tooltip has. */
const NEWLINE = String.fromCharCode(10);

/** This client's own monotonic clock, for measuring how old a sample is. */
function monotonic(): number {
  return performance.now();
}

/**
 * Whether this machine has been asked not to animate anything.
 *
 * The beat strip and the playhead bars are the moving things in this client, so
 * they are the things that have to ask. Where the answer is yes they still show
 * what the server reported, at each sample — that is information, not
 * decoration — and simply do not run between samples.
 */
function stillness(): boolean {
  return (
    typeof window.matchMedia === 'function' &&
    window.matchMedia('(prefers-reduced-motion: reduce)').matches
  );
}

/** A fader's travel, in steps. Unit values are sent as thousandths of full. */
const FADER_STEPS = 1000;

/** What the live screen needs from the rest of the window. */
export interface LiveDeps {
  ovs: OvsBridge;
  /** Says something to the operator in the fault line at the foot of the window. */
  note: (message: string) => void;
  /**
   * Opens Settings at one thing in the show.
   *
   * The work area's way out. Several of its tabs are named after show
   * configuration — a slice, a generator's parameters, an effect chain — and
   * the honest thing a live screen can do with those is show them and offer the
   * editor at exactly that thing. Nothing here edits: what crosses is a request
   * to look, and the editor decides what to do with it, including refusing when
   * it is holding work nobody has saved.
   */
  openEditor: (target: EditTarget) => void;
}

/** One output the preview can be pointed at. */
export interface Surface {
  id: string;
  name: string;
}

/**
 * The two surfaces a server always has, in the order they matter.
 *
 * The composition first, because it is what the show *is* and is the one an
 * operator watches all evening. MAPPED second: the same picture with everything
 * no piece takes blacked out, which answers "is any of this being cut off?" by
 * being looked at straight after the first. The outputs come after both and are
 * for watching a wall rather than for watching the show (owner, 2026-09-23).
 */
const ALWAYS: readonly Surface[] = [
  { id: COMPOSITION_SURFACE, name: 'Composition' },
  // "Mapped" until the owner renamed it (2026-09-23). It is the composition
  // with everything that is not mapped onto a wall taken out, which is what
  // the pieces are — and with the outlines gone it is only that.
  { id: MASKED_SURFACE, name: 'Pieces' },
];

/**
 * One fader: a name, a bar drawn like the opacity one, and a number.
 *
 * The owner, 2026-09-23: "wie Opacity nur mit außerhalb Text". The opacity bar
 * writes its caption across itself, which works for one word and one value and
 * for nothing else; a column of six wants its names in a column of their own,
 * so the words come outside and the bar is only the bar.
 *
 * It works in the unit that is *shown* — per cent, degrees — rather than in the
 * model's. What reads the value converts it; what is dragged is the thing the
 * operator can see.
 */
interface FaderNodes {
  root: HTMLElement;
  bar: HTMLElement;
  input: HTMLInputElement;
  fill: HTMLElement;
  value: HTMLElement;
}

interface FaderSpec {
  /** What it is called, on the left. */
  name: string;
  /** The ends, in the unit shown. */
  min: number;
  max: number;
  /** How fine a step the arrow keys and the drag take. */
  step: number;
  /** What follows the number: `%`, `°`. */
  unit: string;
  /** While it is being dragged. */
  onInput?: (shown: number) => void;
  /** When it is let go, or changed from the keyboard. */
  onCommit: (shown: number) => void;
}

/**
 * A fader without its name and its number: the track, the fill, and the input.
 *
 * Lifted out of [`fader`] so that the parameter controls can be the same thing
 * (owner, 2026-09-27: "mach alle Slider in unserem Stil, wie wir alle anderen
 * schon haben"). They were the browser's own range input — a grey track and a
 * round knob — beside a master row of dark bars with an orange fill.
 *
 * A parameter row carries the name and the value in its own grid, so it takes
 * the bar alone; the master row wants all three and takes [`fader`]. The ends
 * travel with it because painting the fill needs them and nothing else here
 * knows them.
 */
interface FaderBar {
  bar: HTMLElement;
  fill: HTMLElement;
  input: HTMLInputElement;
  min: number;
  max: number;
}

/** Builds one. Its value is written by [`paintBar`], never here. */
function faderBar(spec: { min: number; max: number; step: number; label: string }): FaderBar {
  const bar = make('span', 'fader-bar');
  // Both ways from the middle, so the fill grows out of nought rather than out
  // of the left-hand end — which is what a position of −80% means.
  bar.classList.toggle('bipolar', spec.min < 0);
  const fill = make('span', 'fader-fill');
  const input = make('input');
  input.type = 'range';
  input.min = String(spec.min);
  input.max = String(spec.max);
  input.step = String(spec.step);
  input.setAttribute('aria-label', spec.label);
  bar.append(fill, input);
  return { bar, fill, input, min: spec.min, max: spec.max };
}

/** How far the fill goes, for the value the input is at. */
function paintBar(nodes: FaderBar, shown: number): void {
  const span = nodes.max - nodes.min || 1;
  const at = ((Math.min(nodes.max, Math.max(nodes.min, shown)) - nodes.min) / span) * 100;
  // Nought, as a position along the track. For a fader that only goes one way
  // this is the left-hand end, which is where its fill starts anyway.
  const zero = ((Math.min(nodes.max, Math.max(nodes.min, 0)) - nodes.min) / span) * 100;
  nodes.fill.style.setProperty('--from', `${Math.min(at, zero)}%`);
  nodes.fill.style.setProperty('--to', `${Math.max(at, zero)}%`);
}

/** Builds a fader. Its value is written by [`setFaderTo`], never here. */
function fader(spec: FaderSpec): FaderNodes {
  const root = make('label', 'fader');
  const name = make('span', 'fader-name', spec.name);
  const value = make('span', 'fader-value', '—');

  const track = faderBar({ min: spec.min, max: spec.max, step: spec.step, label: spec.name });
  const nodes: FaderNodes = {
    root,
    bar: track.bar,
    input: track.input,
    fill: track.fill,
    value,
  };

  const shown = (): number => Number(track.input.value);
  track.input.addEventListener('input', () => {
    paintFader(nodes, shown(), spec);
    spec.onInput?.(shown());
  });
  track.input.addEventListener('change', () => spec.onCommit(shown()));

  root.append(name, track.bar, value);
  return nodes;
}

/**
 * Writes a value onto a fader: the input, the number, and how far the fill goes.
 *
 * One place, because a fader is written from two — the drag, which must not
 * wait for the server, and the update, which must not fight the drag.
 */
function paintFader(nodes: FaderNodes, shown: number, spec: FaderSpec): void {
  paintBar({ ...nodes, min: spec.min, max: spec.max }, shown);
  const rounded = Math.round(shown * 10) / 10;
  nodes.value.textContent = `${rounded}${spec.unit}`;
}

/** Puts a server reading on a fader, unless it is the one being dragged. */
function setFaderTo(nodes: FaderNodes, shown: number, spec: FaderSpec, held: boolean): void {
  if (held || document.activeElement === nodes.input) {
    return;
  }
  nodes.input.value = String(shown);
  paintFader(nodes, shown, spec);
}

/**
 * The ends of each transform fader, in the unit an operator reads.
 *
 * The owner's ranges (2026-09-23). Position is per cent of the composition
 * rather than pixels, because a position that means the same thing on a 1080p
 * wall and a 4K one is the useful one; the model keeps pixels and this converts.
 *
 * Size is two controls in one place: the plain one moves both axes together,
 * which is what an operator reaches for, and the axes underneath move one at a
 * time. All three run to ten times (owner, 2026-09-23) — the plain one was
 * capped at full size, which made it the only one of the three that could not
 * reach where the other two could.
 */
const FADER_X: FaderSpec = { name: 'Position X', min: -200, max: 200, step: 1, unit: '%', onCommit: () => {} };
const FADER_Y: FaderSpec = { name: 'Position Y', min: -200, max: 200, step: 1, unit: '%', onCommit: () => {} };
const FADER_ROTATION: FaderSpec = { name: 'Rotation Z', min: -180, max: 180, step: 1, unit: '°', onCommit: () => {} };
const FADER_SIZE: FaderSpec = { name: 'Size', min: 0, max: 1000, step: 1, unit: '%', onCommit: () => {} };
const FADER_SIZE_X: FaderSpec = { name: 'Size X', min: 0, max: 1000, step: 1, unit: '%', onCommit: () => {} };
const FADER_SIZE_Y: FaderSpec = { name: 'Size Y', min: 0, max: 1000, step: 1, unit: '%', onCommit: () => {} };

/** The faders that place a source, and the box they live in. */
interface TransformNodes {
  root: HTMLElement;
  x: FaderNodes;
  y: FaderNodes;
  rotation: FaderNodes;
  size: FaderNodes;
  sizeX: FaderNodes;
  sizeY: FaderNodes;
}

/** Everything in one layer row that changes without the row being rebuilt. */
interface RowNodes {
  root: HTMLElement;
  name: HTMLButtonElement;
  /** The layer's own name, under its number in the same block. */
  nameText: HTMLElement;
  /** Takes the layer out of the composite and puts it back. */
  mute: HTMLButtonElement;
  routes: Map<Route, HTMLButtonElement>;
  blend: HTMLSelectElement;
  opacity: HTMLInputElement;
  /** The bar itself, which carries the control's state and takes the pointer. */
  opacityBar: HTMLElement;
  /** The orange face, clipped to how far along the value is. */
  opacityFill: HTMLElement;
  /** The percentage, written on both faces so it reads over either. */
  opacityValues: HTMLElement[];
  picture: HTMLElement;
  /** The progress bar, hidden outright on a layer the server reported no playhead for. */
  playhead: HTMLElement;
  playheadFill: HTMLElement;
  playheadTime: HTMLElement;
  pads: HTMLElement;
  padNodes: Map<number, PadNodes>;
  /** What the pad strip was built for, so it is rebuilt only when it must be. */
  padShape: string;
}

/**
 * How small a picture in the matrix may get, and how large.
 *
 * One number for every picture in the row — the layer's own and each slot
 * pad's — because they stand on one line (owner, 2026-09-23). The floor is
 * where a thumbnail stops being one; a window too short for that many layers
 * scrolls instead of shrinking past it. The ceiling is there because one layer
 * on a tall window should not be given a preview the size of a postcard simply
 * because the room exists.
 */
const MIN_TILE_HEIGHT = 40;
const MAX_TILE_HEIGHT = 132;

/** One pad. */
interface PadNodes {
  root: HTMLButtonElement;
  picture: HTMLElement;
  label: HTMLElement;
}

/**
 * The OPTIONS tab's controls, which are built once per layer.
 *
 * It is the one tab with controls in it, so it is the one tab that is not
 * rebuilt from the view each time: a fader rebuilt mid-drag is a fader taken
 * out from under the operator's finger.
 */
/** The selected slot's controls. Everything here is the document. */
interface SlotOptionNodes {
  root: HTMLElement;
  /** Which layer the slot belongs to, so the panel can name what it is about. */
  layerId: string;
  /** What the operator calls this pad. Empty means "after what it plays". */
  name: HTMLInputElement;
  /** How fast this slot runs through one pass, on either kind of sync. */
  speedSteps: Map<number, HTMLButtonElement>;
  /** What that setting asks of the decoder, when it is worth saying. */
  rate: HTMLElement;
  playbackMode: HTMLSelectElement;
  direction: HTMLSelectElement;
  sync: HTMLSelectElement;
  beats: HTMLInputElement;
  beatsRow: HTMLElement;
  transform: TransformNodes;
  /** Empties the pad, which a locked generator refuses. */
  clear: HTMLButtonElement;
}

interface OptionNodes {
  /** The layer these were built for. A different one is a rebuild. */
  layerId: string;
  root: HTMLElement;
  /** Where the layer's picture sits. Live, and driven by the Layer Fixture. */
  place: TransformNodes;
}

/**
 * One effect parameter's control, held so the panel is updated and not rebuilt.
 *
 * `declared` is the manifest's own definition, kept because it is what formats
 * the value and what says which of the inputs below there are: one range, one
 * checkbox, or four colour channels.
 */
/** Whatever a declared parameter is drawn with: a field, a box or a menu. */
type ParameterInput = HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement;

interface ParameterNodes {
  declared: ModuleParameter;
  /**
   * Whatever the manifest's kind is drawn with.
   *
   * Not only `input`: a choice is a menu and text is a box, and typing both as
   * inputs is what made them get drawn as colour channels instead.
   */
  inputs: ParameterInput[];
  /**
   * The fader behind one of those inputs, where it is one, at the same index.
   *
   * A range input carries its value and the fill is drawn from it, so writing
   * the value without this leaves the orange where it was — a fader reading one
   * thing and saying another.
   */
  bars: (FaderBar | null)[];
  value: HTMLElement;
  show: HTMLElement;
  refusal: HTMLElement;
}

/**
 * The EFFECTS tab's controls, while that is the tab on screen.
 *
 * Built once per shape and updated in place afterwards, for the reason
 * `OptionNodes` is: an operator with a blur radius under their finger must not
 * have it rebuilt underneath them. Everything is keyed the way the pending map
 * keys it — `controlKey` — so a reading and its control are found by the same
 * name.
 */
interface EffectsNodes {
  /** The layer and the chains these were built for. A different one is a rebuild. */
  shape: string;
  root: HTMLElement;
  parameters: Map<string, ParameterNodes>;
  /** The offer to put one instance back, by instance id. */
  resets: Map<string, HTMLButtonElement>;
  /** Where a refused reset is shown, by instance id. */
  resetNotes: Map<string, HTMLElement>;
  /** Whether what a chain or a slot is on is on air, by chain or by instance. */
  onAir: Map<string, HTMLElement>;
}

/**
 * Which kind of module one row of controls belongs to, and which instance.
 *
 * An effect and a generator are both plugins with declared parameters, and the
 * owner's instruction of 2026-09-23 is that they use one model — so the panel
 * that draws one draws the other, and this is the only thing that differs
 * between them. Two variants rather than one because the identities are
 * different types on the server and a single key would be the one place in the
 * system where an effect and a generator could collide.
 */
type PluginRef = { of: 'effect' | 'generator'; id: string; name: string };

/** Which control an operator is touching, for a plugin's parameter or its reset. */
function pluginControl(plugin: PluginRef, parameter: string | null): ControlId {
  return plugin.of === 'effect'
    ? { of: 'effect', effectInstanceId: plugin.id, parameter }
    : { of: 'generator', generatorInstanceId: plugin.id, parameter };
}

/** Moving one of a plugin's parameters. */
function pluginCommand(plugin: PluginRef, key: string, value: ParameterValue): LiveCommand {
  return plugin.of === 'effect'
    ? effectCommand(plugin.id, key, value)
    : generatorCommand(plugin.id, key, value);
}

/** Putting one plugin instance back to what the show configured. */
function pluginReset(plugin: PluginRef): LiveCommand {
  return plugin.of === 'effect' ? effectReset(plugin.id) : generatorReset(plugin.id);
}

/**
 * What a control a lighting console holds says when it is pointed at.
 *
 * Shown and not offered (owner, 2026-10-19): the server refuses it, and the
 * one way to take it by hand is the DMX switch, which is what this says.
 */
const DESK_HELD = 'Driven by the lighting console while DMX is on. Switch DMX off to change it here.';

/**
 * Marks a control as the desk's, or as this client's again.
 *
 * The class draws it; the title, when one is given, is what it says while it
 * is held, and goes back to `otherwise` when it is not.
 */
function holdForDesk(node: HTMLElement, held: boolean, otherwise?: string): void {
  node.classList.toggle('desk', held);
  if (otherwise !== undefined) {
    node.title = held ? DESK_HELD : otherwise;
  }
}

/** The BROWSER tab's own nodes, which outlive a redraw so typing survives one. */
interface BrowserNodes {
  search: HTMLInputElement;
  /** Which folder of the library the grid is, as a path back up. MEDIA only. */
  path: HTMLElement;
  grid: HTMLElement;
  note: HTMLElement;
  /**
   * What the grid was built for, so it is only built again when that changes.
   *
   * **This is what makes dragging work at all.** The live screen redraws on
   * every sample the server sends — a fader moving, a slot switching, a
   * thumbnail arriving — and rebuilding the tiles on each of those took the
   * tile out from under the pointer before a drag could begin (owner,
   * 2026-09-23). The same reason the layer rows and the pads are built once
   * per shape and updated in place.
   */
  shape: string;
  /** One tile per thing that can be dragged, by what identifies it. */
  tiles: Map<string, HTMLElement>;
}

/** A drop that is waiting for the operator to say yes. */
interface ArmedDrop {
  layerId: string;
  slotIndex: number;
  source: SlotSource;
  /** What is in the pad now, to name in the question. */
  replacing: string;
}
/** The live show screen. */
export class LivePanel {
  readonly #ovs: OvsBridge;
  readonly #note: (message: string) => void;
  readonly #openEditor: (target: EditTarget) => void;

  #view: ShowView = emptyView();
  /** What has been asked for and not confirmed. */
  readonly #pending = new PendingControls();

  /** Whether the realtime connection is live, as opposed to on its way back. */
  #connected = false;
  /** Whether this is the mode on screen; the preview costs a machine running a show. */
  #visible = true;
  /** The server signed in to, which is the preview socket's address. */
  #origin: string | null = null;

  /** Which bank of slots the matrix is showing, counting from zero. */
  #bank = 0;
  /** The banks the open show has slots for, worked out once per redraw. */
  #banks: SlotBank[] = [];
  /** Which layer the OPTIONS tab is about. */
  #selected: string | null = null;
  /**
   * Which slot of the selected layer OPTIONS is about, or null for the layer.
   *
   * Clicking a pad's *picture* plays it; clicking its *name* chooses it here
   * (owner, 2026-09-23). Two different things to do with a pad, so two
   * different places to press.
   */
  #selectedSlot: number | null = null;
  /** What the OPTIONS panel was built for, so it is rebuilt only when that changes. */
  #optionsShape = '';
  /** The selected slot's own controls, when a slot is what is selected. */
  #slotOptions: SlotOptionNodes | null = null;
  /** The OPTIONS tab's controls, while that is the tab on screen. */
  #options: OptionNodes | null = null;
  /** The EFFECTS tab's controls, while that is the tab on screen. */
  #effects: EffectsNodes | null = null;
  /**
   * The selected slot's own generator controls, in OPTIONS.
   *
   * The same nodes the GENERATOR tab uses, for one slot instead of all of them:
   * an operator who has chosen a pad is asking about that pad, and a screenful
   * of faders for the others is a screenful of ways to move the wrong one.
   */
  #slotGenerator: EffectsNodes | null = null;

  /** The BROWSER tab's search box and grid, while that is the tab on screen. */
  #browser: BrowserNodes | null = null;
  /**
   * What the operator has typed into the browser's search.
   *
   * Held on the panel rather than read off the input, so that it survives the
   * tab being left and come back to, and so that a redraw cannot lose it.
   */
  #librarySearch = '';
  /** Which folder of the library the MEDIA browser has open. The root is ''. */
  #browserFolder = '';
  /** Every folder the library has, empty ones included. */
  #libraryFolders: string[] = [];
  /**
   * What is being dragged out of the browser, while a drag is in progress.
   *
   * The drag's own `dataTransfer` carries it too, and that is what makes the
   * gesture legible to the platform. This is what the drop reads, because
   * `getData` is deliberately empty during `dragover` and the pad has to
   * decide whether to light up before the drop happens — and, now that two
   * different things can be carried, what it would do with one.
   */
  #dragging: Dragged | null = null;
  /**
   * A drop onto an occupied pad, waiting to be confirmed.
   *
   * The owner's rule is that a taken slot asks before it is overwritten
   * (2026-09-23). The asking is on the pad itself rather than in a dialogue:
   * the operator's hand is already there, the pad is the thing at risk, and a
   * modal over a live screen hides the show it is asking about.
   */
  #replacing: ArmedDrop | null = null;
  /** The master speed buttons, by the speed each one asks for. */
  readonly #speedSteps = new Map<number, HTMLButtonElement>();

  /** A control the operator has under their finger, which must not be redrawn. */
  #holding: string | null = null;
  /** One command in flight per control, with the latest value waiting behind it. */
  readonly #inFlight = new Set<string>();
  readonly #queued = new Map<string, () => Promise<void>>();

  /**
   * Which contextual tool is open, and what the preview box is pointed at.
   *
   * The client's own state, not the show's and not the document's. It survives
   * a reconnection untouched (`afterReconnect`) because where an operator was
   * looking is not a fact about a server.
   */
  #work: WorkArea = openWorkArea();

  /** The library as the server holds it, for the BROWSER tab. */
  #library: MediaItem[] = [];
  /** What this server publishes, for reading a generator's and an effect's parameters. */
  #catalogue: Catalogue = emptyCatalogue();

  /**
   * The NDI senders the server can see.
   *
   * Replaced whole whenever the server says so, never merged: a sender that has
   * been switched off has to leave the list, and a tile that outlived its sender
   * is a pad assignment that goes dark on the night.
   */
  #ndiSources: readonly string[] = [];
  /** Display names for media items, from `/media`. Absent means not asked yet. */
  readonly #names = new Map<string, string>();
  /** A picture per media item: absent means nobody asked, null means there is none. */
  readonly #thumbnails = new Map<string, string | null>();
  /**
   * Moving previews, by item: ten frames of a clip in one picture.
   *
   * Null for an item the server has none for, which is a still, one registered
   * before this existed, and one nothing could be read from. Asked for beside
   * the still and never separately, so a pad is never half-drawn.
   */
  readonly #strips = new Map<string, string | null>();
  /** Items a thumbnail has been asked for, so it is asked for once. */
  readonly #asked = new Set<string>();

  /** The rows on screen, by layer, so a redraw updates rather than replaces. */
  readonly #rows = new Map<string, RowNodes>();
  /** What the matrix was built for: the layers, in order, and the bank. */
  #matrixShape = '';
  /** What the tile was last sized for: the row count and the matrix's box. */
  #tileFor = '';
  /**
   * The placement each transform panel has in hand, before the server answers.
   *
   * By the panel's key, so a layer's and a slot's do not tread on each other.
   * Cleared as soon as nothing is being dragged.
   */
  readonly #inHand = new Map<string, Transform2D>();

  // --- the moving things ----------------------------------------------------
  //
  // The last thing the server said about its clock and about each playing
  // layer's playhead, and one frame loop predicting from both. Everything that
  // decides where the beat and the playheads are lives in `shared/clock.ts` and
  // `shared/playhead.ts`; what is here is the strip, the bars, and the rule that
  // they cost nothing when nobody is looking at them.

  /** The last clock telemetry carried, with the moment it arrived. */
  #clock: ClockSample | null = null;
  /**
   * The last playhead reported per layer, with the moment it arrived.
   *
   * Replaced wholesale by each telemetry sample rather than merged into: a layer
   * the server has stopped reporting is a layer with no playhead, and a bar left
   * over from the last sample would go on describing a clip that has finished.
   */
  #playheads = new Map<string, PlayheadSample>();
  /** The frame the strip and the bars are waiting on, or null when nothing animates. */
  #beatFrame: number | null = null;
  /** The marks on screen, so a frame writes one number rather than rebuilding them. */
  #beatMarks: HTMLElement[] = [];
  /** The beat in figures, for a bar too long to draw a mark for each beat. */
  #beatCount: HTMLElement | null = null;
  /** What the strip was built for: how many marks, and whether they are counted. */
  #beatShape = '';
  /** Which mark is lit, so a frame touches a class only when the beat moves. */
  #beatLit = -1;
  /** What the strip last said in words, so it is written when it changes and not per frame. */
  #beatSaid = '';

  /**
   * What the preview can be pointed at.
   *
   * The composition and the composition masked by the pieces are always
   * there, whatever the server is driving — the second is the first with a
   * mask over it rather than anything a show configures. The outputs are
   * added when telemetry says what they are, and are the secondary ones: they
   * are for watching a wall rather than for watching the show (owner,
   * 2026-09-23).
   */
  #surfaces: Surface[] = [...ALWAYS];
  #surface = COMPOSITION_SURFACE;
  #stream: StreamHandle | null = null;
  #player: StreamPlayer | null = null;
  #canvas: CanvasRenderingContext2D | null = null;
  #previewTimer: number | null = null;

  readonly #ui = {
    tap: element<HTMLButtonElement>('tap'),
    tapNote: element('tap-note'),
    bpm: element<HTMLInputElement>('bpm'),
    bpmDown: element<HTMLButtonElement>('bpm-down'),
    bpmUp: element<HTMLButtonElement>('bpm-up'),
    dmxEnabled: element<HTMLButtonElement>('dmx-enabled'),
    saveLive: element<HTMLButtonElement>('save-live'),
    beatStrip: element('beat-strip'),
    phaseReset: element<HTMLButtonElement>('phase-reset'),
    banks: element('banks'),
    crossfader: element<HTMLInputElement>('crossfader'),
    crossfaderValue: element('crossfader-value'),
    crossfaderValueOn: element('crossfader-value-on'),
    crossfaderFill: element('crossfader-fill'),
    dimmerValueOn: element('dimmer-value-on'),
    dimmerFill: element('dimmer-fill'),
    dimmer: element<HTMLInputElement>('dimmer'),
    dimmerValue: element('dimmer-value'),
    dimmerReported: element('dimmer-reported'),
    speedScale: element<HTMLInputElement>('speed-scale'),
    speedScaleValue: element('speed-scale-value'),
    speedScaleReported: element('speed-scale-reported'),
    freeze: element<HTMLButtonElement>('freeze'),
    matrix: element('matrix'),
    surfaces: element('preview-surfaces'),
    preview: element<HTMLImageElement>('preview'),
    previewCanvas: element<HTMLCanvasElement>('preview-canvas'),
    previewNote: element('preview-note'),
    workHead: element('work-head'),
    workTabs: element('work-tabs'),
    browserTabs: element('browser-tabs'),
    workTitle: element('work-title'),
    workKind: element('work-kind'),
    workSays: element('work-says'),
    workTool: element('work-tool'),
  };

  constructor(deps: LiveDeps) {
    this.#ovs = deps.ovs;
    this.#note = deps.note;
    this.#openEditor = deps.openEditor;
    this.#wireGlobal();
    this.#buildTabs();
    this.#drawSurfaces();
    this.#render();

    // A window that is minimised, or behind another one, is a window nobody is
    // reading. The strip and the bars stop there as they stop in another mode: a
    // metronome animating out of sight is somebody's battery spent on a screen
    // that is not being looked at.
    document.addEventListener('visibilitychange', () => this.#syncFrames());

    // The matrix changes size without the layers changing: the window is
    // resized, or the work area under it is opened or closed. Watching the box
    // catches all of them, and watching the box rather than the rows means
    // sizing the tile cannot set this off again — the tile changes what is
    // inside the matrix, never the matrix itself.
    if (typeof ResizeObserver === 'function') {
      new ResizeObserver(() => {
        this.#sizeTiles();
      }).observe(this.#ui.matrix);
    }

    this.#ui.saveLive.addEventListener('click', () => void this.#saveLive());

    // Escape takes back a question a pad is asking. A drop onto an occupied
    // slot arms it and waits; the operator must be able to walk away from that
    // without pressing anything on the matrix.
    document.addEventListener('keydown', (event) => {
      if (event.key === 'Escape') {
        this.#cancelReplace();
      }
    });

    this.#drawBeat();
  }

  // --- what the rest of the window tells it ---------------------------------

  /**
   * The connection changed.
   *
   * A connection that is on its way back keeps the matrix — the show is carrying
   * on without this client and the layout is the last thing the server said —
   * but drops both previews, because a picture cannot be dimmed the way the
   * matrix is to say that it is old.
   */
  setConnection(status: ConnectionStatus, origin: string | null): void {
    const connected = status.state === 'live';
    this.#origin = origin;

    if (connected && !this.#connected) {
      // A fresh connection is a fresh set of facts. Anything this client was
      // waiting to hear about was asked of a server it is no longer talking to.
      this.#pending.clear();
      // Which tab was open is not one of those facts, and restoring it sends
      // nothing: the work area is where the operator was looking, not something
      // the server was told.
      this.#work = afterReconnect(this.#work).area;
      void this.#loadLibrary();
      void this.#loadCatalogues();
    }
    this.#connected = connected;

    this.#ui.tap.disabled = !connected;
    this.#ui.phaseReset.disabled = !connected;
    this.#ui.bpm.disabled = !connected;
    this.#ui.bpmDown.disabled = !connected;
    this.#ui.bpmUp.disabled = !connected;
    this.#ui.dmxEnabled.disabled = !connected;
    this.#ui.saveLive.disabled = !connected;
    this.#ui.crossfader.disabled = !connected;
    this.#ui.dimmer.disabled = !connected;
    this.#ui.speedScale.disabled = !connected;
    this.#ui.freeze.disabled = !connected;
    if (!connected) {
      this.#ui.tapNote.textContent = '';
      // The show is carrying on without this client, and where its beat and its
      // playheads are now is something only the server can say. Both stop rather
      // than running on from the last thing this client heard.
      this.#clock = null;
      this.#playheads = new Map();
      this.#drawBeat();
      this.#drawPlayheads();
      this.#syncFrames();
    }

    this.#syncStream();
    this.#render();
  }

  /** Whether the live screen is the one on screen. */
  setVisible(visible: boolean): void {
    this.#visible = visible;
    this.#syncStream();
    this.#syncFrames();
  }

  /**
   * Where the server says its master clock is.
   *
   * Telemetry rather than state, and absent while no show is open, because
   * there is no show for a beat to belong to (`ClockTelemetry`). It arrives on
   * the same samples the output list does, about once a second, and each one
   * corrects the prediction the strip has been running on.
   */
  setClock(clock: ClockTelemetry | null): void {
    this.#clock = sampleClock(clock, monotonic());
    // Drawn here rather than left to the next frame: a correction that waited
    // for the animation would not land at all in a mode where it is not running.
    this.#drawBeat();
    this.#syncFrames();
  }

  /**
   * Where the server says each playing layer's playhead is.
   *
   * One entry per layer actually playing a clip: a generator layer, a black
   * layer and a clip that has played once and blacked report nothing, and the
   * bar for a layer that is not in this list is not drawn at all
   * (`LayerPlayhead`). Each sample corrects the prediction the bars have been
   * running on, and replaces the set of layers that have one.
   */
  setPlayheads(playheads: readonly LayerPlayhead[] | null | undefined): void {
    this.#playheads = samplePlayheads(playheads, monotonic());
    this.#drawPlayheads();
    this.#syncFrames();
  }

  /** A new view from the server. */
  draw(view: ShowView): void {
    // Nothing is settled here. A control stops waiting when the server answers
    // its request; this is the update stream, which is what *corrects* a
    // control — a value the server clamped, or one another source is holding.
    this.#view = view;
    this.#askForThumbnails();
    this.#render();
  }

  /** The outputs the server says it is driving, from telemetry. */
  setSurfaces(outputs: readonly Surface[]): void {
    const wanted: Surface[] = [...ALWAYS, ...outputs];
    if (wanted.map((entry) => entry.id).join(',') === this.#surfaces.map((e) => e.id).join(',')) {
      return;
    }
    this.#surfaces = wanted;

    // An output that has gone takes the panel back to the composition, which is
    // the one surface every server has.
    if (!wanted.some((entry) => entry.id === this.#surface)) {
      this.#watch(COMPOSITION_SURFACE);
    }
    this.#drawSurfaces();
  }

  /**
   * The NDI senders the server can see.
   *
   * Compared before redrawing: the server sends the whole list after every
   * reconnect, and replacing the grid for an identical list would take a tile
   * out from under somebody's pointer in the middle of a drag.
   */
  setNdiSources(sources: readonly string[]): void {
    if (sources.join(',') === this.#ndiSources.join(',')) {
      return;
    }
    this.#ndiSources = [...sources];
    // Only the one browser shows them, so only it is redrawn — and it redraws
    // itself from the list, so there is nothing else to tell.
    if (this.#work.tab === 'inputs') {
      this.#drawBrowser();
    }
  }

  // --- sending --------------------------------------------------------------

  /**
   * Sends one live command and remembers that it is out.
   *
   * `control` is what the command is about, so the screen can say that this one
   * has not been answered yet.
   *
   * Nothing here waits: a command is sent and the screen carries on. A server
   * that never answers leaves a control pending, which is exactly what has
   * happened.
   */
  #send(command: LiveCommand, control: ControlId): Promise<void> {
    if (!isLiveCommand(command)) {
      // Unreachable through the type, and checked anyway: this is the one place
      // a control on the live screen can reach a server, and the rule it keeps
      // is that nothing here writes to the show.
      return Promise.resolve();
    }
    this.#pending.asked(control, wantedOf(command, control));
    this.#render();

    return this.#dispatch(command, control);
  }

  async #dispatch(command: LiveCommand, control: ControlId): Promise<void> {
    const result = await this.#ovs.run(command);
    if (!result.ok) {
      // The server's own words, against the control that caused them. An
      // effect refusal quotes the manifest — "radius_px must be a number
      // between 0 and 64" — and paraphrasing it would be this client inventing
      // a range the server never stated.
      this.#pending.refused(control, result.message);
      this.#note(result.message);
      this.#render();
      return;
    }
    // The answer is the answer. A live endpoint replies 204 when it accepted
    // the change and when the change moved nothing, and publishes only when
    // something moved — so a control that waited for the publication would hang
    // for ever on a fader held still or a value re-sent. What the control shows
    // from here is the mirror's, and the update stream corrects it if the
    // server did something other than what was asked.
    this.#pending.acknowledged(control);
    this.#render();
  }

  /**
   * Sends for a control that is being dragged, one at a time.
   *
   * A fader produces an event per pixel and each one is an HTTP request. Keeping
   * one in flight per control, with only the latest value waiting behind it,
   * means an operator's whole gesture arrives as the position they stopped at
   * rather than as four hundred requests queued against a busy server.
   */
  #sendLatest(key: string, send: () => Promise<void>): void {
    if (this.#inFlight.has(key)) {
      // Only the latest is kept. An operator's gesture is the position they
      // stopped at, not every pixel on the way there, and a queue of those would
      // still be arriving at a busy server after they let go.
      this.#queued.set(key, send);
      return;
    }
    this.#inFlight.add(key);
    void send()
      // A request that failed is still a request that finished. It has already
      // been reported by `#dispatch`; what matters here is that this control can
      // send again, because a control that stopped sending would be stuck at
      // whatever the last failure was.
      .catch(() => undefined)
      .finally(() => {
        this.#inFlight.delete(key);
        const queued = this.#queued.get(key);
        if (queued !== undefined) {
          this.#queued.delete(key);
          this.#sendLatest(key, queued);
        }
      });
  }

  // --- the global area ------------------------------------------------------

  #wireGlobal(): void {
    this.#ui.tap.addEventListener('click', () => void this.#tap());

    // **Freeze.** It lived in a `#wireMaster` of its own, which nothing ever
    // called — so the button drew, lit, enabled and disabled itself correctly
    // and had no click handler at all (owner, 2026-09-24: "Freeze funktioniert
    // immer noch nicht"). The server half had been working the whole time.
    //
    // Here rather than in a second method, because every other control on this
    // bar is here and a method that has to be remembered is a method that will
    // be forgotten again.
    this.#ui.freeze.addEventListener('click', () => {
      const now = this.#view.master?.freeze ?? null;
      if (now === null) {
        // Nothing to invert. Freezing a show whose state has not arrived would
        // be guessing which way the switch is.
        this.#note('The server has not reported whether the output is frozen.');
        return;
      }
      const command = masterCommand(this.#view.master, { freeze: !now });
      if (command !== null) {
        void this.#send(command, { of: 'master', field: 'freeze' });
      }
    });
    this.#ui.phaseReset.addEventListener('click', () => {
      // Not a state change on the server: the phase is the clock's and nothing
      // in the authoritative state describes it (`live_routes::reset_phase`).
      // So there is nothing to wait for and nothing to mark pending — what an
      // operator sees is the next beat landing where they asked for it.
      void this.#ovs.run({ do: 'reset_phase' });
    });

    this.#ui.bpm.addEventListener('change', () => {
      // A comma is a decimal point on a German keyboard, and typing one into a
      // tempo is what somebody standing at this machine will do.
      const typed = Number(this.#ui.bpm.value.trim().replace(',', '.'));
      if (!Number.isFinite(typed)) {
        this.#render();
        return;
      }
      this.#setTempo(typed);
    });

    // One BPM at a time, from wherever the clock is now rather than from
    // whatever is in the field: somebody halfway through typing has not asked
    // for their half-typed number to be stepped.
    this.#ui.bpmDown.addEventListener('click', () => this.#stepTempo(-1));
    this.#ui.bpmUp.addEventListener('click', () => this.#stepTempo(1));

    // Taking the show off the console, which is what this window is for when
    // something out there has gone wrong (owner, 2026-09-23).
    this.#ui.dmxEnabled.addEventListener('click', () => {
      const now = this.#view.master?.dmxEnabled ?? true;
      const command = masterCommand(this.#view.master, { dmxEnabled: !now });
      if (command !== null) {
        void this.#send(command, { of: 'master', field: 'dmxEnabled' });
      }
    });

    this.#fader(this.#ui.crossfader, 'crossfader', (value) => ({ crossfader: value }));
    this.#fader(this.#ui.dimmer, 'dimmer', (value) => ({ dimmer: value }));

    // Six buttons, one per step, because the scale has six settings and
    // nothing in between (owner, 2026-09-23). A fader across this range is a
    // control an operator can move away from 1x and never quite put back.
    for (const step of SPEED_SCALE_STEPS) {
      const node = button(speedLabel(step), () => this.#chooseSpeed(step), {
        className: 'speed-step',
        title: `Run the show's clock at ${speedLabel(step)}`,
      });
      this.#speedSteps.set(step, node);
      this.#ui.speedScale.append(node);
    }
  }

  /** Puts the master clock on one of its six speeds. */
  #chooseSpeed(scale: number): void {
    this.#ui.speedScaleValue.textContent = speedLabel(scale);
    const command = masterCommand(this.#view.master, { speedScale: scale });
    if (command !== null) {
      this.#sendLatest('master.speedScale', () =>
        this.#send(command, { of: 'master', field: 'speedScale' }),
      );
    }
  }

  /** Wires one unit fader: crossfader or dimmer. */
  #fader(
    input: HTMLInputElement,
    field: 'crossfader' | 'dimmer',
    wish: (value: number) => { crossfader?: number; dimmer?: number },
  ): void {
    const key = `master.${field}`;
    input.addEventListener('pointerdown', () => {
      this.#holding = key;
    });
    input.addEventListener('pointerup', () => {
      this.#holding = null;
    });
    input.addEventListener('input', () => {
      const value = Number(input.value) / FADER_STEPS;
      this.#drawMasterLabels(value, field);
      const command = masterCommand(this.#view.master, wish(value));
      if (command !== null) {
        this.#sendLatest(key, () => this.#send(command, { of: 'master', field }));
      }
    });
  }

  /** What a fader says while it is being dragged, before the server answers. */
  #drawMasterLabels(value: number, field: 'crossfader' | 'dimmer'): void {
    if (field === 'crossfader') {
      const said = crossfaderLine({ state: 'live', value });
      this.#ui.crossfaderValue.textContent = said;
      this.#ui.crossfaderValueOn.textContent = said;
      fillFrom(this.#ui.crossfaderFill, value, 0.5);
    } else {
      const said = percentLine({ state: 'live', value });
      this.#ui.dimmerValue.textContent = said;
      this.#ui.dimmerValueOn.textContent = said;
      fillFrom(this.#ui.dimmerFill, value, 0);
    }
  }

  async #tap(): Promise<void> {
    const result = await this.#ovs.run({ do: 'tap' });
    if (!result.ok) {
      this.#note(result.message);
      return;
    }
    // The first tap of a run never changes anything: one tap is not an interval,
    // and the server says so (`TapTempoResponse.changed`). Saying "tapping…"
    // until it turns true is showing the truth rather than a tempo that did not
    // move.
    this.#ui.tapNote.textContent = result.tap?.changed === false ? 'tapping…' : '';
  }

  // --- drawing --------------------------------------------------------------

  #render(): void {
    this.#drawMaster();
    this.#drawBanks();
    this.#drawMatrix();
    this.#drawWork();
  }

  /**
   * Asks the server for a tempo, clamped to what the model allows.
   *
   * Two decimal places, because a tempo read off a track is 128.35 and not 128
   * (owner, 2026-09-23). More than two would be a number nobody could tap to.
   */
  #setTempo(wanted: number): void {
    const bpm = Math.round(Math.min(MAX_BPM, Math.max(MIN_BPM, wanted)) * 100) / 100;
    const command = masterCommand(this.#view.master, { bpm });
    if (command !== null) {
      void this.#send(command, { of: 'master', field: 'bpm' });
    } else {
      this.#render();
    }
  }

  /** One BPM up or down from where the clock actually is. */
  #stepTempo(by: 1 | -1): void {
    const now = valueOf(this.#pending.read<number>({ of: 'master', field: 'bpm' }, this.#view.master?.bpm));
    if (now === null) {
      return;
    }
    this.#setTempo(now + by);
  }

  #drawMaster(): void {
    const master = this.#view.master;

    const bpm = this.#pending.read<number>({ of: 'master', field: 'bpm' }, master?.bpm);
    if (document.activeElement !== this.#ui.bpm) {
      // Empty rather than zero: a tempo nobody has reported is not a tempo of
      // nought, and this field is also where one is typed.
      // Two places when it has them and none when it does not: 128 reads better
      // than 128.00 on a clock somebody is glancing at.
      const tempo = valueOf(bpm);
      this.#ui.bpm.value =
        bpm.state === 'unknown' || tempo === null
          ? ''
          : String(Math.round(tempo * 100) / 100);
    }
    mark(this.#ui.bpm, bpm, 'the master tempo');

    const crossfader = this.#pending.read<number>(
      { of: 'master', field: 'crossfader' },
      master?.crossfader,
    );
    const dimmer = this.#pending.read<number>({ of: 'master', field: 'dimmer' }, master?.dimmer);
    const speedScale = this.#pending.read<number>(
      { of: 'master', field: 'speedScale' },
      master?.speedScale,
    );
    const freeze = this.#pending.read<boolean>({ of: 'master', field: 'freeze' }, master?.freeze);

    // Both faces of each, because the words are written across the bar and the
    // upper one is clipped to the fill: one of them has to read at every value.
    this.#ui.crossfaderValue.textContent = crossfaderLine(crossfader);
    this.#ui.crossfaderValueOn.textContent = this.#ui.crossfaderValue.textContent;
    this.#ui.dimmerValue.textContent = percentLine(dimmer);
    this.#ui.dimmerValueOn.textContent = this.#ui.dimmerValue.textContent;
    this.#ui.speedScaleValue.textContent = speedScaleLine(speedScale);

    // Beside each fader, what the server itself last said. While a fader is
    // pending the two differ, and that difference is the whole point of the line.
    this.#ui.dimmerReported.textContent = reportedLine(dimmer, (value) =>
      percentLine({ state: 'live', value }),
    );
    this.#ui.speedScaleReported.textContent = reportedLine(speedScale, (value) =>
      speedScaleLine({ state: 'live', value }),
    );

    setFader(this.#ui.crossfader, crossfader, this.#holding === 'master.crossfader');
    setFader(this.#ui.dimmer, dimmer, this.#holding === 'master.dimmer');
    // The bar's fill. From the middle for the crossfader, because the middle is
    // where it sits when neither bus has it; from the left for the dimmer,
    // because nought there means off.
    if (this.#holding !== 'master.crossfader') {
      fillFrom(this.#ui.crossfaderFill, valueOf(crossfader) ?? 0, 0.5);
    }
    if (this.#holding !== 'master.dimmer') {
      fillFrom(this.#ui.dimmerFill, valueOf(dimmer) ?? 0, 0);
    }
    setSpeedSteps(this.#speedSteps, speedScale);

    mark(this.#ui.crossfader, crossfader, 'the crossfader');
    mark(this.#ui.dimmer, dimmer, 'the master dimmer');
    mark(this.#ui.speedScale, speedScale, 'the master speed scale');

    // What a patched master fixture holds is shown and not offered while DMX
    // is on (owner, 2026-10-19). The tempo is never held: a tap counts from
    // either side.
    const held = master?.held ?? {
      dimmer: false,
      crossfader: false,
      speedScale: false,
      freeze: false,
    };
    this.#ui.crossfader.disabled = !this.#connected || held.crossfader;
    this.#ui.dimmer.disabled = !this.#connected || held.dimmer;
    for (const [element, isHeld] of [
      [this.#ui.crossfader.parentElement, held.crossfader],
      [this.#ui.dimmer.parentElement, held.dimmer],
      [this.#ui.speedScale, held.speedScale],
    ] as const) {
      if (element instanceof HTMLElement) {
        holdForDesk(element, isHeld);
      }
    }
    for (const step of this.#speedSteps.values()) {
      step.disabled = !this.#connected || held.speedScale;
    }

    const frozen = valueOf(freeze);
    this.#ui.freeze.classList.toggle('on', frozen === true);
    this.#ui.freeze.textContent = frozen === null ? 'FREEZE —' : 'FREEZE';
    // **Dead rather than deceptively alive** when the server has not said
    // whether the output is frozen (owner, 2026-09-23: "Freeze Taste ohne
    // Funktion"). Freezing a show whose state has not arrived would be guessing
    // which way the switch is, so the press was refused with a note — which
    // from the far side of a room is a button that does nothing. A server too
    // old to report it is the case this is really about.
    this.#ui.freeze.disabled = !this.#connected || frozen === null || held.freeze;
    this.#ui.freeze.title =
      frozen === null
        ? 'This server has not said whether the output is frozen, so this cannot be switched.'
        : readingNote(freeze, 'whether the output is frozen');
    mark(this.#ui.freeze, freeze, 'whether the output is frozen');
    if (held.freeze) {
      holdForDesk(this.#ui.freeze, true, '');
    } else {
      this.#ui.freeze.classList.remove('desk');
    }

    // The console's own switch. Lit green while the desk is driving, which is
    // nearly always; red and unmistakable while somebody has taken the show by
    // hand, because that is the state that has to announce itself across a room.
    const dmx = this.#pending.read<boolean>(
      { of: 'master', field: 'dmxEnabled' },
      master?.dmxEnabled,
    );
    const driving = valueOf(dmx);
    this.#ui.dmxEnabled.classList.toggle('on', driving === true);
    this.#ui.dmxEnabled.classList.toggle('off', driving === false);
    mark(this.#ui.dmxEnabled, dmx, 'whether the console is driving this show');
    this.#ui.dmxEnabled.title =
      driving === false
        ? 'Art-Net and sACN are being ignored. The show is being driven from here.'
        : 'The lighting console is driving this show. Switch off to take it by hand.';
  }

  // --- the beat strip and the playhead bars ---------------------------------

  /**
   * Starts or stops the one frame loop, and hides the strip when it has no
   * business animating.
   *
   * Three conditions, and each is a cost somebody pays: the mode has to be on
   * screen, the window has to be one somebody can see, and there has to be a
   * sample to predict from — of the clock, or of a playhead. A metronome running
   * in a background tab is a machine running a show spending frames on a screen
   * nobody is looking at.
   *
   * One loop for both, rather than one each. They are corrected by the same
   * telemetry sample and drawn against the same monotonic clock, and a second
   * `requestAnimationFrame` chain would be a second wake-up per frame for
   * nothing.
   */
  #syncFrames(): void {
    // Hidden with the mode rather than frozen in it: the tempo beside it is
    // state and stays true while Settings is on screen, but a beat that stopped
    // moving would be saying the show had.
    this.#ui.beatStrip.hidden = !this.#visible;

    const predicting = this.#clock !== null || this.#playheads.size > 0;
    if (this.#visible && !document.hidden && predicting && !stillness()) {
      this.#startFrames();
    } else {
      this.#stopFrames();
    }
  }

  #startFrames(): void {
    if (this.#beatFrame !== null) {
      return;
    }
    const step = (): void => {
      // Booked before drawing, because drawing is what discovers that the
      // server has gone quiet and cancels this loop.
      this.#beatFrame = window.requestAnimationFrame(step);
      this.#drawBeat();
      this.#drawPlayheads();
    };
    this.#beatFrame = window.requestAnimationFrame(step);
  }

  #stopFrames(): void {
    if (this.#beatFrame !== null) {
      window.cancelAnimationFrame(this.#beatFrame);
      this.#beatFrame = null;
    }
  }

  /**
   * Draws where the beat is, predicted from the last sample.
   *
   * Called per frame while the strip is running, and once whenever a sample
   * arrives. It rebuilds nothing unless the bar has changed length: what a frame
   * costs is one transform on one mark, and a class when the beat moves on.
   */
  #drawBeat(): void {
    const reading = beatAt(this.#clock, monotonic());
    if (reading.state !== 'live' && this.#clock !== null) {
      // The last sample is too old to predict from. Let it go, and stop: a strip
      // beating on from a sample nothing has confirmed for seconds is this
      // client's own metronome wearing the server's colours.
      this.#clock = null;
      this.#syncFrames();
    }

    const strip = stripView(reading);
    const shape = `${strip.marks}${strip.label === '' ? '' : '+count'}`;
    if (shape !== this.#beatShape) {
      this.#buildBeatMarks(shape, strip.marks, strip.label !== '');
    }
    this.#ui.beatStrip.classList.toggle('unknown', strip.marks === 0);

    if (strip.lit !== this.#beatLit) {
      this.#beatMarks[this.#beatLit]?.classList.remove('on');
      const lit = this.#beatMarks[strip.lit];
      lit?.classList.add('on');
      // The mark the beat has left keeps nothing: a filled mark behind the
      // current one would read as a bar filling up, which is not what this is.
      this.#fill(this.#beatLit, 0);
      this.#beatLit = strip.lit;
    }
    this.#fill(strip.lit, strip.phase);

    if (this.#beatCount !== null && this.#beatCount.textContent !== strip.label) {
      this.#beatCount.textContent = strip.label;
    }

    const said = beatNote(reading);
    if (said !== this.#beatSaid) {
      this.#beatSaid = said;
      this.#ui.beatStrip.title = said;
      this.#ui.beatStrip.setAttribute('aria-label', said);
    }
  }

  /** Rebuilds the marks, which happens when the bar changes length and not per frame. */
  #buildBeatMarks(shape: string, count: number, counted: boolean): void {
    this.#beatShape = shape;
    this.#beatLit = -1;
    this.#beatMarks = [];
    this.#beatCount = null;

    if (count === 0) {
      // An em dash, exactly as an unreported tempo is drawn. Not a row of unlit
      // marks, which would say the clock is somewhere this client cannot see;
      // there is no clock at all.
      this.#ui.beatStrip.replaceChildren(make('span', 'beat-none', '—'));
      return;
    }

    for (let index = 0; index < count; index += 1) {
      const node = make('div', 'beat');
      node.append(make('i', 'beat-fill'));
      // Numbered, like the bar counter on a CDJ (owner, 2026-09-23). A lit
      // mark says the clock is running; a lit *number* says which beat of the
      // bar it is on, which is what tells somebody whether they are with the
      // music or a beat out.
      node.append(make('span', 'beat-number', String(index + 1)));
      this.#beatMarks.push(node);
    }
    if (counted) {
      this.#beatCount = make('span', 'beat-count');
    }
    this.#ui.beatStrip.replaceChildren(
      ...this.#beatMarks,
      ...(this.#beatCount === null ? [] : [this.#beatCount]),
    );
  }

  /** How far through one mark the beat is, as the one thing a frame writes. */
  #fill(index: number, phase: number): void {
    const fill = this.#beatMarks[index]?.firstElementChild;
    if (fill instanceof HTMLElement) {
      fill.style.transform = `scaleX(${phase})`;
    }
  }

  /**
   * Draws every layer's progress bar, predicted from its last sample.
   *
   * Called per frame while the loop is running, and once whenever a sample
   * arrives, so that a correction lands even in a mode where nothing animates.
   * A sample nothing has confirmed for the staleness window is let go here, the
   * way the beat strip lets its own go: a bar still travelling from a sample
   * that old would be this client animating a clip it knows nothing about.
   */
  #drawPlayheads(): void {
    const now = monotonic();
    for (const [layerId, sample] of [...this.#playheads]) {
      if (isStale(sample, now)) {
        this.#playheads.delete(layerId);
      }
    }
    if (this.#playheads.size === 0 && this.#beatFrame !== null) {
      // Nothing left to predict from. The loop may still be owed to the beat
      // strip, which is what decides.
      this.#syncFrames();
    }

    for (const layer of this.#view.layers) {
      const row = this.#rows.get(layer.id);
      if (row !== undefined) {
        this.#drawPlayhead(row, layer, now);
      }
    }
  }

  /**
   * One layer's bar.
   *
   * A layer the server reported no playhead for has no bar: hidden outright,
   * rather than an empty track or one sitting at zero, because "nothing is
   * playing a clip there" and "a clip that has just started" are different
   * pictures and only one of them is a position (`LayerPlayhead`). The same goes
   * for a sample taken before the pad moved, which `playheadAt` throws away.
   *
   * What a frame writes is one transform and, when the second changes, one
   * string. The transform is what keeps the cost off the layout: a bar that
   * animated its width would ask the page to lay out seven rows of controls
   * sixty times a second on a machine that is also running a show.
   */
  #drawPlayhead(row: RowNodes, layer: LayerView, now: number): void {
    const reading = playheadAt(this.#playheads.get(layer.id), layer, now);
    if (reading.state !== 'live') {
      row.playhead.hidden = true;
      return;
    }

    const position = reading.value;
    row.playhead.hidden = false;
    row.playheadFill.style.transform = `scaleX(${position.through})`;
    // Held is the server's own word for it: `speed_ms_per_second` is zero for a
    // clip that has played out and frozen, and a bar that simply stood still
    // would look like a client that had stopped hearing.
    row.playhead.classList.toggle('held', position.speedMsPerSecond === 0);

    const line = playheadLine(reading);
    if (row.playheadTime.textContent !== line) {
      row.playheadTime.textContent = line;
    }
    const said = playheadNote(reading);
    if (row.playhead.title !== said) {
      row.playhead.title = said;
      row.playhead.setAttribute('aria-label', `${layer.name} playhead: ${said}`);
    }
  }

  #drawBanks(): void {
    const banks = slotBanks(this.#view.layers);
    this.#banks = banks;
    if (this.#bank >= banks.length) {
      this.#bank = 0;
    }
    const wanted = banks.map((bank) => bank.label).join(',');
    if (this.#ui.banks.dataset['shape'] !== wanted) {
      this.#ui.banks.dataset['shape'] = wanted;
      this.#ui.banks.replaceChildren(
        ...banks.map((bank, index) =>
          button(
            bank.label,
            () => {
              this.#bank = index;
              this.#render();
            },
            { className: 'bank', title: `Slots ${bank.first} to ${bank.last}` },
          ),
        ),
      );
    }
    [...this.#ui.banks.children].forEach((node, index) => {
      node.classList.toggle('on', index === this.#bank);
    });
  }

  /** The bank currently on screen. Worked out once per redraw, in `#drawBanks`. */
  #currentBank(): SlotBank {
    return this.#banks[this.#bank] ?? this.#banks[0] ?? { first: 1, last: SLOTS_PER_BANK, label: '1–12' };
  }

  #drawMatrix(): void {
    // Top of the list is the top of the composite, which is how an operator
    // reads a stack of layers.
    const ordered = [...this.#view.layers].reverse();
    // The bank is deliberately not part of this: paging is not a reason to
    // rebuild seven rows of controls, and each row's pad strip rebuilds itself
    // when the slots in it change, which paging does.
    const shape = `${ordered.map((layer) => layer.id).join(',')}|${this.#connected}`;

    if (shape !== this.#matrixShape) {
      this.#matrixShape = shape;
      this.#rows.clear();
      this.#ui.matrix.replaceChildren(
        ...ordered.map((layer, position) => this.#buildRow(layer, ordered.length - position)),
      );
      if (ordered.length === 0) {
        this.#ui.matrix.replaceChildren(
          make(
            'p',
            'empty-note',
            this.#connected
              ? 'The open show has no layers. Add one in Settings.'
              : 'Sign in to a server to see its layers.',
          ),
        );
      }
    }

    for (const layer of ordered) {
      this.#updateRow(layer);
    }

    this.#sizeTiles();
  }

  /**
   * Sizes every picture in the matrix, which is one number.
   *
   * The owner, 2026-09-23: *"Es sollte alles in einer Flucht sein, heißt, alles
   * was links ist kleiner, Slots etwas höher/breiter damit alles auf einer
   * Ebene/Linie ist."*
   *
   * So one number sets every height on the row. It is a slot pad's picture; a
   * pad is that and its caption; and the name block, the layer's own picture
   * and the MUTE are each exactly as tall as a whole pad. Everything therefore
   * starts and ends on the same two lines, and the layer's picture is larger
   * than a pad's by the height of one caption — which is the drawing.
   *
   * **The width decides it, and nothing else.** A row is the layer's picture
   * plus a pad per column, all at sixteen by nine, so the width fixes how tall
   * a picture may be. Which is why what is left of the row is kept narrow — a
   * name, three small controls and an upright MUTE — because every pixel the
   * left half gives up is a pixel of height for the whole line.
   *
   * It used to be the lower of that and the row's share of the matrix height,
   * so eight layers got an eighth each and one layer got the lot. Past four
   * layers the height was the lower one, and then the whole desk changed size
   * under an operator who had only added a layer — and a row sized to a height
   * no longer fills the width, which is where the empty space down the right
   * came from. The owner, 2026-09-27: *"wenn ich mehr als 4 Layer habe
   * verändert sich die Größe der Slots und die Layer Höhe, statt einfach gleich
   * zu bleiben und Scrollen zu lassen. Alle Slots sind nach Links gerutscht und
   * rechts ist sau viel Leerer Space entstanden."*
   *
   * So the rows keep their size and the matrix scrolls, which it has always
   * been able to do (`#matrix { overflow-y: auto }`). A pad is one size for a
   * given window width, and adding a layer moves nothing that was already on
   * the screen.
   *
   * Everything is measured off rendered nodes rather than restated here: what a
   * row is besides its picture, and what the left half holds besides it. A
   * change in the stylesheet cannot put this out of step with it.
   */
  #sizeTiles(): void {
    const matrix = this.#ui.matrix;
    const rows = this.#rows.size;
    const width = matrix.clientWidth;
    // Only when something that feeds the answer has moved. This reads the
    // layout back, which forces the browser to finish it, and doing that on
    // every telemetry frame would be sixty reflows a second on a machine that
    // is also running a show.
    //
    // The matrix's height is not in this, because it is no longer part of the
    // answer. Growing the window taller now adds rows to what is on screen
    // rather than making every row taller. A scrollbar arriving or leaving is
    // still caught: that *is* a change in `clientWidth`.
    const asked = `${rows}|${Math.round(width)}`;
    if (rows === 0 || width === 0 || asked === this.#tileFor) {
      return;
    }
    this.#tileFor = asked;

    // Twice: the first pass is solved from a row measured at the old size, the
    // second checks the answer against a row measured at the new one. It
    // settles there, and a third pass would only confirm the second.
    for (let pass = 0; pass < 2; pass += 1) {
      const tile = this.#tileThatFits();
      if (tile === null) {
        return;
      }
      matrix.style.setProperty('--tile-height', `${tile}px`);
    }
  }

  /** The height every picture in a row takes, or null when there is nothing to measure. */
  #tileThatFits(): number | null {
    const first = this.#rows.values().next().value;
    if (first === undefined) {
      return null;
    }
    const root = first.root;
    const status = root.querySelector('.layer-status');
    const pad = first.pads.firstElementChild;
    if (!(status instanceof HTMLElement) || !(pad instanceof HTMLElement)) {
      return null;
    }

    const matrixStyle = getComputedStyle(this.#ui.matrix);
    const now = parseFloat(matrixStyle.getPropertyValue('--tile-height'));
    if (!Number.isFinite(now) || now <= 0) {
      return null;
    }

    // How wide a picture may be. A row is the left half's fixed parts, plus
    // the layer's picture, plus a pad per column. The fixed parts are what is
    // left when the layer's picture is taken off the left half.
    //
    // `columns + 1` pictures across, and one caption's worth of width on top of
    // that: the layer's picture is a whole pad tall, so it is wider than a pad
    // by the caption at sixteen by nine. Taking that off first is what leaves
    // an equation in one unknown.
    const rootStyle = getComputedStyle(root);
    const inner =
      root.clientWidth -
      (parseFloat(rootStyle.paddingLeft) || 0) -
      (parseFloat(rootStyle.paddingRight) || 0);
    // A pad's caption, measured on a pad rather than restated here.
    const caption = Math.max(0, pad.getBoundingClientRect().height - now);
    const beside = status.getBoundingClientRect().width - ((now + caption) * 16) / 9;
    const slots = getComputedStyle(first.pads);
    const columns = slots.gridTemplateColumns.split(' ').filter((each) => each !== '').length;
    const padGap = parseFloat(slots.columnGap) || 0;
    const budget =
      inner -
      beside -
      (caption * 16) / 9 -
      (parseFloat(rootStyle.columnGap) || 0) -
      padGap * (columns - 1);
    const byWidth = ((budget / (columns + 1)) * 9) / 16;

    return Math.max(MIN_TILE_HEIGHT, Math.min(MAX_TILE_HEIGHT, Math.floor(byWidth)));
  }

  #buildRow(layer: LayerView, number: number): HTMLElement {
    const root = make('div', 'layer');

    const status = make('div', 'layer-status');

    // Where the layer sits in the composite, counting from the bottom — which
    // is what `Layer::order` means — with the layer's own name under it, as the
    // owner drew it on 2026-09-23.
    //
    // The name had been taken out and left in the tooltip, on the grounds that
    // the picture beside it says what the layer is showing. It does, but that
    // is not the same question: which layer this *is* is what somebody reaching
    // for a fader needs, and on a desk of eight rows a name is how a row is
    // found. The number alone was a twenty-pixel button.
    const name = make('button', 'layer-name');
    name.type = 'button';
    const nameNumber = make('span', 'layer-name-number', String(number));
    const nameText = make('span', 'layer-name-text');
    name.append(nameNumber, nameText);
    name.title = `${layer.name} — composites ${
      number === 1 ? 'first, at the bottom' : ordinal(number)
    }. Press to set the layer up in OPTIONS.`;
    name.addEventListener('click', () => {
      // The layer rather than one of its slots, which is the other thing
      // OPTIONS can be about (owner, 2026-09-23). Pressing the layer again
      // lets go of it, so there is a way back to nothing selected.
      //
      // Either way no slot name is lit afterwards: the subject is the layer,
      // and a pad still glowing would be the screen naming two subjects.
      const same = this.#selected === layer.id && this.#selectedSlot === null;
      this.#selected = same ? null : layer.id;
      this.#selectedSlot = null;
      if (!same) {
        this.#work = chooseTab(this.#work, 'options').area;
      }
      this.#render();
    });
    // Out of the composite in one press, and back in the same way. It is the
    // control OPTIONS has as "In the composition", on the row where it is
    // reached for: taking one layer out to see what is under it is done while
    // watching the wall, not in a panel about the layer.
    //
    // Upright, in the narrow gap between the controls and the pads, because
    // that is where the owner drew it and because a row has no width to spare.
    const mute = button('MUTE', () => this.#toggleEnabled(layer.id), {
      className: 'layer-mute',
    });

    // A/B/M, side by side under the blend mode as the drawing has them. A live
    // control, and deliberately not part of the standard Layer Fixture.
    const routes = make('div', 'routes');
    const routeButtons = new Map<Route, HTMLButtonElement>();
    for (const route of ROUTES) {
      const node = button(
        route,
        () => {
          const current = this.#layer(layer.id);
          if (current === null) {
            return;
          }
          const command = layerCommand(current, { route });
          if (command !== null) {
            void this.#send(command, { of: 'layer', layerId: layer.id, field: 'route' });
          }
        },
        { className: 'route', title: `Feed bus ${route}` },
      );
      routeButtons.set(route, node);
      routes.append(node);
    }

    // There was a rate readout here — FREE, or the beat count with the show's
    // tempo beside it — from an earlier version of this row. It was four nodes
    // built on every row, appended to nothing, and `RowNodes` never carried
    // them, so nothing could have written to them either. Gone rather than
    // wired: the row the owner settled on has no room it, and code that looks
    // like a working readout is how a dead control survives being reported.
    //
    // What the layer is showing, which is the question the whole screen exists
    // to answer at a glance.
    const showingBlock = make('div', 'showing');
    const picture = make('div', 'showing-picture');

    // The layer's picture is where an effect for the *whole layer* lands
    // (owner, 2026-09-23). A source dropped here is refused rather than
    // quietly treated as an effect: a clip belongs in a slot, and this is not
    // one.
    picture.addEventListener('dragover', (event) => {
      if (this.#dragging?.of !== 'effect') {
        return;
      }
      event.preventDefault();
      if (event.dataTransfer !== null) {
        event.dataTransfer.dropEffect = 'copy';
      }
      picture.classList.add('drop-target');
    });
    picture.addEventListener('dragleave', () => picture.classList.remove('drop-target'));
    picture.addEventListener('drop', (event) => {
      event.preventDefault();
      picture.classList.remove('drop-target');
      const carried = this.#dragging;
      if (carried === null) {
        return;
      }
      if (carried.of !== 'effect') {
        this.#note('A clip goes in a slot. Drop it on one of the pads.');
        return;
      }
      void this.#addEffect(layer.id, null, carried);
    });

    const showing = make('span', 'showing-name');
    const playback = make('span', 'showing-mode');

    // Where the clip has got to, under what it is. Built empty and hidden: a
    // layer the server has not reported a playhead for gets no bar at all, and
    // an empty track would say there is a clip sitting at its beginning.
    const playhead = make('div', 'playhead');
    playhead.hidden = true;
    const playheadTrack = make('div', 'playhead-track');
    const playheadFill = make('i', 'playhead-fill');
    playheadTrack.append(playheadFill);
    const playheadTime = make('span', 'playhead-time');
    playhead.append(playheadTrack, playheadTime);

    showingBlock.append(picture, showing, playback, playhead);

    const controls = make('div', 'layer-controls');

    // No transport (owner, 2026-09-23). Next, previous and BLK are three
    // buttons for things the pads already do — slot 0 is the first pad in every
    // bank — and re-selecting the playing clip *is* the retrigger, which is
    // what an operator reaches for anyway.
    // A bar with its own name written in it, rather than a track with a label
    // beside it and a number after it (owner, 2026-09-23). Three things on one
    // line cost three times the width of one, and the width of this column is
    // height for every picture on the row.
    //
    // It is two faces of the same text, one over the other: the lower one light
    // on the track, the upper one dark on the orange and clipped to how far the
    // value has gone. So the caption reads at every setting, including nought,
    // where a single dark caption would be dark text on a dark bar.
    //
    // The control underneath is still a real `input[type=range]`, transparent
    // and over the whole bar: it is what makes the thing draggable, focusable
    // and arrow-key-able, and what a screen reader finds.
    const opacityRow = make('label', 'layer-opacity');
    const opacityValues: HTMLElement[] = [];
    const faces = ['layer-opacity-face', 'layer-opacity-face on'].map((className) => {
      const face = make('span', className);
      const value = make('span', 'layer-opacity-value');
      opacityValues.push(value);
      face.append(make('span', 'layer-opacity-caption', 'OPACITY'), value);
      return face;
    });
    const opacityFill = faces[1]!;
    const opacity = make('input');
    opacity.type = 'range';
    opacity.min = '0';
    opacity.max = String(FADER_STEPS);
    opacity.setAttribute('aria-label', `${layer.name} opacity`);
    const opacityKey = `layer.${layer.id}.opacity`;
    opacity.addEventListener('pointerdown', () => {
      this.#holding = opacityKey;
    });
    opacity.addEventListener('pointerup', () => {
      this.#holding = null;
    });
    opacity.addEventListener('input', () => {
      const value = Number(opacity.value) / FADER_STEPS;
      paintOpacity(opacityFill, opacityValues, { state: 'live', value });
      const current = this.#layer(layer.id);
      if (current === null) {
        return;
      }
      const command = layerCommand(current, { opacity: value });
      if (command !== null) {
        this.#sendLatest(opacityKey, () =>
          this.#send(command, { of: 'layer', layerId: layer.id, field: 'opacity' }),
        );
      }
    });
    opacityRow.append(faces[0]!, opacityFill, opacity);

    // How this layer composites onto its bus. It was in the show editor,
    // which is the wrong place: a blend mode is something an operator changes
    // while looking at the wall (owner, 2026-09-23).
    const blend = make('select', 'layer-blend');
    blend.setAttribute('aria-label', `${layer.name} blend mode`);
    for (const mode of BLEND_MODES) {
      const option = make('option', undefined, BLEND_MODE_LABELS[mode] ?? mode);
      option.value = mode;
      blend.append(option);
    }
    blend.addEventListener('change', () => {
      const current = this.#layer(layer.id);
      if (current === null) {
        return;
      }
      const command = layerCommand(current, { blendMode: blend.value as BlendMode });
      if (command !== null) {
        void this.#send(command, { of: 'layer', layerId: layer.id, field: 'blendMode' });
      }
    });

    // Opacity, then how it composites, then which bus it composites onto: the
    // three questions in the order the owner's drawing asks them.
    controls.append(opacityRow, blend, routes);
    const body = make('div', 'status-body');
    body.append(name, showingBlock, controls, mute);
    status.append(body);

    const pads = make('div', 'slots');
    root.append(status, pads);

    this.#rows.set(layer.id, {
      root,
      name,
      nameText,
      mute,
      routes: routeButtons,
      blend,
      opacity,
      opacityBar: opacityRow,
      opacityFill,
      opacityValues,
      picture,
      playhead,
      playheadFill,
      playheadTime,
      pads,
      padNodes: new Map(),
      padShape: '',
    });
    return root;
  }

  #updateRow(layer: LayerView): void {
    const row = this.#rows.get(layer.id);
    if (row === undefined) {
      return;
    }

    row.root.classList.toggle('disabled', !layer.enabled);
    // **One subject anywhere in the matrix** (owner, 2026-09-23): a slot name
    // or a layer, never both. A row that stayed lit under a chosen slot was
    // the screen naming two things and OPTIONS talking about one of them.
    row.root.classList.toggle(
      'selected',
      this.#selected === layer.id && this.#selectedSlot === null,
    );
    // The name, under the number, in a block wide enough to hold one. It is
    // written every update rather than at build time because a layer renamed
    // in Settings does not rebuild this row.
    row.nameText.textContent = layer.name;
    row.name.title = `${layer.name} — its live state. Its order and slots are the show’s, and are edited in Settings.`;

    // Muted is the layer being out of the composite, which is live: the
    // show keeps its own value and the layer opens on that again.
    const inComposite = this.#pending.read<boolean>(
      { of: 'layer', layerId: layer.id, field: 'enabled' },
      layer.enabled,
    );
    const contributing = valueOf(inComposite) ?? true;
    row.mute.classList.toggle('on', !contributing);
    row.mute.disabled = !this.#connected;
    mark(row.mute, inComposite, 'whether this layer is in the composition');
    row.mute.title = contributing
      ? 'Take this layer out of the composite. The show keeps its own value.'
      : 'Muted: this layer contributes nothing. Press to put it back.';

    const route = this.#pending.read<Route>(
      { of: 'layer', layerId: layer.id, field: 'route' },
      layer.route,
    );
    const chosen = valueOf(route);
    const routeDiffers = chosen !== null && chosen !== layer.defaults.route;
    for (const [name, node] of row.routes) {
      const on = chosen === name;
      node.classList.toggle('on', on);
      node.classList.toggle('pending', on && route.state === 'pending');
      node.classList.toggle('refused', on && route.state === 'refused');
      node.classList.toggle('differs', on && routeDiffers);
      node.title = on
        ? `${readingNote(route, 'this layer’s bus')} The show opens it on ${layer.defaults.route}.`
        : `Feed bus ${name}`;
    }

    // How it composites, which is a live value now and takes effect at once.
    const blend = this.#pending.read<BlendMode>(
      { of: 'layer', layerId: layer.id, field: 'blendMode' },
      layer.blendMode,
    );
    const mode = valueOf(blend);
    if (document.activeElement !== row.blend && mode !== null) {
      row.blend.value = mode;
    }
    mark(row.blend, blend, 'how this layer composites');

    const opacity = this.#pending.read<number>(
      { of: 'layer', layerId: layer.id, field: 'opacity' },
      layer.opacity,
    );
    const held = this.#holding === `layer.${layer.id}.opacity`;
    setFader(row.opacity, opacity, held);
    if (!held) {
      paintOpacity(row.opacityFill, row.opacityValues, opacity);
    }
    const opacityDiffers = valueOf(opacity) !== null && valueOf(opacity) !== layer.defaults.opacity;
    for (const node of row.opacityValues) {
      node.classList.toggle('differs', opacityDiffers);
    }
    // On the bar rather than on the input, which is transparent and over the
    // whole of it: the border that shows pending and refused is the bar's.
    mark(row.opacityBar, opacity, 'this layer’s opacity');
    row.opacity.disabled = !this.#connected || layer.held.opacity;
    holdForDesk(
      row.opacityBar,
      layer.held.opacity,
      `${readingNote(opacity, 'this layer’s opacity')} The show opens it at ${Math.round(
        layer.defaults.opacity * 100,
      )}%.`,
    );
    // The pads as a strip: a desk holds the choosing, not any one pad.
    holdForDesk(row.pads, layer.held.slot);

    this.#drawShowing(row, layer);
    // A row that has just been rebuilt has a bar with nothing in it, and the
    // next animation frame may not come at all — this may not be the mode on
    // screen. So the bar is drawn with the row rather than only by the loop.
    this.#drawPlayhead(row, layer, monotonic());

    this.#drawPads(row, layer);
  }

  /**
   * What the layer is playing, as a picture and nothing else.
   *
   * The name of the clip, the number of the slot and what its end does were all
   * written under this thumbnail, and all three are things the thumbnail and the
   * lit pad beside it already say (owner, 2026-09-23). What is left in words is
   * the title, for somebody who wants the detail and can hover for it, and the
   * one state a picture genuinely cannot show: a slot selected and waiting for
   * a boundary, which is drawn on the picture's own edge.
   */
  #drawShowing(row: RowNodes, layer: LayerView): void {
    const slot = layer.slots.find((candidate) => candidate.index === layer.activeSlot) ?? null;
    if (layer.activeSlot === BLACK_SLOT_INDEX) {
      row.picture.replaceChildren();
      row.picture.className = 'showing-picture black';
    } else {
      row.picture.className = 'showing-picture';
      this.#paintSource(row.picture, slot);
    }
    row.picture.title =
      layer.activeSlot === BLACK_SLOT_INDEX
        ? 'Slot 0 is permanently black on every layer.'
        : slot === null
          ? `The server says this layer is on slot ${layer.activeSlot}, which the open show has nothing in.`
          : this.#slotNote(slot);
  }

  /** One slot, for a title: which slot, what is in it, what its end does. */
  #slotNote(slot: SlotView): string {
    const ends = playbackLabel(slot.playbackMode, slot.direction);
    return `Slot ${slot.index} — ${this.#sourceName(slot)} — ${ends}`;
  }

  #drawPads(row: RowNodes, layer: LayerView): void {
    const bank = this.#currentBank();
    const pads = padsInBank(layer, bank);
    const shape = pads.map((pad) => `${pad.index}:${pad.empty ? '' : sourceKey(pad.slot)}`).join(',');

    if (shape !== row.padShape) {
      row.padShape = shape;
      row.padNodes.clear();
      row.pads.replaceChildren(...pads.map((pad) => this.#buildPad(layer, pad, row)));
    }

    const slotReading = this.#pending.read<number>({ of: 'slot', layerId: layer.id }, layer.activeSlot);
    for (const pad of pads) {
      const nodes = row.padNodes.get(pad.index);
      if (nodes === undefined) {
        continue;
      }
      nodes.root.classList.toggle('active', pad.active);
      // The name lights for the slot OPTIONS is about, which is one pad in the
      // whole matrix — never one per layer.
      nodes.root.classList.toggle(
        'chosen',
        this.#selected === layer.id && this.#selectedSlot === pad.index,
      );
      nodes.root.classList.toggle(
        'asked',
        slotReading.state === 'pending' && valueOf(slotReading) === pad.index,
      );
      nodes.root.classList.toggle(
        'refused',
        slotReading.state === 'refused' && valueOf(slotReading) === pad.index,
      );
      const armed =
        this.#replacing !== null &&
        this.#replacing.layerId === layer.id &&
        this.#replacing.slotIndex === pad.index;
      nodes.root.classList.toggle('confirming', armed);
      this.#paintPad(nodes, pad, armed ? this.#replacing : null);
    }
  }

  #buildPad(layer: LayerView, pad: PadView, row: RowNodes): HTMLButtonElement {
    const node = make('button', 'pad');
    node.type = 'button';
    node.classList.toggle('black', pad.black);
    node.classList.toggle('empty', pad.empty);

    const picture = make('div', 'pad-picture');
    const label = make('span', 'pad-label');
    node.append(picture, label);

    const nextBank = 'Right-click for the next bank.';
    const selects = pad.empty ? '' : ' Playing it sets it up in OPTIONS.';
    node.title = pad.empty
      ? `${layer.name}, slot ${pad.index} — empty, so playing it shows nothing. Drag a clip from the browser onto it. ${nextBank}`
      : `${layer.name}, slot ${pad.index}.${selects} ${nextBank}`;

    // **The two halves of a pad say two different things** (owner,
    // 2026-09-23). The picture lights when the slot is actually on stage; the
    // name lights when the slot is the one OPTIONS is about. They are usually
    // the same slot, because playing one chooses it, but they part company the
    // moment an operator looks at a slot that is not playing.
    //
    // The name was a link with an underline on hover, which is the wrong idea:
    // it is a state to read rather than a thing to press. It still answers a
    // press — that is the only way to set up a slot without putting it on stage
    // — but it no longer advertises it.
    if (!pad.empty) {
      label.title = `Set up slot ${pad.index} in OPTIONS without playing it`;
      label.addEventListener('click', (event) => {
        // Not the pad's click as well, which would play it.
        event.stopPropagation();
        this.#cancelReplace();
        this.#chooseSlot(layer.id, pad.index);
      });
    }

    // One handler for every pad rather than one for the ones that can be
    // triggered, because a pad that is asking a question has to answer it
    // whether or not there is anything in it to play.
    node.addEventListener('click', () => {
      const armed = this.#replacing;
      if (armed !== null && armed.layerId === layer.id && armed.slotIndex === pad.index) {
        void this.#assign(armed.layerId, armed.slotIndex, armed.source);
        return;
      }
      // Any other pad cancels the question. An operator who reaches past it has
      // decided not to answer it, and leaving it armed would make the next
      // press mean something they did not ask for.
      this.#cancelReplace();
      // **An empty pad is black** (owner, 2026-09-23). Pressing one takes the
      // layer off stage, which is what the Black pad used to be for and what
      // an empty pad says by being empty. It was ignored, so the one gesture
      // that clears a layer needed a pad of its own.
      this.#trigger(layer.id, pad.index);
    });

    // Right-click asks for the next bank, rather than waiting for this one to
    // fill (owner, 2026-09-23). On the matrix because that is where an
    // operator notices they have run out of pads.
    node.addEventListener('contextmenu', (event) => {
      event.preventDefault();
      void this.#addBank();
    });

    {
      node.addEventListener('dragover', (event) => {
        if (this.#dragging === null) {
          return;
        }
        event.preventDefault();
        if (event.dataTransfer !== null) {
          event.dataTransfer.dropEffect = 'copy';
        }
        node.classList.add('drop-target');
      });
      node.addEventListener('dragleave', () => node.classList.remove('drop-target'));
      node.addEventListener('drop', (event) => {
        event.preventDefault();
        node.classList.remove('drop-target');
        this.#dropOn(layer, pad.index);
      });
    }

    row.padNodes.set(pad.index, { root: node, picture, label });
    return node;
  }

  #paintPad(nodes: PadNodes, pad: PadView, armed: ArmedDrop | null): void {
    if (armed !== null) {
      // The question, on the pad it is about. The picture stays: what is at
      // risk is what the operator can see, which is the point of asking.
      nodes.label.textContent = 'REPLACE?';
      nodes.root.title =
        `Slot ${pad.index} plays ${armed.replacing}. ` +
        'Press this pad again to replace it, or press anything else to leave it alone.';
      nodes.picture.className = 'pad-picture';
      this.#paintSource(nodes.picture, pad.slot);
      return;
    }
    nodes.picture.className = 'pad-picture';
    nodes.label.textContent =
      pad.slot === null ? String(pad.index) : `${pad.index} · ${this.#sourceName(pad.slot)}`;
    this.#paintSource(nodes.picture, pad.slot);
  }

  /**
   * A slot's picture.
   *
   * A generator's is the one shipped with this client, the same file its tile
   * in the browser carries: an operator scanning a bank sees what each pad
   * draws rather than reading ten short words. A media item whose picture the
   * server has not sent — one registered before thumbnails existed, or one
   * nothing could be read from — is left empty, and the name under it is what
   * the operator was going to read anyway. A broken image is not, and a
   * placeholder that looks like a fault is worse.
   */
  #paintSource(target: HTMLElement, slot: SlotView | null): void {
    if (slot === null) {
      target.replaceChildren();
      return;
    }
    if (slot.source.kind === 'generator') {
      paintModulePicture(target, 'generators', slot.source, this.#sourceName(slot), 'pad-generator');
      return;
    }
    if (slot.source.kind === 'ndi') {
      // The mark, as on the tile this pad was dragged from. A stream has no
      // thumbnail to draw and no strip to scrub — what it looks like is
      // whatever is being sent this second, and receiving it here to fill a
      // pad would put a second receiver on the network for every sender
      // nobody is watching. So the picture says what kind of thing this is
      // and the label underneath says which one.
      paintNdiMark(target);
      return;
    }
    const item = slot.source.mediaItemId;
    const strip = this.#strips.get(item) ?? null;
    if (strip !== null) {
      this.#paintStrip(target, strip);
      return;
    }
    const thumbnail = this.#thumbnails.get(item) ?? null;
    if (thumbnail === null) {
      target.replaceChildren();
      return;
    }
    const existing = target.firstElementChild;
    if (existing instanceof HTMLImageElement && existing.src === thumbnail) {
      return;
    }
    const picture = make('img');
    picture.src = thumbnail;
    picture.alt = '';
    target.replaceChildren(picture);
  }

  /**
   * Draws a clip's moving preview into a box, showing its first frame.
   *
   * **A strip rather than an animated image** (owner, 2026-09-23). What was
   * asked for is a preview that plays while the pointer is over a pad and shows
   * its first frame otherwise, and an animated image cannot be stopped on a
   * frame: a browser plays one whenever it is on screen. Ten frames in one
   * picture can — the box shows a tenth of it at a time, and stepping through
   * those tenths is what playing means here.
   *
   * It is a background rather than an `img` for the same reason: which part of
   * a picture is showing is a thing CSS can say and animate, and a frame is a
   * whole number of steps rather than a position that drifts.
   */
  #paintStrip(target: HTMLElement, strip: string): void {
    const url = `url("${strip}")`;
    const existing = target.firstElementChild;
    if (existing instanceof HTMLElement && existing.classList.contains('strip')) {
      if (existing.style.backgroundImage !== url) {
        existing.style.backgroundImage = url;
      }
      return;
    }
    const picture = make('span', 'strip');
    picture.style.backgroundImage = url;
    target.replaceChildren(picture);
  }

  /**
   * What a slot is called, in the operator's words.
   *
   * **The name they gave this pad, where they gave it one.** It wins over what
   * the source is called, which is the whole point of it: two pads of one clip
   * with different effects on them are the same four words otherwise (owner,
   * 2026-10-15). Nothing else is affected — the library's name for the clip is
   * still the library's, and so is the generator's.
   */
  #sourceName(slot: SlotView): string {
    const given = slot.name?.trim() ?? '';
    return given === '' ? this.#sourceOwnName(slot) : given;
  }

  /** What a slot would be called with no name of its own: what it plays. */
  #sourceOwnName(slot: SlotView): string {
    if (slot.source.kind === 'generator') {
      return slot.source.typeId.replace(/^ovs\./, '');
    }
    if (slot.source.kind === 'ndi') {
      // The sender's half of `MACHINE (Source Name)`, which is what tells two
      // streams from one machine apart.
      return slot.source.name.replace(/^[^(]*\(/, '').replace(/\)$/, '') || slot.source.name;
    }
    // The library's own display name where this client has it. Never a path and
    // never a storage key, because neither is ever sent.
    return this.#names.get(slot.source.mediaItemId) ?? `Slot ${slot.index}`;
  }

  // --- the contextual work area ---------------------------------------------
  //
  // The drawing's two tab strips, and what each tab opens. Which of the three
  // kinds each tab is — a view, live control, or a way into Settings — is decided
  // in `shared/work.ts` and printed at the head of whatever it opens, because a
  // live screen whose tabs are named after configuration has to answer "does
  // this write to my show?" before anybody has to ask it.

  /** Both rails, built once: they are fixed lists, not something the server sends. */
  #buildTabs(): void {
    // Two rails, on opposite sides of the panel (owner, 2026-09-23): OPTIONS
    // alone on the left, and the four browsers as a 2x2 on the right. They are
    // two different jobs — editing what is selected, and finding a thing to
    // drag onto a pad — and putting them on one rail made the panel's left edge
    // a list of five things with no shape to it.
    const open = (tab: TabDefinition<WorkTab>): HTMLButtonElement =>
      this.#tabButton(tab, () => {
        this.#work = chooseTab(this.#work, tab.id).area;
        this.#render();
      });

    this.#ui.workTabs.replaceChildren(
      ...WORK_TABS.filter((tab) => tab.id === 'options').map(open),
    );
    this.#ui.browserTabs.replaceChildren(
      ...BROWSER_TABS.map((id) => {
        const tab = WORK_TABS.find((each) => each.id === id);
        if (tab === undefined) {
          throw new Error(`no tab definition for ${id}`);
        }
        return open(tab);
      }),
    );
  }

  #tabButton<Id extends string>(tab: TabDefinition<Id>, onClick: () => void): HTMLButtonElement {
    const node = button(tab.label, onClick, { className: 'tab', title: tab.says });
    // Which tab this is, for the stylesheet and for finding it again. By name
    // rather than by position: the rail is not in the order the list is.
    node.dataset['tab'] = tab.id;
    return node;
  }

  #drawWork(): void {
    // By name rather than by position: there are two rails and neither is in
    // the order the list is.
    for (const rail of [this.#ui.workTabs, this.#ui.browserTabs]) {
      for (const node of rail.children) {
        node.classList.toggle(
          'on',
          node instanceof HTMLElement && node.dataset['tab'] === this.#work.tab,
        );
      }
    }

    this.#drawTool();
  }

  // --- the work panel: BROWSER, GENERATOR, OPTIONS, EFFECTS -----------------

  #drawTool(): void {
    const tab = WORK_TABS.find((candidate) => candidate.id === this.#work.tab) ?? WORK_TABS[0];
    if (tab === undefined) {
      return;
    }
    this.#ui.workTitle.textContent = tab.label;
    this.#ui.workKind.textContent = {
      view: 'a view',
      live: 'live control',
      'into-edit': 'shows, and opens Settings',
      assigns: 'fills slots',
    }[tab.kind];
    this.#ui.workKind.className = `tab-kind ${tab.kind === 'live' ? 'live' : ''}`;
    // **No line of prose under any of these titles** (owner, 2026-09-23): "Die
    // nehmen unnötig Platz weg", and then the same of OPTIONS's. What a tab is,
    // is on the tab and on the title, and what OPTIONS is about is written over
    // its controls in the subject row rather than described above them.
    this.#ui.workSays.textContent = '';
    this.#ui.workSays.hidden = true;

    // The two tabs with controls in them are built once and updated in place —
    // rebuilding one while an operator has a fader under their finger would take
    // the fader out from under them, exactly as it would in a layer row.
    if (tab.id === 'options') {
      this.#effects = null;
      // The browser's search box goes with it: it belongs to a browser, and a
      // box saying "Search the clips" over a panel of faders is a control that
      // does nothing to what is under it.
      this.#browser?.search.remove();
      this.#browser = null;
      this.#drawOptions();
      return;
    }
    // The four browsers. Built once and filled in place, because each has a
    // search box in it: rebuilding the tool on every sample would take the box
    // out from under an operator who is typing into it.
    this.#options = null;
    this.#effects = null;
    // **What OPTIONS was built for is forgotten when a browser takes the
    // panel** (owner, 2026-09-23): "Wenn ich auf INPUTS klicke und dann wieder
    // auf Options, dann werden keine Options angzeigt sondern weiterhin die
    // INPUTS."
    //
    // The shape is what stops OPTIONS rebuilding its controls under an
    // operator's finger every sample, and it was comparing against a panel that
    // no longer held them: the browser had replaced the children and OPTIONS,
    // finding nothing changed, put nothing back.
    this.#optionsShape = '';
    if (this.#browser === null) {
      this.#browser = this.#buildBrowser();
      this.#ui.workTool.replaceChildren(this.#browser.path, this.#browser.note, this.#browser.grid);
    }
    // The search goes on the title line, which is the panel's head rather than
    // its tool. Moved rather than rebuilt: it holds what somebody typed.
    if (this.#browser.search.parentElement !== this.#ui.workHead) {
      this.#ui.workHead.append(this.#browser.search);
    }
    this.#drawBrowser();
  }

  /**
   * BROWSER — where a clip is put in a slot.
   *
   * Icons rather than rows: a picture, the name under it, and a search over
   * the lot (owner, 2026-09-23). A longer hover gives the full title and what
   * the server measured, which is the detail that used to be printed under
   * every row whether anybody wanted it or not.
   *
   * Dragging one onto a pad assigns it. That is a show edit, and it is the
   * one this screen makes: `slot_routes` exists so that it can be made without
   * reloading the show. An occupied pad asks first.
   *
   * Built once and updated in place, unlike the old read-only list: an
   * operator typing into the search would otherwise lose the box under their
   * fingers on the next sample.
   */
  #buildBrowser(): BrowserNodes {
    const search = make('input', 'browser-search');
    search.type = 'search';
    search.placeholder = 'Search the library';
    search.value = this.#librarySearch;
    search.addEventListener('input', () => {
      this.#librarySearch = search.value;
      this.#drawBrowser();
    });

    const grid = make('div', 'browser-grid');
    const note = make('p', 'empty-note');
    const path = make('nav', 'browse-path browser-path');
    path.setAttribute('aria-label', 'Which folder this is');
    return { search, path, grid, note, shape: '', tiles: new Map() };
  }

  /**
   * Opens one folder of the library in the MEDIA browser.
   *
   * The search goes with it, for the reason it does on the MEDIA screen:
   * somebody who picked a folder out of the results asked for its contents.
   */
  #browseInto(folder: string): void {
    this.#browserFolder = folder;
    this.#librarySearch = '';
    if (this.#browser !== null) {
      this.#browser.search.value = '';
    }
    this.#drawBrowser();
  }

  /** Where the MEDIA browser is, as a path back up. */
  #drawBrowserPath(path: HTMLElement, crumbs: readonly Crumb[], searching: boolean): void {
    const parts: HTMLElement[] = [];
    crumbs.forEach((crumb, index) => {
      if (index > 0) {
        parts.push(make('span', 'browse-path-gap', '›'));
      }
      parts.push(
        index === crumbs.length - 1 && !searching
          ? make('span', 'browse-path-here', crumb.name)
          : button(crumb.name, () => this.#browseInto(crumb.path), {
              className: 'browse-path-step',
              title: `Open ${crumb.name}`,
            }),
      );
    });
    if (searching) {
      parts.push(
        make('span', 'browse-path-gap', '›'),
        make('span', 'browse-path-here', 'every folder'),
      );
    }
    path.replaceChildren(...parts);
  }

  /** One folder of the library as a tile: pressed, it opens. */
  #folderTile(folder: FolderTile | { path: string; up: true }, searching: boolean): HTMLElement {
    const up = 'up' in folder;
    const tile = button('', () => this.#browseInto(folder.path), {
      className: up ? 'folder-tile browser-folder up' : 'folder-tile browser-folder',
      title: up ? `Back up to ${whereLabel(folder.path)}` : `Open ${whereLabel(folder.path)}`,
    });
    const glyph = make('div', 'folder-glyph-frame');
    glyph.append(make('span', up ? 'folder-glyph up' : 'folder-glyph'));
    tile.append(glyph, make('span', 'folder-tile-name', up ? '..' : folder.name));
    tile.append(
      make(
        'span',
        'folder-tile-line',
        up
          ? whereLabel(folder.path)
          : searching
            ? `in ${folder.where}`
            : folder.count === 0
              ? 'empty'
              : `${folder.count} item${folder.count === 1 ? '' : 's'}`,
      ),
    );
    return tile;
  }

  /**
   * One browser, showing the one kind of thing its tab is named after.
   *
   * It was one tab holding clips, generators and effects under three headings
   * (owner, 2026-09-23). Split, a search in MEDIA searches the media — which
   * is what somebody typing into it meant — and GENERATORS is a list of six
   * rather than six things at the bottom of four hundred clips.
   *
   * INPUTS is empty on purpose and says so. Capture cards, NDI and a page are
   * not in this build, and a tile for one that could not be dragged anywhere
   * would be a promise the renderer cannot keep.
   */
  #drawBrowser(): void {
    const browser = this.#browser;
    if (browser === null) {
      return;
    }
    const kind = this.#work.tab;

    if (!this.#connected) {
      browser.note.textContent = 'Sign in to a server to see what it holds.';
      browser.note.hidden = false;
      browser.search.hidden = true;
      browser.grid.replaceChildren();
      browser.tiles.clear();
      browser.shape = '';
      return;
    }

    browser.search.hidden = false;
    browser.search.placeholder =
      kind === 'media'
        ? 'Search folders and clips'
        : kind === 'generators'
          ? 'Search the generators'
          : kind === 'inputs'
            ? 'Search the senders'
            : 'Search the effects';

    const inputs =
      kind === 'inputs' ? matchingNdi(ndiTiles(this.#ndiSources), this.#librarySearch) : [];
    // **One folder at a time, as folders** (owner, 2026-10-20: "Bei Live
    // wurden einfach alle Videos im Fenster angezeigt"). It was every clip in
    // the library in one grid, and a search over names only — so a folder
    // could not be found, and two clips of one name could not be told apart.
    const library =
      kind === 'media'
        ? browse(
            libraryLines(this.#library, this.#view.layers),
            (line) => line.item,
            this.#libraryFolders,
            this.#browserFolder,
            this.#librarySearch,
          )
        : null;
    if (library !== null) {
      this.#browserFolder = library.at;
    }
    const clips = library?.items ?? [];
    const folders = library?.folders ?? [];
    const up = library !== null && !library.searching ? library.up : null;
    const searching = library?.searching ?? false;
    browser.path.hidden = library === null;
    if (library !== null) {
      this.#drawBrowserPath(browser.path, library.crumbs, searching);
    }
    const generators =
      kind === 'generators'
        ? matchingGenerators(generatorTiles(this.#catalogue), this.#librarySearch)
        : [];
    const effects =
      kind === 'effects' ? matchingEffects(effectTiles(this.#catalogue), this.#librarySearch) : [];

    const typed = this.#librarySearch.trim();
    const empty =
      clips.length === 0 &&
      folders.length === 0 &&
      generators.length === 0 &&
      effects.length === 0 &&
      inputs.length === 0;
    browser.note.hidden = !empty;
    browser.note.textContent =
      typed !== ''
        ? kind === 'media'
          ? `Nothing in any folder matches “${typed}”.`
          : `Nothing matches “${typed}”.`
        : kind === 'media'
          ? this.#library.length === 0
            ? 'This server holds no media yet. Import some in MEDIA.'
            : 'Nothing is filed in this folder yet. Files are put in folders in MEDIA.'
          : kind === 'inputs'
            ? 'No NDI\u00AE senders on this network.'
            : `This server publishes no ${kind}.`;

    // What the grid is made of, as one string. Which things are in it and in
    // what order — not what any of them is doing, because that changes with
    // every sample and rebuilding on it would take a tile out from under
    // somebody's pointer mid-drag.
    const shape = [
      kind,
      library === null ? '' : `${library.at}:${searching ? 'search' : 'folder'}:${up ?? ''}`,
      folders.map((folder) => `${folder.path}=${folder.count}`).join(','),
      clips.map((line) => line.item.id).join(','),
      generators.map((tile) => tile.typeId).join(','),
      effects.map((tile) => tile.typeId).join(','),
      inputs.map((tile) => tile.name).join(','),
    ].join('|');

    if (shape !== browser.shape) {
      browser.shape = shape;
      browser.tiles.clear();

      // No headings: the tab is the heading, and one over a grid of the only
      // kind in it says nothing the rail did not already say.
      const children: HTMLElement[] = [];
      if (up !== null) {
        children.push(this.#folderTile({ path: up, up: true }, false));
      }
      for (const folder of folders) {
        children.push(this.#folderTile(folder, searching));
      }
      for (const line of clips) {
        const tile = this.#browserTile(
          line,
          searching ? whereLabel(line.item.folder ?? ROOT_FOLDER) : null,
        );
        browser.tiles.set(line.item.id, tile);
        children.push(tile);
      }
      children.push(...generators.map((tile) => this.#generatorTile(tile)));
      children.push(...effects.map((tile) => this.#effectTile(tile)));
      children.push(...inputs.map((tile) => this.#ndiTile(tile)));
      browser.grid.replaceChildren(...children);
    }

    // What each clip is doing, updated in place: whether it is on air now, and
    // the picture, which arrives after the tile was built.
    for (const line of clips) {
      const tile = browser.tiles.get(line.item.id);
      if (tile === undefined) {
        continue;
      }
      tile.classList.toggle('on-air-tile', line.onAir);
      tile.title = this.#tileTitle(line);
      const picture = tile.firstElementChild;
      if (picture instanceof HTMLElement) {
        this.#paintTilePicture(picture, line.item.id);
      }
    }
  }

  /**
   * A library tile's picture, once the server has sent one.
   *
   * Replaced only when it is not already there, so that a redraw does not hand
   * the element a new `<img>` and make the browser flicker.
   */
  #paintTilePicture(target: HTMLElement, mediaItemId: string): void {
    this.#askForThumbnail(mediaItemId);
    const thumbnail = this.#thumbnails.get(mediaItemId) ?? null;
    if (thumbnail === null) {
      return;
    }
    const existing = target.firstElementChild;
    if (existing instanceof HTMLImageElement && existing.src === thumbnail) {
      return;
    }
    const image = make('img');
    image.src = thumbnail;
    image.alt = '';
    // The picture travels with the drag rather than a ghost of the whole tile,
    // so what the operator is carrying is the clip they picked up.
    image.draggable = false;
    target.replaceChildren(image);
  }

  /** One published generator as an icon that can be dragged onto a pad. */
  #generatorTile(tile: GeneratorTile): HTMLElement {
    const node = make('div', 'browser-tile generator-tile');
    node.draggable = true;

    const picture = make('div', 'browser-tile-picture');
    paintModulePicture(picture, 'generators', tile, tile.name, 'tile-mark');
    node.append(picture, make('span', 'browser-tile-name', tile.name));
    node.title = `${tile.name} — ${tile.description}${NEWLINE}Drag onto a pad to put it there.`;

    node.addEventListener('dragstart', (event) => {
      this.#dragging = { of: 'source', source: tile.source, name: tile.name };
      this.#replacing = null;
      const transfer = event.dataTransfer;
      if (transfer !== null) {
        transfer.effectAllowed = 'copy';
        transfer.setData(SOURCE_DRAG_TYPE, tile.typeId);
        transfer.setData('text/plain', tile.name);
      }
    });
    node.addEventListener('dragend', () => {
      this.#dragging = null;
      this.#clearDropTargets();
    });

    return node;
  }

  /**
   * One item of the library as an icon that can be dragged onto a pad.
   *
   * Under the name, whether it is a clip or a still and how long a clip runs —
   * and during a search, which folder it is in. A library where a clip and a
   * still, or two clips in two folders, share a name has to say which one a
   * tile is without being hovered over (owner, 2026-10-20).
   */
  #browserTile(line: LibraryLine, where: string | null = null): HTMLElement {
    const source: SlotSource = { kind: 'media', mediaItemId: line.item.id };
    const tile = make('div', 'browser-tile');
    tile.draggable = true;
    tile.classList.toggle('on-air-tile', line.onAir);

    const picture = make('div', 'browser-tile-picture');
    this.#paintTilePicture(picture, line.item.id);
    tile.append(picture, make('span', 'browser-tile-name', line.item.display_name));
    const length = durationLabel(line.item.duration_ms);
    tile.append(
      make(
        'span',
        'browser-tile-line',
        line.item.kind === 'image' ? 'Still' : length === null ? 'Video' : `Video ${length}`,
      ),
    );
    if (where !== null) {
      tile.append(make('span', 'browser-tile-line where', where));
    }
    if (line.onAir) {
      tile.append(make('span', 'on-air', 'ON AIR'));
    }

    // The long hover. Everything that used to be printed under the row: what
    // it is, what the server measured about it, and where tonight's show
    // already plays it.
    tile.title = this.#tileTitle(line);

    tile.addEventListener('dragstart', (event) => {
      this.#dragging = { of: 'source', source, name: line.item.display_name };
      this.#replacing = null;
      const transfer = event.dataTransfer;
      if (transfer !== null) {
        transfer.effectAllowed = 'copy';
        transfer.setData(SOURCE_DRAG_TYPE, line.item.id);
        // Some platforms will not start a drag without this one.
        transfer.setData('text/plain', line.item.display_name);
      }
    });
    tile.addEventListener('dragend', () => {
      this.#dragging = null;
      this.#clearDropTargets();
    });

    return tile;
  }


  /** One NDI sender as an icon that can be dragged onto a pad. */
  #ndiTile(tile: NdiTile): HTMLElement {
    const node = make('div', 'browser-tile ndi-tile');
    node.draggable = true;

    // **The mark as the picture, the stream as the name.** A source has no
    // thumbnail to show — a receiver that decoded one just to fill a tile
    // would be a second receiver on the network for every sender an operator
    // is not watching — so the picture says what kind of thing this is, and
    // the text says which one.
    //
    // **NDI's own file, from the brand assets in the SDK** — see
    // `paintNdiMark`, which is also what a pad holding one of these shows. A
    // vector, so 92 pixels and 180 are both exactly right.
    //
    // The machine has moved into the tooltip, where the full name already
    // was. On a rig where three machines each send "Program" the tiles now
    // read alike and only the hover tells them apart; that is the cost of one
    // line of text and it is the owner's call, taken 2026-10-15.
    const picture = make('div', 'browser-tile-picture');
    paintNdiMark(picture);
    node.append(picture, make('span', 'browser-tile-name', tile.stream));
    node.title = `${tile.name}${NEWLINE}Drag onto a pad to put it there.`;

    node.addEventListener('dragstart', (event) => {
      this.#dragging = { of: 'source', source: tile.source, name: tile.stream };
      this.#replacing = null;
      const transfer = event.dataTransfer;
      if (transfer !== null) {
        transfer.effectAllowed = 'copy';
        // The full name, because that is the identifier. The halves on the tile
        // are for reading.
        transfer.setData(SOURCE_DRAG_TYPE, tile.name);
        transfer.setData('text/plain', tile.name);
      }
    });
    node.addEventListener('dragend', () => {
      this.#dragging = null;
      this.#clearDropTargets();
    });

    return node;
  }

  /** One published effect as an icon that can be dropped on a slot or a layer. */
  #effectTile(tile: EffectTile): HTMLElement {
    const node = make('div', 'browser-tile effect-tile');
    node.draggable = true;

    // **The same reference picture with the effect on it**, drawn on a card by
    // `cargo run -p ovs-renderer --bin ovs-effect-pictures` and shipped with
    // this client. An effect draws whatever is under it, so a tile can only be
    // "this effect, on something" — and the something is the house's own
    // service pattern, which has colour, a black level, hard edges and
    // lettering in one picture.
    //
    // Each is drawn at a setting that shows what the effect does rather than
    // at its published defaults, because every effect is neutral at those: a
    // tile drawn there would be the same picture fourteen times.
    const picture = make('div', 'browser-tile-picture');
    paintModulePicture(picture, 'effects', tile, tile.name, 'tile-mark');
    node.append(picture, make('span', 'browser-tile-name', tile.name));
    node.title =
      `${tile.name} — ${tile.description}${NEWLINE}` +
      'Drop it on a pad for that clip, or on a layer’s picture for the whole layer. ' +
      'It stacks on what is already there.';

    node.addEventListener('dragstart', (event) => {
      this.#dragging = {
        of: 'effect',
        typeId: tile.typeId,
        version: tile.version,
        name: tile.name,
      };
      this.#replacing = null;
      const transfer = event.dataTransfer;
      if (transfer !== null) {
        transfer.effectAllowed = 'copy';
        transfer.setData(SOURCE_DRAG_TYPE, tile.typeId);
        transfer.setData('text/plain', tile.name);
      }
    });
    node.addEventListener('dragend', () => {
      this.#dragging = null;
      this.#clearDropTargets();
    });

    return node;
  }

  /** What a longer hover over a library icon says. */
  #tileTitle(line: LibraryLine): string {
    const parts = [
      line.item.display_name,
      whereLabel(line.item.folder ?? ROOT_FOLDER),
      line.item.original_filename,
      stateLabel(line.item.state),
    ];
    for (const entry of facts(line.item)) {
      parts.push(`${entry.label}: ${entry.value}`);
    }
    const where =
      line.usedBy.length === 0
        ? 'No slot of the open show plays this.'
        : line.usedBy
            .map((use) => `${use.layerName}, slot ${use.slotIndex}${use.onAir ? ' — on air' : ''}`)
            .join('; ');
    return `${parts.join(' · ')}${NEWLINE}${where}${NEWLINE}Drag onto a pad to put it there.`;
  }

  // --- putting something in a slot ------------------------------------------

  /**
   * What a drop onto one pad comes to, and then doing it.
   *
   * The rule is in `shared/work.ts` so that it can be read and tested without
   * a window. What is here is the gesture: which pad, what was on it, and the
   * two answers that are not “send it” — saying why not, and asking first.
   */
  #dropOn(layer: LayerView, slotIndex: number): void {
    const carried = this.#dragging;
    if (carried === null) {
      return;
    }
    const held = layer.slots.find((slot) => slot.index === slotIndex) ?? null;

    if (carried.of === 'effect') {
      // An effect stacks on what the pad plays. It never replaces it, so
      // there is nothing to ask about.
      const verdict = dropEffectOnPad(slotIndex, held !== null);
      if (verdict.do === 'refuse') {
        this.#note(verdict.because);
        return;
      }
      void this.#addEffect(layer.id, slotIndex, carried);
      return;
    }

    const source = carried.source;
    const verdict = dropOnPad({
      slotIndex,
      holding: held === null ? null : held.source,
      dropping: source,
      holdingName: held === null ? '' : this.#sourceName(held),
    });

    if (verdict.do === 'refuse') {
      this.#note(verdict.because);
      return;
    }
    if (verdict.do === 'nothing') {
      this.#note(`${layer.name}, slot ${slotIndex} already plays that.`);
      return;
    }
    if (verdict.do === 'ask') {
      this.#replacing = {
        layerId: layer.id,
        slotIndex,
        source,
        replacing: verdict.replacing,
      };
      this.#drawMatrix();
      return;
    }
    void this.#assign(layer.id, slotIndex, source);
  }

  /** Sends the assignment, and says what the server said. */
  async #assign(layerId: string, slotIndex: number, source: SlotSource): Promise<void> {
    const showId = this.#view.showId;
    if (showId === null) {
      this.#note('No show is open, so there is no slot to fill.');
      return;
    }
    this.#replacing = null;
    const answer = await this.#ovs.assignSlot(showId, layerId, slotIndex, wireSource(source));
    if (!answer.ok) {
      this.#note(answer.message);
      return;
    }
    // Nothing is set from here. What the pad shows next comes back over the
    // socket, as everything on this screen does.
  }


  /**
   * Stacks an effect on one slot, or on the whole layer when no slot is named.
   *
   * Nothing is set from here either. What the chain looks like next comes back
   * over the socket, as everything on this screen does.
   */
  async #addEffect(
    layerId: string,
    slotIndex: number | null,
    effect: { typeId: string; version: number; name: string },
  ): Promise<void> {
    const showId = this.#view.showId;
    if (showId === null) {
      this.#note('No show is open, so there is nothing to put an effect on.');
      return;
    }
    const answer = await this.#ovs.addEffect(
      showId,
      layerId,
      slotIndex,
      effect.typeId,
      effect.version,
    );
    if (!answer.ok) {
      this.#note(answer.message);
    }
  }

  /**
   * Takes one effect off a chain.
   *
   * A document edit made from the live screen, exactly like putting one on. The
   * owner's rule is that a chain is worked on where it is used: "alles war in
   * den Options gedacht. Settings sind weitgehende eingriffe. Options schnellere
   * Zugriffe" (2026-09-27).
   *
   * `addEffect` has gone through here since an effect could be dropped on a pad.
   * This is its other half, and it was missing everywhere — the bridge, the main
   * process and the API all carry it, and no screen called it.
   */
  async #removeEffect(chain: ChainLine, effect: EffectLine): Promise<void> {
    const showId = this.#view.showId;
    if (showId === null) {
      this.#note('No show is open, so there is no chain to take an effect off.');
      return;
    }
    const answer = await this.#ovs.removeEffect(showId, chain.layerId, chain.slotIndex, effect.id);
    if (!answer.ok) {
      this.#note(answer.message);
    }
  }

  /**
   * Sets the order of a chain.
   *
   * The whole order, because that is what the route takes: a move is a pair of
   * positions worked out from a list that may already be behind, and an order
   * either matches what the server holds or is refused.
   */
  async #orderEffects(chain: ChainLine, order: readonly string[]): Promise<void> {
    const showId = this.#view.showId;
    if (showId === null) {
      this.#note('No show is open, so there is no chain to put in order.');
      return;
    }
    const answer = await this.#ovs.orderEffects(showId, chain.layerId, chain.slotIndex, order);
    if (!answer.ok) {
      this.#note(answer.message);
    }
  }

  /**
   * Locks one effect or generator where it is, or unlocks it.
   *
   * Offered to an administrator only (owner, 2026-10-19), and the server
   * decides again. What the row looks like next comes back over the socket.
   */
  async #setLock(target: SetLockRequest['target'], locked: boolean): Promise<void> {
    const showId = this.#view.showId;
    if (showId === null) {
      this.#note('No show is open, so there is nothing to lock.');
      return;
    }
    const answer = await this.#ovs.setLock(showId, { target, locked });
    if (!answer.ok) {
      this.#note(answer.message);
    }
  }

  /** The lock's own button, for an administrator. Null for anybody else. */
  #lockButton(target: SetLockRequest['target'], locked: boolean, name: string): HTMLButtonElement | null {
    if (!this.#view.mayAdminister) {
      return null;
    }
    return button(locked ? 'Unlock' : 'Lock', () => void this.#setLock(target, !locked), {
      className: 'small',
      title: locked
        ? `Let ${name} be removed and moved again.`
        : `Lock ${name} where it is: nobody can remove it, move it or put it in another order until it is unlocked. Its values still follow the patch, and switching it on or off stays open to everybody.`,
    });
  }

  /**
   * Changes how the clip in a layer's active slot runs.
   *
   * A document edit made from the live screen, like putting a source in a slot
   * or an effect on a layer: it goes to `PUT …/slots/{n}/settings`, which
   * edits the document the server already holds and installs it without the
   * restart policy running. Storing the whole show instead would put every
   * layer back to its opening slot.
   *
   * The active slot, because that is the clip the operator is watching. Slot 0
   * is Black and has nothing to configure.
   */
  /**
   * Takes the source out of a pad, leaving it empty.
   *
   * An empty pad is black, so the layer goes off stage if it was playing this
   * one — which is what an empty pad means and what the server does with it.
   * Recoverable by dragging a source back onto it, which is why this asks
   * nothing first: nothing is lost that was not one drag away.
   */
  async #clearSlot(layerId: string, index: number): Promise<void> {
    const showId = this.#view.showId;
    if (showId === null || this.#layer(layerId) === null) {
      return;
    }
    if (index === BLACK_SLOT_INDEX) {
      this.#note('Slot 0 is Black on every layer, and there is nothing in it to clear.');
      return;
    }

    const answer = await this.#ovs.clearSlot(showId, layerId, index);
    if (!answer.ok) {
      this.#note(answer.message);
      return;
    }
    // Back to the layer, because the thing these controls were about is gone.
    this.#selectedSlot = null;
    this.#render();
  }

  async #setSlot(layerId: string, index: number, change: SetSlotRequest): Promise<void> {
    const showId = this.#view.showId;
    const layer = this.#layer(layerId);
    if (showId === null || layer === null) {
      return;
    }
    if (index === BLACK_SLOT_INDEX) {
      this.#note('Slot 0 is Black on every layer, and there is nothing in it to set.');
      return;
    }

    const answer = await this.#ovs.setSlot(showId, layerId, index, change);
    if (!answer.ok) {
      this.#note(answer.message);
    }
    // Nothing is drawn from here: the edit comes back on the state stream as a
    // new document, and drawing it twice would show the operator their own
    // wish before the server had agreed to it.
  }

  /**
   * Asks every layer for one more bank of twelve.
   *
   * Every layer rather than the one that was clicked: the matrix pages as one
   * strip — bank 2 is bank 2 on all of them — so a bank added to one layer
   * alone would be a page with a single row on it.
   *
   * A layer that is already at the ceiling is left where it is rather than
   * refused, so the others still gain theirs.
   */
  async #addBank(): Promise<void> {
    const showId = this.#view.showId;
    if (showId === null) {
      this.#note('No show is open, so there is no matrix to extend.');
      return;
    }
    const wanted = this.#banks.length + 1;
    if (wanted > MAX_BANKS) {
      this.#note(`A layer holds ${MAX_BANKS} banks, so slot ${MAX_SLOT_INDEX} is the last one.`);
      return;
    }

    for (const layer of this.#view.layers) {
      if (layer.banks >= wanted) {
        continue;
      }
      const answer = await this.#ovs.setBanks(showId, layer.id, wanted);
      if (!answer.ok) {
        this.#note(answer.message);
        return;
      }
    }
    // The strip is drawn from the mirror, so the new bank appears when the
    // server says it is there. Paging to it is this client's own business.
    this.#bank = wanted - 1;
  }

  /**
   * Stores what is on stage into the open show.
   *
   * The owner's Save (2026-09-23): "Save stores the complete current live state
   * into the show", and "a save must make no change to the live show".
   * Both halves are the server's — it captures the live state into the document
   * it already holds and installs the result without applying the restart
   * policy — so this sends nothing but the identity of the show.
   *
   * Nothing here waits for anything to stop, because nothing stops.
   */
  async #saveLive(): Promise<void> {
    const showId = this.#view.showId;
    if (showId === null) {
      this.#note('No show is open, so there is nothing to save into.');
      return;
    }
    this.#ui.saveLive.disabled = true;
    const answer = await this.#ovs.captureLiveState(showId);
    this.#ui.saveLive.disabled = !this.#connected;
    if (!answer.ok) {
      this.#note(answer.message);
      return;
    }
    this.#note(`Saved ${this.#view.showName ?? 'the show'}. Nothing stopped.`);
  }

  /** Takes the question off the pad without answering it. */
  #cancelReplace(): void {
    if (this.#replacing === null) {
      return;
    }
    this.#replacing = null;
    this.#drawMatrix();
  }

  /**
   * Takes the drop highlight off everything.
   *
   * Everything rather than the pads alone: a layer's picture takes an effect
   * too, and a highlight left on one after the drag ended is the screen saying
   * something is about to happen that is not.
   */
  #clearDropTargets(): void {
    for (const node of document.querySelectorAll('.drop-target')) {
      node.classList.remove('drop-target');
    }
  }
  /**
   * `compact` is OPTIONS, where the panel already says which slot it is about
   * and where there is no reason to offer a way into Settings: a slot's
   * generator is set here, which is what "Options schnellere Zugriffe" means.
   */
  #buildGenerators(
    lines: readonly GeneratorLine[],
    shape: string,
    compact = false,
  ): EffectsNodes {
    const nodes: EffectsNodes = {
      shape,
      root: make('div'),
      parameters: new Map(),
      resets: new Map(),
      resetNotes: new Map(),
      onAir: new Map(),
    };
    nodes.root.append(this.#endpointNote('generator'));

    if (!this.#catalogue.loaded) {
      nodes.root.append(
        make(
          'p',
          'empty-note',
          'This server has not published its generator catalogue yet, so nothing here can be read against it.',
        ),
      );
      return nodes;
    }
    if (lines.length === 0) {
      nodes.root.append(make('p', 'empty-note', 'No slot of the open show plays a generator.'));
      return nodes;
    }

    for (const line of lines) {
      nodes.root.append(this.#buildGenerator(line, nodes, compact));
    }
    return nodes;
  }

  #buildGenerator(line: GeneratorLine, nodes: EffectsNodes, compact = false): HTMLElement {
    const row = make('div', 'work-row');
    const head = make('div', 'work-row-head');
    head.append(make('span', 'work-row-name', line.name));
    if (!compact) {
      head.append(make('span', 'work-row-where', `${line.layerName}, slot ${line.slotIndex}`));
    }

    const onAir = make('span', 'on-air', 'ON AIR');
    nodes.onAir.set(generatorKey(line), onAir);
    head.append(onAir);
    if (line.locked) {
      // Locked in its slot by an administrator (owner, 2026-10-19).
      head.append(make('span', 'lock-mark', 'LOCKED'));
    }

    // Only where there is an instance to name. A show this server has not
    // loaded has generators with no identity yet, and a "put back" naming one
    // this client invented would be refused for the wrong reason.
    if (line.instanceId !== null) {
      const plugin: PluginRef = { of: 'generator', id: line.instanceId, name: line.name };
      const reset = button('Put back', () => this.#putPluginBack(plugin), { className: 'small' });
      nodes.resets.set(line.instanceId, reset);
      head.append(reset);
      const lock = this.#lockButton(
        { kind: 'generator', generator_instance_id: line.instanceId },
        line.locked,
        line.name,
      );
      if (lock !== null) {
        head.append(lock);
      }
    }

    if (!compact) {
      // **The layer, not the slot.** It used to name the slot, and Settings has
      // had no slot panel since slots moved to OPTIONS — so it opened the
      // editor, selected a pad, and showed nothing about it. A layer is still
      // edited there: its name, its opacity, its own chain.
      head.append(
        this.#editButton('Open this layer in Settings', {
          layerId: line.layerId,
          what: line.layerName,
        }),
      );
    }
    row.append(head);

    if (line.instanceId !== null) {
      const note = make('p', 'work-row-note refusal');
      nodes.resetNotes.set(line.instanceId, note);
      row.append(note);
    }

    if (!line.published) {
      row.append(
        make(
          'p',
          'work-row-note',
          `This server does not publish ${line.typeId}, so what its ` +
            'parameters mean cannot be read here — and a control drawn without the manifest ' +
            'that declared them would be one this client invented.',
        ),
      );
      return row;
    }
    if (line.parameters.length === 0) {
      row.append(make('p', 'work-row-note', 'This one declares no parameters.'));
      return row;
    }
    if (line.instanceId === null) {
      // The one case with values and no way to move them. Said, rather than
      // shown as controls that do nothing: an operator pushing a dead fader has
      // been told nothing.
      row.append(
        make(
          'p',
          'work-row-note',
          'This slot’s generator has no instance identity yet, which happens only for a show ' +
            'this server has not opened. Open it and the controls appear.',
        ),
      );
      row.append(parameterGrid(line.parameters));
      return row;
    }

    const plugin: PluginRef = { of: 'generator', id: line.instanceId, name: line.name };
    const grid = make('div', 'live-facts');
    for (const parameter of line.parameters) {
      grid.append(this.#buildParameter(plugin, parameter, nodes));
    }
    row.append(grid);
    return row;
  }

  #updateGenerators(nodes: EffectsNodes, lines: readonly GeneratorLine[]): void {
    for (const line of lines) {
      const marker = nodes.onAir.get(generatorKey(line));
      if (marker !== undefined) {
        marker.hidden = !line.onAir;
      }
      if (line.instanceId === null) {
        continue;
      }
      const plugin: PluginRef = { of: 'generator', id: line.instanceId, name: line.name };
      this.#updateReset(nodes, plugin, line.moved);
      for (const parameter of line.parameters) {
        this.#updateParameter(nodes, plugin, parameter);
      }
    }
  }

  /**
   * OPTIONS — the selected layer's live controls, with room to read them.
   *
   * The one tab of the four that is live control, and everything it sends is a
   * field of `set_layer`: route, opacity, trigger grid and rate, which is the
   * whole of what a layer's live state is. The matrix row has
   * the same four in the space the drawing gives it; here they have room for
   * what the show opens the layer on beside each one, and for the things
   * that are the document — the blend mode, whether the layer is switched on,
   * what the end of the running clip does — shown and not offered.
   *
   * Built once per layer and updated in place, like a row: an operator with the
   * opacity fader under their finger must not have it rebuilt underneath them.
   */
  #drawOptions(): void {
    // **There is always exactly one subject** (owner, 2026-09-23): "Es gibt
    // immer mindestens und maximum ein Objekt was selektiert sein muss."
    //
    // So a panel with nothing chosen chooses: the top layer, which is the one
    // an operator reaches for first. An empty panel telling somebody to click
    // something is a panel asking them to do what it could have done.
    if (this.#selected === null || this.#layer(this.#selected) === null) {
      const top = [...this.#view.layers].reverse()[0] ?? null;
      this.#selected = top?.id ?? null;
      this.#selectedSlot = null;
    }
    const layer = this.#selected === null ? null : this.#layer(this.#selected);
    if (layer === null) {
      this.#options = null;
      this.#slotOptions = null;
      this.#effects = null;
      this.#optionsShape = '';
      // The one case left: a show with no layers at all, where there is
      // nothing to be the subject.
      this.#ui.workTool.replaceChildren(
        make(
          'p',
          'empty-note',
          this.#connected
            ? 'The open show has no layers. Add one in Settings.'
            : 'Sign in to a server to see its layers.',
        ),
      );
      return;
    }

    // Which slot, if a slot is what was chosen. One that has since been
    // emptied falls back to the layer rather than showing controls for
    // something that is not there.
    const slot =
      this.#selectedSlot === null
        ? null
        : layer.slots.find((each) => each.index === this.#selectedSlot) ?? null;

    // Only the chains belonging to what is selected. A screenful of faders for
    // things the operator is not looking at is a screenful of ways to move the
    // wrong one.
    const chains = effectChains([layer], this.#catalogue).filter((chain) =>
      slot === null
        ? chain.where === 'layer'
        : chain.where === 'slot' && chain.slotIndex === slot.index,
    );

    // The generator this slot plays, if it plays one. Filtered to the slot for
    // the same reason the chains above are: these are controls.
    const generators =
      slot === null
        ? []
        : generatorLines([layer], this.#catalogue).filter(
            (line) => line.slotIndex === slot.index,
          );

    const shape = [
      layer.id,
      slot === null ? 'layer' : `slot:${slot.index}:${slot.source.kind}`,
      effectsShape(layer.id, chains, this.#catalogue.loaded),
      generatorsShape(generators, this.#catalogue.loaded),
    ].join('|');

    if (this.#optionsShape !== shape) {
      this.#optionsShape = shape;
      this.#options = slot === null ? this.#buildOptions(layer) : null;
      this.#slotOptions = slot === null ? null : this.#buildSlotOptions(layer, slot);
      // What the panel is about, in the operator's words: a slot when a slot is
      // selected, the layer when it is not.
      const subject =
        slot === null ? layer.name : `slot ${slot.index} — ${this.#sourceName(slot)}`;
      this.#effects = this.#buildEffects(subject, chains, shape);

      // Two columns (owner, 2026-09-23): what the thing *is* on the left —
      // the layer's or the slot's own settings, and its transform — and what
      // is being done *to* it on the right, which is the effect chain and
      // every parameter of it. They are two lists that grow at different
      // rates, and stacked the second was always below the fold.
      // **What the slot plays**, under the settings for how it plays it. Only
      // for a generator: a clip's own parameters are the file's, and there is
      // nothing here to move.
      this.#slotGenerator =
        generators.length === 0 ? null : this.#buildGenerators(generators, shape, true);

      const controls = this.#options?.root ?? this.#slotOptions?.root ?? make('div');
      const split = make('div', 'options-split');
      const left = make('div', 'options-column');
      const right = make('div', 'options-column');
      left.append(controls);
      if (this.#slotGenerator !== null) {
        left.append(this.#slotGenerator.root);
      }
      right.append(this.#effects.root);
      split.append(left, right);
      this.#ui.workTool.replaceChildren(this.#subjectRow(layer, slot), split);
    }

    if (this.#options !== null) {
      this.#updateOptions(this.#options, layer);
    }
    if (this.#slotOptions !== null && slot !== null) {
      this.#updateSlotOptions(this.#slotOptions, slot);
    }
    if (this.#slotGenerator !== null) {
      this.#updateGenerators(this.#slotGenerator, generators);
    }
    if (this.#effects !== null) {
      this.#updateEffects(this.#effects, chains);
    }
  }

  /**
   * What OPTIONS is about, and the way back to the layer from a slot.
   *
   * Named rather than inferred from the matrix: an operator who chose slot 7
   * and then looked away has to be able to see that slot 7 is what these
   * controls are for.
   */
  #subjectRow(layer: LayerView, slot: SlotView | null): HTMLElement {
    const row = make('div', 'options-subject');
    row.append(
      make(
        'span',
        'options-subject-name',
        slot === null ? layer.name : `${layer.name} · slot ${slot.index} — ${this.#sourceName(slot)}`,
      ),
    );
    if (slot !== null) {
      row.append(
        button(
          'The layer',
          () => {
            this.#selectedSlot = null;
            this.#render();
          },
          { className: 'small', title: `Set up ${layer.name} itself instead` },
        ),
      );
    }
    return row;
  }

  /**
   * The selected slot's own settings.
   *
   * Every one of these is the *document* rather than live state — they
   * describe the clip, not what the show is doing with it — and every one is
   * decided while watching the wall, which is why they are here (owner,
   * 2026-09-23). They reach the server through `PUT …/slots/{n}/settings`,
   * which edits the document under the running show and restarts nothing.
   */
  /**
   * Where a source sits, as five faders and a fold.
   *
   * The owner, 2026-09-23: position both ways from the middle, a rotation, and
   * a size — "aufklappbar die einzelne XY Achse, ansonsten werden XY zusammen
   * verändert". It was five number boxes in a row, which is five places to type
   * a number nobody knows the units of.
   *
   * Every one of them sends the **whole** transform: a placement is one thing,
   * and sending it a field at a time would walk the picture through positions
   * nobody asked for. `read` is called at the moment of the gesture rather than
   * captured, because the panel outlives many updates.
   */
  #buildTransform(
    key: string,
    read: () => Transform2D,
    commit: (transform: Transform2D) => void,
  ): TransformNodes {
    // Per cent of the composition, both ways. A composition this client has not
    // been told about yet is taken as 1920x1080, which is the shape everything
    // else here defaults to; the number an operator sets is still what they see.
    const canvas = (): { width: number; height: number } =>
      this.#view.composition ?? { width: 1920, height: 1080 };

    // **Sent while the fader moves, not when it is let go** (owner,
    // 2026-09-23): "Scaling und Positioning inkl Rotation beim bewegen des
    // Sliders schon ausführen… Es sind Live Parameter." A placement judged
    // against a wall has to be judged while it is being changed; a fader that
    // only arrives on release is a fader an operator has to let go of to see
    // what they did.
    //
    // Coalesced through `#sendLatest`, which keeps one request in flight and
    // one queued: an operator's gesture is where they stopped, not every pixel
    // on the way, and a queue of those would still be arriving after they let
    // go. One key per fader, so moving X does not cancel a rotation.
    // **What is in hand**, which is not what the server has said yet.
    //
    // Every fader sends the whole placement, because a placement is one thing.
    // Two of them moved in the same breath would each build on `read()` — the
    // last value the server confirmed — and the second would carry the first's
    // old number back over it. So each builds on the last thing sent, and the
    // server's own value is taken again as soon as nothing is being dragged
    // (`#showTransform`).
    const put = (field: string, change: Partial<Transform2D>): void => {
      const placed = { ...(this.#inHand.get(key) ?? read()), ...change };
      this.#inHand.set(key, placed);
      this.#sendLatest(`${key}.${field}`, async () => {
        commit(placed);
        return Promise.resolve();
      });
    };

    const x = fader({
      ...FADER_X,
      onInput: (shown) => put('x', { x: (shown / 100) * canvas().width }),
      onCommit: (shown) => put('x', { x: (shown / 100) * canvas().width }),
    });
    const y = fader({
      ...FADER_Y,
      onInput: (shown) => put('y', { y: (shown / 100) * canvas().height }),
      onCommit: (shown) => put('y', { y: (shown / 100) * canvas().height }),
    });
    const rotation = fader({
      ...FADER_ROTATION,
      onInput: (shown) => put('rotation', { rotation_deg: shown }),
      onCommit: (shown) => put('rotation', { rotation_deg: shown }),
    });

    // One size moves both axes, which is what "ansonsten werden XY zusammen
    // verändert" asks for: a source keeps its shape unless somebody opens the
    // fold and says otherwise.
    const both = (shown: number): void =>
      put('size', { scale_x: shown / 100, scale_y: shown / 100 });
    const size = fader({ ...FADER_SIZE, onInput: both, onCommit: both });

    const alongX = (shown: number): void => put('size-x', { scale_x: shown / 100 });
    const sizeX = fader({ ...FADER_SIZE_X, onInput: alongX, onCommit: alongX });

    const alongY = (shown: number): void => put('size-y', { scale_y: shown / 100 });
    const sizeY = fader({ ...FADER_SIZE_Y, onInput: alongY, onCommit: alongY });

    // `details`, because that is what folding is and the browser already knows
    // how to do it from the keyboard.
    const axes = make('details', 'option-axes');
    const summary = make('summary', undefined, 'Each axis');
    axes.append(summary, sizeX.root, sizeY.root);

    const root = make('div', 'option-place');
    root.append(x.root, y.root, rotation.root, size.root, axes);
    return { root, x, y, rotation, size, sizeX, sizeY };
  }

  /** Puts a transform on its faders, in the units they show. */
  #showTransform(nodes: TransformNodes, shown: Transform2D, key?: string): void {
    let transform = shown;
    const canvas = this.#view.composition ?? { width: 1920, height: 1080 };
    const held = (one: FaderNodes): boolean => document.activeElement === one.input;

    // Nothing under a hand: the server's value is the truth again, and what was
    // in hand is forgotten. Keeping it would mean a value moved on a console
    // never reached these faders.
    const dragging = [nodes.x, nodes.y, nodes.rotation, nodes.size, nodes.sizeX, nodes.sizeY].some(
      (one) => held(one),
    );
    if (!dragging && key !== undefined) {
      this.#inHand.delete(key);
    }

    // While a hand is on one of these, what is in hand is the truth and the
    // server's value is a round trip behind it. Painting the server's would
    // snap the other five faders back to where they were a moment ago every
    // time one of them moved.
    const showing = (key === undefined ? undefined : this.#inHand.get(key)) ?? transform;
    transform = dragging ? showing : transform;

    setFaderTo(nodes.x, (transform.x / (canvas.width || 1)) * 100, FADER_X, held(nodes.x));
    setFaderTo(nodes.y, (transform.y / (canvas.height || 1)) * 100, FADER_Y, held(nodes.y));
    setFaderTo(nodes.rotation, transform.rotation_deg, FADER_ROTATION, held(nodes.rotation));

    // The plain one shows the mean of the two axes, which is the number it
    // would set them both to. A source that has been stretched on one axis
    // reads as something between, rather than as one axis pretending to be both.
    const mean = ((transform.scale_x + transform.scale_y) / 2) * 100;
    setFaderTo(nodes.size, mean, FADER_SIZE, held(nodes.size));
    setFaderTo(nodes.sizeX, transform.scale_x * 100, FADER_SIZE_X, held(nodes.sizeX));
    setFaderTo(nodes.sizeY, transform.scale_y * 100, FADER_SIZE_Y, held(nodes.sizeY));
  }

  #buildSlotOptions(layer: LayerView, slot: SlotView): SlotOptionNodes {
    const root = make('div', 'live-facts');
    const layerId = layer.id;
    const index = slot.index;

    // **What this pad is called**, and the first row, because it is the thing
    // an operator reads off the matrix from across a room.
    //
    // The owner, 2026-10-15: "Manchmal kann ja der selbe Inhalt sein, aber mit
    // anderen Effekte drauf etc. für Dauerhafte installation wäre das noch
    // gut." Left empty, the pad is named after what it plays, and the
    // placeholder shows what that name is — so an empty field reads as "this
    // one is called Strobe because that is what is in it" rather than as a
    // setting nobody has filled in.
    //
    // On `change` rather than on every keystroke: this is a document edit, and
    // one a letter would be one per letter.
    const name = make('input', 'option-name');
    name.type = 'text';
    name.maxLength = MAX_SLOT_NAME_CHARS;
    name.setAttribute('aria-label', 'What this pad is called');
    name.addEventListener('change', () => {
      void this.#setSlot(layerId, index, { name: name.value });
    });

    const playbackMode = make('select', 'layer-blend');
    playbackMode.setAttribute('aria-label', 'What the end of this clip does');
    for (const mode of PLAYBACK_MODES) {
      const option = make('option', undefined, PLAYBACK_MODE_LABELS[mode] ?? mode);
      option.value = mode;
      playbackMode.append(option);
    }
    playbackMode.addEventListener('change', () => {
      void this.#setSlot(layerId, index, { playback_mode: playbackMode.value as PlaybackMode });
    });

    // **How fast this slot runs**, on both kinds of sync (owner, 2026-09-23):
    // "pro Slot Scale sollt aber verfügbar sein für Free Run und BPM Sync."
    //
    // A trigger grid used to live here and is gone — "Trigger Grid weg. Beats
    // ist das was man braucht." This is the control that was missing: on free
    // run it is the only rate a clip has, because the master's scale
    // deliberately does not reach one, and on BPM sync it sits on top of the
    // beats.
    // Six pads rather than a menu (owner, 2026-09-23), and the same six the
    // master's scale has: the ladder has six settings and nothing in between,
    // and a menu hides five of them behind a press. It is also how an operator
    // reads their own desk — the step they are on is lit, not spelled out in a
    // closed box.
    const speed = make('div', 'speed-steps');
    const speedSteps = new Map<number, HTMLButtonElement>();
    for (const step of SPEED_SCALE_STEPS) {
      const node = button(
        speedLabel(step),
        () => {
          void this.#setSlot(layerId, index, { speed: step });
        },
        {
          className: 'speed-step',
          title: `Run this clip at ${speedLabel(step)} of what its sync asks for`,
        },
      );
      speedSteps.set(step, node);
      speed.append(node);
    }

    const direction = make('select', 'layer-blend');
    direction.setAttribute('aria-label', 'Which way this clip runs');
    for (const way of PLAYBACK_DIRECTIONS) {
      const option = make('option', undefined, PLAYBACK_DIRECTION_LABELS[way] ?? way);
      option.value = way;
      direction.append(option);
    }
    direction.addEventListener('change', () => {
      void this.#setSlot(layerId, index, { direction: direction.value as PlaybackDirection });
    });

    // Free run or a number of beats. The beat count only exists in the second
    // case, so it is a field that appears rather than a disabled one that sits
    // there; switching to beats and straight back keeps the number somebody
    // counted.
    const sync = make('select', 'layer-blend');
    sync.setAttribute('aria-label', 'Whether this clip runs free or to the beat');
    for (const [value, label] of [
      ['FREE_RUN', 'Free run'],
      ['BEAT_SYNC', 'BPM sync'],
    ] as const) {
      const option = make('option', undefined, label);
      option.value = value;
      sync.append(option);
    }
    sync.addEventListener('change', () => {
      void this.#setSlot(layerId, index, {
        sync:
          sync.value === 'BEAT_SYNC'
            ? { kind: 'BEAT_SYNC', beats: Number(beats.value) || DEFAULT_SLOT_BEATS }
            : { kind: 'FREE_RUN' },
      });
    });

    const beats = make('input', 'option-beats');
    beats.type = 'number';
    beats.min = String(MIN_SLOT_BEATS);
    beats.step = '1';
    beats.setAttribute('aria-label', 'How many beats one pass takes');
    beats.addEventListener('change', () => {
      void this.#setSlot(layerId, index, {
        sync: { kind: 'BEAT_SYNC', beats: Number(beats.value) },
      });
    });
    const beatsRow = optionRow('Beats a pass', beats);

    // **What the setting is asking of the decoder**, under the controls that
    // decide it. This is the whole lesson of 2026-09-23: seven beats on a
    // seventy-five-second clip is twenty-one times speed, the card cannot
    // decode it, and until now nothing anywhere said so — the wall simply
    // stopped and the late-frame counter climbed with no reason beside it.
    const rate = make('p', 'option-rate');

    // Where this source sits, before the layer's own transform. The slot's,
    // not the layer's: two clips in one layer are rarely framed the same way,
    // and this is where one of them is put right without moving the other.
    const transform = this.#buildTransform(
      `slot.${layerId}.${index}.transform`,
      () =>
        this.#layer(layerId)
          ?.slots.find((each) => each.index === index)
          ?.transform ?? IDENTITY_TRANSFORM,
      (placed) => {
        void this.#setSlot(layerId, index, { transform: placed });
      },
    );

    // **Empties this pad.** Last, and on its own row, because it is the one
    // control here that takes something away rather than changing how it plays
    // — and because an operator looking for it looks at the bottom.
    //
    // Here rather than on the pad itself: a press on a pad is a trigger, which
    // is the gesture an operator makes fifty times an evening, and it must not
    // sit one slip away from emptying what it plays.
    const clear = button(
      'Clear',
      () => {
        void this.#clearSlot(layerId, index);
      },
      {
        className: 'danger',
        title: 'take the source out of this pad, leaving it empty',
      },
    );
    clear.id = 'live-clear-slot';
    // The same two-column line every other setting uses, so it sits with them
    // rather than beside them.
    const clearRow = optionRow('This pad', clear);

    root.append(
      optionRow('Name', name),
      optionRow('Playback', playbackMode),
      optionRow('Direction', direction),
      optionRow('Clip sync', sync),
      beatsRow,
      optionRow('Speed', speed),
      rate,
      transform.root,
      clearRow,
    );

    return {
      root,
      layerId,
      name,
      speedSteps,
      rate,
      playbackMode,
      direction,
      sync,
      beats,
      beatsRow,
      transform,
      clear,
    };
  }

  /** Keeps the slot's controls showing what the document says. */
  #updateSlotOptions(nodes: SlotOptionNodes, slot: SlotView): void {
    const settable = (node: HTMLElement): boolean => document.activeElement !== node;

    // A generator an administrator has locked in this pad stays in it (owner,
    // 2026-10-19), and the server refuses emptying it whoever asks.
    const locked = slot.source.kind === 'generator' && slot.source.locked;
    nodes.clear.disabled = locked;
    nodes.clear.title = locked
      ? 'Locked in this pad; an administrator has to unlock it first.'
      : 'take the source out of this pad, leaving it empty';

    // Not while it is being typed into: this panel is repainted on every
    // state the server sends, and a field reset under a hand is a name that
    // cannot be typed.
    if (settable(nodes.name)) {
      nodes.name.value = slot.name ?? '';
    }
    // The placeholder is what the pad is called when the field is empty, and
    // that moves when the source does.
    nodes.name.placeholder = this.#sourceOwnName(slot);

    // Nearest by ratio, as the master's pads are read: the server stores an
    // `f64`, so a value that arrives as 0.9999999 is still 1x and should light
    // the pad that says so.
    const step = nearestSpeedScale(slot.speed);
    for (const [value, node] of nodes.speedSteps) {
      node.classList.toggle('on', value === step);
    }
    if (settable(nodes.playbackMode)) {
      nodes.playbackMode.value = slot.playbackMode;
    }
    if (settable(nodes.direction)) {
      nodes.direction.value = slot.direction;
    }

    // Narrowed on the value rather than on a boolean taken from it, so the
    // beat count is reachable without an assertion.
    const beats = slot.sync.kind === 'BEAT_SYNC' ? slot.sync.beats : null;
    if (settable(nodes.sync)) {
      nodes.sync.value = beats === null ? 'FREE_RUN' : 'BEAT_SYNC';
    }
    if (beats !== null && settable(nodes.beats)) {
      nodes.beats.value = String(beats);
    } else if (nodes.beats.value === '') {
      nodes.beats.value = String(DEFAULT_SLOT_BEATS);
    }
    nodes.beatsRow.hidden = beats === null;

    // Recomputed every time anything it reads can have moved: the slot's own
    // settings, the clip under it, and the master tempo, which a console can
    // change without this panel being touched.
    const note = rateNote(this.#clipOf(slot), slot.sync, slot.speed, showTempo(this.#view.master));
    nodes.rate.textContent = note?.text ?? '';
    nodes.rate.hidden = note === null;
    nodes.rate.classList.toggle('strained', note?.strained === true);

    this.#showTransform(
      nodes.transform,
      slot.transform,
      `slot.${nodes.layerId}.${slot.index}.transform`,
    );
  }

  /**
   * A layer's OPTIONS: where its picture sits, and nothing else.
   *
   * The owner, 2026-09-23: "Layer Options brauchen keine Opacity, keine Route,
   * kein Showing, kein Waiting for, kein Sync, kein In the Composition, kein
   * Ends, kein Blendmode, kein Open Layer in Settings Button… Ich brauch keine
   * Buttons über die GUI verteilt 3 mal, sondern nur an den stellen, wo man die
   * wirklich braucht."
   *
   * Every one of those is on the layer's own row in the matrix, an arm's length
   * up the screen, or is a reading of something the row already shows. A panel
   * that repeats them is a second place to look for one answer and a second
   * place to change it by accident.
   *
   * **A layer has no trigger grid here either.** A grid is beats, and beats
   * belong to the clip: that moved onto the slot this morning, and a layer
   * showing one would be the screen saying a layer has a tempo.
   */
  #buildOptions(layer: LayerView): OptionNodes {
    const root = make('div', 'live-facts');
    const layerId = layer.id;

    const place = this.#buildTransform(
      `layer.${layerId}.transform`,
      () => this.#layer(layerId)?.transform ?? IDENTITY_TRANSFORM,
      (transform) => {
        const current = this.#layer(layerId);
        if (current === null) {
          return;
        }
        const command = layerCommand(current, { transform });
        if (command !== null) {
          void this.#send(command, { of: 'layer', layerId, field: 'transform' });
        }
      },
    );

    root.append(place.root);
    return { layerId, root, place };
  }

  /** Keeps the layer's placement showing what the server says. */
  #updateOptions(nodes: OptionNodes, layer: LayerView): void {
    this.#showTransform(nodes.place, layer.transform, `layer.${layer.id}.transform`);
    // A desk patched to the layer drives where its picture sits, all of it
    // together, while DMX is on (owner, 2026-10-19).
    const place = nodes.place;
    for (const fader of [place.x, place.y, place.rotation, place.size, place.sizeX, place.sizeY]) {
      fader.input.disabled = layer.held.transform;
    }
    holdForDesk(place.root, layer.held.transform, '');
  }

  /**
   * Takes a layer out of the composition, or puts it back.
   *
   * Live only. The show keeps its own value, which is what the layer opens
   * with next time and what the line under this button reports
   * (owner decision Q16).
   */
  #toggleEnabled(layerId: string): void {
    const layer = this.#view.layers.find((each) => each.id === layerId);
    if (layer === undefined) {
      return;
    }
    void this.#send(
      { do: 'set_layer', layerId, enabled: !layer.enabled },
      { of: 'layer', layerId, field: 'enabled' },
    );
  }

  #buildEffects(subject: string, chains: readonly ChainLine[], shape: string): EffectsNodes {
    const nodes: EffectsNodes = {
      shape,
      root: make('div'),
      parameters: new Map(),
      resets: new Map(),
      resetNotes: new Map(),
      onAir: new Map(),
    };
    if (!this.#catalogue.loaded) {
      nodes.root.append(
        make(
          'p',
          'empty-note',
          'This server has not published its effect catalogue yet, so nothing here can be read against it.',
        ),
      );
      return nodes;
    }
    if (chains.length === 0) {
      // **Named after what is actually selected** (owner, 2026-09-23): it said
      // the layer's name while a slot of that layer was the subject, and a slot
      // has its own chain. `subject` is what OPTIONS is about.
      nodes.root.append(
        make(
          'p',
          'empty-note',
          `Nothing on ${subject} has an effect on it. Drop one from the catalogue onto it.`,
        ),
      );
      return nodes;
    }

    for (const chain of chains) {
      const row = make('div', 'work-row');
      const head = make('div', 'work-row-head');
      head.append(
        make(
          'span',
          'work-row-name',
          chain.where === 'layer' ? `${chain.layerName} — layer chain` : `${chain.layerName}, slot ${chain.slotIndex}`,
        ),
      );
      head.append(make('span', 'work-row-where', `${chain.effects.length} in the chain`));

      const onAir = make('span', 'on-air', 'ON AIR');
      nodes.onAir.set(chainKey(chain), onAir);
      head.append(onAir);

      // **No way out to Settings.** It used to offer one, and the panel it
      // opened is gone: a chain is worked on where it is used.
      row.append(head);

      if (!chain.rendered) {
        row.append(
          make(
            'p',
            'work-row-note',
            'A slot chain is configured and validated, and this renderer does not run it yet ' +
              '. The server keeps a value moved here; nothing of it reaches the wall.',
          ),
        );
      }

      for (const effect of chain.effects) {
        row.append(this.#buildEffect(effect, nodes, chain));
      }
      nodes.root.append(row);
    }
    return nodes;
  }

  /**
   * One effect, folded up (owner, 2026-09-23).
   *
   * A chain of six with every declared parameter under it is a page of faders,
   * and an operator reaching for one of them is scrolling past five they are
   * not touching. `<details>` rather than a button and a class, because that
   * is what folding is and the browser already knows how to do it — including
   * from the keyboard.
   *
   * Open when it is switched on: a chain is usually read to find the effect
   * that is doing something.
   */
  #buildEffect(effect: EffectLine, nodes: EffectsNodes, chain: ChainLine): HTMLElement {
    const entry = make('details', 'work-row');
    (entry as HTMLDetailsElement).open = effect.enabled;
    const head = make('summary', 'work-row-head');
    head.append(make('span', 'work-row-name', effect.name));
    head.append(make('span', 'work-row-where', effect.enabled ? 'on' : 'off — the chain skips it'));
    if (effect.locked) {
      // Locked by an administrator (owner, 2026-10-19): it stays where it is,
      // and its values still follow the patch like any other effect's.
      head.append(make('span', 'lock-mark', 'LOCKED'));
    }

    // **Where it sits in the chain.** An effect chain is an order — a blur
    // before a colour shift is not the same picture as after it — and until now
    // nothing anywhere could change one: the only way was to take an effect off
    // and put it back on the end, which loses every parameter set on it.
    //
    // Disabled at the ends rather than absent, so the row keeps its shape and a
    // press says why by not moving. The order is read off the chain this row was
    // built from, which the panel rebuilds whenever it changes.
    const at = chain.effects.findIndex((each) => each.id === effect.id);
    // A locked effect keeps its place, so neither it nor its neighbour may
    // swap across it; the server refuses both, whoever asks.
    const mayMove = (by: number): boolean => {
      const other = chain.effects[at + by];
      return at >= 0 && other !== undefined && !effect.locked && !other.locked;
    };
    const lockedNote = 'Locked where it is; an administrator has to unlock it first.';
    const move = (by: number): void => {
      const order = chain.effects.map((each) => each.id);
      const to = at + by;
      if (at < 0 || to < 0 || to >= order.length) {
        return;
      }
      const moved = order[at];
      const displaced = order[to];
      if (moved === undefined || displaced === undefined) {
        return;
      }
      order[at] = displaced;
      order[to] = moved;
      void this.#orderEffects(chain, order);
    };
    const lockedHere = (by: number): boolean =>
      effect.locked || chain.effects[at + by]?.locked === true;
    head.append(
      button('▲', () => move(-1), {
        className: 'small',
        title: lockedHere(-1) ? lockedNote : 'one place earlier in the chain',
        disabled: !mayMove(-1),
      }),
      button('▼', () => move(1), {
        className: 'small',
        title: lockedHere(1) ? lockedNote : 'one place later in the chain',
        disabled: !mayMove(1),
      }),
    );

    // **Taking it off, beside putting it back.** One undoes what an operator
    // did to the effect; the other takes the effect out of the chain, which is
    // a document edit and says so by being the only red thing on the row.
    const off = button(
      '✕',
      () => {
        void this.#removeEffect(chain, effect);
      },
      {
        className: 'small danger',
        title: effect.locked ? lockedNote : `take ${effect.name} off this chain`,
        disabled: effect.locked,
      },
    );
    head.append(off);
    const lock = this.#lockButton(
      { kind: 'effect', effect_instance_id: effect.id },
      effect.locked,
      effect.name,
    );
    if (lock !== null) {
      head.append(lock);
    }

    const plugin: PluginRef = { of: 'effect', id: effect.id, name: effect.name };
    const reset = button('Put back', () => this.#putPluginBack(plugin), { className: 'small' });
    nodes.resets.set(effect.id, reset);
    head.append(reset);
    entry.append(head);

    const note = make('p', 'work-row-note refusal');
    nodes.resetNotes.set(effect.id, note);
    entry.append(note);

    if (!effect.published) {
      entry.append(
        make(
          'p',
          'work-row-note',
          `This server does not publish ${effect.typeId}, so what its ` +
            'parameters mean cannot be read here — and a control drawn without the manifest that ' +
            'declared them would be one this client invented.',
        ),
      );
      return entry;
    }
    if (effect.parameters.length === 0) {
      entry.append(make('p', 'work-row-note', 'This one declares no parameters.'));
      return entry;
    }

    const grid = make('div', 'live-facts');
    for (const parameter of effect.parameters) {
      grid.append(this.#buildParameter(plugin, parameter, nodes));
    }
    entry.append(grid);
    return entry;
  }

  /**
   * One parameter's control, drawn from what the manifest declares it takes.
   *
   * A colour is four linear channels and gets four fields rather than a swatch,
   * for the reason the editor gives four: the model stores linear RGBA and a
   * swatch would be an sRGB conversion this client has no business making on the
   * operator's behalf.
   */
  #buildParameter(
    plugin: PluginRef,
    parameter: EffectParameter,
    nodes: EffectsNodes,
  ): HTMLElement {
    const control = pluginControl(plugin, parameter.key);
    const key = controlKey(control);
    const value = make('span', 'live-fact-value');
    const show = make('span', 'live-fact-show');
    const refusal = make('p', 'live-fact-refusal');
    const inputs: ParameterInput[] = [];
    // Same length as `inputs`, with a hole where a control is not a fader.
    const bars: (FaderBar | null)[] = [];
    const cell = make('div', 'option-control');

    const send = (next: ParameterValue): void => {
      this.#sendLatest(key, () => this.#send(pluginCommand(plugin, parameter.key, next), control));
    };

    // **One branch per kind the manifest can declare, and there are five.**
    // It was `number`, `toggle` and an `else` that drew a colour, so a menu and
    // a line of words were drawn as four colour channels — with the shape
    // generator's `shape` on the owner's own desk while it was (2026-09-26).
    const declared = parameter.declared.parameter_type;
    switch (declared.kind) {
      case 'number': {
        // The house fader, not the browser's. The name is in the row's own
        // grid on the left and the number is the `.live-fact-value` on the
        // right, so this is the bar between them.
        const track = faderBar({
          min: declared.min,
          max: declared.max,
          // A thousand steps across whatever the manifest declared, rather than
          // a step this client chose in the parameter's own units: only the
          // manifest knows whether this is a radius in pixels or an amount
          // between nought and one.
          step: (declared.max - declared.min) / FADER_STEPS,
          label: `${plugin.name} ${parameter.name}`,
        });
        track.input.addEventListener('pointerdown', () => {
          this.#holding = key;
        });
        track.input.addEventListener('pointerup', () => {
          this.#holding = null;
        });
        track.input.addEventListener('input', () => {
          const shown = Number(track.input.value);
          const moved: ParameterValue = { type: 'float', value: shown };
          paintBar(track, shown);
          value.textContent = parameterText(parameter.declared, moved);
          send(moved);
        });
        inputs.push(track.input);
        bars.push(track);
        cell.append(track.bar);
        break;
      }
      case 'toggle': {
        const input = make('input');
        input.type = 'checkbox';
        input.setAttribute('aria-label', `${plugin.name} ${parameter.name}`);
        input.addEventListener('change', () => send({ type: 'bool', value: input.checked }));
        inputs.push(input);
        cell.append(input);
        break;
      }
      case 'colour': {
        // **A swatch and an alpha field.** It was four linear channels, which
        // is exact and asks an operator mid-show to know that 0.2140 is the
        // grey that looks halfway (owner, 2026-09-27: "für alle parameter mit
        // Farbe nehme doch bitte einen Colorpicker, damit es einfacher wird").
        //
        // The element speaks sRGB and the model stores linear light, so every
        // value through here goes past `shared/colour.ts`. Alpha is beside it
        // rather than in it because `input[type=color]` has no alpha channel.
        const channels = make('div', 'option-colour');
        const swatch = make('input');
        swatch.type = 'color';
        swatch.setAttribute('aria-label', `${plugin.name} ${parameter.name}`);
        // **Alpha is a fader of its own** (owner, 2026-09-27: "mach alpha für
        // die farben als extra slider"). It was a number field, which is the
        // one control on this row an operator would have had to type into
        // mid-show.
        const alpha = faderBar({
          min: 0,
          max: 1,
          step: 0.001,
          label: `${plugin.name} ${parameter.name} alpha`,
        });
        alpha.bar.title = 'How opaque, from 0 to 1';
        const picked = (): void => {
          const [red, green, blue] = linearFromHex(swatch.value);
          const opacity = Number(alpha.input.value);
          send({
            type: 'color',
            value: [red, green, blue, Number.isFinite(opacity) ? opacity : 1],
          });
        };
        // The swatch commits once, when the picker is closed; the fader sends
        // as it moves, like every other fader here, and `#sendLatest` keeps
        // only the last of them.
        swatch.addEventListener('change', picked);
        alpha.input.addEventListener('pointerdown', () => {
          this.#holding = key;
        });
        alpha.input.addEventListener('pointerup', () => {
          this.#holding = null;
        });
        alpha.input.addEventListener('input', () => {
          paintBar(alpha, Number(alpha.input.value));
          picked();
        });
        inputs.push(swatch, alpha.input);
        bars.push(null, alpha);
        channels.append(swatch, alpha.bar);
        cell.append(channels);
        break;
      }
      case 'text': {
        // A box rather than a line, for the reason the editor gives one: the
        // text generator sets a line break as a line break, so somebody
        // writing two lines has to be able to type one.
        //
        // `change` rather than `input`: it fires when the field is left, and a
        // request per keystroke would put a show's title on the wall one letter
        // at a time.
        const input = document.createElement('textarea');
        input.rows = 2;
        input.maxLength = declared.max_chars;
        input.setAttribute('aria-label', `${plugin.name} ${parameter.name}`);
        input.addEventListener('change', () => send({ type: 'text', value: input.value }));
        inputs.push(input);
        cell.append(input);
        break;
      }
      case 'choice': {
        // A menu, which is the whole reason this kind exists: the shape
        // generator used to offer a fader labelled "Shape (0 ellipse, 1
        // rectangle, 2 triangle)" and ask an operator to guess.
        const select = document.createElement('select');
        for (const choice of declared.choices) {
          const option = document.createElement('option');
          option.value = choice.id;
          option.textContent = choice.name;
          select.append(option);
        }
        select.setAttribute('aria-label', `${plugin.name} ${parameter.name}`);
        select.addEventListener('change', () => send({ type: 'choice', value: select.value }));
        inputs.push(select);
        cell.append(select);
        break;
      }
      case 'image': {
        const namedOption = (value: string, label: string): HTMLOptionElement => {
          const node = document.createElement('option');
          node.value = value;
          node.textContent = label;
          return node;
        };
        // The library, as the editor offers it. Live as well as configured,
        // because swapping the stencil is a thing to do during a show rather
        // than only while building one — and because every other parameter
        // here is live, so one that was not would be the odd one out.
        const select = document.createElement('select');
        select.append(namedOption('', 'None'));
        for (const item of this.#library) {
          if (item.kind === 'image' && item.state === 'ready') {
            select.append(namedOption(item.id, item.display_name));
          }
        }
        select.setAttribute('aria-label', `${plugin.name} ${parameter.name}`);
        select.addEventListener('change', () => send({ type: 'text', value: select.value }));
        inputs.push(select);
        cell.append(select);
        break;
      }
      default: {
        // A seventh kind is a compile error here rather than a wrong control.
        const unreachable: never = declared;
        return unreachable;
      }
    }

    cell.append(value);
    const line = make('div', 'live-fact live');
    line.append(make('span', 'live-fact-label', parameter.name), cell, show, refusal);
    nodes.parameters.set(key, {
      declared: parameter.declared,
      inputs,
      bars,
      value,
      show,
      refusal,
    });
    return line;
  }

  #updateEffects(nodes: EffectsNodes, chains: readonly ChainLine[]): void {
    for (const chain of chains) {
      const marker = nodes.onAir.get(chainKey(chain));
      if (marker !== undefined) {
        marker.hidden = !chain.onAir;
      }
      for (const effect of chain.effects) {
        const plugin: PluginRef = { of: 'effect', id: effect.id, name: effect.name };
        this.#updateReset(nodes, plugin, effect.moved);
        for (const parameter of effect.parameters) {
          this.#updateParameter(nodes, plugin, parameter);
        }
      }
    }
  }

  #updateReset(nodes: EffectsNodes, plugin: PluginRef, moved: boolean): void {
    const reset = nodes.resets.get(plugin.id);
    const note = nodes.resetNotes.get(plugin.id);
    if (reset === undefined || note === undefined) {
      return;
    }
    // Nothing the server reports *is* the reset, so this reading is only ever
    // waiting, refused, or nothing at all.
    const reading = this.#pending.read<boolean>(pluginControl(plugin, null), null);
    const what = plugin.of === 'effect' ? 'effect' : 'generator';
    reset.disabled = !this.#connected || !moved;
    reset.classList.toggle('pending', reading.state === 'pending');
    reset.classList.toggle('refused', reading.state === 'refused');
    reset.title =
      reading.state === 'refused'
        ? `The server refused this: ${reading.reason}`
        : moved
          ? `Put every parameter of this ${what} back to what the show configured. Sent as a reset rather than as those values, because a live value equal to the configured one is still a live value.`
          : `Nothing on this ${what} has been moved, so there is nothing to put back.`;
    note.textContent = reading.state === 'refused' ? reading.reason : '';
  }

  #updateParameter(
    nodes: EffectsNodes,
    plugin: PluginRef,
    parameter: EffectParameter,
  ): void {
    const control = pluginControl(plugin, parameter.key);
    const key = controlKey(control);
    const node = nodes.parameters.get(key);
    if (node === undefined) {
      return;
    }

    const reading = this.#pending.read<ParameterValue>(control, parameter.value);
    const shown = valueOf(reading);
    setParameterInputs(node, shown, this.#holding === key);
    for (const input of node.inputs) {
      input.disabled = !this.#connected || parameter.held;
    }
    node.value.textContent = parameterText(node.declared, shown);
    mark(node.inputs[0] ?? node.value, reading, `${plugin.name}’s ${parameter.name}`);
    node.show.textContent = parameter.held ? 'DMX' : effectShowLine(parameter, reading);
    const line = node.value.closest('.live-fact');
    if (line instanceof HTMLElement) {
      holdForDesk(line, parameter.held, '');
    }
    // The server's own sentence, against the control that caused it. It quotes
    // the manifest — "radius_px must be a number between 0 and 64" — and a
    // paraphrase would be a range this client made up.
    node.refusal.textContent = reading.state === 'refused' ? reading.reason : '';
  }

  /** Puts one plugin instance back to what the show configured. */
  #putPluginBack(plugin: PluginRef): void {
    void this.#send(pluginReset(plugin), pluginControl(plugin, null));
  }

  /** What this server lets an operator do with one kind of module's parameters. */
  #endpointNote(what: 'effect' | 'generator'): HTMLElement {
    const control = parameterControl(what);
    const note = make('p', 'work-row-note', control.why);
    note.classList.add('hint');
    return note;
  }

  /**
   * A button that opens Settings at one thing, or says why it cannot.
   *
   * Disabled when the server has no show open, because there is then nothing
   * to open the editor at and a button that pretended otherwise would take an
   * operator somewhere empty mid-show.
   */
  #editButton(label: string, at: Omit<EditTarget, 'showId'>): HTMLButtonElement {
    // Worked out when it is pressed rather than when it is built. The OPTIONS
    // panel outlives many views, and a show opened after this button was
    // made is a show this button should be able to open.
    const node = button(
      label,
      () => {
        const target = editTarget(this.#view, at);
        if (target === null) {
          this.#note('The server has no show open, so there is nothing to open the editor at.');
          return;
        }
        this.#openEditor(target);
      },
      {
        className: 'small',
        title: `Show ${at.what} in Settings. Nothing on stage changes: the editor holds a copy of the show, and only a save reaches the server.`,
      },
    );
    node.disabled = this.#view.showId === null;
    return node;
  }

  // --- what an operator does ------------------------------------------------

  /**
   * The length and rate of what a slot plays, where this client knows them.
   *
   * From the library listing, which carries both. `null` for a generator, for a
   * clip this client has not listed yet, and for an item whose length the
   * server never worked out — in all three cases there is no arithmetic to do
   * and nothing to say about it.
   */
  #clipOf(slot: SlotView): { durationMs: number; fps: number } | null {
    const source = slot.source;
    if (source.kind !== 'media') {
      return null;
    }
    const item = this.#library.find((candidate) => candidate.id === source.mediaItemId);
    const durationMs = item?.duration_ms ?? null;
    if (durationMs === null || durationMs <= 0) {
      return null;
    }
    const rate = item?.frame_rate ?? null;
    const fps = rate === null || rate.denominator === 0 ? 0 : rate.numerator / rate.denominator;
    return fps > 0 ? { durationMs, fps } : null;
  }

  #layer(layerId: string): LayerView | null {
    return this.#view.layers.find((candidate) => candidate.id === layerId) ?? null;
  }

  /**
   * Selects a slot, which is a trigger.
   *
   * `null` means the one that is already playing, which is a retrigger: nothing
   * about the slot index changes and the server's trigger counter is the only
   * thing that says it happened (`OvsState::switch_now`).
   */
  #trigger(layerId: string, slotIndex: number | null): void {
    const layer = this.#layer(layerId);
    if (layer === null) {
      return;
    }
    // **This layer's slots are a console's** (owner, 2026-10-16 and
    // 2026-10-19). Refused here as well as by the server, so nothing is sent
    // and nothing flashes as refused: a picture on a screen is easy to press
    // by accident, and a press that travels and comes back rejected is still a
    // press that looked like it did something.
    //
    // Read from the current view rather than from whatever the pad was built
    // with, because the patch and the switch can change while the matrix is on
    // screen.
    if (!layer.mayChooseSlot) {
      this.#note(
        `${layer.name} takes its slot from the lighting console while DMX is on — switch DMX off to select here, or set its slots to GUI in Settings`,
      );
      return;
    }
    const index = slotIndex ?? layer.activeSlot;
    // Putting a slot on stage makes it the one OPTIONS is about (owner,
    // 2026-09-23): the slot an operator has just started is the slot they are
    // about to want the settings of, and asking them to press its name as well
    // is asking them to say the same thing twice.
    //
    // Slot 0 is the exception: Black has nothing to set up, so it plays without
    // taking the panel away from whatever was being worked on.
    if (index !== 0) {
      this.#chooseSlot(layerId, index);
    }
    void this.#send({ do: 'select_slot', layerId, slotIndex: index }, { of: 'slot', layerId });
  }

  /**
   * Makes one slot the subject of OPTIONS, and no other.
   *
   * **Exactly one name is lit anywhere in the matrix** (owner, 2026-09-23), so
   * this is the only place a slot becomes the subject: one field holding one
   * layer and one index cannot describe two lit pads.
   */
  #chooseSlot(layerId: string, slotIndex: number): void {
    this.#selected = layerId;
    this.#selectedSlot = slotIndex;
    this.#work = chooseTab(this.#work, 'options').area;
    this.#render();
  }

  // --- the library, for names and pictures ----------------------------------

  /**
   * The display names of everything in the library.
   *
   * Asked once per connection, like the editor's lists, and again whenever the
   * MEDIA screen changes it ([`reloadLibrary`](LivePanel#reloadLibrary)). A
   * slot holds a media id and nothing else; without this the matrix would
   * label a pad with a ULID, which is exactly the physical-identity leak
   * the pipeline exists to prevent.
   */
  /**
   * Reads the library again, because it has changed.
   *
   * Live Mode reads it once per connection, which is right for something that
   * only grows while nobody is looking — and wrong the moment an operator
   * deletes something on the MEDIA screen. Without this the BROWSER tab goes
   * on listing media the server no longer has, and the pads go on naming it,
   * for as long as the client stays signed in.
   */
  reloadLibrary(): void {
    void this.#loadLibrary();
  }

  async #loadLibrary(): Promise<void> {
    const answer = await this.#ovs.mediaLibrary();
    if (!answer.ok) {
      // Not worth interrupting an operator for. The pads fall back to their slot
      // numbers, which is what they were before names existed, and the BROWSER
      // tab says the library could not be read rather than that it is empty.
      this.#library = [];
      this.#render();
      return;
    }
    this.#library = answer.value.items;
    this.#libraryFolders = answer.value.folders;
    this.#names.clear();
    for (const item of answer.value.items) {
      this.#names.set(item.id, item.display_name);
    }
    this.#askForThumbnails();
    this.#render();
  }

  /**
   * What this server publishes, for reading a parameter against its manifest.
   *
   * Asked once per connection, as the editor asks. A parameter value on its own
   * is a number with no name, no range and no unit: only the manifest that
   * declared it says what it is, and a client that guessed would be inventing
   * one. Until this arrives, the GENERATOR and EFFECTS tabs say so rather than
   * showing bare keys.
   */
  async #loadCatalogues(): Promise<void> {
    const answer = await this.#ovs.catalogues();
    if (!answer.ok) {
      this.#catalogue = emptyCatalogue();
      this.#render();
      return;
    }
    this.#catalogue = { loaded: true, effects: answer.value.effects, generators: answer.value.generators };
    this.#render();
  }

  /** Asks for the picture of every media item a slot refers to, once each. */
  #askForThumbnails(): void {
    for (const layer of this.#view.layers) {
      for (const slot of layer.slots) {
        if (slot.source.kind === 'media') {
          this.#askForThumbnail(slot.source.mediaItemId);
        }
      }
    }
  }

  /**
   * One item's picture, asked for once.
   *
   * The browser asks as it draws a row rather than for the whole library at
   * once: a library of a few hundred items is a few hundred requests, and the
   * ones worth making are for what is actually on screen.
   */
  #askForThumbnail(mediaItemId: string): void {
    if (!this.#connected || this.#asked.has(mediaItemId)) {
      return;
    }
    this.#asked.add(mediaItemId);
    void this.#ovs.mediaThumbnail(mediaItemId).then((thumbnail) => {
      this.#thumbnails.set(mediaItemId, thumbnail);
      this.#render();
    });
    // And the moving one, in the same breath. A clip has both or neither, and a
    // second round trip when the pointer arrives would mean a pad that does
    // nothing for the first half second somebody hovers it.
    void this.#ovs.mediaStrip(mediaItemId).then((strip) => {
      this.#strips.set(mediaItemId, strip);
      this.#render();
    });
  }

  // --- the preview work area ------------------------------------------------
  //
  // Two paths to one panel, exactly as before: a socket of encoded pictures
  // decoded here, and the composition as a PNG for a server that will not stream
  // at all. Whichever is not on screen is stopped, because both cost a machine
  // that is running a show — and neither can affect what is on the wall
  // (house rule §17).

  /**
   * The rail beside the picture: which surface it is showing.
   *
   * Upright tabs rather than a row of buttons in the panel's head (owner,
   * 2026-09-23): the head's row was a row of the picture's own height, and the
   * question was being asked in two places at once.
   */
  #drawSurfaces(): void {
    this.#ui.surfaces.replaceChildren(
      ...this.#surfaces.map((entry) => {
        const node = button(entry.name.toUpperCase(), () => this.#watch(entry.id), {
          className: `tab${entry.id === this.#surface ? ' on' : ''}`,
          title: `Watch ${entry.name}`,
        });
        node.dataset['surface'] = entry.id;
        return node;
      }),
    );
  }

  #watch(next: string): void {
    this.#surface = next;
    this.#drawSurfaces();
    // Both previews go, because both are showing the surface that was being
    // watched a moment ago and neither can be relabelled.
    this.#stopStream();
    this.#stopPreview();
    this.#syncStream();
  }

  #syncStream(): void {
    if (this.#connected && this.#visible) {
      this.#startStream();
    } else {
      this.#stopStream();
    }
    this.#syncPreview();
  }

  #startStream(): void {
    if (this.#stream !== null) {
      return;
    }
    if (this.#origin === null) {
      // A window that was reloaded while the session carried on: the main
      // process still has the server, this page no longer knows where it is.
      this.#ui.previewNote.textContent = 'Sign in from this window to see the live preview.';
      return;
    }

    this.#canvas ??= this.#ui.previewCanvas.getContext('2d');
    this.#player = new StreamPlayer({
      onFrame: (frame) => this.#paint(frame),
      onStatus: (status) => this.#drawPreviewStatus(status),
    });
    this.#drawPreviewStatus(this.#player.status);

    // The surface this stream is for, fixed now. Minting is asynchronous and a
    // ticket is minted for one surface: if the operator switches panels while
    // one is in flight, the ticket that comes back must still be the one this
    // socket was opened for.
    const watching = this.#surface;
    this.#stream = openStream({
      url: streamUrl(this.#origin, watching),
      credential: () => this.#ovs.streamCredential(watching),
      onChunk: (chunk) => this.#player?.feed(chunk),
      onEncoding: (encoding) => this.#player?.setEncoding(encoding),
      onError: (reason) => this.#player?.failed(reason),
      onClose: (reason) => this.#player?.failed(reason),
    });
  }

  #stopStream(): void {
    this.#stream?.close();
    this.#stream = null;
    this.#player?.close();
    this.#player = null;
  }

  #paint(frame: VideoFrame): void {
    if (this.#canvas === null) {
      return;
    }
    // The canvas is sized in pixels by its attributes rather than by the CSS
    // that fits it to the panel: left at its default every picture would be
    // drawn small and blown up again by the browser.
    if (this.#ui.previewCanvas.width !== frame.displayWidth) {
      this.#ui.previewCanvas.width = frame.displayWidth;
    }
    if (this.#ui.previewCanvas.height !== frame.displayHeight) {
      this.#ui.previewCanvas.height = frame.displayHeight;
    }
    this.#canvas.drawImage(frame, 0, 0, frame.displayWidth, frame.displayHeight);
  }

  #drawPreviewStatus(status: StreamStatus): void {
    this.#ui.previewNote.textContent = statusLine(status);
    this.#syncPreview();
  }

  /** Puts whichever preview has a picture on screen, and stops the other one. */
  #syncPreview(): void {
    const playing = this.#player?.status.state === 'playing';
    this.#ui.previewCanvas.hidden = !playing;
    this.#ui.preview.hidden = playing;

    if (this.#connected && this.#visible && !playing) {
      this.#startPreview();
    } else {
      this.#stopPreview();
    }
  }

  #startPreview(): void {
    if (this.#previewTimer !== null) {
      return;
    }
    const tick = async (): Promise<void> => {
      // The surface the panel is pointed at, not always the composition: this
      // is the fallback for a server that will not stream, and a picture of the
      // composition under a heading that says OUTPUT 2 is worse than none.
      const png = await this.#ovs.preview(this.#surface);
      if (png === null) {
        this.#ui.preview.removeAttribute('src');
      } else {
        this.#ui.preview.src = png;
      }
    };
    void tick();
    this.#previewTimer = window.setInterval(() => void tick(), PREVIEW_INTERVAL_MS);
  }

  #stopPreview(): void {
    if (this.#previewTimer !== null) {
      window.clearInterval(this.#previewTimer);
      this.#previewTimer = null;
    }
    this.#ui.preview.removeAttribute('src');
  }
}

// --- small drawing helpers ---------------------------------------------------

/** What a command asks for, for the control it is about. */
function wantedOf(command: LiveCommand, control: ControlId): unknown {
  switch (control.of) {
    case 'slot':
      return command.do === 'select_slot' ? command.slotIndex : null;
    case 'master':
      return command.do === 'set_master' ? command[control.field] : null;
    case 'layer':
      return command.do === 'set_layer' ? command[control.field] : null;
    case 'effect':
      if (command.do !== 'set_effect') {
        return null;
      }
      // The reset control asks for one thing and it is not a value: that this
      // instance goes back to what the show configured.
      return control.parameter === null
        ? command.reset === true
        : (command.overrides?.[control.parameter] ?? null);
    case 'generator':
      // **The case that was missing.** Identical to the effect above, because
      // an effect and a generator are one parameter model (owner, 2026-09-23) --
      // and its absence was invisible while the generator's controls lived in a
      // tab the rail no longer offered. Moving them into OPTIONS is what
      // reached it, not what broke it.
      if (command.do !== 'set_generator') {
        return null;
      }
      return control.parameter === null
        ? command.reset === true
        : (command.overrides?.[control.parameter] ?? null);
    default: {
      // **A sixth kind of control is a compile error here, not a dead panel.**
      // The return type is `unknown`, so a switch with a hole in it compiles
      // and answers `undefined` -- which reads as a value everywhere that
      // checks for `null`. This is the only thing that makes the omission
      // impossible rather than unlikely.
      const unreachable: never = control;
      return unreachable;
    }
  }
}

/**
 * Puts a fader where the server says, unless the operator is holding it.
 *
 * A fader the server has said nothing about is left alone rather than dropped to
 * its minimum: a dimmer sitting at zero is a claim about the stage.
 */
/**
 * Writes an opacity reading onto the bar: how far the orange goes, and the
 * number on both faces of it.
 *
 * One place, because the bar is written from two — the drag, which must not
 * wait for the server, and the update, which must not fight the drag.
 */
function paintOpacity(
  fill: HTMLElement,
  values: readonly HTMLElement[],
  reading: Reading<number>,
): void {
  const value = valueOf(reading);
  if (value !== null) {
    fill.style.setProperty('--fill', `${Math.round(value * 1000) / 10}%`);
  }
  const text = percentLine(reading);
  for (const node of values) {
    node.textContent = text;
  }
}

/**
 * How far a bar's fill goes, for a value in `0..=1` growing out of `zero`.
 *
 * The same shape `paintFader` writes, for the two faders in the master row that
 * are not built by `fader`: they are in the page rather than made here, because
 * the master row is one line of markup an operator's eye runs along and the
 * order of it is not this panel's to decide.
 */
function fillFrom(fill: HTMLElement, value: number, zero: number): void {
  const at = Math.min(1, Math.max(0, value)) * 100;
  const from = zero * 100;
  fill.style.setProperty('--from', `${Math.min(at, from)}%`);
  fill.style.setProperty('--to', `${Math.max(at, from)}%`);
}

function setFader(input: HTMLInputElement, reading: Reading<number>, held: boolean): void {
  if (held) {
    return;
  }
  const value = valueOf(reading);
  if (value === null) {
    return;
  }
  input.value = String(Math.round(value * FADER_STEPS));
}

/**
 * Lights the speed button the show is actually running at.
 *
 * Nearest by ratio rather than exactly equal: the server stores an `f64` and a
 * desk sends a band, so a value that arrives as 0.9999999 is still `1x` and
 * should light the button that says so.
 */
function setSpeedSteps(
    steps: ReadonlyMap<number, HTMLButtonElement>,
    reading: Reading<number>,
): void {
  const value = valueOf(reading);
  const on = value === null ? null : nearestSpeedScale(value);
  for (const [step, node] of steps) {
    node.classList.toggle('on', on !== null && step === on);
    node.classList.toggle('unknown', on === null);
  }
}


/** Marks a control with what its reading is doing, and says so in its tooltip. */
function mark(node: HTMLElement, reading: Reading<unknown>, what: string): void {
  node.classList.toggle('unknown', reading.state === 'unknown');
  node.classList.toggle('pending', reading.state === 'pending');
  node.classList.toggle('refused', reading.state === 'refused');
  node.title = readingNote(reading, what);
}

/** What the server itself last said, for the line beside a control. */
function reportedLine(reading: Reading<number>, format: (value: number) => string): string {
  if (reading.state !== 'pending' && reading.state !== 'refused') {
    return '';
  }
  const reported = reportedBy(reading);
  return reported === null ? 'server: —' : `server: ${format(reported)}`;
}

/** A stable name for one chain of one layer, for finding its row again. */
function chainKey(chain: ChainLine): string {
  return chain.where === 'layer' ? `${chain.layerId}.layer` : `${chain.layerId}.slot.${chain.slotIndex}`;
}

/**
 * What the EFFECTS panel was built for.
 *
 * Everything that decides which controls there are and what each one is: the
 * layer, the chains, the instances in them and the parameters each declares.
 * Anything else — a value moving, a chain going on air — is an update rather
 * than a rebuild, because a rebuild takes a fader out from under a finger.
 */
/** A stable key for one generator slot, which is the slot it is in. */
function generatorKey(line: GeneratorLine): string {
  return `${line.layerId}/${line.slotIndex}`;
}

/**
 * What the GENERATOR tab was built for, so it is rebuilt only when that changes.
 *
 * The identities and the parameter keys, and not the values: a rebuild on a
 * value would take a fader out from under an operator's finger every time the
 * server answered.
 */
function generatorsShape(lines: readonly GeneratorLine[], loaded: boolean): string {
  const slots = lines.map((line) =>
    [
      generatorKey(line),
      line.instanceId ?? 'no-instance',
      line.typeId,
      line.published ? 'published' : 'unpublished',
      line.locked ? 'locked' : 'free',
      line.parameters.map((parameter) => parameter.key).join('+'),
    ].join(':'),
  );
  return [loaded ? 'catalogue' : 'no-catalogue', ...slots].join('|');
}

function effectsShape(layerId: string, chains: readonly ChainLine[], loaded: boolean): string {
  const effects = chains.map((chain) =>
    [
      chainKey(chain),
      ...chain.effects.map((effect) =>
        [
          effect.id,
          effect.typeId,
          effect.published ? 'published' : 'unpublished',
          effect.enabled ? 'on' : 'off',
          // The lock decides which of the row's buttons are offered, which is
          // drawn when the row is built.
          effect.locked ? 'locked' : 'free',
          effect.parameters.map((parameter) => parameter.key).join('+'),
        ].join(':'),
      ),
    ].join('/'),
  );
  return [layerId, loaded ? 'catalogue' : 'no-catalogue', ...effects].join('|');
}

/**
 * Puts an effect parameter's control where the value is, unless it is being used.
 *
 * The same rule as `setFader`, and for the same reason: a control taken back
 * from under an operator's finger is worse than one that is a moment out of
 * date. A number field is left alone while it has focus as well, because a
 * field that rewrites itself while somebody is typing in it cannot be typed in.
 */
function setParameterInputs(
  nodes: ParameterNodes,
  value: ParameterValue | null,
  held: boolean,
): void {
  if (held || value === null) {
    return;
  }
  const { inputs, bars } = nodes;
  const kind = nodes.declared.parameter_type.kind;
  switch (kind) {
    case 'number': {
      const input = inputs[0];
      if (input !== undefined && (value.type === 'float' || value.type === 'int')) {
        input.value = String(value.value);
        // And the fill, which the input does not draw. Without this the orange
        // stays where the last drag left it while the number says otherwise.
        const track = bars[0];
        if (track) {
          paintBar(track, value.value);
        }
      }
      return;
    }
    case 'toggle': {
      const input = inputs[0];
      if (input instanceof HTMLInputElement && value.type === 'bool') {
        input.checked = value.value;
      }
      return;
    }
    case 'colour': {
      if (value.type !== 'color') {
        return;
      }
      // Two controls now, not four channels: the swatch carries the colour and
      // the field beside it carries alpha. Each left alone while it has focus,
      // for the reason the text box below is.
      const [swatch, alpha] = inputs;
      if (swatch !== undefined && document.activeElement !== swatch) {
        swatch.value = hexFromLinear(value.value);
      }
      const opacity = value.value[3] ?? 1;
      const track = bars[1];
      if (alpha !== undefined && document.activeElement !== alpha) {
        alpha.value = String(opacity);
        if (track) {
          paintBar(track, opacity);
        }
      }
      return;
    }
    case 'text':
    case 'image':
    case 'choice': {
      // Left alone while it has focus, like a colour channel and for the same
      // reason: a box that rewrites itself under somebody typing in it cannot
      // be typed in, and a menu that reopens at the server's value takes the
      // choice away mid-gesture.
      const input = inputs[0];
      // A picture is carried as text, like the words in a caption: the kind
      // says what the string means and the value is a string either way.
      const wanted = kind === 'choice' ? value.type === 'choice' : value.type === 'text';
      if (input !== undefined && wanted && document.activeElement !== input) {
        input.value = String(value.value);
      }
      return;
    }
    default: {
      // **The same hole `wantedOf` had, and silent here because this returns
      // `void`:** a kind with no case is simply never written to, so the
      // control sits at whatever it was built with while the server says
      // something else.
      const unreachable: never = kind;
      return unreachable;
    }
  }
}

/**
 * The line under an effect control: the show's value, and the server's own.
 *
 * The show's is always there, because "what have I changed" is the question
 * an operator is asking when they open this tab. The server's own appears only
 * while a request is out, for the same reason `reportedLine` shows it on the
 * master: the control is showing where the operator put it, and this is what is
 * on stage.
 */
function effectShowLine(parameter: EffectParameter, reading: Reading<ParameterValue>): string {
  const show = `Show: ${parameter.configuredText}${parameter.moved ? ' · moved' : ''}`;
  if (reading.state !== 'pending' && reading.state !== 'refused') {
    return show;
  }
  const reported = reportedBy(reading);
  return `${show} · server: ${reported === null ? '—' : parameterText(parameter.declared, reported)}`;
}

/**
 * One instance's parameters as they stand, read-only.
 *
 * Read-only is the whole point and it is marked rather than implied: a value
 * the show set is drawn plainly, one taken from the manifest's own default
 * is dimmed and says so, and neither is a control. There is nowhere to send a
 * change to (`parameterControl`), and writing one into the document from here
 * would be an edit made from the live screen.
 */
function parameterGrid(parameters: readonly ParameterLine[]): HTMLElement {
  const grid = make('div', 'work-parameters');
  if (parameters.length === 0) {
    return make('p', 'work-row-note', 'This one declares no parameters.');
  }
  for (const parameter of parameters) {
    const name = make('span', `work-parameter${parameter.set ? '' : ' default'}`, parameter.name);
    const value = make('span', 'work-parameter-value', parameter.text);
    value.title = parameter.set
      ? 'What the show sets. Changed in Settings.'
      : 'What the module was published with. The show sets nothing here.';
    // The dim marker belongs to the pair, so the grid's own class carries it.
    if (!parameter.set) {
      name.classList.add('default');
      value.classList.add('default');
    }
    grid.append(name, value);
  }
  return grid;
}

/**
 * One line of the OPTIONS tab: a name, the thing, and what the show says.
 *
 * `show` is the second line under a control that is live. Its presence is
 * what distinguishes the four live values from the rest of the list: a layer's
 * `default_opacity` is not its opacity, and an operator halfway through a show
 * has to be able to see which of the two they are looking at.
 */
function optionRow(
  label: string,
  control: HTMLElement,
  show?: HTMLElement,
  value?: HTMLElement,
): HTMLElement {
  const line = make('div', show === undefined ? 'live-fact' : 'live-fact live');
  // The control and the number beside it are one cell, so the line stays two
  // columns however many pieces the control itself is made of.
  const cell = make('div', 'option-control');
  cell.append(control);
  if (value !== undefined) {
    cell.append(value);
  }
  line.append(make('span', 'live-fact-label', label), cell);
  if (show !== undefined) {
    line.append(show);
  }
  return line;
}

/** `2nd`, `3rd`, for where a layer sits in the composite. */
function ordinal(value: number): string {
  const suffix = value % 100 >= 11 && value % 100 <= 13
    ? 'th'
    : ({ 1: 'st', 2: 'nd', 3: 'rd' } as Record<number, string>)[value % 10] ?? 'th';
  return `${value}${suffix}`;
}

/**
 * NDI's own mark, from the brand assets in the SDK.
 *
 * The light one, because everything it sits on here is dark: the SDK names its
 * two by the colour of the mark rather than of what is behind it.
 */
const NDI_MARK = '../assets/ndi.svg';

/**
 * Puts the NDI mark in a box, as the picture of a source on the network.
 *
 * Shown whole and never cropped — `object-fit: contain` does that — because
 * the mark is 1047 by 550 against a 16:9 box and a crop is an alteration,
 * which is not ours to make.
 */
function paintNdiMark(target: HTMLElement): void {
  const existing = target.firstElementChild;
  if (existing instanceof HTMLImageElement && existing.getAttribute('src') === NDI_MARK) {
    return;
  }
  const mark = make('img', 'ndi-mark');
  mark.src = NDI_MARK;
  mark.alt = 'NDI';
  // The pad or the tile carries the drag; an image that started its own would
  // hand the target a picture instead of a source.
  mark.draggable = false;
  target.replaceChildren(mark);
}

/**
 * Paints what a published module looks like into a box, from a file shipped
 * with this client.
 *
 * **A committed picture rather than a request or a drawing.** What a gradient
 * looks like is the renderer's business, so a copy drawn here in TypeScript
 * would eventually be a picture of something the wall does not show; and
 * asking the server for one would have a show machine drawing ten
 * compositions because somebody opened a list of names. So the renderer draws
 * them once — `ovs-generator-pictures` and `ovs-effect-pictures` — and they
 * travel with the client. A Rust test fails if one stops matching what its
 * module draws.
 *
 * By type alone, because there is one module per type and so one picture of
 * it: "TV Glitch V2 ist TV Glitch" (owner, 2026-10-18). A pad saved with
 * `version: 2` shows the same picture as that generator's tile in the browser.
 * A server offering a module this client has no file for falls back to
 * the name on a plain field, which is the tile these had before the pictures
 * existed; the box remembers which module that was, so a repaint does not
 * fetch the missing file again.
 */
function paintModulePicture(
  target: HTMLElement,
  kind: 'generators' | 'effects',
  source: { typeId: string },
  name: string,
  markClass: string,
): void {
  // `encodeURIComponent` because a type id is whatever the server says it is,
  // and this is about to be part of a path into the client's own bundle. A
  // published id is `ovs.noise`, which it leaves alone.
  const src = `../assets/${kind}/${encodeURIComponent(source.typeId)}.png`;
  if (target.dataset.noPictureFor === src) {
    return;
  }
  const existing = target.firstElementChild;
  if (existing instanceof HTMLImageElement && existing.getAttribute('src') === src) {
    return;
  }

  const drawn = make('img');
  drawn.src = src;
  drawn.alt = '';
  // The tile or the pad carries the drag; an image that started its own would
  // hand the target a picture instead of a source.
  drawn.draggable = false;
  drawn.addEventListener('error', () => {
    const box = drawn.parentElement;
    if (box === null) {
      return;
    }
    box.dataset.noPictureFor = src;
    box.classList.add('mark-only');
    box.replaceChildren(make('span', markClass, name));
  });

  delete target.dataset.noPictureFor;
  target.classList.remove('mark-only');
  target.replaceChildren(drawn);
}

/** A stable key for what a slot plays, so a pad is rebuilt when its source changes. */
function sourceKey(slot: SlotView | null): string {
  if (slot === null) {
    return '';
  }
  if (slot.source.kind === 'media') {
    return `m:${slot.source.mediaItemId}`;
  }
  return slot.source.kind === 'ndi' ? `n:${slot.source.name}` : `g:${slot.source.typeId}`;
}
