// The contextual tools in Live Mode's work area, and what each of them *is*.
//
// The reference drawing puts two strips of tabs along the bottom band: one on
// the preview box — COMPOSITION and PIECES — and one on the work panel beside
// it — BROWSER, GENERATOR, OPTIONS and EFFECTS. The
// screen is `ui/live.ts`; everything that can be decided without a window is
// decided here.
//
// The decision this file exists to make is the one those names invite an
// implementer to get wrong. **Live Mode is live control, not editing.** Several
// of those tabs are named after things that are show configuration — a slice
// is a `Piece`, a generator's parameters are a slot's configuration, an effect
// chain is a list in the document — and a tab that quietly wrote to the show
// would break the rule the whole client is built on (
// §35). So every tab declares which of three things it is, and the screen says
// it out loud:
//
//   - **a view** — it shows what is there and changes nothing;
//   - **live control** — it changes live state, which is what this screen is
//     for, and everything it sends is a `LiveCommand`;
//   - **a way into Settings** — it shows what is configured and offers to open the
//     editor at that thing, which is honest and is not the same as editing here.
//
// **One thing on this screen does write to the show**, and it is new:
// assigning a slot. The owner moved it here on 2026-09-23 — a clip is
// dragged from the browser onto a pad, which is what the gesture is for —
// and the server grew an endpoint that does exactly that and nothing else
// (`slot_routes`): one field of one slot, without reloading the show. So
// there is a fourth kind, `assigns`, and BROWSER is it. It is not a hole in
// the rule that live control and editing are separate: it is one named
// exception with its own route, its own confirmation and its own ceiling,
// and everything else about a slot is still the editor's.
//
// "Exposed effect parameters" are live state, and the protocol names
// them `effect.<instance>.parameter.<name>` and
// `generator.<instance>.parameter.<name>` in the parameter registry. The two are
// no longer in the same position. This server publishes
// `PUT /live/effects/{effect_instance_id}`, so EFFECTS is live control and sends
// `set_effect`. It publishes nothing that sets a generator's parameter — `/live`
// carries the layer, the slot, the master, the effects, the tap and the phase,
// and nothing else — so GENERATOR still says so in full rather than offering a
// control that would either do nothing or, far worse, write the value into the
// show and call it live.

import type {
  MediaItem,
  ModuleParameter,
  ParameterValue,
  Piece,
  RectPx,
  SizePx,
  SourceRef,
} from '@ovs/schema';

import type { LiveCommand } from './live.js';
import {
  describeOverlap,
  drawOrder,
  findOutput,
  isOffScreen,
  isPixelExact,
  overlaps,
  sequenceOf,
  type Mapping,
} from './mapping.js';
import { BLACK_SLOT_INDEX, MAX_BANKS, MAX_SLOT_INDEX } from './limits.js';
import { newestFirst } from './media.js';
import {
  findEffect,
  findGenerator,
  parameterValue,
  valueInForce,
  type Catalogue,
  type Module,
} from './modules.js';
import { COMPOSITION_SURFACE, MASKED_SURFACE } from './stream.js';
import type { SlotSource } from './types.js';
import type { EffectView, LayerView, ShowView } from './view.js';

// --- what a tab is ------------------------------------------------------------

/**
 * What the preview box is pointed at.
 *
 * Gone as a *tab* (owner, 2026-09-23): "es soll nur noch Composition und Pieces
 * (Mapped ohne overlay) exisitieren." There was a rail of two beside the
 * preview and a row of surface buttons above it, and between them they asked
 * the same question twice — with the rail's second answer drawing outlines over
 * the picture, which is the overlay the owner took out.
 *
 * What is left is the surfaces, which the server actually sends: the
 * composition, and the composition with everything not mapped onto a wall taken
 * out. That second one is what Pieces means now.
 */

/**
 * Which contextual tool the work panel is showing, along the second strip.
 *
 * Four browsers and one editor (owner, 2026-09-23). The four are all the same
 * gesture — find a thing, drag it onto a pad — and they were one tab holding
 * three kinds of thing plus a tab that described generators without offering
 * them. Split, each one holds one kind and nothing else, which is what makes
 * searching in it worth anything.
 *
 * `options` is the odd one and sits apart from them for that reason: it does
 * not offer anything to drag, it edits whatever is selected.
 */
export type WorkTab = 'inputs' | 'media' | 'generators' | 'effects' | 'options';

/** The four that are browsers, in the order the 2x2 reads. */
export const BROWSER_TABS: readonly WorkTab[] = ['inputs', 'media', 'generators', 'effects'];

/**
 * What a tab is, which is the question to answer before drawing one.
 *
 * `into-edit` is not a weaker kind of `live`. It is the honest answer for a tab
 * named after configuration: showing what is set and offering to open the
 * editor at it is useful, and it is not an edit made from the live screen.
 *
 * `assigns` is the one that does write to the show, and it says so rather
 * than hiding behind one of the other three. It means exactly one thing: it
 * puts a source in a slot, through the endpoint built for doing that with a
 * show running. Nothing else about the slot, the layer or the document moves,
 * and the show does not reload.
 */
export type TabKind = 'view' | 'live' | 'into-edit' | 'assigns';

/** One tab, and the promise it makes. */
export interface TabDefinition<Id extends string> {
  id: Id;
  /** As the drawing letters it. */
  label: string;
  kind: TabKind;
  /** What it is, in the words the tab itself puts on screen. */
  says: string;
  /**
   * The live commands this tab may send.
   *
   * Empty for every tab that is not live control, and that emptiness is the
   * point: it is the machine-checkable half of "choosing a tab changes nothing
   * about the show". Anything named here is still checked by `isLiveCommand` on
   * the one path that reaches a server.
   */
  sends: readonly LiveCommand['do'][];
}

