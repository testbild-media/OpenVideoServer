// The show editor.
//
// A show is one document. It is loaded whole, edited in memory here, and
// saved whole, because the server has no per-field endpoint: a half-applied
// edit cannot leave a show internally inconsistent if there is no way to
// apply half of one.
//
// Everything this panel edits is configuration. `default_opacity` and
// are what a layer *starts* from; the crossfader, the master dimmer and what a layer is doing right now
// are live state, belong to the console and to Live Mode, and are never written
// here. The labels say "default" for that reason and not out of
// pedantry: the two numbers are different numbers.
//
// No control in this file is drawn from a list written into this client. The
// routes, blend modes, grids and playback modes come from `limits.ts`, which is
// checked against the Rust; every effect and generator parameter comes from the
// catalogue the server publishes, with the type, range and default it declares.
// The reasoning — what an edit does, and whether the server will accept it —
// is in `shared/editor.ts`, which is tested without a page.

import type {
  ConnectorInfo,
  DisplayMode,
  DmxProtocols,
  EffectInstance,
  MasterConfig,
  MediaItem,
  ModuleParameter,
  OutputConfig,
  ParameterValue,
  Piece,
  Show,
  ShowListEntry,
  ValidationError,
} from '@ovs/schema';

import type { OvsBridge } from '../preload.js';
import {
  CROSSFADER_CURVES,
  CROSSFADER_CURVE_LABELS,
  MAX_BPM,
  MAX_FRACTION,
  MAX_SPEED_SCALE,
  MIN_BEATS_PER_BAR,
  MIN_BPM,
  MIN_FRACTION,
  MIN_SPEED_SCALE,
  SLOTS_PER_BANK,
} from '../shared/limits.js';
import {
  emptyCatalogue,
  findEffect,
  parameterValue,
  type Catalogue,
  type Module,
  type ParameterMap,
} from '../shared/modules.js';
import { hexFromLinear, linearFromHex } from '../shared/colour.js';
// **No slot editing here.** `assignSlot`, `bankIndices`, `clearSlot`,
// `findSlot`, `setGeneratorParameter`, `setSlot` and `slotPath` were imported
// and never called: the remains of a panel that moved to OPTIONS, where a slot
// is now put together. They stayed because nothing fails on an import nobody
// uses — `tsc` is not run with `noUnusedLocals` — so the only thing that ever
// named them was a search for why a generator had no controls.
import {
  addEffect,
  addLayer,
  bankCount,
  effectPath,
  findLayer,
  isDirty,
  layerPath,
  markSaved,
  mergedForSave,
  moveEffect,
  moveLayer,
  openDraft,
  orderedLayers,
  removeEffect,
  removeLayer,
  renameLayer,
  renameShow,
  revert,
  setAutoSave,
  setEffectEnabled,
  setEffectParameter,
  setLayer,
  setMaster,
  setNotes,
  validate,
  type ChainRef,
  type Draft,
  type Edit,
} from '../shared/editor.js';
import { emptyFixtures, type FixtureCatalogue } from '../shared/patch.js';
import {
  ROOT_FOLDER,
  countsByFolder,
  folderNameProblem,
  folderTree,
  isInside,
  itemsIn,
  joinFolder,
  nameOf,
  parentOf,
  type FolderNode,
} from '../shared/folders.js';
import { button, element, make } from './dom.js';
import { MappingPanel } from './mapping.js';
import { PatchPanel } from './patch.js';

/** What the editor needs from the rest of the window. */
export interface EditorDeps {
  ovs: OvsBridge;
  /** Says something to the operator in the fault line at the foot of the window. */
  note: (message: string) => void;
  /**
   * The show the server currently has open, or null.
   *
   * Saving that one no longer reloads it — a save never interrupts the show
   * (owner, 2026-09-23) — but it does put the edit on the wall, which is
   * still something to say before somebody presses Save during a night.
   */
  liveShowId: () => string | null;
}

export class EditorPanel {
  readonly #ovs: OvsBridge;
  readonly #note: (message: string) => void;
  readonly #liveShowId: () => string | null;

  #draft: Draft | null = null;
  /** The show LOAD has been offered for once and is waiting to be confirmed. */
  #loading: string | null = null;
  /**
   * The show DELETE has been offered for once and is waiting to be confirmed.
   *
   * Two presses, like LOAD on the same row. A deleted show does not come back,
   * and a dialogue for it would be a second thing to learn where the list
   * already teaches one.
   */
  #deleting: string | null = null;
  #shows: ShowListEntry[] = [];
  /**
   * The media library, for the one parameter kind that names something in it.
   *
   * Read when the editor loads, like the catalogues: a picture is chosen from
   * a menu of what has been uploaded, so an editor without this offers nothing
   * rather than a box to type an identifier into.
   */
  #media: MediaItem[] = [];
  #catalogue: Catalogue = emptyCatalogue();
  /** The fixture personalities this server publishes. Empty until it has said. */
  #fixtures: FixtureCatalogue = emptyFixtures();
  /** The sockets this server reported. Empty until a renderer has said. */
  #connectors: ConnectorInfo[] = [];
  /** What a stream may run, as this server publishes it. */
  #networkModes: DisplayMode[] = [];

  /**
   * Replaces what this machine is wired to, and draws it.
   *
   * Told by the server rather than asked for: the list is read once when a
   * client signs in, and a monitor changed afterwards would otherwise leave
   * every chooser describing the one that had been there before.
   */
  setConnectors(connectors: readonly ConnectorInfo[]): void {
    this.#connectors = [...connectors];
    this.draw();
  }
  /** Field-level problems from the last save the server refused. */
  #refused: ValidationError[] = [];
  #selectedLayer: string | null = null;
  /** Every folder the show list holds, in path order. */
  #folders: string[] = [];
  /** The folder whose shows are shown. Empty is the root. */
  #folder = ROOT_FOLDER;
  /** Which folders are drawn open, by path. */
  readonly #open = new Set<string>();
  /** The show being dragged onto a folder, while a drag is in progress. */
  #draggingShow: string | null = null;
  /** Which show the middle panel is showing, which is not the open one. */
  #peeking: string | null = null;
  /** What that show turned out to hold, once the server has said. */
  #peeked: Show | null = null;
  /** A folder being named, either a new one or one being renamed. */
  #naming: { of: 'new'; parent: string } | { of: 'rename'; path: string } | null = null;
  #bank = 0;
  #saving = false;
  /** Problems by path, recomputed at the top of every draw. */
  #problems = new Map<string, string[]>();
  #controlSeq = 0;