/**
 * The preview box's two tabs.
 *
 * Both are views of the same picture. The drawing captions that box
 * "COMPOSITION/SLICES PREVIEW/STREAM", so these choose its subject rather than
 * opening a tool: PIECES draws the mapping over whatever surface is being
 * watched instead of fetching a second picture of it.
 */

/** The work panel's tabs: four browsers, then the editor. */
export const WORK_TABS: readonly TabDefinition<WorkTab>[] = [
  {
    id: 'inputs',
    label: 'INPUTS',
    kind: 'assigns',
    // **The trademark notice is here because this is where the name is
    // used.** The NDI SDK licence allows the name only to say a product is
    // compatible with NDI, and asks for a clear notation that the mark is
    // theirs wherever it is used; the registered designation goes on the first
    // use in a view and not on every one. This panel is where an operator
    // meets the word, so this is where it says whose it is. Nothing here
    // claims a relationship with them, because there is not one.
    says:
      'Live sources this machine can take. Cameras and other senders arrive here over NDI\u00AE as they are ' +
      'switched on. ' +
      'Capture cards and a page are not part of this build, and neither is faked. ' +
      'NDI\u00AE is a registered trademark of Vizrt NDI AB.',
    sends: [],
  },
  {
    id: 'media',
    label: 'MEDIA',
    kind: 'assigns',
    says:
      'The clips the server holds, and where each is used in the open show. ' +
      'Drag one onto a pad to put it there — an occupied pad asks first. Nothing else about the slot changes.',
    sends: [],
  },
  {
    id: 'generators',
    label: 'GENERATORS',
    kind: 'assigns',
    says:
      'What this server can draw without a file. Drag one onto a pad; its own settings are then in OPTIONS.',
    sends: [],
  },
  {
    id: 'effects',
    label: 'EFFECTS',
    kind: 'assigns',
    says:
      'What this server can put on top of a picture. Drag one onto a pad for that clip alone, or onto a layer’s picture for the whole layer. ' +
      'It never replaces anything — effects stack.',
    sends: [],
  },
  {
    id: 'options',
    label: 'OPTIONS',
    kind: 'live',
    says:
      'Whatever is selected: a layer, or one of its slots. Click a pad’s name to choose the slot; click the layer’s name to choose the layer.',
    sends: ['set_layer', 'set_effect'],
  },
];

// --- which tab is open --------------------------------------------------------

/**
 * Which tab of each strip is open.
 *
 * The one piece of state in the work area that is the client's own. It is not
 * show state and it is not document state: it is where the operator was
 * looking, so it survives a reconnection untouched — see [`afterReconnect`].
 */
export interface WorkArea {
  readonly tab: WorkTab;
}

/** What the panel opens on: the composition, and the clips. */
export function openWorkArea(): WorkArea {
  return { tab: 'media' };
}

/**
 * The result of touching the work area.
 *
 * `commands` is always empty and is here so that a test can hold it empty.
 * Choosing a tab is the operator deciding what to look at, and a live screen
 * that sent something when an operator looked at something would be a live
 * screen whose show depended on where its operator's eyes were.
 */
export interface WorkChange {
  area: WorkArea;
  commands: readonly LiveCommand[];
}

/** Opens one of the work panel's tools. Sends nothing. */
export function chooseTab(area: WorkArea, tab: WorkTab): WorkChange {
  return { area: { ...area, tab }, commands: [] };
}


/**
 * What the work area does when the connection comes back.
 *
 * Nothing. A reconnection replaces everything the server said — the mirror is
 * rebuilt from a fresh snapshot and `PendingControls` is emptied, because those
 * are facts about a server this client was no longer talking to. Which tab an
 * operator had open is not one of those facts, and re-sending anything to
 * restore it would be this client putting traffic on a socket that has only
 * just come back.
 */
export function afterReconnect(area: WorkArea): WorkChange {
  return { area, commands: [] };
}

// --- a way into Settings ----------------------------------------------------------

/**
 * Something in the show to open the editor at.
 *
 * A request to *look* at a thing in Settings, not a change to it. It names what the
 * operator pressed and nothing else; what happens to the document, including
 * refusing to throw away unsaved work, is the editor's own business.
 */
export interface EditTarget {
  showId: string;
  layerId?: string;
  slotIndex?: number;
  pieceId?: string;
  /** What was pressed, for the sentence the client says when it lands. */
  what: string;
}

/**
 * Where in Settings a thing lives, or null when there is no show to open.
 *
 * Null rather than a guess: the server has no show open, so there is nothing
 * named `showId` and a button that pretended otherwise would open the editor
 * at nothing.
 */
export function editTarget(
  view: ShowView,
  at: Omit<EditTarget, 'showId'>,
): EditTarget | null {
  return view.showId === null ? null : { showId: view.showId, ...at };
}

// --- which exposed parameters this client can move ----------------------------

/**
 * Whether this client can change a module's exposed parameters, and how.
 *
 * The parameter registry calls both an effect's and a generator's live values
 *, and this server now publishes a route for one of them
 * and not the other. So the answer is asked per subject rather than stated once:
 * EFFECTS draws controls and names the route it sends to, and GENERATOR prints
 * `why` rather than drawing a slider that would have nowhere to send what an
 * operator did with it.
 */
export interface ParameterControl {
  /** Whether the value is live state at all, as the specification has it. */
  live: boolean;
  /** The route that sets it, or null when this server publishes none. */
  endpoint: string | null;
  /** What the tab says about it, in either case. */
  why: string;
}

/** What this server lets an operator do with one kind of module's parameters. */
export function parameterControl(what: 'effect' | 'generator'): ParameterControl {
  if (what === 'effect') {
    return {
      live: true,
      endpoint: 'PUT /api/v1/live/effects/{effect_instance_id}',
      why:
        'An effect instance’s exposed parameters are live state, named ' +
        'effect.<instance>.parameter.<name> — and this server publishes ' +
        'PUT /api/v1/live/effects/{effect_instance_id} for them. A value moved here sits on top of ' +
        'what the show configured and is never written into it, so the show opens tomorrow at ' +
        'the value it was built with. Putting one back is a reset, not a value.',
    };
  }
  return {
    live: true,
    endpoint: 'PUT /api/v1/live/generators/{generator_instance_id}',
    why:
      'A generator instance’s exposed parameters are live state, named ' +
      'generator.<instance>.parameter.<name> — and this server publishes ' +
      'PUT /api/v1/live/generators/{generator_instance_id} for them. A value moved here sits on ' +
      'top of what the show configured and is never written into it, so the show opens ' +
      'tomorrow at the value it was built with. Putting one back is a reset, not a value.',
  };
}

// --- parameters, as they are set ----------------------------------------------

/**
 * One parameter of one instance, as a screen with no control for it reads it.
 *
 * A narrowing of [`EffectParameter`], and every list of them is built as one of
 * those now: both kinds of module have live controls, so the only caller left
 * is the one case that has values and nothing to move them with — a generator
 * in a show this server has not opened, which therefore has no instance to
 * name in a request.
 */
export interface ParameterLine {
  key: string;
  /** What the manifest calls it. */
  name: string;
  /** The value in force, formatted. */
  text: string;
  /** Whether the show set it, as opposed to taking the published default. */
  set: boolean;
}

/**
 * One value, in the terms the manifest declared it in.
 *
 * A colour is four linear channels and is shown as four numbers, for the reason
 * the editor gives four fields rather than a colour well: the model stores
 * linear RGBA and a swatch would be an sRGB conversion this client has no
 * business making on the operator's behalf.
 */
export function parameterText(parameter: ModuleParameter, value: ParameterValue | null): string {
  if (value === null) {
    // The manifest declares it and neither the show nor the manifest has a
    // value for it. That is a disagreement between the two, not a zero.
    return '—';
  }
  switch (value.type) {
    case 'bool':
      return value.value ? 'on' : 'off';
    case 'int':
    case 'float':
      return `${round(value.value, 4)}${unitOf(parameter)}`;
    case 'text':
      // A picture is carried as text, and no picture is an empty one: shown as
      // the word rather than as a blank, which reads as a missing value. What
      // it shows when there is one is the identifier, because the name belongs
      // to the library and this module has none to ask.
      return parameter.parameter_type.kind === 'image' && value.value === '' ? 'none' : value.value;
    case 'color':
      return value.value.map((channel) => round(channel, 3)).join(', ');
    case 'choice':
      return labelOf(parameter, value.value);
  }
}

/**
 * What to show for a chosen identifier: its published name.
 *
 * The identifier itself when the manifest does not declare it, which is a
 * show written against another build. Showing the raw name is more use than
 * showing nothing, because it is the thing an operator would have to tell
 * somebody about.
 */
function labelOf(parameter: ModuleParameter, id: string): string {
  const type = parameter.parameter_type;
  if (type.kind !== 'choice') {
    return id;
  }
  return type.choices.find((choice) => choice.id === id)?.name ?? id;
}

/** The range a number lives in, for the line beside it, or nothing when it has none. */
function unitOf(parameter: ModuleParameter): string {
  const type = parameter.parameter_type;
  return type.kind === 'number' ? ` (${round(type.min, 4)}–${round(type.max, 4)})` : '';
}

// --- BROWSER ------------------------------------------------------------------

/** One place in the open show that a media item is used. */
export interface SlotUse {
  layerId: string;
  layerName: string;
  slotIndex: number;
  /** Whether that layer is playing that slot right now. */
  onAir: boolean;
}

/** One item of the library, with what the open show does with it. */
export interface LibraryLine {
  item: MediaItem;
  /** Every slot of the open show that plays it. */
  usedBy: SlotUse[];
  /** Whether any layer is playing it at this moment. */
  onAir: boolean;
}

/**
 * The library, newest first, with where the open show uses each item.
 *
 * The usage is the reason this is a browser in Live Mode rather than the media
 * library over again: what an operator wants at a show is "what is this, and
 * where is it in tonight's show", and the second half of that is something
 * only the open show can answer.
 */
export function libraryLines(
  items: readonly MediaItem[],
  layers: readonly LayerView[],
): LibraryLine[] {
  const uses = new Map<string, SlotUse[]>();
  for (const layer of layers) {
    for (const slot of layer.slots) {
      if (slot.source.kind !== 'media') {
        continue;
      }
      const list = uses.get(slot.source.mediaItemId) ?? [];
      list.push({
        layerId: layer.id,
        layerName: layer.name,
        slotIndex: slot.index,
        onAir: layer.activeSlot === slot.index,
      });
      uses.set(slot.source.mediaItemId, list);
    }
  }

  return newestFirst(items).map((item) => {
    const usedBy = uses.get(item.id) ?? [];
    return { item, usedBy, onAir: usedBy.some((use) => use.onAir) };
  });
}


/**
 * The library narrowed to what the operator typed.
 *
 * Matched against the name the library shows and the filename the item came
 * in as, because an operator looking for a clip half-remembers one or the
 * other. Case and surrounding space are ignored; an empty search is every
 * line, in the order it already had.
 *
 * Substring rather than fuzzy: a search that matched things an operator did
 * not type is a search whose results they have to read twice, and this is a
 * thing done during a show.
 */
export function matching(lines: readonly LibraryLine[], query: string): LibraryLine[] {
  const needle = query.trim().toLowerCase();
  if (needle === '') {
    return [...lines];
  }
  return lines.filter((line) => {
    const name = line.item.display_name.toLowerCase();
    const filename = line.item.original_filename.toLowerCase();
    return name.includes(needle) || filename.includes(needle);
  });
}