  readonly #ui = {
    showList: element('show-list'),
    showFolders: element('show-folders'),
    newFolder: element<HTMLButtonElement>('show-new-folder'),
    exportShow: element<HTMLButtonElement>('show-export'),
    exportAll: element<HTMLButtonElement>('show-export-all'),
    importShow: element<HTMLButtonElement>('show-import'),
    previewTitle: element<HTMLHeadingElement>('show-preview-title'),
    previewSays: element('show-preview-says'),
    previewBody: element('show-preview-body'),
    newShowForm: element<HTMLFormElement>('new-show-form'),
    newShowName: element<HTMLInputElement>('new-show-name'),
    documentFields: element('document-fields'),
    save: element<HTMLButtonElement>('settings-save'),
    revert: element<HTMLButtonElement>('settings-discard'),
    documentNote: element('document-note'),
    documentErrors: element('document-errors'),
    masterFields: element('master-fields'),
    addLayer: element<HTMLButtonElement>('add-layer'),
    layerList: element('layer-list'),
    layerPanel: element('layer-panel'),
    layerTitle: element('layer-title'),
    layerFields: element('layer-fields'),
    layerEffectsPanel: element('layer-effects-panel'),
    layerEffects: element('layer-effects'),
    layersErrors: element('layers-errors'),
  };

  /**
   * Composition → pieces → outputs.
   *
   * Its own panel because it is its own problem, but the same document: it is
   * handed the draft to read, the way to apply an edit, and this editor's
   * problem list, so a refused save about a piece lands beside that piece.
   */
  readonly #mapping: MappingPanel;

  /**
   * The DMX-over-IP patch.
   *
   * Its own mode on screen and its own problem, but the same document: the
   * patch lives in `Show.fixture_patch`, so it is handed the draft to read,
   * the way to apply an edit, this editor's problem list and this editor's
   * Save. There is no second way to save a show.
   */
  readonly #patch: PatchPanel;

  constructor(deps: EditorDeps) {
    this.#ovs = deps.ovs;
    this.#note = deps.note;
    this.#liveShowId = deps.liveShowId;

    this.#mapping = new MappingPanel({
      show: () => this.#draft?.show ?? null,
      apply: (edit) => this.#edit(edit),
      note: (message) => this.#note(message),
      problems: (path) => this.#problemsUnder(path),
      connectors: () => this.#connectors,
      networkModes: () => this.#networkModes,
      pickImage: () => this.#pickImage(),
      liveShowId: () => this.#liveShowId(),
      setMapping: (outputs, pieces) => this.#setMapping(outputs, pieces),
      setTestPattern: (mode, highlighted) => {
        // Nothing waits on it: the wall either changes or it does not, and a
        // screen that froze while asking would be worse than one that says so
        // afterwards.
        void this.#ovs.setTestPattern(mode, highlighted).then((answer) => {
          if (!answer.ok) {
            this.#note(answer.message);
          }
        });
      },
    });

    this.#patch = new PatchPanel({
      show: () => this.#draft?.show ?? null,
      apply: (edit) => this.#edit(edit),
      note: (message) => this.#note(message),
      problems: (path) => this.#problemsUnder(path),
      problemList: () => this.#problemList(),
      catalogue: () => this.#fixtures,
      save: () => void this.#save(),
      revert: () => this.#revert(),
      dirty: () => this.#draft !== null && isDirty(this.#draft),
      saving: () => this.#saving,
      liveShowId: () => this.#liveShowId(),
      dmxProtocols: () => this.#dmxProtocols(),
      setDmxProtocols: (protocols) => this.#setDmxProtocols(protocols),
    });

    this.#ui.newShowForm.addEventListener('submit', (event) => {
      event.preventDefault();
      void this.#createShow();
    });
    this.#ui.save.addEventListener('click', () => void this.#save());
    this.#ui.revert.addEventListener('click', () => this.#revert());
    // A folder in the show list, named where it will appear.
    // (The Save and Discard beside the tabs are wired below, once.)
    this.#ui.newFolder.addEventListener('click', () => {
      this.#naming = { of: 'new', parent: this.#folder };
      this.draw();
    });

    // **A show that exists only on one server is a show that can be lost**
    // (owner, 2026-09-23): "Alles ist nur auf dem Server und kann nicht extra
    // gesichert werden."
    //
    // The one being looked at rather than the one that is open: looking is how
    // an operator finds the show they want a copy of, and the open one is
    // whatever the wall is running.
    this.#ui.exportShow.addEventListener('click', () => void this.#exportShow());
    this.#ui.exportAll.addEventListener('click', () => void this.#exportShow(true));
    this.#ui.importShow.addEventListener('click', () => void this.#importShow());
    this.#ui.addLayer.addEventListener('click', () => {
      if (this.#draft === null) {
        this.#note('open a show before adding a layer');
        return;
      }
      const before = this.#draft.show.layers.map((layer) => layer.id);
      if (this.#apply(addLayer(this.#draft.show))) {
        const added = this.#draft.show.layers.find((layer) => !before.includes(layer.id));
        this.#selectedLayer = added?.id ?? this.#selectedLayer;
        this.draw();
      }
    });
  }

  /**
   * Asks the server for everything the editor offers.
   *
   * The catalogues and the media library are what a slot and an effect chain
   * can be filled from, so an editor that has not got them yet offers nothing
   * rather than offering something it made up.
   */
  async load(): Promise<void> {
    const [shows, catalogues, media, connectors, networkModes, fixtures] = await Promise.all([
      this.#ovs.shows(),
      this.#ovs.catalogues(),
      this.#ovs.mediaLibrary(),
      this.#ovs.connectors(),
      this.#ovs.networkModes(),
      this.#ovs.fixtures(),
    ]);

    if (fixtures.ok) {
      this.#fixtures = { loaded: true, fixtures: fixtures.value };
    } else {
      this.#note(`the fixture personalities could not be read: ${fixtures.message}`);
    }
    if (networkModes.ok) {
      this.#networkModes = networkModes.value;
    }
    if (connectors.ok) {
      this.#connectors = connectors.value;
    } else {
      this.#note(`this server's connectors could not be read: ${connectors.message}`);
    }
    if (shows.ok) {
      this.#shows = shows.value;
    } else {
      this.#note(`the show list could not be read: ${shows.message}`);
    }
    if (catalogues.ok) {
      this.#catalogue = {
        loaded: true,
        effects: catalogues.value.effects,
        generators: catalogues.value.generators,
      };
    } else {
      this.#note(`the effect and generator catalogues could not be read: ${catalogues.message}`);
    }
    if (media.ok) {
      // Kept, because an effect parameter can name one: the Mask effect takes
      // a picture out of the library rather than a number.
      this.#media = media.value.items;
    } else {
      this.#note(`the media library could not be read: ${media.message}`);
    }

    // What reaches the show. A server setting, so it is read here rather than
    // taken from the document.
    await this.#patch.loadProtocols();

    // And then the document the server is actually running, so that signing
    // in puts an operator in front of tonight's show rather than a list.
    await this.followTheOpenShow();

    this.draw();
  }

  /**
   * Opens this editor at one thing in a show, for Live Mode's work area.
   *
   * A request to *look*, sent from a screen that must not edit: the work area's
   * SLICES, GENERATOR and EFFECTS tabs are named after configuration, so what
   * they offer is the editor opened at exactly that thing rather than a control
   * that would write to the document from the live screen.
   *
   * Nothing on stage changes. Loading a show here takes a copy; only a save
   * reaches the server, and a save does not interrupt the show. Unsaved work
   * is never thrown away to honour this:
   * `#openShow` refuses while the draft is dirty and says why, and this
   * answers whether it got there.
   */
  async openAt(target: {
    showId: string;
    layerId?: string;
    slotIndex?: number;
    pieceId?: string;
  }): Promise<boolean> {
    if (this.#draft?.show.id !== target.showId) {
      await this.#openShow(target.showId);
      if (this.#draft?.show.id !== target.showId) {
        // Refused: either the load failed or there is unsaved work in hand.
        // `#openShow` has already said which, in the operator's own words.
        return false;
      }
    }

    if (target.layerId !== undefined && findLayer(this.#draft.show, target.layerId)) {
      this.#selectedLayer = target.layerId;
    }
    if (target.slotIndex !== undefined) {
      // The bank the slot is actually in, so the pad an operator was sent to
      // is one they can see rather than one on another page.
      this.#bank = Math.max(0, Math.ceil(target.slotIndex / SLOTS_PER_BANK) - 1);
    }
    if (target.pieceId !== undefined) {
      this.#mapping.select(target.pieceId);
    }
    this.draw();
    return true;
  }

  /** Draws the whole panel from the document in hand. */
  draw(): void {
    this.#settleSelection();
    this.#problems = this.#collectProblems();

    this.#drawShowFolders();
    this.#drawShows();
    this.#drawPreview();
    this.#drawDocument();
    this.#drawMaster();
    this.#drawLayers();
    this.#drawLayer();
    this.#mapping.draw();
    this.#patch.draw();
  }

  // --- the document ---------------------------------------------------------

  async #createShow(): Promise<void> {
    const name = this.#ui.newShowName.value.trim();
    if (name === '') {
      this.#note('a show needs a name');
      return;
    }
    // A new show takes the editor's attention, so the one in hand would go
    // with it. Nothing an operator did is thrown away without them saying so.
    if (this.#draft !== null && isDirty(this.#draft)) {
      this.#note('the open show has unsaved changes. Save or revert before starting another.');
      return;
    }
    const created = await this.#ovs.createShow(name);
    if (!created.ok) {
      this.#note(created.message);
      return;
    }
    this.#ui.newShowName.value = '';
    this.#adopt(created.value);
    await this.#refreshShows();
  }

  /**
   * Opens whatever the server has open, when nothing else is being edited.
   *
   * **The server owns the show, not the client** (owner, 2026-09-23). An
   * operator signing in is signing in to a running show, and the document
   * behind that show is the one they came to change; being asked to pick it
   * out of a list first is being asked which show they mean when there is
   * only one answer.
   *
   * It only ever fills an empty editor. A draft already in hand is somebody's
   * work — possibly unsaved, possibly a different show they opened on
   * purpose — and a client that replaced it because the server said something
   * would be taking the document out from under them.
   */
  async followTheOpenShow(): Promise<void> {
    const live = this.#liveShowId();
    if (live === null || this.#draft !== null) {
      return;
    }
    await this.#openShow(live);
  }

  /**
   * Asks the operator for a picture, as a data URL.
   *
   * For the mapping canvas's backdrop and nothing else. It never reaches a
   * show and never reaches a server: it is a working aid, so it lives in
   * this window for as long as the window does (owner, 2026-09-23).
   */
  async #pickImage(): Promise<string | null> {
    const answer = await this.#ovs.pickImage();
    if (!answer.ok) {
      this.#note(answer.message);
      return null;
    }
    return answer.value;
  }

  /**
   * What reaches this show, from the server.
   *
   * Null when it could not be read, which is a different thing from "nothing
   * reaches this show" and is drawn differently: the switches are disabled
   * rather than showing off.
   */
  async #dmxProtocols(): Promise<DmxProtocols | null> {
    const answer = await this.#ovs.dmxProtocols();
    if (!answer.ok) {
      this.#note(answer.message);
      return null;
    }
    return answer.value;
  }

  /** Sets what reaches this show, and answers with what it is now. */
  async #setDmxProtocols(protocols: DmxProtocols): Promise<DmxProtocols | null> {
    const answer = await this.#ovs.setDmxProtocols(protocols);
    if (!answer.ok) {
      this.#note(answer.message);
      // Back to whatever the server actually has, rather than leaving the
      // switch showing something nobody agreed to.
      return this.#dmxProtocols();
    }
    return protocols;
  }

  /**
   * Puts the mapping in hand on the wall, without saving the show.
   *
   * Answers with the server's words when it refused and null when it took it,
   * so the mapping panel can say so beside the canvas rather than in the fault
   * line at the foot of the window — which is where an operator lining a wall
   * up is not looking.
   */
  async #setMapping(outputs: OutputConfig[], pieces: Piece[]): Promise<string | null> {
    const show = this.#draft?.show;
    if (show === undefined) {
      return 'no show is open';
    }
    const answer = await this.#ovs.setMapping(show.id, outputs, pieces);
    if (!answer.ok) {
      return answer.message;
    }
    // The document the server has now holds this mapping, so what is in hand
    // is no longer a change against it: stamping the draft keeps Save honest
    // about what is left to store.
    if (this.#draft !== null) {
      this.#draft = {
        saved: { ...this.#draft.saved, outputs, pieces, updated_at: answer.value.updated_at },
        show: { ...this.#draft.show, updated_at: answer.value.updated_at },
      };
    }
    this.draw();
    return null;
  }

  /**
   * Puts a show in the editor, which means putting it on the wall.
   *
   * **Editing is loading** (owner, 2026-09-24: "man kann keine fremde show
   * bearbeiten wenn eine andere show geladen ist"). The server refuses every
   * edit to a show it does not have open, so a client that fetched the document
   * and let somebody work on it would be a client that collects an hour of
   * changes and then cannot save one of them.
   *
   * So this asks first, in as many words, and the operator decides. The show
   * already running is the one case with nothing to ask about.
   */
  async #openShow(showId: string): Promise<void> {
    if (this.#draft !== null && isDirty(this.#draft)) {
      this.#note('this show has unsaved changes. Save or revert before opening another.');
      return;
    }

    // Loading it is what makes it editable, so it is asked once — on the
    // press, the way handing the wall to another show is asked. The show that
    // is already running has nothing to ask about.
    const handed = this.#liveShowId() !== showId;
    if (handed) {
      if (this.#loading !== showId) {
        this.#loading = showId;
        this.draw();
        return;
      }
      this.#loading = null;

      const started = await this.#ovs.runShow(showId);
      if (!started.ok) {
        this.#note(started.message);
        return;
      }
    }

    const loaded = await this.#ovs.loadShow(showId);
    if (!loaded.ok) {
      this.#note(loaded.message);
      return;
    }
    this.#adopt(loaded.value);
    if (handed) {
      this.#note(`${loaded.value.name} is the running show now.`);
    }
    // The list's own facts — when each show was last written — rather than
    // which one is live, which the mirror already reports. Loading one writes
    // to it, so the row it came from is out of date the moment this returns.
    await this.#refreshShows();
  }

  #adopt(show: Show): void {
    this.#draft = openDraft(show);
    this.#refused = [];
    this.#selectedLayer = orderedLayers(show).at(-1)?.id ?? null;
    this.#bank = 0;
    this.#mapping.reset();
    this.#patch.reset();
    this.draw();
  }

  /** Throws the edits away and goes back to what the server last confirmed. */
  #revert(): void {
    if (this.#draft === null) {
      return;
    }
    this.#draft = revert(this.#draft);
    this.#refused = [];
    this.draw();
  }

  async #refreshShows(): Promise<void> {
    const shows = await this.#ovs.shows();
    if (shows.ok) {
      this.#shows = shows.value;
      this.draw();
    }
  }

  /**
   * Sends the whole document, and puts what comes back where it belongs.
   *
   * A refusal carries the server's own field-level problems, which are kept and
   * drawn beside the fields they name. They are dropped again the moment the
   * document changes: they describe a version of it the server has seen, and
   * one it has not seen has not been judged.
   */
  async #save(): Promise<void> {
    const draft = this.#draft;
    if (draft === null || this.#saving) {
      return;
    }

    this.#saving = true;
    this.draw();
    try {
      // What the server has *now*, not what this editor read when it opened
      // the document. Slots, effects and banks are edited on the live screen
      // while this panel is open, and a save that sent this copy back whole
      // would put an evening's work back to the moment it was read (owner,
      // 2026-09-23). `mergedForSave` keeps what was changed here and takes
      // everything else as it stands.
      //
      // A document the server cannot give back is a document this cannot
      // merge with, so the save is refused rather than guessed at: overwriting
      // is the failure being fixed.
      const current = await this.#ovs.loadShow(draft.show.id);
      if (!current.ok) {
        this.#note(
          `The server could not say what this show holds now, so it has not been saved: ${current.message}`,
        );
        return;
      }

      const saved = await this.#ovs.saveShow(mergedForSave(draft, current.value));
      if (saved.ok) {
        this.#refused = [];
        // The merged document, not the draft: what was stored is what the
        // editor must now be holding, or the next save would merge against a
        // copy that never existed.
        this.#draft = markSaved(
          { saved: draft.saved, show: mergedForSave(draft, current.value) },
          saved.value.updated_at,
        );
        void this.#refreshShows();
      } else {
        this.#refused = saved.details;
        this.#note(saved.message);
      }
    } finally {
      this.#saving = false;
      this.draw();
    }
  }

  /** Puts an edit in hand, or says why it was refused. */
  #apply(edit: Edit): boolean {
    const draft = this.#draft;
    if (draft === null) {
      return false;
    }
    if (!edit.ok) {
      this.#note(edit.reason);
      this.draw();
      return false;
    }
    this.#draft = { saved: draft.saved, show: edit.show };
    // The server's verdict was about the document as it was, so it is no longer
    // about this one.
    this.#refused = [];
    if (edit.note !== undefined) {
      this.#note(edit.note);
    }
    return true;
  }

  /** Applies an edit and redraws, which is what nearly every control wants. */
  #edit(edit: Edit): void {
    if (this.#apply(edit)) {
      this.draw();
    }
  }

  #settleSelection(): void {
    const show = this.#draft?.show;
    if (!show) {
      this.#selectedLayer = null;
      return;
    }
    if (this.#selectedLayer !== null && !findLayer(show, this.#selectedLayer)) {
      this.#selectedLayer = orderedLayers(show).at(-1)?.id ?? null;
    }
    // The chooser offers one bank beyond what is configured, so that bank is a
    // legitimate place to be standing; anything past it is not.
    const layer = this.#selectedLayer === null ? null : findLayer(show, this.#selectedLayer);
    if (layer && this.#bank > bankCount(layer)) {
      this.#bank = 0;
    }
  }

  /**
   * Everything wrong anywhere inside one part of the document.
   *
   * What puts a mark on a layer row or a slot pad: the field with the problem
   * may be three panels away, and an operator should not have to go looking
   * through every layer to find which one a refused save was about.
   */
  #problemsUnder(prefix: string): string[] {
    const found: string[] = [];
    for (const [path, reasons] of this.#problems) {
      if (path === prefix || path.startsWith(`${prefix}.`) || path.startsWith(`${prefix}[`)) {
        found.push(...reasons);
      }
    }
    return found;
  }

  /**
   * Every problem in the document, flattened.
   *
   * Listed in full wherever it is listed at all, including problems in parts of
   * the document the panel doing the listing has no control for: a refused save
   * about the patch must not be a message nobody standing in Settings ever sees, and
   * one about a layer must not be invisible to somebody standing in PATCH.
   */
  #problemList(): Array<{ path: string; reason: string }> {
    return [...this.#problems.entries()].flatMap(([path, reasons]) =>
      reasons.map((reason) => ({ path, reason })),
    );
  }

  #collectProblems(): Map<string, string[]> {
    const problems = new Map<string, string[]>();
    const add = (problem: ValidationError) => {
      const existing = problems.get(problem.path);
      if (existing) {
        existing.push(problem.message);
      } else {
        problems.set(problem.path, [problem.message]);
      }
    };

    if (this.#draft !== null) {
      for (const problem of validate(this.#draft.show, this.#catalogue, this.#fixtures)) {
        add(problem);
      }
    }
    for (const problem of this.#refused) {
      add(problem);
    }
    return problems;
  }

  // --- drawing --------------------------------------------------------------

  #drawShows(): void {
    const editing = this.#draft?.show.id ?? null;
    const live = this.#liveShowId();
    // Export is about the show being looked at, so it is dead until one is.
    this.#ui.exportShow.disabled = this.#peeking === null;
    this.#ui.exportShow.title =
      this.#peeking === null
        ? 'Click a show in the list to export it'
        : 'Write this show to a file: the document and what media it needs';
    this.#ui.exportAll.disabled = this.#peeking === null;
    this.#ui.exportAll.title =
      this.#peeking === null
        ? 'Click a show in the list to export it'
        : 'Write this show and every clip it uses into a folder';
    // What is filed in the folder that is open, and not what is under it: a
    // folder that showed its descendants' contents would put the same show
    // in two places.
    const here = itemsIn(this.#shows, this.#folder);

    if (this.#shows.length === 0) {
      this.#ui.showList.replaceChildren(
        make('p', 'empty-note', 'No shows on this server yet.'),
      );
      return;
    }
    if (here.length === 0) {
      this.#ui.showList.replaceChildren(
        make(
          'p',
          'empty-note',
          this.#folder === ROOT_FOLDER
            ? 'Every show is filed in a folder.'
            : `Nothing is filed in ${nameOf(this.#folder)} yet. Drag a show onto it.`,
        ),
      );
      return;
    }

    this.#ui.showList.replaceChildren(
      ...here.map((entry) => {
        const line = make('div', 'show-line');
        line.classList.toggle('editing', entry.id === editing);
        line.classList.toggle('live', entry.id === live);
        line.classList.toggle('peeking', entry.id === this.#peeking);

        // Clicking says what is in it. Editing is a press of its own, because
        // clicking through a list of forty shows to find the right one must not
        // take the document out from under whatever is open (owner,
        // 2026-09-23).
        const row = button('', () => void this.#peek(entry.id), { className: 'show' });

        const what = make('span', 'what', entry.name);
        const when = make(
          'span',
          'when',
          entry.loadable ? whenText(entry.updated_at) : `schema v${entry.schema_version}`,
        );
        row.append(what, when);
        row.title = entry.loadable
          ? 'See what is in this show. LOAD beside it hands the wall to it and opens it for editing.'
          : `this build cannot open a show written at schema version ${entry.schema_version}`;

        // Filed by dragging it onto a folder. Nothing about the show changes.
        line.draggable = true;
        line.addEventListener('dragstart', (event) => {
          this.#draggingShow = entry.id;
          if (event.dataTransfer !== null) {
            event.dataTransfer.effectAllowed = 'move';
            event.dataTransfer.setData('text/plain', entry.name);
          }
        });
        line.addEventListener('dragend', () => {
          this.#draggingShow = null;
        });

        line.append(row);

        // **The one press.** There was an Edit beside this, and the two did
        // the same thing: `#openShow` hands the wall over before it fetches
        // the document, because the server refuses every edit to a show it
        // does not have open. So "Edit" was LOAD under a word that hid the
        // consequence (owner, 2026-09-27: "Editiert wird nur die aktive Show
        // … tun aber irgendwie das selbe").
        //
        // It still asks twice, because it is still the press that takes what
        // is on the wall away: the server applies the restart policy and every
        // layer starts at its opening slot.
        if (entry.loadable) {
          line.append(
            entry.id === live
              ? make('span', 'show-live', 'ON AIR')
              : button(
                  this.#loading === entry.id ? 'LOAD — press again' : 'LOAD',
                  () => void this.#openShow(entry.id),
                  {
                    className:
                      this.#loading === entry.id ? 'show-load arming' : 'show-load',
                    title: `Hand the wall to ${entry.name} and open it for editing. Everything it names is started, and what is running now goes back to its opening slot.`,
                  },
                ),
          );
        }
        // Last on the row, because it is the one press that takes something
        // away. Offered for a show that cannot be opened too: a document this
        // build cannot read is exactly one somebody may want rid of.
        line.append(
          button(
            this.#deleting === entry.id ? 'Delete — press again' : 'Delete',
            () => void this.#removeShow(entry),
            {
              className: this.#deleting === entry.id ? 'show-delete arming' : 'show-delete',
              title: `Delete ${entry.name}. It does not come back.`,
            },
          ),
        );

        return line;
      }),
    );
  }

  /**
   * Deletes a show, on the second press.
   *
   * The first press arms it and says so on the button; any other press
   * disarms, because an operator who reached past it has decided not to answer
   * the question.
   */
  async #removeShow(entry: ShowListEntry): Promise<void> {
    if (this.#deleting !== entry.id) {
      this.#deleting = entry.id;
      this.draw();
      return;
    }
    this.#deleting = null;

    const answer = await this.#ovs.deleteShow(entry.id);
    if (!answer.ok) {
      this.#note(answer.message);
      this.draw();
      return;
    }

    // What was being looked at, or edited, is gone. Cleared rather than left
    // pointing at something the server no longer has.
    if (this.#peeking === entry.id) {
      this.#peeking = null;
    }
    if (this.#draft?.show.id === entry.id) {
      this.#draft = null;
    }
    this.#note(`${entry.name} is deleted.`);
    void this.load();
  }

  // --- the show list's own folders -----------------------------------------

  /**
   * The folder tree down the left of the show list.
   *
   * The same shape as the library's, because it is the same idea and an
   * operator should not have to learn it twice. Everything is one root called
   * "All shows", which is where every show nothing has filed already is.
   */
  #drawShowFolders(): void {
    const counts = countsByFolder(this.#shows);
    const rows: HTMLElement[] = [
      this.#folderRow(
        { path: ROOT_FOLDER, name: 'All shows', depth: -1, children: [] },
        this.#shows.length,
      ),
    ];

    if (this.#naming?.of === 'new' && this.#naming.parent === ROOT_FOLDER) {
      rows.push(this.#folderNameBox(0));
    }

    const walk = (nodes: readonly FolderNode[]): void => {
      for (const node of nodes) {
        if (this.#naming?.of === 'rename' && this.#naming.path === node.path) {
          rows.push(this.#folderNameBox(node.depth, node.name));
        } else {
          rows.push(this.#folderRow(node, counts.get(node.path) ?? 0));
        }
        if (this.#naming?.of === 'new' && this.#naming.parent === node.path) {
          rows.push(this.#folderNameBox(node.depth + 1));
        }
        // Closed folders hide what is under them, but a folder on the way to
        // the one that is open is always shown: otherwise selecting a nested
        // folder would make it disappear.
        if (this.#open.has(node.path) || isInside(this.#folder, node.path)) {
          walk(node.children);
        }
      }
    };
    walk(folderTree(this.#folders));

    this.#ui.showFolders.replaceChildren(...rows);
  }

  #folderRow(node: FolderNode, count: number): HTMLElement {
    const row = make('div', 'folder-row');
    row.style.setProperty('--depth', String(Math.max(0, node.depth)));
    row.classList.toggle('selected', node.path === this.#folder);

    // The root's children are always drawn, so it has nothing to twist open.
    // A control that does nothing is worse than no control.
    const hasChildren =
      node.path !== ROOT_FOLDER && this.#folders.some((path) => parentOf(path) === node.path);
    const twist = button(
      hasChildren ? (this.#openAt(node.path) ? '\u25be' : '\u25b8') : '',
      () => {
        if (this.#open.has(node.path)) {
          this.#open.delete(node.path);
        } else {
          this.#open.add(node.path);
        }
        this.draw();
      },
      { className: 'folder-twist', disabled: !hasChildren },
    );

    const name = button(
      node.path === ROOT_FOLDER ? 'All shows' : node.name,
      () => {
        this.#folder = node.path;
        this.#open.add(node.path);
        this.draw();
      },
      {
        className: 'folder-name',
        title:
          node.path === ROOT_FOLDER ? 'Everything nothing has filed' : node.path,
      },
    );

    row.append(twist, name);
    if (count > 0) {
      row.append(make('span', 'folder-count', String(count)));
    }

    if (node.path !== ROOT_FOLDER) {
      row.append(
        button('+', () => {
          this.#naming = { of: 'new', parent: node.path };
          this.#open.add(node.path);
          this.draw();
        }, { className: 'folder-act', title: 'a folder inside this one' }),
        button('\u270e', () => {
          this.#naming = { of: 'rename', path: node.path };
          this.draw();
        }, { className: 'folder-act', title: 'rename this folder' }),
        button('\u2715', () => void this.#deleteFolder(node.path), {
          className: 'folder-act',
          title: 'remove this folder. One that still holds a show is refused.',
        }),
      );
    }

    // A show dropped on a folder is filed there. Nothing about the show
    // changes: filing is a label on this server's list.
    row.addEventListener('dragover', (event) => {
      if (this.#draggingShow === null) {
        return;
      }
      event.preventDefault();
      if (event.dataTransfer !== null) {
        event.dataTransfer.dropEffect = 'move';
      }
      row.classList.add('drop-target');
    });
    row.addEventListener('dragleave', () => row.classList.remove('drop-target'));
    row.addEventListener('drop', (event) => {
      event.preventDefault();
      row.classList.remove('drop-target');
      const carried = this.#draggingShow;
      this.#draggingShow = null;
      if (carried !== null) {
        void this.#fileShow(carried, node.path);
      }
    });

    return row;
  }

  /** Whether a folder is drawn open, which the selected one's ancestors are. */
  #openAt(path: string): boolean {
    return this.#open.has(path) || isInside(this.#folder, path);
  }

  /** The box a folder is named in, drawn where the folder will be. */
  #folderNameBox(depth: number, value = ''): HTMLElement {
    const row = make('div', 'folder-row naming');
    row.style.setProperty('--depth', String(depth));
    const box = make('input', 'folder-name-box');
    box.type = 'text';
    box.value = value;
    box.placeholder = 'Folder name';

    const finish = (): void => {
      const naming = this.#naming;
      this.#naming = null;
      const name = box.value.trim();
      if (naming === null || name === '' || name === value) {
        this.draw();
        return;
      }
      const wrong = folderNameProblem(name);
      if (wrong !== null) {
        this.#note(wrong);
        this.draw();
        return;
      }
      if (naming.of === 'new') {
        void this.#createFolder(joinFolder(naming.parent, name));
      } else {
        void this.#renameFolder(naming.path, joinFolder(parentOf(naming.path), name));
      }
    };

    box.addEventListener('keydown', (event) => {
      if (event.key === 'Enter') {
        event.preventDefault();
        box.blur();
      }
      if (event.key === 'Escape') {
        event.preventDefault();
        this.#naming = null;
        this.draw();
      }
    });
    box.addEventListener('blur', finish);

    row.append(box);
    // Focused after it is in the page, which is the only moment it can be.
    window.setTimeout(() => box.focus(), 0);
    return row;
  }

  async #loadFolders(): Promise<void> {
    const answer = await this.#ovs.showFolders();
    if (!answer.ok) {
      this.#note(answer.message);
      return;
    }
    this.#folders = answer.value;
    this.draw();
  }

  async #createFolder(path: string): Promise<void> {
    const answer = await this.#ovs.createShowFolder(path);
    if (!answer.ok) {
      this.#note(answer.message);
      this.draw();
      return;
    }
    this.#open.add(parentOf(path));
    this.#folder = path;
    await this.#loadFolders();
  }

  async #renameFolder(from: string, to: string): Promise<void> {
    const answer = await this.#ovs.renameShowFolder(from, to);
    if (!answer.ok) {
      this.#note(answer.message);
      this.draw();
      return;
    }
    if (isInside(this.#folder, from)) {
      this.#folder = to + this.#folder.slice(from.length);
    }
    await this.#loadFolders();
    await this.#refreshShows();
  }

  async #deleteFolder(path: string): Promise<void> {
    // No "are you sure": the server refuses a folder that still holds a
    // show, so the only folder this can remove is an empty one, and an
    // empty folder is nothing to lose.
    const answer = await this.#ovs.deleteShowFolder(path);
    if (!answer.ok) {
      this.#note(answer.message);
      return;
    }
    if (isInside(this.#folder, path)) {
      this.#folder = parentOf(path);
    }
    await this.#loadFolders();
  }

  /** Files a show in a folder. The document is untouched. */
  async #fileShow(showId: string, folder: string): Promise<void> {
    const answer = await this.#ovs.fileShow(showId, folder);
    if (!answer.ok) {
      this.#note(answer.message);
      return;
    }
    await this.#refreshShows();
  }

  // --- a look inside one that is not open -------------------------------------

  /**
   * What the show in the middle panel holds.
   *
   * The owner's question, 2026-09-23: which outputs are in it, how many, and
   * which pieces — "wenn man auf ein anderes Projekt als das aktive klickt".
   * A list of names cannot answer "which of these forty is the one with the two
   * walls in it", and opening a show to find out is an edit nobody asked
   * for.
   *
   * The whole document, because `GET /shows/{id}` already answers with it
   * and a summary endpoint would be a second thing to keep in step. Nothing is
   * adopted: this is read, drawn, and forgotten when another is clicked.
   */
  #drawPreview(): void {
    const show = this.#peeked;
    if (show === null) {
      this.#ui.previewTitle.textContent = 'Show';
      this.#ui.previewSays.textContent = '';
      this.#ui.previewBody.replaceChildren(
        make(
          'p',
          'empty-note',
          this.#peeking === null
            ? 'Click a show to see what is in it. Editing it is a separate press, and LOAD is another.'
            : 'Reading…',
        ),
      );
      return;
    }

    this.#ui.previewTitle.textContent = show.name;
    const pieces = show.pieces.length;
    const layers = show.layers.length;
    this.#ui.previewSays.textContent = `${show.outputs.length} ${
      show.outputs.length === 1 ? 'output' : 'outputs'
    } \u00b7 ${pieces} ${pieces === 1 ? 'piece' : 'pieces'} \u00b7 ${layers} ${
      layers === 1 ? 'layer' : 'layers'
    }`;

    const blocks: HTMLElement[] = [];

    if (show.outputs.length === 0) {
      blocks.push(
        make('p', 'empty-note', 'No outputs'),
      );
    }

    for (const output of show.outputs) {
      const drawn = show.pieces.filter(
        (piece) => piece.output_id === output.id && piece.enabled,
      );
      const all = show.pieces.filter((piece) => piece.output_id === output.id);

      const block = make('div', 'preview-output');
      block.append(
        make('div', 'preview-output-name', output.name),
        make(
          'div',
          'preview-output-said',
          `${output.mode.size.width}\u00d7${output.mode.size.height} \u00b7 ${drawn.length} drawn of ${all.length}`,
        ),
      );

      // The pieces on it, to scale. A picture rather than a list: "two walls
      // side by side" and "one wall in four strips" read the same as numbers
      // and not at all the same as rectangles.
      const stage = make('div', 'stage preview-stage');
      const { width, height } = output.mode.size;
      stage.style.aspectRatio = `${width} / ${height}`;
      // The height budget as a width cap. A box with an aspect ratio takes its
      // height from its width, so capping the height would squash it rather
      // than narrow it — which is how a 1920x1080 wall was being drawn at
      // about 5:2 (owner, 2026-09-23).
      stage.style.maxWidth = `calc(38vh * ${width} / ${height})`;
      for (const piece of all) {
        const box = make('div', 'stage-rect');
        box.classList.toggle('off', !piece.enabled);
        box.style.left = `${(piece.dest_rect.x / output.mode.size.width) * 100}%`;
        box.style.top = `${(piece.dest_rect.y / output.mode.size.height) * 100}%`;
        box.style.width = `${(piece.dest_rect.width / output.mode.size.width) * 100}%`;
        box.style.height = `${(piece.dest_rect.height / output.mode.size.height) * 100}%`;
        if (piece.rotation_deg !== 0) {
          box.style.transform = `rotate(${piece.rotation_deg}deg)`;
        }
        box.append(make('span', 'stage-rect-label', piece.name));
        stage.append(box);
      }
      block.append(stage);
      blocks.push(block);
    }

    this.#ui.previewBody.replaceChildren(...blocks);
  }

  /** Reads one show's document, to show what is in it. */
  /**
   * Writes the show being looked at to a file.
   *
   * The package is the document and an inventory of the media it needs, not
   * the media itself: a package is something a server can validate before a
   * byte of video moves, and the files follow through the ordinary upload
   * path — only the ones the receiving server does not already hold.
   */
  async #exportShow(withMedia = false): Promise<void> {
    const showId = this.#peeking;
    if (showId === null) {
      this.#note('choose a show in the list before exporting it');
      return;
    }
    const named = this.#shows.find((entry) => entry.id === showId)?.name ?? 'show';
    if (withMedia) {
      this.#note(`writing ${named} and its clips…`);
    }
    const answer = withMedia
      ? await this.#ovs.exportShowWithMedia(showId, named)
      : await this.#ovs.exportShow(showId, named);
    if (!answer.ok) {
      this.#note(answer.message);
      return;
    }
    if (answer.value !== null) {
      this.#note(`${named} written to ${answer.value}`);
    }
  }

  /**
   * Reads a package and puts it on this server.
   *
   * What it says afterwards is what the server had to change: an import reuses
   * media this server already holds, by content, so a package carried from
   * another machine can arrive pointing at clips that are already here under
   * different identities. Saying nothing about that would leave an operator to
   * discover it on a wall.
   */
  async #importShow(): Promise<void> {
    const answer = await this.#ovs.importShow();
    if (!answer.ok) {
      this.#note(answer.message);
      return;
    }
    if (answer.value === null) {
      return;
    }

    const { show_id: id, relinked, missing } = answer.value;
    const notes: string[] = [];
    if (relinked.length > 0) {
      notes.push(`${relinked.length} ${relinked.length === 1 ? 'clip was' : 'clips were'} pointed at what this server already holds`);
    }
    if (missing.length > 0) {
      notes.push(`${missing.length} ${missing.length === 1 ? 'clip is' : 'clips are'} not here yet and must be uploaded`);
    }
    this.#note(notes.length === 0 ? 'the show was imported' : `imported: ${notes.join('; ')}`);

    await this.#refreshShows();
    await this.#peek(id);
  }

  async #peek(showId: string): Promise<void> {
    if (this.#peeking === showId) {
      return;
    }
    this.#peeking = showId;
    this.#peeked = null;
    this.draw();

    const answer = await this.#ovs.loadShow(showId);
    // Another one was clicked while this was in flight. What came back is
    // about a show nobody is looking at any more.
    if (this.#peeking !== showId) {
      return;
    }
    if (!answer.ok) {
      this.#note(answer.message);
      this.#peeking = null;
      this.draw();
      return;
    }
    this.#peeked = answer.value;
    this.draw();
  }

  #drawDocument(): void {
    const draft = this.#draft;
    // One Save for the whole of Settings (owner, 2026-09-23). Every tab edits
    // the same document, so which one it was pressed on is not a distinction
    // the server has.
    //
    // Both are dead until something has actually changed, and dead again the
    // moment it is stored: "Ich brauche nichts speichern oder wiederrufen,
    // wenn ich nichts geändert habe." The button's own state is the whole of
    // what has to be said, so there is no word beside it saying it again.
    const dirty = draft !== null && isDirty(draft);
    this.#ui.save.disabled = !dirty || this.#saving;
    this.#ui.save.textContent = this.#saving ? 'Saving…' : 'Save';
    this.#ui.revert.disabled = !dirty || this.#saving;

    if (draft === null) {
      this.#ui.documentFields.replaceChildren(
        make('p', 'empty-note', 'Open a show to edit it, or make a new one.'),
      );
      this.#ui.documentNote.textContent = '';
      this.#ui.documentErrors.replaceChildren();
      this.#ui.layersErrors.replaceChildren();
      return;
    }

    const show = draft.show;
    const name = this.#text(show.name, (value) =>
      this.#edit(renameShow(show, value)),
    );
    const notes = make('textarea');
    notes.rows = 3;
    notes.value = show.settings.notes ?? '';
    notes.addEventListener('change', () => this.#edit(setNotes(show, notes.value)));

    const autoSave = this.#toggle(show.settings.auto_save, (value) =>
      this.#edit(setAutoSave(show, value)),
    );

    this.#ui.documentFields.replaceChildren(
      this.#field('Name', name, 'name'),
      this.#field('Notes', notes),
      this.#inline('Save automatically on the server', autoSave),
    );

    const messages: string[] = [];
    if (isDirty(draft)) {
      messages.push('This document has changes the server has not got.');
    }
    if (show.id === this.#liveShowId()) {
      messages.push(
        'This is the show the server has open: saving it puts these changes on the wall. '  +
          'The show keeps running — nothing restarts.',
      );
    }
    this.#ui.documentNote.textContent = messages.join(' ');

    // Every problem, including ones in parts of the document this editor has no
    // control for. A refused save that mentioned outputs would otherwise be a
    // message the operator never saw.
    const problems = () =>
      this.#problemList().map((problem) => {
        const row = make('li');
        row.append(
          make('code', undefined, problem.path),
          document.createTextNode(` ${problem.reason}`),
        );
        return row;
      });
    this.#ui.documentErrors.replaceChildren(...problems());
    // Built twice rather than moved: one list of nodes cannot be in two
    // places, and a refused save has to be readable from whichever tab the
    // operator pressed it on.
    this.#ui.layersErrors.replaceChildren(...problems());
  }

  /**
   * The master settings.
   *
   * There is one master clock and one crossfader, and this is what they are
   * restored to — at load, and after a restart, because live state is not kept
   * across one. What the master is doing *now* is the strip in
   * Live Mode, which sends to the server and never writes here; the labels say
   * "default" for that reason and not out of pedantry.
   *
   * Every bound is the model's, from `limits.ts`. A value outside one is
   * refused at the control rather than at the save, which is the hole this
   * closes: until now a document with a bad BPM was reported and could not be
   * corrected, because there was no control to correct it with.
   */
  #drawMaster(): void {
    const draft = this.#draft;
    if (!draft) {
      this.#ui.masterFields.replaceChildren(make('p', 'empty-note', 'No show open.'));
      return;
    }

    const show = draft.show;
    const master = show.master;
    const change = (next: Parameters<typeof setMaster>[1]): void =>
      this.#edit(setMaster(show, next));

    const bpm = this.#numeric(master.default_bpm, { min: MIN_BPM, max: MAX_BPM, step: 'any' });
    bpm.addEventListener('change', () => change({ default_bpm: Number(bpm.value) }));

    const beats = this.#numeric(master.beats_per_bar, { min: MIN_BEATS_PER_BAR, step: '1' });
    beats.addEventListener('change', () => change({ beats_per_bar: Number(beats.value) }));

    const speed = this.#numeric(master.default_speed_scale, {
      min: MIN_SPEED_SCALE,
      max: MAX_SPEED_SCALE,
      step: 'any',
    });
    speed.addEventListener('change', () => change({ default_speed_scale: Number(speed.value) }));

    const curve = this.#select(
      CROSSFADER_CURVES.map((value) => ({ value, label: CROSSFADER_CURVE_LABELS[value] })),
      master.crossfader_curve,
      (value) => change({ crossfader_curve: value as MasterConfig['crossfader_curve'] }),
    );

    const dimmer = this.#fraction(master.default_dimmer, (value) =>
      change({ default_dimmer: value }),
    );
    const crossfader = this.#fraction(master.default_crossfader, (value) =>
      change({ default_crossfader: value }),
    );

    // Two to a line. Six one-line numbers down a narrow column made a panel
    // that had to be scrolled to read six numbers (owner, 2026-09-23), and
    // these pair naturally: the tempo with its bar, the speed with the curve,
    // and the two faders with each other.
    const pair = (left: HTMLElement, right: HTMLElement): HTMLElement => {
      const row = make('div', 'row');
      row.append(left, right);
      return row;
    };

    this.#ui.masterFields.replaceChildren(
      pair(
        this.#field('Default BPM', bpm, 'master.default_bpm'),
        this.#field('Beats per bar', beats, 'master.beats_per_bar'),
      ),
      pair(
        this.#field('Master speed', speed, 'master.default_speed_scale'),
        this.#field('Crossfader curve', curve, 'master.crossfader_curve'),
      ),
      // The two faders stay one to a line. A slider needs a run of width to be
      // worth dragging, and paired they were narrower than their own labels.
      this.#field('Dimmer at startup', dimmer.control, 'master.default_dimmer', dimmer.readout),
      this.#field(
        'Crossfader at startup',
        crossfader.control,
        'master.default_crossfader',
        crossfader.readout,
      ),
    );
  }

  #drawLayers(): void {
    const show = this.#draft?.show;
    if (!show) {
      this.#ui.layerList.replaceChildren(make('p', 'empty-note', 'No show open.'));
      this.#ui.addLayer.disabled = true;
      return;
    }
    this.#ui.addLayer.disabled = false;

    if (show.layers.length === 0) {
      this.#ui.layerList.replaceChildren(
        make('p', 'empty-note', 'No layers yet'),
      );
      return;
    }

    // Top of the list is the top of the composite, which is how an operator
    // reads a stack of layers and how the Live Mode matrix draws one.
    const ordered = [...orderedLayers(show)].reverse();
    this.#ui.layerList.replaceChildren(
      ...ordered.map((layer) => {
        const row = make('div', 'layer-row');
        row.classList.toggle('selected', layer.id === this.#selectedLayer);
        row.classList.toggle('off', !layer.enabled);

        const wrong = this.#problemsUnder(layerPath(show, layer.id) ?? '');
        row.classList.toggle('wrong', wrong.length > 0);
        if (wrong.length > 0) {
          row.title = wrong.join('; ');
        }

        const enabled = this.#toggle(layer.enabled, (value) =>
          this.#edit(setLayer(show, layer.id, { enabled: value })),
        );
        enabled.title = 'whether this layer contributes to its bus at all';

        const name = make('button', 'layer-row-name', layer.name);
        name.type = 'button';
        name.addEventListener('click', () => {
          this.#selectedLayer = layer.id;
          this.#bank = 0;
          this.draw();
        });

        const stack = make('div', 'stack');
        stack.append(
          button('▲', () => this.#edit(moveLayer(show, layer.id, 'up')), {
            title: 'one place towards the front of the composite',
          }),
          button('▼', () => this.#edit(moveLayer(show, layer.id, 'down')), {
            title: 'one place towards the back of the composite',
          }),
          button('✕', () => this.#edit(removeLayer(show, layer.id)), {
            title: 'remove this layer',
          }),
        );

        row.append(enabled, name, make('span', 'order', `${layer.route} · ${layer.order}`), stack);
        return row;
      }),
    );
  }

  #drawLayer(): void {
    const show = this.#draft?.show;
    const layer = show && this.#selectedLayer ? findLayer(show, this.#selectedLayer) : null;
    this.#ui.layerPanel.hidden = layer === null;
    this.#ui.layerEffectsPanel.hidden = layer === null;
    if (!show || !layer) {
      return;
    }

    const base = layerPath(show, layer.id) ?? '';
    this.#ui.layerTitle.textContent = `Layer · ${layer.name}`;

    const name = this.#text(layer.name, (value) =>
      this.#edit(renameLayer(show, layer.id, value)),
    );

    const opacity = this.#fraction(layer.default_opacity, (value) =>
      this.#edit(setLayer(show, layer.id, { default_opacity: value })),
    );

    // The name, and what it opens at. Nothing else (owner, 2026-09-23).
    //
    // **Route, blend mode and transform are live state**, and an operator
    // moves all three while looking at the wall — route and blend on the layer
    // row, and a transform is a gesture rather than a form. They were here as
    // "what the show opens with", which is a distinction nobody was asking
    // for and a second place to set the same three things.
    //
    // **A layer has no quantisation at all.** It belongs to the slot, because
    // how many beats a piece of footage takes is a fact about the footage; one
    // on the layer applied a single answer to every slot on it.
    this.#ui.layerFields.replaceChildren(
      this.#field('Name', name, `${base}.name`),
      this.#field(
        'Opacity at startup',
        opacity.control,
        `${base}.default_opacity`,
        opacity.readout,
      ),
    );

    this.#drawChain(this.#ui.layerEffects, { layerId: layer.id }, layer.effects);
  }

  // --- effect chains --------------------------------------------------------

  #drawChain(container: HTMLElement, chain: ChainRef, effects: EffectInstance[]): void {
    const show = this.#draft?.show;
    if (!show) {
      return;
    }

    const blocks: HTMLElement[] = [];
    if (effects.length === 0) {
      blocks.push(
        make(
          'p',
          'empty-note',
          'No effects',
        ),
      );
    }

    for (const instance of effects) {
      const block = make('div', 'effect');
      block.classList.toggle('off', !instance.enabled);
      const path = effectPath(show, chain, instance.id) ?? '';

      const published = findEffect(this.#catalogue, instance.type_id, instance.version);
      const head = make('div', 'effect-head');
      head.append(
        this.#toggle(instance.enabled, (value) =>
          this.#edit(setEffectEnabled(show, chain, instance.id, value)),
        ),
        make('span', 'effect-name', published?.name ?? instance.type_id),
        make('span', 'effect-version', `v${instance.version}`),
        button('▲', () => this.#edit(moveEffect(show, chain, instance.id, 'earlier')), {
          title: 'one place earlier in the chain',
        }),
        button('▼', () => this.#edit(moveEffect(show, chain, instance.id, 'later')), {
          title: 'one place later in the chain',
        }),
        button('✕', () => this.#edit(removeEffect(show, chain, instance.id)), {
          title: 'remove this effect',
        }),
      );
      block.append(head);

      if (published) {
        if (published.description !== '') {
          block.append(make('p', 'hint', published.description));
        }
        block.append(
          ...this.#parameters(published, instance.parameters, `${path}.parameters`, (key, value) =>
            this.#edit(
              setEffectParameter(show, this.#catalogue, chain, instance.id, key, value),
            ),
          ),
        );
      } else {
        block.append(
          make(
            'p',
            'empty-note',
            `This server does not publish ${instance.type_id} version ${instance.version}. It is kept in the document exactly as it was.`,
          ),
        );
      }

      blocks.push(block);
    }

    const add = make('div', 'add-effect');
    const chooser = this.#select(
      [
        { value: '', label: 'Add an effect…' },
        ...this.#catalogue.effects.map((effect) => ({
          value: `${effect.type_id}:${effect.version}`,
          label: effect.name,
        })),
      ],
      '',
      (value) => {
        const split = value.lastIndexOf(':');
        if (split < 0) {
          return;
        }
        this.#edit(
          addEffect(
            show,
            this.#catalogue,
            chain,
            value.slice(0, split),
            Number(value.slice(split + 1)),
          ),
        );
      },
    );
    add.append(chooser);

    const chainNode = make('div', 'chain');
    chainNode.append(...blocks);
    container.replaceChildren(chainNode, add);
  }

  /**
   * Draws one control per parameter, from what the catalogue declares.
   *
   * The type decides the control, the range bounds it and the default is what
   * it shows when the show has set nothing. None of the three is written
   * into this client, which is what lets a server publish an effect this build
   * has never heard of and have it appear here with working controls.
   */
  #parameters(
    module: Module,
    values: ParameterMap | undefined,
    basePath: string,
    onSet: (key: string, value: ParameterValue | null) => void,
  ): HTMLElement[] {
    return module.parameters.map((parameter) => {
      const current = parameterValue(module, values, parameter.key);
      const set = values?.[parameter.key] !== undefined;
      const control = this.#parameterControl(parameter, current, (value) =>
        onSet(parameter.key, value),
      );
      const extra = set
        ? button('Default', () => onSet(parameter.key, null), {
            title: 'back to what the effect was published with',
          })
        : make('span', 'when', 'default');
      return this.#field(parameter.name, control, `${basePath}.${parameter.key}`, extra);
    });
  }

  #parameterControl(
    parameter: ModuleParameter,
    current: ParameterValue | null,
    onSet: (value: ParameterValue) => void,
  ): HTMLElement {
    const type = parameter.parameter_type;
    switch (type.kind) {
      case 'number': {
        const input = make('input');
        input.type = 'number';
        input.min = String(type.min);
        input.max = String(type.max);
        // No step: the catalogue says what the ends of the range are and says
        // nothing about how finely it may be divided, so neither does this.
        input.step = 'any';
        input.value = String(current?.type === 'float' || current?.type === 'int' ? current.value : type.default);
        input.addEventListener('change', () =>
          onSet({ type: 'float', value: Number(input.value) }),
        );
        return input;
      }
      case 'colour': {
        // **A picker, and alpha beside it** (owner, 2026-09-27). It was four
        // linear RGBA fields, with a note here saying a picker would need an
        // sRGB conversion this client had no business choosing on an
        // operator's behalf — which was about choosing one silently. It is
        // chosen now, written out in `shared/colour.ts` and tested against its
        // own anchors, so the note no longer applies.
        //
        // Alpha stays a field because `input[type=color]` has none, and this is
        // the document: a colour saved with the wrong alpha is saved wrong.
        const channels = current?.type === 'color' ? current.value : type.default;
        const row = make('div', 'row');
        const swatch = make('input');
        swatch.type = 'color';
        swatch.value = hexFromLinear(channels);
        const alpha = make('input');
        alpha.type = 'number';
        alpha.min = '0';
        alpha.max = '1';
        alpha.step = 'any';
        alpha.value = String(channels[3] ?? 1);
        const picked = (): void => {
          const [red, green, blue] = linearFromHex(swatch.value);
          const opacity = Number(alpha.value);
          onSet({
            type: 'color',
            value: [red, green, blue, Number.isFinite(opacity) ? opacity : 1],
          });
        };
        swatch.addEventListener('change', picked);
        alpha.addEventListener('change', picked);
        row.append(this.#field('Colour', swatch), this.#field('Alpha', alpha));
        return row;
      }
      case 'toggle': {
        return this.#toggle(current?.type === 'bool' ? current.value : type.default, (value) =>
          onSet({ type: 'bool', value }),
        );
      }
      case 'text': {
        // A box rather than a line: the text generator sets a line break as a
        // line break, so somebody writing two lines has to be able to type
        // one (owner decision Q11).
        const input = document.createElement('textarea');
        input.rows = 2;
        input.maxLength = type.max_chars;
        input.value = current?.type === 'text' ? current.value : type.default;
        input.addEventListener('change', () => onSet({ type: 'text', value: input.value }));
        return input;
      }
      case 'choice': {
        // A menu, which is the whole reason this kind exists: the shape
        // generator used to offer a fader labelled "Shape (0 ellipse, 1
        // rectangle, 2 triangle)" and ask an operator to guess.
        const select = document.createElement('select');
        for (const choice of type.choices) {
          const option = document.createElement('option');
          option.value = choice.id;
          option.textContent = choice.name;
          select.append(option);
        }
        select.value = current?.type === 'choice' ? current.value : type.default;
        select.addEventListener('change', () => onSet({ type: 'choice', value: select.value }));
        return select;
      }
      case 'image': {
        const option = (value: string, label: string): HTMLOptionElement => {
          const node = document.createElement('option');
          node.value = value;
          node.textContent = label;
          return node;
        };
        // The library, never a text box. This is the one parameter whose value
        // names something somebody uploaded, and typing a twenty-six character
        // identifier is not a thing anybody can do.
        const select = document.createElement('select');
        select.append(option('', 'None'));
        for (const item of this.#media) {
          // Stills only, and only the ones that finished arriving: a clip
          // cannot be a stencil and an item still uploading has no pixels yet.
          if (item.kind === 'image' && item.state === 'ready') {
            select.append(option(item.id, item.display_name));
          }
        }
        const chosen = current?.type === 'text' ? current.value : '';
        if (chosen !== '' && !this.#media.some((item) => item.id === chosen)) {
          // A picture the library no longer has is still what the show says.
          // Dropping it from the menu would quietly empty the parameter the
          // next time anybody touched this screen.
          select.append(option(chosen, `${chosen} â€” not in the library`));
        }
        select.value = chosen;
        select.addEventListener('change', () => onSet({ type: 'text', value: select.value }));
        return select;
      }
    }
  }

  // --- controls -------------------------------------------------------------

  #field(label: string, control: HTMLElement, path?: string, extra?: HTMLElement): HTMLElement {
    const wrap = make('div', 'field');
    const labelNode = make('label');
    labelNode.append(make('span', undefined, label));
    if (extra) {
      labelNode.append(extra);
    }

    if (control.id === '') {
      this.#controlSeq += 1;
      control.id = `control-${this.#controlSeq}`;
    }
    labelNode.htmlFor = control.id;

    wrap.append(labelNode, control);

    if (path !== undefined) {
      wrap.dataset['path'] = path;
      // Anything the server or this editor said about this field, said here
      // rather than only in the list at the side.
      const reasons = this.#problems.get(path);
      if (reasons) {
        wrap.classList.add('wrong');
        for (const reason of reasons) {
          wrap.append(make('span', 'why', reason));
        }
      }
    }
    return wrap;
  }

  #inline(label: string, control: HTMLElement): HTMLElement {
    const wrap = make('div', 'inline');
    wrap.append(control, make('span', undefined, label));
    return wrap;
  }

  #text(value: string, onCommit: (value: string) => void): HTMLInputElement {
    const input = make('input');
    input.type = 'text';
    input.value = value;
    input.autocomplete = 'off';
    // On change rather than on input: a document is not edited a keystroke at a
    // time, and a name is not empty because somebody is halfway through typing
    // one.
    input.addEventListener('change', () => onCommit(input.value));
    return input;
  }

  #toggle(value: boolean, onChange: (value: boolean) => void): HTMLInputElement {
    const input = make('input');
    input.type = 'checkbox';
    input.checked = value;
    input.addEventListener('change', () => onChange(input.checked));
    return input;
  }

  /**
   * A number with the model's own range on it.
   *
   * The bounds go on the control as well as into the check, so the spinner
   * stops where the model does; the check still runs, because a typed value is
   * not bound by `min` and `max` and the useful answer to one outside them is
   * the reason, not a silent correction.
   */
  #numeric(
    value: number,
    bounds: { min?: number; max?: number; step?: string },
  ): HTMLInputElement {
    const input = make('input');
    input.type = 'number';
    if (bounds.min !== undefined) {
      input.min = String(bounds.min);
    }
    if (bounds.max !== undefined) {
      input.max = String(bounds.max);
    }
    input.step = bounds.step ?? 'any';
    input.value = String(value);
    return input;
  }

  #select(
    options: Array<{ value: string; label: string }>,
    value: string,
    onChange: (value: string) => void,
  ): HTMLSelectElement {
    const select = make('select');
    select.replaceChildren(
      ...options.map((option) => {
        const node = make('option', undefined, option.label);
        node.value = option.value;
        return node;
      }),
    );
    select.value = value;
    select.addEventListener('change', () => onChange(select.value));
    return select;
  }

  /** A 0..1 control with its own readout, for opacity. */
  #fraction(
    value: number,
    onCommit: (value: number) => void,
  ): { control: HTMLInputElement; readout: HTMLElement } {
    const readout = make('span', 'when', `${Math.round(value * 100)}%`);
    const input = make('input');
    input.type = 'range';
    input.min = String(MIN_FRACTION);
    input.max = String(MAX_FRACTION);
    input.step = 'any';
    input.value = String(value);
    input.addEventListener('input', () => {
      readout.textContent = `${Math.round(Number(input.value) * 100)}%`;
    });
    input.addEventListener('change', () => onCommit(Number(input.value)));
    return { control: input, readout };
  }

  // --- sources --------------------------------------------------------------

}

// --- reading and labelling ----------------------------------------------------

/** A timestamp as a local date and time, or nothing when there is none. */
function whenText(millis: number): string {
  return new Date(millis).toLocaleString();
}