/**
 * What dragging something onto a pad comes to.
 *
 * Decided here rather than in the window, because it is a rule and not a
 * gesture: which pads can hold anything, and when the operator has to be
 * asked first.
 */
export type DropVerdict =
  /** Send it. The pad was empty. */
  | { do: 'assign' }
  /**
   * Ask first, naming what is there now.
   *
   * The owner's rule: a slot that is already taken asks before it is
   * overwritten (2026-09-23). A pad in a running show may be the one thing
   * the operator is about to press.
   */
  | { do: 'ask'; replacing: string }
  /** It is already playing exactly that. Sending would change nothing. */
  | { do: 'nothing' }
  /** It cannot go there, and this is what to say. */
  | { do: 'refuse'; because: string };

/** What is being dragged, reduced to what the decision needs. */
export interface DropCandidate {
  /** The slot the pointer is over. */
  slotIndex: number;
  /** What is in that slot now, or null when the pad is empty. */
  holding: SlotSource | null;
  /** What the operator is dropping on it. */
  dropping: SlotSource;
  /** What to call what is in it now, in the operator's words. */
  holdingName: string;
}

/**
 * Whether a drop can happen, and whether to ask first.
 *
 * Slot 0 is refused because it is not storage: it is permanently black on
 * every layer and has no configuration anywhere. A slot
 * past the last bank is refused because the server refuses it, and a client
 * that sent one would turn the owner's ceiling into a failed request.
 */
export function dropOnPad(candidate: DropCandidate): DropVerdict {
  if (candidate.slotIndex === BLACK_SLOT_INDEX) {
    return {
      do: 'refuse',
      because: 'Slot 0 is permanently black on every layer and holds nothing.',
    };
  }
  if (candidate.slotIndex > MAX_SLOT_INDEX) {
    return {
      do: 'refuse',
      because: `A layer holds ${MAX_BANKS} banks, so slot ${MAX_SLOT_INDEX} is the last one.`,
    };
  }
  if (candidate.holding === null) {
    return { do: 'assign' };
  }
  if (sameSource(candidate.holding, candidate.dropping)) {
    return { do: 'nothing' };
  }
  // The server refuses it whoever asks (owner, 2026-10-19), so asking first
  // would be asking a question whose only answer is no.
  if (candidate.holding.kind === 'generator' && candidate.holding.locked) {
    return {
      do: 'refuse',
      because: `${candidate.holdingName} is locked in this slot; an administrator has to unlock it first.`,
    };
  }
  return { do: 'ask', replacing: candidate.holdingName };
}

/** Whether two sources are the same thing, for the purpose of not asking. */
function sameSource(one: SlotSource, other: SlotSource): boolean {
  if (one.kind === 'media' && other.kind === 'media') {
    return one.mediaItemId === other.mediaItemId;
  }
  if (one.kind === 'generator' && other.kind === 'generator') {
    // The type, not the instance: two slots playing the same generator are
    // different instances by definition, and an operator dropping the same
    // generator on the same pad means the same thing. Not the version either:
    // a module has none (owner, 2026-10-18), so a pad saved as `version: 2`
    // already plays the generator being dropped on it.
    return one.typeId === other.typeId;
  }
  return false;
}

/** What goes on the wire for a source this client holds. */
export function wireSource(source: SlotSource): SourceRef {
  if (source.kind === 'media') {
    return { kind: 'media', media_item_id: source.mediaItemId };
  }
  if (source.kind === 'ndi') {
    return { kind: 'ndi', name: source.name };
  }
  return {
    kind: 'generator',
    type_id: source.typeId,
    version: source.version,
    // The instance identity is deliberately left off. The server mints one
    // for a slot that arrives without it, and an identity invented here
    // would be a second answer to a question the document already settles.
  };
}

/**
 * One published generator, as something to pick up and drop on a pad.
 *
 * The owner's instruction is that generators are treated like videos: they
 * stand as an icon and drag onto a slot (2026-09-23). This is the icon.
 *
 * **There is no preview picture, and none is invented here.** A generator's
 * picture is whatever the renderer draws it as, and a gradient approximated
 * in CSS is a picture of something the wall will not show. A real one has to
 * come from the renderer, as a frame it rendered; until it does, the tile is
 * the generator's name and what it is for, which is true.
 */
export interface GeneratorTile {
  typeId: string;
  /** What this server calls it. Never a type id shown to an operator. */
  name: string;
  description: string;
  /** What dropping it on a pad puts there. */
  source: SlotSource;
}

/**
 * Every generator this server publishes, by name.
 *
 * By name rather than in catalogue order, because the catalogue's order is
 * the order somebody wrote the manifests in and an operator reads a list of
 * names alphabetically.
 *
 * The parameters are left empty on purpose. Each one has a default in its own
 * manifest, and a client that copied those defaults into the document would
 * be writing down values nobody chose — which then stop following the
 * manifest the day it changes.
 */
export function generatorTiles(catalogue: Catalogue): GeneratorTile[] {
  return [...catalogue.generators]
    .map((generator) => ({
      typeId: generator.type_id,
      name: generator.name,
      description: generator.description,
      source: {
        kind: 'generator' as const,
        instanceId: null,
        typeId: generator.type_id,
        // The catalogue's own number, because the document has the field. It
        // is data and nothing more: the generator is found by its type.
        version: generator.version,
        parameters: {},
        live: {},
        // Not on a pad yet, so nothing holds it and nobody has locked it.
        held: [],
        locked: false,
      },
    }))
    .sort((one, other) => one.name.localeCompare(other.name));
}

/**
 * The generators narrowed to what the operator typed.
 *
 * Over the name and the type id, for the same reason the library's search is
 * over the name and the filename: somebody looking for one half-remembers
 * what it is called or what it is.
 */
/** One NDI sender on the network, ready to be dragged onto a pad. */
export interface NdiTile {
  /**
   * The full name, which is the identifier.
   *
   * Carried whole into the show. The two halves below are for reading, never
   * for rebuilding this from — a machine or a source name containing a bracket
   * would not survive the round trip.
   */
  name: string;
  /** The machine part, before the bracket. */
  machine: string;
  /** The source part, inside the bracket. */
  stream: string;
  /** What dropping it on a pad puts there. */
  source: SlotSource;
}

/**
 * Every NDI sender on the network, by name.
 *
 * NDI announces a source as `MACHINE (Source Name)` and that whole string is
 * the only handle there is: nothing survives a sender restarting except this,
 * and a position in a discovery list means nothing a moment later.
 *
 * Split for reading only. The machine matters: three playback machines each
 * sending "Program" are three entries whose source names are identical, and the
 * machine is the only thing that tells an operator which wall they are about to
 * put on. A source name may itself contain brackets, so the machine ends at the
 * first one and the stream ends at the last.
 *
 * Sorted by the full name, so the list does not reshuffle itself between two
 * looks at it — the server sorts too, and both do it because neither should
 * have to trust the other for something an operator would notice.
 */
export function ndiTiles(sources: readonly string[]): NdiTile[] {
  return [...sources]
    .map((name) => {
      const opened = name.indexOf('(');
      const closed = name.lastIndexOf(')');
      const split = opened > 0 && closed > opened;
      return {
        name,
        machine: split ? name.slice(0, opened).trim() : name,
        stream: split ? name.slice(opened + 1, closed) : name,
        source: { kind: 'ndi' as const, name },
      };
    })
    .sort((one, other) => one.name.localeCompare(other.name));
}

/** Those whose name matches what somebody typed. */
export function matchingNdi(tiles: readonly NdiTile[], query: string): NdiTile[] {
  const needle = query.trim().toLowerCase();
  if (needle === '') {
    return [...tiles];
  }
  // Against the whole name, so typing a machine finds everything it sends and
  // typing a source name finds it on every machine.
  return tiles.filter((tile) => tile.name.toLowerCase().includes(needle));
}

export function matchingGenerators(
  tiles: readonly GeneratorTile[],
  query: string,
): GeneratorTile[] {
  const needle = query.trim().toLowerCase();
  if (needle === '') {
    return [...tiles];
  }
  return tiles.filter(
    (tile) =>
      tile.name.toLowerCase().includes(needle) || tile.typeId.toLowerCase().includes(needle),
  );
}

/**
 * One published effect, as something to pick up and drop on a slot or a layer.
 *
 * “Effects are icons too, dragged onto a slot or onto the layer preview. They
 * never replace anything — they stack on top” (owner, 2026-09-23).
 *
 * No preview picture, for the reason [`GeneratorTile`] gives: what an effect
 * looks like depends on what it is applied to, and a thumbnail of it applied
 * to nothing would be a picture of nothing.
 */
export interface EffectTile {
  typeId: string;
  /**
   * The catalogue's own number, carried only because the request that adds an
   * effect has the field. It names nothing: there is one effect per type.
   */
  version: number;
  name: string;
  description: string;
}

/** Every effect this server publishes, by name. */
export function effectTiles(catalogue: Catalogue): EffectTile[] {
  return [...catalogue.effects]
    .map((effect) => ({
      typeId: effect.type_id,
      version: effect.version,
      name: effect.name,
      description: effect.description,
    }))
    .sort((one, other) => one.name.localeCompare(other.name));
}

/** The effects narrowed to what the operator typed, over name and type. */
export function matchingEffects(tiles: readonly EffectTile[], query: string): EffectTile[] {
  const needle = query.trim().toLowerCase();
  if (needle === '') {
    return [...tiles];
  }
  return tiles.filter(
    (tile) =>
      tile.name.toLowerCase().includes(needle) || tile.typeId.toLowerCase().includes(needle),
  );
}

/**
 * What the operator is carrying out of the browser.
 *
 * Two things can be picked up there and they mean different things when they
 * land: a source replaces what a pad plays, an effect stacks on it. A pad has
 * to know which it is being offered before it decides whether to light up.
 */
export type Dragged =
  | { of: 'source'; source: SlotSource; name: string }
  | { of: 'effect'; typeId: string; version: number; name: string }
  /**
   * A pad of the matrix itself, picked up to move it to another pad (owner,
   * 2026-10-21: "Slot 1 per Drag and Drop auf Slot 10 schieben").
   */
  | { of: 'pad'; layerId: string; slotIndex: number; name: string };

/** What dropping a pad on a pad comes to. */
export type PadDropVerdict =
  | { do: 'move' }
  | { do: 'swap'; with: string }
  | { do: 'nothing' }
  | { do: 'refuse'; because: string };

/**
 * Whether a pad can be moved where it was dropped, and what that will do.
 *
 * **To any pad, on its own layer or another** (owner, 2026-10-21: "ich kann
 * keinen Slot von einem Layer auf einen anderen Layer packen.. das sollte
 * ebenfalls möglich sein"). It was its own layer only, on the reasoning that
 * the browser already puts a clip on another layer — which copies the clip
 * and leaves behind everything the slot was set up with.
 *
 * A pad that is taken swaps rather than being overwritten, so a drop that
 * missed by one pad is undone by dropping it back.
 */
export function dropPadOnPad(
  carried: { layerId: string; slotIndex: number },
  onto: { layerId: string; slotIndex: number; holdingName: string | null },
): PadDropVerdict {
  if (onto.slotIndex === BLACK_SLOT_INDEX) {
    return { do: 'refuse', because: 'Slot 0 is permanently black on every layer and holds nothing.' };
  }
  if (carried.layerId === onto.layerId && carried.slotIndex === onto.slotIndex) {
    return { do: 'nothing' };
  }
  return onto.holdingName === null ? { do: 'move' } : { do: 'swap', with: onto.holdingName };
}

/** What dropping an effect on a pad comes to. */
export type EffectDropVerdict = { do: 'add' } | { do: 'refuse'; because: string };

/**
 * Whether an effect can go on a pad.
 *
 * An effect belongs to a source. An empty pad has none, and inventing a slot
 * to hang one on would put something in the matrix nobody asked for — the
 * server refuses it for the same reason.
 */
export function dropEffectOnPad(slotIndex: number, occupied: boolean): EffectDropVerdict {
  if (slotIndex === BLACK_SLOT_INDEX) {
    return {
      do: 'refuse',
      because: 'Slot 0 is permanently black on every layer and has no effects.',
    };
  }
  if (!occupied) {
    return { do: 'refuse', because: 'There is nothing in this pad to put an effect on.' };
  }
  return { do: 'add' };
}
// --- GENERATOR ----------------------------------------------------------------

/** One generator slot of the open show, and what it is set to. */
export interface GeneratorLine {
  layerId: string;
  layerName: string;
  slotIndex: number;
  /**
   * The instance a live value is keyed by, or null when this slot has none.
   *
   * Null only for a show the server has not loaded — it mints one then — so
   * on the live screen this is always set. The controls are drawn against it
   * and are disabled without it, because a request naming an instance this
   * client invented would be refused for the wrong reason.
   */
  instanceId: string | null;
  typeId: string;
  /** What the catalogue calls it, or the bare type when this server publishes none. */
  name: string;
  /**
   * Whether this server publishes that type.
   *
   * The type alone, whatever `version` the slot was saved with: there is one
   * module per type and no versions of it (owner, 2026-10-18).
   */
  published: boolean;
  /** Whether the layer is playing this slot right now. */
  onAir: boolean;
  /**
   * Whether anything on this instance has been moved since the show opened.
   *
   * What the offer to put it back is for, and what says whether there is
   * anything to put back. The generator twin of [`EffectLine.moved`].
   */
  moved: boolean;
  /** Whether an administrator has locked it in its slot. */
  locked: boolean;
  parameters: EffectParameter[];
}

/**
 * Every generator slot in the open show, with its parameters in force.
 *
 * An unpublished type is reported rather than hidden: a show written against
 * a build that had a generator this one does not is something the operator has
 * to be told about, and the slot is still on the matrix either way.
 */
export function generatorLines(
  layers: readonly LayerView[],
  catalogue: Catalogue,
): GeneratorLine[] {
  const lines: GeneratorLine[] = [];
  for (const layer of layers) {
    for (const slot of layer.slots) {
      if (slot.source.kind !== 'generator') {
        continue;
      }
      const module = findGenerator(catalogue, slot.source.typeId);
      lines.push({
        layerId: layer.id,
        layerName: layer.name,
        slotIndex: slot.index,
        instanceId: slot.source.instanceId,
        typeId: slot.source.typeId,
        name: module?.name ?? slot.source.typeId,
        published: module !== null,
        onAir: layer.activeSlot === slot.index,
        moved: Object.keys(slot.source.live).length > 0,
        // The same builder the effects use, because it is the same question:
        // what the manifest declares, what the show set, what a control
        // source has moved and which of those is in force (owner, 2026-09-23 —
        // generators get no separate special solution).
        locked: slot.source.locked,
        parameters:
          module === null
            ? []
            : moduleParameters(
                module,
                slot.source.parameters,
                slot.source.live,
                slot.source.held,
              ),
      });
    }
  }
  return lines;
}

// --- EFFECTS ------------------------------------------------------------------

/**
 * One declared parameter of one effect instance, as a live control reads it.
 *
 * Three things rather than one, because an operator halfway through a show has
 * to be able to see which is which: what it is doing now, what the show
 * configured, and whether the difference between them is something somebody
 * moved. `declared` is the manifest's own definition, and it is what draws the
 * control — the kind, the range and the default all come from the catalogue this
 * server published, so an effect this build has never heard of still arrives
 * with a working control.
 */
export interface EffectParameter {
  key: string;
  /** What the manifest calls it. */
  name: string;
  /** What the manifest declares it to take. */
  declared: ModuleParameter;
  /** The value in force: live over configured over published. */
  value: ParameterValue | null;
  /** That value, formatted. */
  text: string;
  /** What the show configured, which is where a reset puts it back to. */
  configured: ParameterValue | null;
  /** That value, formatted. */
  configuredText: string;
  /** Whether a control source has moved this one since the show opened. */
  moved: boolean;
  /** Whether the show set it, as opposed to taking the published default. */
  set: boolean;
  /**
   * Whether a lighting console holds it right now.
   *
   * Shown and not offered while it is (owner, 2026-10-19): the server refuses
   * a client moving it, and the way to take it by hand is the DMX switch.
   */
  held: boolean;
}

/** One effect instance of a chain, and what its parameters are doing. */
export interface EffectLine {
  id: string;
  typeId: string;
  /** What the catalogue calls it, or the bare type when this server publishes none. */
  name: string;
  /** Whether this server publishes that type, by the rule [`GeneratorLine.published`] gives. */
  published: boolean;
  /** Whether the chain runs it at all. */
  enabled: boolean;
  /**
   * Whether anything on this instance has been moved since the show opened.
   *
   * What the offer to put it back is for, and what says whether there is
   * anything to put back.
   */
  moved: boolean;
  /** Whether an administrator has locked it where it is. */
  locked: boolean;
  parameters: EffectParameter[];
}

/** One effect chain: a layer's own, or one configured on a slot. */
export interface ChainLine {
  layerId: string;
  layerName: string;
  /** A layer chain, or the chain of one slot on it. */
  where: 'layer' | 'slot';
  /** Which slot, for a slot chain. */
  slotIndex: number | null;
  /** Whether what this chain is on is on air at this moment. */
  onAir: boolean;
  /**
   * Whether the renderer runs this chain at all.
   *
   * A layer chain runs. A slot chain is persisted, validated
   * and editable, and the renderer does not run it yet — so a show can carry
   * one that does nothing, and an operator looking at a wall needs that said
   * rather than inferred.
   */
  rendered: boolean;
  effects: EffectLine[];
}

/**
 * Every effect chain in the open show, or in one layer of it, layer chains first.
 *
 * `layers` is what the caller wants read: the whole show, or the one layer whose
 * controls the EFFECTS tab is showing. Empty chains are left out — a layer with
 * no effects on it takes exactly the path it took before chains existed, and a
 * row saying so on every layer would bury the ones that have something in them.
 */
export function effectChains(layers: readonly LayerView[], catalogue: Catalogue): ChainLine[] {
  const chains: ChainLine[] = [];
  for (const layer of layers) {
    if (layer.effects.length > 0) {
      chains.push({
        layerId: layer.id,
        layerName: layer.name,
        where: 'layer',
        slotIndex: null,
        onAir: layer.enabled,
        rendered: true,
        effects: layer.effects.map((effect) => effectLine(effect, catalogue)),
      });
    }
    for (const slot of layer.slots) {
      if (slot.effects.length === 0) {
        continue;
      }
      chains.push({
        layerId: layer.id,
        layerName: layer.name,
        where: 'slot',
        slotIndex: slot.index,
        onAir: layer.activeSlot === slot.index,
        rendered: false,
        effects: slot.effects.map((effect) => effectLine(effect, catalogue)),
      });
    }
  }
  return chains;
}

function effectLine(effect: EffectView, catalogue: Catalogue): EffectLine {
  const module = findEffect(catalogue, effect.typeId);
  return {
    id: effect.id,
    typeId: effect.typeId,
    name: module?.name ?? effect.typeId,
    published: module !== null,
    enabled: effect.enabled,
    // Read off the live map rather than by comparing values. A live value that
    // happens to equal the configured one is still a value somebody put there,
    // and an operator who cannot see that it is there cannot know to take it
    // off before the show is edited under it.
    moved: Object.keys(effect.live).length > 0,
    locked: effect.locked,
    parameters: module === null ? [] : effectParameters(module, effect),
  };
}

/**
 * Every parameter one effect declares, live beside configured.
 *
 * Driven by the manifest rather than by either map, exactly as the editor's
 * controls are: a show that set nothing wants the published defaults, a live
 * map holds only what somebody moved, and a parameter is only readable beside
 * the definition that declared it. A key in either map that the manifest does
 * not declare is left out for the same reason the editor leaves it out — a
 * value with no definition has no name, no range and no meaning, and neither
 * map can hold one the server accepted anyway.
 */
export function effectParameters(module: Module, effect: EffectView): EffectParameter[] {
  return moduleParameters(module, effect.parameters, effect.live, effect.held);
}

/**
 * The same, for any module: what it declares, configured beside live.
 *
 * One function rather than one per kind. An effect and a generator are both
 * plugins with declared parameters, and the owner's instruction (2026-09-23) is
 * that they use one model — a client with two would be the place the two
 * models could disagree about what a value is.
 */
export function moduleParameters(
  module: Module,
  configuredValues: Record<string, ParameterValue>,
  liveValues: Record<string, ParameterValue>,
  held: readonly string[] = [],
): EffectParameter[] {
  return module.parameters.map((parameter) => {
    const configured = parameterValue(module, configuredValues, parameter.key);
    const value = valueInForce(module, configuredValues, liveValues, parameter.key);
    return {
      key: parameter.key,
      name: parameter.name,
      declared: parameter,
      value,
      text: parameterText(parameter, value),
      configured,
      configuredText: parameterText(parameter, configured),
      moved: liveValues[parameter.key] !== undefined,
      set: configuredValues[parameter.key] !== undefined,
      held: held.includes(parameter.key),
    };
  });
}

// --- SLICES -------------------------------------------------------------------

/** One piece of the mapping, as the work area lists it. */
export interface SliceLine {
  pieceId: string;
  name: string;
  outputId: string;
  /** The output's name, or null when the show has no output by that identifier. */
  outputName: string | null;
  enabled: boolean;
  source: RectPx;
  dest: RectPx;
  rotationDeg: number;
  /** Whether it maps its source without resampling (`Piece::is_pixel_exact`). */
  pixelExact: boolean;
  /** Where in its output's draw order it lands, counting from 1, or null when it is not drawn. */
  drawnAt: number | null;
  /** Whether it lies entirely off its output, and so reaches nothing. */
  offScreen: boolean;
}

/**
 * Every slice of the mapping, grouped by output and in draw order within it.
 *
 * The order is the renderer's own (`sequenceOf`, which is `OutputMapping::build`
 * sorted stably), because a list that disagreed with the renderer about which
 * piece is on top would be describing a wall the operator does not have.
 */
export function sliceLines(mapping: Mapping): SliceLine[] {
  const lines: SliceLine[] = [];
  for (const output of mapping.outputs) {
    const drawn = drawOrder(mapping, output.id);
    for (const piece of sequenceOf(mapping, output.id)) {
      lines.push(sliceLine(mapping, piece, drawn.indexOf(piece)));
    }
  }

  // A piece whose output the show no longer has is still in the document and
  // still reaches nothing. Left out of this list it would simply vanish, which
  // is the one thing a mapping view must not do.
  for (const piece of mapping.pieces) {
    if (findOutput(mapping, piece.output_id) === null) {
      lines.push(sliceLine(mapping, piece, -1));
    }
  }
  return lines;
}

function sliceLine(mapping: Mapping, piece: Piece, drawnIndex: number): SliceLine {
  const output = findOutput(mapping, piece.output_id);
  return {
    pieceId: piece.id,
    name: piece.name,
    outputId: piece.output_id,
    outputName: output?.name ?? null,
    enabled: piece.enabled,
    source: piece.source_rect,
    dest: piece.dest_rect,
    rotationDeg: piece.rotation_deg,
    pixelExact: isPixelExact(piece),
    drawnAt: drawnIndex < 0 ? null : drawnIndex + 1,
    offScreen: output !== null && isOffScreen(piece.dest_rect, output.mode.size),
  };
}

/** What the mapping comes to, in one line under the picture. */
export function sliceSummary(mapping: Mapping): string {
  const pieces = mapping.pieces.length;
  if (pieces === 0) {
    return 'This show maps no pieces yet.';
  }
  const drawn = mapping.outputs.reduce(
    (total, output) => total + drawOrder(mapping, output.id).length,
    0,
  );
  const outputs = mapping.outputs.length;
  const meeting = overlaps(mapping).length;
  const overlapping = meeting === 0 ? '' : ` · ${count(meeting, 'overlap')}`;
  return `${count(pieces, 'piece')} on ${count(outputs, 'output')} · ${drawn} drawn${overlapping}`;
}

/** Which piece wins where two meet, in the mapping editor's own words. */
export function overlapNotes(mapping: Mapping): string[] {
  return overlaps(mapping).map(describeOverlap);
}

/** One piece outlined over the preview picture, placed as a fraction of it. */
export interface Outline {
  pieceId: string;
  name: string;
  /** Fractions of the surface, so the overlay is honest at any size. */
  left: number;
  top: number;
  width: number;
  height: number;
  rotationDeg: number;
  /**
   * The point it turns about, as a fraction of its own size.
   *
   * Drawn as well as carried: a box turned about its centre when the piece
   * turns about a corner is an outline in the wrong place, and this overlay is
   * read to decide whether a mapping is right.
   */
  anchor: { x: number; y: number };
  /** Whether the renderer draws it. */
  drawn: boolean;
  /** What it is, for the outline's own tooltip. */
  title: string;
}

/**
 * The pieces to draw over one surface's preview, or null when it cannot be said.
 *
 * Which rectangle depends on which surface is being watched, and they are not
 * interchangeable: over the composition a piece is the region it *takes*, and
 * over an output it is the region it *fills*. Null means this client does not
 * know the size of the surface — no show is open, or the server is driving
 * an output the open show does not configure — and the panel says so rather
 * than drawing an empty overlay that would read as "nothing is mapped here".
 */
export function sliceOutlines(
  mapping: Mapping,
  composition: SizePx | null,
  surface: string,
): Outline[] | null {
  // The masked surface is the composition, so a piece over it is the region it
  // takes — and over that picture the outlines land exactly on the edges of
  // what survived, which is how an operator sees that the two agree.
  if (surface === COMPOSITION_SURFACE || surface === MASKED_SURFACE) {
    if (composition === null) {
      return null;
    }
    // Over the composition there is no draw order to speak of: every piece is
    // taking its own region of the same canvas, and they may legitimately take
    // the same one. Whether it reaches a wall is whether it is switched on.
    // Turned, because the input end of a piece turns too (owner,
    // 2026-09-23). Outlined as the upright box it was written as, this overlay
    // drew horizontal bars over a masked picture that has diagonal ones — two
    // views of the same thing disagreeing, on the screen an operator uses to
    // decide whether the mapping is right.
    return mapping.pieces.map((piece) =>
      outline(
        piece,
        piece.source_rect,
        composition,
        piece.source_rotation_deg ?? 0,
        piece.source_anchor ?? CENTRE,
        piece.enabled,
        `takes ${rectText(piece.source_rect)}`,
      ),
    );
  }

  const output = findOutput(mapping, surface);
  if (output === null) {
    return null;
  }
  const drawn = drawOrder(mapping, surface);
  return sequenceOf(mapping, surface).map((piece) =>
    outline(
      piece,
      piece.dest_rect,
      output.mode.size,
      piece.rotation_deg,
      piece.anchor ?? CENTRE,
      drawn.includes(piece),
      `fills ${rectText(piece.dest_rect)} of ${output.name}`,
    ),
  );
}

/** What a piece turns about unless it says otherwise (`AnchorPoint::CENTRE`). */
const CENTRE = { x: 0.5, y: 0.5 };

/** One outlined rectangle, placed as a fraction of the surface it is on. */
function outline(
  piece: Piece,
  rect: RectPx,
  surface: SizePx,
  rotationDeg: number,
  anchor: { x: number; y: number },
  drawn: boolean,
  what: string,
): Outline {
  return {
    pieceId: piece.id,
    name: piece.name,
    left: rect.x / surface.width,
    top: rect.y / surface.height,
    width: rect.width / surface.width,
    height: rect.height / surface.height,
    rotationDeg,
    anchor,
    drawn,
    title: `${piece.name} — ${what}${piece.enabled ? '' : ' · switched off, so nothing of it is drawn'}`,
  };
}

/** A rectangle in the terms an LED operator patches one in. */
export function rectText(rect: RectPx): string {
  return `${rect.width}×${rect.height} px at ${rect.x}, ${rect.y}`;
}

// --- small helpers ------------------------------------------------------------

function count(value: number, noun: string): string {
  return `${value} ${noun}${value === 1 ? '' : 's'}`;
}

/** Rounds for display without turning 0.1 + 0.2 into a paragraph. */
function round(value: number, places: number): number {
  const scale = 10 ** places;
  return Math.round(value * scale) / scale;
}
