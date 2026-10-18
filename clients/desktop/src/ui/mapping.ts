// The mapping editor: composition → pieces → outputs.
//
// A piece takes a rectangle of the composition and puts it on a rectangle of an
// output. This panel shows both ends of that at once — the
// composition with every piece drawn on it, and each output with what lands on
// it — because a mapping is a relationship between two surfaces and looking at
// one of them tells an operator half the story.
//
// Numbers as well as dragging, and the numbers are not the afterthought. An LED
// wall is patched to exact pixel boundaries: a processor's documentation says
// the panel starts at 1920, 0 and is 1536×768, and an operator types that. The
// rectangles are for seeing whether the arrangement is right; the fields are for
// making it right.
//
// Three things it will not do.
//
// **It will not invent an output.** The connectors, their modes and what each
// socket is for all come from `/server/capabilities`. A machine that has not
// reported any says so rather than being offered a plausible HDMI-0.
//
// **It will not quietly correct a rectangle.** Dragging is bounded, because a
// gesture that ends in a refusal every time is not a control; typing is not,
// because the answer to a number that cannot be stored is to say so.
//
// **It will not call a legitimate mapping an error.** Two pieces overlapping on
// one output is something the server accepts and the renderer settles by draw
// order, so it is reported as what it is — which piece wins where they meet —
// on the row it belongs to rather than as a red field.

import type {
  ConnectorInfo,
  DisplayMode,
  OutputConfig,
  Piece,
  RectPx,
  Show,
  SizePx,
  TestPatternMode,
} from '@ovs/schema';

import {
  addNetworkOutput,
  addOutput,
  addPiece,
  duplicatePiece,
  movePiece,
  outputPath,
  piecePath,
  removeOutput,
  removePiece,
  renameOutput,
  restoreMapping,
  renamePiece,
  setComposition,
  setOutputConnector,
  setOutputOnNetwork,
  setOutputEnabled,
  setOutputMode,
  setPiece,
  setPieceDest,
  setPieceSource,
  type Edit,
} from '../shared/editor.js';
import { CONNECTOR_ROLE_LABELS, FRAME_RATES, MIN_DIMENSION_PX } from '../shared/limits.js';
import {
  boundsOf,
  nudged,
  proportionalScale,
  anchorOf,
  placedAt,
  sourceFromKey,
  sourceKeyOf,
  PIECE_SOURCES,
  resizedAbout,
  sizedAbout,
  CENTRE_ANCHOR,
  snapGuides,
  snapPointsOf,
  type SnapGuides,
  type SnapPoint,
  surfacePoints,
  SNAP_WITHIN_PX,
  scaledAbout,
  connectorFor,
  connectorKey,
  connectorOf,
  findOutput,
  findPiece,
  millihertzOf,
  modeText,
  rateFromMillihertz,
  rateText,
  sequenceOf,
} from '../shared/mapping.js';
import { button, element, make } from './dom.js';

/** What the mapping panel needs from the editor that owns the document. */
export interface MappingDeps {
  /** The document in hand, or null when no show is open. */
  show: () => Show | null;
  /** Applies an edit and redraws the whole editor, or says why it was refused. */
  apply: (edit: Edit) => void;
  /**
   * Which show the server is running, or null when it is running none.
   *
   * What decides whether an edit here can reach a wall: a mapping made for
   * another show is a document being written, and there is nothing to put
   * it on.
   */
  liveShowId: () => string | null;
  /**
   * Puts a mapping on the wall without saving the show.
   *
   * Answers with the server's own words when it refused, and null when it
   * took it (owner, 2026-09-23).
   */
  setMapping: (outputs: OutputConfig[], pieces: Piece[]) => Promise<string | null>;
  /**
   * Puts a test pattern on the walls, or takes it off.
   *
   * `highlighted` is the piece to draw as the chosen one, read only in
   * `PIECES` mode.
   */
  setTestPattern: (mode: TestPatternMode, highlighted: string | null) => void;
  /** Says something to the operator in the fault line at the foot of the window. */
  note: (message: string) => void;
  /** Everything wrong at exactly this path, from validation or a refused save. */
  problems: (path: string) => string[];
  /** The sockets this server reported. Empty until a renderer has said. */
  connectors: () => ConnectorInfo[];
  /**
   * Every mode a stream may run, as this server publishes it.
   *
   * The server's list rather than this client's: a stream has no panel to
   * satisfy, so what it may run is what the build allows.
   */
  networkModes: () => DisplayMode[];
  /**
   * Asks the operator for a picture to put behind the canvas, as a data URL.
   *
   * Null when they chose nothing. A working aid: it never reaches a show
   * and never reaches a server.
   */
  pickImage: () => Promise<string | null>;
}

/** Which half of a piece is being worked on. */
export type MappingFace = 'input' | 'output';

/**
 * A picture behind the canvas, and how strongly it shows.
 *
 * **Local to this client, always** (owner, 2026-09-23). It is never written
 * into a show, never sent to a server and never stored anywhere: it lives
 * in this window for as long as the window does. A photograph of a venue is a
 * working aid for the person lining rectangles up, not part of the show, and a
 * server that held one would be holding something no other operator asked for.
 */
interface Backdrop {
  /** A data URL, made in the main process from a file the operator chose. */
  source: string;
  /** Between 0 and 1. */
  opacity: number;
}

/** A rectangle drawn on a surface, and what it is. */
interface Drawn {
  pieceId: string;
  rect: RectPx;
  label: string;
  /** Whether the renderer would draw it at all. */
  drawn: boolean;
  rotationDeg: number;
  /** The point it turns about, as a fraction of its own size. */
  anchor: { x: number; y: number };
}

/** Which surface a gesture is moving a rectangle on. */
type Side = 'source' | 'dest';

/** Which corner or edge a drag is pulling, or the whole rectangle. */
type Grip = 'move' | 'n' | 's' | 'e' | 'w' | 'nw' | 'ne' | 'sw' | 'se';

/** The eight handles, in the order they are drawn round a rectangle. */
const GRIPS: readonly Exclude<Grip, 'move'>[] = ['nw', 'n', 'ne', 'e', 'se', 's', 'sw', 'w'];

/**
 * What the zoom steps through, as output pixels per screen pixel.
 *
 * `1` is one for one: what is on screen is what is on the wall, which is the
 * only scale at which a rectangle can be judged to the pixel. Fitted is not in
 * this list -- it is whatever the window happens to need, usually well under
 * one, and it is a state rather than a number.
 */
const ZOOMS: readonly number[] = [0.1, 0.25, 0.5, 1, 2, 4, 8, 16];

/**
 * The connector entry that means "on the network".
 *
 * Beside the sockets rather than in a chooser of its own, so an output can be
 * moved between a cable and the network the same way it is moved between two
 * cables (owner, 2026-09-25). Cannot collide with a real one: a connector's key
 * is an address and a name with a slash between them.
 */
const NETWORK_OUTPUT = 'ndi';

/** Composition → pieces → outputs. */
export class MappingPanel {
  readonly #deps: MappingDeps;

  #selectedPiece: string | null = null;
  /**
   * The output the controls act on, when no piece is selected.
   *
   * One or the other, never both: the buttons over the list have to be
   * about one thing, and an output with a piece selected inside it would
   * make Delete ambiguous at the moment it is pressed.
   */
  #selectedOutput: string | null = null;
  /**
   * A mode that has been typed or chosen and not yet set.
   *
   * On the panel rather than in the inputs, so a redraw between typing and
   * pressing Set does not lose it. Cleared when the selection moves, because
   * numbers meant for one output must never be sent to another.
   */
  #pendingMode: {
    outputId: string;
    chosen: string;
    mode: DisplayMode;
  } | null = null;
  /**
   * Output pixels per screen pixel, or `null` for fitted to the window.
   *
   * Fitted is a state rather than a number because the number it works out to
   * changes with the window. Not remembered across shows or faces: a canvas
   * that opened at the zoom somebody left it at would open showing a corner of
   * something.
   */
  #zoom: number | null = null;
  /**
   * Whether this screen has the pieces pattern on the walls.
   *
   * Held here rather than read back, because what it is for is knowing
   * what to take down: a show is not on the wall while this is true,
   * and nothing else on the machine knows this screen put it there.
   */
  #testingPieces = false;
  /** What the wall was last told to highlight, so it is not told twice. */
  #highlightSent: string | null = null;
  /**
   * Everything selected, including the one above.
   *
   * Several at once, so a row of panels can be moved and scaled as the row it
   * is (owner, 2026-09-23). `#selectedPiece` is still the one the right-hand
   * panel is about — a panel of numbers is about one rectangle — and is always
   * a member of this set while it is not null.
   */
  readonly #alsoSelected = new Set<string>();
  /**
   * The mapping this session started from, for putting it back.
   *
   * Taken when a show is opened and again after every Put back, so it is
   * always "the mapping before I started moving things". Held rather than
   * relied on from the draft, because the draft follows every edit — that is
   * what makes them visible on the wall — and so no longer remembers what was
   * there.
   */
  #before: { outputs: OutputConfig[]; pieces: Piece[] } | null = null;
  /** Whether a live push is in flight, so two drags cannot race each other. */
  #pushing = false;
  /** Whether another push was asked for while one was in flight. */
  #pushAgain = false;
  /** What the last push said, when it said anything. */
  #pushNote = '';
  /**
   * Which half of a piece this screen is about.
   *
   * Input Selection and Output Transformation, as the reference drawing has
   * them (owner, 2026-09-23): which rectangle of the composition a piece takes,
   * and where on a wall it lands. An operator is doing one or the other.
   */
  #face: MappingFace = 'input';
  /**
   * Whether the INPUT face shows every wall's pieces or only the chosen wall's.
   *
   * Two different questions. One wall answers "where does this wall's picture
   * come from", which is what somebody placing a piece is asking; all of them
   * answers "how is the composition divided up", which is what somebody planning
   * a rig is asking, and the overlapping outlines are then the answer rather
   * than the problem.
   *
   * This client's own, like the backdrop: it is how somebody is looking at the
   * show, not part of it, so it is never written into one.
   */
  #allPieces = false;
  /** A picture behind each face's canvas, while somebody is lining things up. */
  readonly #backdrops = new Map<MappingFace, Backdrop>();
  #controlSeq = 0;

  readonly #ui = {
    compositionFields: element('composition-fields'),
    compositionStage: element('composition-stage'),
    compositionCaption: element('composition-caption'),
    outputControls: element('output-controls'),
    outputs: element('output-list'),
    stageTitle: element<HTMLHeadingElement>('mapping-stage-title'),
    allPieces: element<HTMLInputElement>('mapping-all-pieces'),
    allPiecesLabel: element('stage-all-pieces'),
    zoomOut: element<HTMLButtonElement>('zoom-out'),
    zoomFit: element<HTMLButtonElement>('zoom-fit'),
    zoomIn: element<HTMLButtonElement>('zoom-in'),
    pieceNone: element('piece-none'),
    faceInput: element<HTMLButtonElement>('mapping-face-input'),
    faceOutput: element<HTMLButtonElement>('mapping-face-output'),
    faceTake: element<HTMLButtonElement>('mapping-face-take'),
    testPieces: element<HTMLButtonElement>('mapping-test-pieces'),
    backdropPick: element<HTMLButtonElement>('mapping-backdrop-pick'),
    backdropClear: element<HTMLButtonElement>('mapping-backdrop-clear'),
    backdropStrength: element<HTMLLabelElement>('mapping-backdrop-strength'),
    backdropOpacity: element<HTMLInputElement>('mapping-backdrop-opacity'),
    backdropValue: element('mapping-backdrop-value'),
    live: element('mapping-live-state'),
    revert: element<HTMLButtonElement>('mapping-revert'),
    addOutputDialog: element<HTMLDialogElement>('add-output-dialog'),
    addOutputFields: element('add-output-fields'),
    addOutputConfirm: element<HTMLButtonElement>('add-output-confirm'),
    addOutputCancel: element<HTMLButtonElement>('add-output-cancel'),
    piecePanel: element('piece-panel'),
    pieceTitle: element<HTMLHeadingElement>('piece-title'),
    pieceFields: element('piece-fields'),
  };

  constructor(deps: MappingDeps) {
    this.#deps = deps;

    this.#ui.revert.addEventListener('click', () => this.#putMappingBack());
    this.#ui.allPieces.addEventListener('change', () => {
      this.#allPieces = this.#ui.allPieces.checked;
      this.draw();
    });
    this.#ui.faceInput.addEventListener('click', () => this.#showFace('input'));
    this.#ui.faceOutput.addEventListener('click', () => this.#showFace('output'));
    this.#ui.outputs.addEventListener('mousedown', (event) => {
      if (event.target === this.#ui.outputs) {
        this.#clearSelection();
        this.draw();
      }
    });
    this.#ui.zoomOut.addEventListener('click', () => this.#stepZoom(-1));
    this.#ui.zoomIn.addEventListener('click', () => this.#stepZoom(1));
    this.#ui.zoomFit.addEventListener('click', () => {
      this.#zoom = null;
      this.draw();
    });
    // Control and the wheel, which is what every drawing program does and what
    // somebody working closely reaches for without being told.
    this.#ui.compositionStage.addEventListener(
      'wheel',
      (event) => {
        if (!event.ctrlKey) {
          return;
        }
        event.preventDefault();
        this.#stepZoom(event.deltaY < 0 ? 1 : -1);
      },
      { passive: false },
    );
    this.#ui.testPieces.addEventListener('click', () => {
      this.#testingPieces = !this.#testingPieces;
      this.#pushTestPattern();
      this.draw();
    });
    this.#ui.faceTake.addEventListener('click', () => {
      this.#takeOtherFace();
      this.draw();
    });
    this.#ui.addOutputCancel.addEventListener('click', () => this.#ui.addOutputDialog.close());
    this.#ui.backdropPick.addEventListener('click', () => void this.#pickBackdrop());
    this.#ui.backdropClear.addEventListener('click', () => {
      this.#backdrops.delete(this.#face);
      this.draw();
    });
    // The arrow keys belong to the window rather than to a box, because what
    // they move is a rectangle on a canvas that nothing focuses. Guarded on
    // the pane being the one on screen, so they do not move a mapping while
    // somebody is in MEDIA.
    document.addEventListener('keydown', (event) => {
      if (document.getElementById('settings-mapping')?.hidden === false) {
        if (event.key === 'Escape') {
          this.#clearSelection();
          this.draw();
          return;
        }
        this.#onArrow(event);
      }
    });
    this.#ui.backdropOpacity.addEventListener('input', () => {
      const backdrop = this.#backdrops.get(this.#face);
      if (backdrop !== undefined) {
        backdrop.opacity = Number(this.#ui.backdropOpacity.value) / 100;
        this.draw();
      }
    });
  }

  /**
   * Applies an edit and puts the mapping it produced on the wall.
   *
   * Every edit this panel makes goes through here, which is what makes the
   * mapping live: an operator lining a wall up sees each rectangle land
   * (owner, 2026-09-23). Edits that are not about the mapping — the
   * composition's own size — still pass through, and simply send a mapping the
   * server already has.
   */
  #edit(edit: Edit): void {
    this.#deps.apply(edit);
    void this.#pushMapping();
  }

  /** Which half of a piece is being worked on. */
  /**
   * Copies the other face's rectangle onto this one, for what is selected.
   *
   * The owner draws the pieces on the composition and then wants the wall to
   * start from those same rectangles rather than from a default that has to
   * be typed over. The two faces measure different surfaces, so this is a
   * starting point and not a link: moving one afterwards does not move the
   * other.
   *
   * Only what is selected. Several at once, because the selection is already
   * several — a row of panels is picked as the row it is — and never the whole
   * show: the mapping reaches the wall as it is made, so a button that rewrote
   * every rectangle would be on the wall before anybody could look at it.
   */
  #takeOtherFace(): void {
    const show = this.#deps.show();
    if (!show || this.#alsoSelected.size === 0) {
      return;
    }

    for (const id of this.#alsoSelected) {
      const piece = findPiece(show, id);
      if (!piece) {
        continue;
      }
      this.#edit(
        this.#face === 'output'
          ? setPieceDest(show, id, { ...piece.source_rect })
          : setPieceSource(show, id, { ...piece.dest_rect }),
      );
    }
  }

  #showFace(face: MappingFace): void {
    this.#face = face;
    this.draw();
  }

  /** Puts the switch where it means something, and takes it away where it does not. */
  #drawAllPieces(): void {
    this.#ui.allPiecesLabel.hidden = this.#face !== 'input';
    this.#ui.allPieces.checked = this.#allPieces;
  }

  /**
   * Puts a picture behind the canvas to line rectangles up against.
   *
   * A photograph of the wall, or a plan of it. **Temporary and this
   * client's own**: it is a working aid rather than show data, so it is
   * never written into the show and never sent to a server (owner,
   * 2026-09-23). Each face keeps its own, because the thing an operator
   * lines the composition up against is not the thing they line the wall
   * up against.
   */
  async #pickBackdrop(): Promise<void> {
    const answer = await this.#deps.pickImage();
    if (answer === null) {
      return;
    }
    this.#backdrops.set(this.#face, { source: answer, opacity: 0.4 });
    this.draw();
  }

  /** Draws the backdrop controls for whichever face is showing. */
  #drawBackdropControls(): void {
    const backdrop = this.#backdrops.get(this.#face) ?? null;
    this.#ui.backdropClear.hidden = backdrop === null;
    this.#ui.backdropStrength.hidden = backdrop === null;
    if (backdrop === null) {
      return;
    }
    const percent = Math.round(backdrop.opacity * 100);
    if (document.activeElement !== this.#ui.backdropOpacity) {
      this.#ui.backdropOpacity.value = String(percent);
    }
    this.#ui.backdropValue.textContent = `${percent}%`;
  }

  /**
   * Selects one piece, for a screen that has sent an operator here to see it.
   *
   * Live Mode's SLICES tab offers this: a slice is show configuration, so
   * what the live screen can honestly do with one is show it and open the
   * editor at it. Selecting changes no rectangle and no document.
   */
  select(pieceId: string | null): void {
    this.#selectedPiece = pieceId;
    this.#alsoSelected.clear();
    if (pieceId !== null) {
      this.#alsoSelected.add(pieceId);
    }
  }

  /**
   * Selects one piece, adding to the selection or replacing it.
   *
   * Ctrl or Shift adds, as it does in every list on every desktop. The one
   * pressed becomes the one the panel is about, because that is the one the
   * operator just pointed at.
   */
  #choose(pieceId: string, adding: boolean): void {
    // An output and a piece are never both active: the panel on the right is
    // about one thing, and Delete has to know which.
    this.#selectedOutput = null;

    if (adding) {
      // Ctrl or Shift on one that is already in takes it out again.
      if (this.#alsoSelected.delete(pieceId)) {
        if (this.#selectedPiece === pieceId) {
          this.#selectedPiece = [...this.#alsoSelected][0] ?? null;
        }
        return;
      }
      this.#alsoSelected.add(pieceId);
      this.#selectedPiece = pieceId;
      return;
    }

    // Pressing the only selected piece again deselects it; pressing one of
    // several makes it the only one, which is what narrowing a selection
    // means and what every list does.
    if (this.#selectedPiece === pieceId && this.#alsoSelected.size === 1) {
      this.#clearSelection();
      return;
    }

    this.#alsoSelected.clear();
    this.#alsoSelected.add(pieceId);
    this.#selectedPiece = pieceId;
  }

  /**
   * Moves one step along the zoom, and stops at either end.
   *
   * Stepping out of Fit starts from whatever Fit currently works out to, so
   * the first press moves a little rather than jumping to a step that might be
   * four times what is on screen.
   */
  #stepZoom(by: number): void {
    const from = this.#zoom ?? this.#fittedScale() ?? 1;
    const at =
      this.#zoom === null
        ? // The step below what fitting works out to, so pressing plus from
          // Fit moves up to the next one rather than past it.
          ZOOMS.reduce((best, step, index) => (step <= from ? index : best), 0)
        : ZOOMS.indexOf(this.#zoom);

    const next = ZOOMS[Math.min(ZOOMS.length - 1, Math.max(0, at + by))];
    if (next !== undefined && next !== this.#zoom) {
      this.#zoom = next;
      this.draw();
    }
  }

  /**
   * What fitting the canvas to the window works out to, when one is drawn.
   *
   * Measured rather than computed: the panel's width is the browser's answer
   * to a column of a grid, and repeating that arithmetic here would be a
   * second answer to go out of step with the first.
   */
  #fittedScale(): number | null {
    const stage = this.#ui.compositionStage.querySelector('.stage');
    const surface = this.#surfaceShown();
    if (!(stage instanceof HTMLElement) || surface === null || surface.width === 0) {
      return null;
    }
    const width = stage.getBoundingClientRect().width;
    return width === 0 ? null : width / surface.width;
  }

  /** The surface the canvas is showing, which the face decides. */
  #surfaceShown(): SizePx | null {
    const show = this.#deps.show();
    if (!show) {
      return null;
    }
    if (this.#face === 'input') {
      return show.composition.size;
    }
    const piece = this.#selectedPiece ? findPiece(show, this.#selectedPiece) : null;
    const output =
      (piece ? findOutput(show, piece.output_id) : null) ?? show.outputs[0] ?? null;
    return output?.mode.size ?? null;
  }

  /**
   * Writes what the canvas is at.
   *
   * After it is drawn, because Fit can only say what it came to once the
   * browser has laid it out -- and what it came to is the useful half: "Fit
   * 43%" says both that it is fitted and how far from real pixels that is.
   */
  #showZoom(): void {
    if (this.#zoom !== null) {
      this.#ui.zoomFit.textContent = `${Math.round(this.#zoom * 100)}%`;
      this.#ui.zoomFit.title = 'fit the canvas to the window';
      return;
    }
    const fitted = this.#fittedScale();
    if (fitted === null) {
      // Nothing laid out yet, which is the first draw after this screen is
      // opened. Asked again once the browser has done it, rather than left
      // saying less than it knows.
      this.#ui.zoomFit.textContent = 'Fit';
      requestAnimationFrame(() => {
        if (this.#zoom === null) {
          const later = this.#fittedScale();
          if (later !== null) {
            this.#ui.zoomFit.textContent = `Fit ${Math.round(later * 100)}%`;
          }
        }
      });
    } else {
      this.#ui.zoomFit.textContent = `Fit ${Math.round(fitted * 100)}%`;
    }
    this.#ui.zoomFit.title = '100% is one pixel of the output on one pixel of this screen';
  }

  /** Nothing selected, which is one of the four states this list has. */
  #clearSelection(): void {
    this.#selectedPiece = null;
    this.#selectedOutput = null;
    this.#alsoSelected.clear();
    // Numbers typed for one output must never be sent to another.
    this.#pendingMode = null;
  }

  /** Every piece being moved together, in the order the document lists them. */
  #selection(show: Show): Piece[] {
    return show.pieces.filter((piece) => this.#alsoSelected.has(piece.id));
  }

  /** Forgets what was selected, for a document that has just been opened. */
  reset(): void {
    this.#selectedPiece = null;
    this.#alsoSelected.clear();
    // Where Put back returns to: the mapping as this document arrived.
    const show = this.#deps.show();
    this.#before =
      show === null
        ? null
        : { outputs: structuredClone(show.outputs), pieces: structuredClone(show.pieces) };
    this.#pushNote = '';
  }

  /**
   * Puts the mapping in hand onto the wall, without saving the show.
   *
   * **Lining a wall up is the one job that cannot be done blind** (owner,
   * 2026-09-23). A piece is a rectangle of the composition put on a rectangle
   * of a wall, and whether it is right is something seen on the wall rather
   * than worked out from four numbers — so every edit here goes straight to
   * the server, which installs it under the running show. Nothing restarts.
   *
   * Only the outputs and the pieces travel. Whatever else is unsaved in this
   * document is not this panel's to store, which is why there is a route for
   * the mapping rather than a save.
   */
  async #pushMapping(): Promise<void> {
    const show = this.#deps.show();
    const showId = this.#deps.liveShowId();
    if (show === null || showId === null || show.id !== showId) {
      // Not the show the server is running. Editing another one is editing
      // a document, and there is no wall to put it on.
      return;
    }
    // Coalesced rather than dropped. A drag of four pieces commits four edits
    // in a row, and a push that was skipped for being second would leave the
    // wall showing three of them.
    if (this.#pushing) {
      this.#pushAgain = true;
      return;
    }

    this.#pushing = true;
    try {
      const answer = await this.#deps.setMapping(show.outputs, show.pieces);
      this.#pushNote = answer === null ? '' : answer;
    } finally {
      this.#pushing = false;
      if (this.#pushAgain) {
        this.#pushAgain = false;
        void this.#pushMapping();
      } else {
        this.draw();
      }
    }
  }

  /**
   * Puts back the mapping this session started from, on the wall as well.
   *
   * One edit through the same door every other gesture uses, so the restored
   * mapping travels the way a dragged one does. Nothing else here pushes on
   * its own: a second push would be the same request twice.
   */
  #putMappingBack(): void {
    const before = this.#before;
    const show = this.#deps.show();
    if (before === null || show === null) {
      return;
    }
    this.#selectedPiece = null;
    this.#alsoSelected.clear();
    this.#edit(
      restoreMapping(show, structuredClone(before.outputs), structuredClone(before.pieces)),
    );
  }

  /** Whether the mapping differs from the one this session started from. */
  #mappingMoved(show: Show | null): boolean {
    const before = this.#before;
    if (before === null || show === null) {
      return false;
    }
    return (
      JSON.stringify(before.pieces) !== JSON.stringify(show.pieces) ||
      JSON.stringify(before.outputs) !== JSON.stringify(show.outputs)
    );
  }

  /** What the live mapping is doing, beside the way back from it. */
  #drawLive(show: Show | null): void {
    const live = show !== null && show.id === this.#deps.liveShowId();
    const moved = this.#mappingMoved(show);
    this.#ui.revert.disabled = !moved;
    this.#ui.revert.title = moved
      ? 'Put the mapping back to what it was when this show was opened. The wall follows.'
      : 'The mapping is where it started.';
    this.#ui.live.textContent =
      this.#pushNote !== ''
        ? this.#pushNote
        : !live
          ? 'this show is not the one on the wall, so nothing here reaches it'
          : moved
            ? 'on the wall now'
            : 'changes go straight to the wall';
  }

  /** Draws the whole panel from the document in hand. */
  draw(): void {
    const show = this.#deps.show();
    this.#settleSelection(show);

    this.#ui.faceInput.classList.toggle('on', this.#face === 'input');
    this.#ui.faceOutput.classList.toggle('on', this.#face === 'output');
    this.#ui.faceTake.textContent =
      this.#face === 'output' ? 'Take from INPUT' : 'Take from OUTPUT';
    this.#ui.zoomOut.disabled = this.#zoom !== null && this.#zoom <= ZOOMS[0]!;
    this.#ui.zoomIn.disabled = this.#zoom !== null && this.#zoom >= ZOOMS[ZOOMS.length - 1]!;
    this.#ui.compositionStage.classList.toggle('zoomed', this.#zoom !== null);
    this.#ui.testPieces.classList.toggle('on', this.#testingPieces);
    this.#ui.testPieces.disabled = show === null || show.outputs.length === 0;
    this.#ui.testPieces.title = this.#testingPieces
      ? 'put the show back on the walls'
      : 'draw the pieces on the walls, on black, at their own pixels';
    this.#ui.faceTake.disabled = this.#alsoSelected.size === 0;
    this.#ui.faceTake.title =
      this.#alsoSelected.size === 0
        ? 'select a piece first'
        : this.#face === 'output'
          ? 'start these pieces off from the rectangles they take'
          : 'start these pieces off from the rectangles they land on';
    this.#drawBackdropControls();
    this.#drawLive(show);

    this.#drawComposition(show);
    this.#drawViewer(show);
    this.#showZoom();
    this.#drawControls(show);
    this.#drawOutputs(show);
    this.#drawPiece(show);

    // The wall follows the selection while the pattern is up, which is how
    // this gets used: press it, then walk the list and watch the wall.
    if (this.#testingPieces && this.#selectedPiece !== this.#highlightSent) {
      this.#highlightSent = this.#selectedPiece;
      this.#pushTestPattern();
    }
  }

  #settleSelection(show: Show | null): void {
    if (!show || (this.#selectedPiece !== null && !findPiece(show, this.#selectedPiece))) {
      this.#selectedPiece = null;
    }
    // A piece that has been deleted is not selected, however it was selected.
    for (const id of [...this.#alsoSelected]) {
      if (!show || !findPiece(show, id)) {
        this.#alsoSelected.delete(id);
      }
    }
  }

  // --- the composition -------------------------------------------------------

  /**
   * The canvas everything composites onto, and the rate it is rendered at.
   *
   * Changing either has consequences the operator should see rather than
   * discover, so the size is committed as one edit — width and height together —
   * and what it did to the pieces comes back from `setComposition` as a note.
   */
  #drawComposition(show: Show | null): void {
    if (!show) {
      this.#ui.compositionFields.replaceChildren(
        make('p', 'empty-note', 'No show open'),
      );
      this.#ui.compositionStage.replaceChildren();
      this.#ui.compositionCaption.textContent = '';
      return;
    }

    const { size } = show.composition;
    const width = this.#number(size.width, MIN_DIMENSION_PX);
    const height = this.#number(size.height, MIN_DIMENSION_PX);
    const commitSize = (): void => {
      const next: SizePx = { width: Number(width.value), height: Number(height.value) };
      if (next.width === size.width && next.height === size.height) {
        return;
      }
      this.#edit(setComposition(show, { size: next }));
    };
    width.addEventListener('change', commitSize);
    height.addEventListener('change', commitSize);

    // The model's rates and every wall's own; see `compositionRateChoices`.
    const { choices: rates, selected: current } = compositionRateChoices(show);
    const chooser = this.#select(rates, current, (value) => {
      const [numerator, denominator] = value.split('/').map(Number);
      this.#edit(
        setComposition(show, {
          render_rate: { numerator: numerator ?? 0, denominator: denominator ?? 0 },
        }),
      );
    });

    const size_row = make('div', 'row');
    size_row.append(
      this.#field('Width px', width, 'composition.size'),
      this.#field('Height px', height, 'composition.size'),
    );

    this.#ui.compositionFields.replaceChildren(
      size_row,
      this.#field('Render rate', chooser, 'composition.render_rate'),
    );

  }

  /**
   * The viewer in the middle: one surface, chosen by the face.
   *
   * Both faces are about one wall, chosen by [`stageOutput`]. INPUT is the
   * composition with that wall's source rectangles on it — which part of the
   * picture it takes. OUTPUT is the wall itself, with the same pieces in draw
   * order. One question asked twice: where they come from, and where they land.
   *
   * INPUT drew *every* piece in the show until 2026-09-25. On one wall that is
   * the whole picture; on two it is two outlines on top of each other, and
   * which belongs to which wall is not in the drawing.
   *
   * One at a time rather than a stage per output down a column (owner,
   * 2026-09-23). An operator is looking at one wall, and a page of stages is a
   * page nobody reads.
   */
  #drawViewer(show: Show | null): void {
    this.#drawAllPieces();

    if (!show) {
      this.#ui.compositionStage.replaceChildren();
      this.#ui.compositionCaption.textContent = '';
      this.#ui.stageTitle.textContent = 'Composition';
      return;
    }

    const { size, render_rate: rate } = show.composition;

    if (this.#face === 'input') {
      // The wall this is about, named in the title: a drawing that shows a
      // subset without saying which is one that looks like the whole. With the
      // switch on there is no subset, so there is nothing to name.
      const wall = this.#allPieces
        ? null
        : stageOutput(show, this.#selectedOutput, this.#selectedPiece);
      this.#ui.stageTitle.textContent = wall ? `Composition \u2014 ${wall.name}` : 'Composition';
      this.#ui.compositionCaption.textContent = `${size.width}×${size.height} · ${rateText(rate)} fps`;
      this.#ui.compositionStage.replaceChildren(
        this.#stage(
          size,
          show.pieces
            .filter((piece) => wall === null || piece.output_id === wall.id)
            .map((piece) => ({
              pieceId: piece.id,
              rect: piece.source_rect,
              label: piece.name,
              drawn: piece.enabled,
              // The input end turns too (owner, 2026-09-23): a wall hung on the
              // diagonal is fed a rectangle taken out of the composition at an
              // angle, and the outline has to show that or it is describing
              // something the renderer is not doing.
              rotationDeg: piece.source_rotation_deg ?? 0,
              anchor: piece.source_anchor ?? { x: 0.5, y: 0.5 },
            })),
          'source',
        ),
      );
      return;
    }

    // OUTPUT: the wall the operator is looking at.
    const output = stageOutput(show, this.#selectedOutput, this.#selectedPiece);
    if (output === undefined || output === null) {
      this.#ui.stageTitle.textContent = 'Output';
      this.#ui.compositionCaption.textContent = '';
      this.#ui.compositionStage.replaceChildren(
        make(
          'p',
          'empty-note',
          'No outputs',
        ),
      );
      return;
    }

    const sequence = sequenceOf(show, output.id);
    this.#ui.stageTitle.textContent = output.name;
    this.#ui.compositionCaption.textContent = `${output.mode.size.width}×${output.mode.size.height} · ${rateText(output.mode.refresh)} Hz`;
    this.#ui.compositionStage.replaceChildren(
      this.#stage(
        output.mode.size,
        // In draw order, so the piece an operator sees on top of the stack
        // here is the piece that is on top on the wall. A piece that is
        // switched off is drawn in its place and marked, rather than
        // vanishing: it still has a position, and that position is what an
        // operator is setting for when it comes back on.
        sequence.map((piece) => ({
          pieceId: piece.id,
          rect: piece.dest_rect,
          label: piece.name,
          drawn: piece.enabled,
          rotationDeg: piece.rotation_deg,
          anchor: piece.anchor ?? { x: 0.5, y: 0.5 },
        })),
        'dest',
      ),
    );
  }

  // --- outputs ---------------------------------------------------------------

  /**
   * The controls over the list, acting on whatever is selected.
   *
   * Four buttons once rather than four on every row. A row of seven pieces
   * carried twenty-eight targets for four actions, and the selection already
   * says which piece is being worked on -- it is the one the canvas is drawing
   * handles around and the one the panel on the right is about.
   *
   * An output has no draw order and nothing to copy it from, so those three
   * are off while one is selected rather than pretending otherwise.
   */
  #drawControls(show: Show | null): void {
    const piece = this.#selectedPiece;
    const output = this.#selectedOutput;
    const nothing = show === null;

    const target = this.#outputForAdding(show);

    // Enabled whenever there is a show. It used to need a reported socket, which
    // was right when a socket was the only thing an output could be: a stream
    // needs none, and a machine whose renderer has not said what it is wired to
    // can still publish one.
    const add = button('Add output', () => this.#askForOutput(), {
      disabled: nothing,
      title: 'a socket on this server, or a stream on the network',
    });

    const addPiece = button(
      'Add piece',
      () => {
        if (show && target) {
          this.#addPiece(show, target);
        }
      },
      { disabled: nothing || target === null },
    );

    const moves = make('div', 'inline');
    moves.append(
      button('\u25b2', () => this.#move('later'), {
        disabled: piece === null,
        title: 'one place towards the front',
      }),
      button('\u25bc', () => this.#move('earlier'), {
        disabled: piece === null,
        title: 'one place towards the back',
      }),
      button('\u29c9', () => this.#duplicate(), {
        disabled: piece === null,
        title: 'copy the selected piece',
      }),
      button('\u2715', () => this.#remove(), {
        disabled: piece === null && output === null,
        title: output === null ? 'remove the selected piece' : 'remove this output and its pieces',
      }),
    );

    // Named, so the audit can press them. A control that is drawn, styled
    // and wired to nothing looks exactly like one that works.
    add.id = 'map-add-output';
    addPiece.id = 'map-add-piece';
    const [up, down, copy, cut] = [...moves.children] as HTMLButtonElement[];
    if (up && down && copy && cut) {
      up.id = 'map-move-up';
      down.id = 'map-move-down';
      copy.id = 'map-duplicate';
      cut.id = 'map-remove';
    }

    const left = make('div', 'inline');
    left.append(add, addPiece);

    this.#ui.outputControls.replaceChildren(left, moves);
  }

  /**
   * Tells the server what the walls should be showing.
   *
   * Called on the toggle and again after the selection moves, so the wall
   * highlights what the operator is looking at. Sending the same thing twice
   * costs one request and is simpler than tracking what was last sent.
   */
  #pushTestPattern(): void {
    this.#deps.setTestPattern(
      this.#testingPieces ? 'PIECES' : 'OFF',
      this.#testingPieces ? this.#selectedPiece : null,
    );
  }

  /** Which output a new piece would go onto. */
  #outputForAdding(show: Show | null): string | null {
    if (!show || show.outputs.length === 0) {
      return null;
    }
    if (this.#selectedOutput !== null) {
      return this.#selectedOutput;
    }
    if (this.#selectedPiece !== null) {
      const piece = findPiece(show, this.#selectedPiece);
      if (piece) {
        return piece.output_id;
      }
    }
    return show.outputs[0]?.id ?? null;
  }

  #move(direction: 'earlier' | 'later'): void {
    const show = this.#deps.show();
    if (show && this.#selectedPiece) {
      this.#edit(movePiece(show, this.#selectedPiece, direction));
    }
  }

  #duplicate(): void {
    const show = this.#deps.show();
    if (show && this.#selectedPiece) {
      this.#edit(duplicatePiece(show, this.#selectedPiece));
    }
  }

  #remove(): void {
    const show = this.#deps.show();
    if (!show) {
      return;
    }
    if (this.#selectedPiece) {
      this.#edit(removePiece(show, this.#selectedPiece));
    } else if (this.#selectedOutput) {
      this.#edit(removeOutput(show, this.#selectedOutput));
      this.#selectedOutput = null;
    }
  }

  /**
   * Asks for a socket and a mode, for a new output.
   *
   * Only for making one. Changing what an output is -- its name, its socket,
   * its mode -- is the panel on the right, where the same three things are
   * edited for a piece (owner, 2026-09-25).
   *
   * Sockets already driving an output are not offered. The model has always
   * refused them, and offering one to refuse it afterwards is a worse way of
   * saying "that one is taken" than not offering it.
   */
  #askForOutput(): void {
    const show = this.#deps.show();
    if (!show) {
      return;
    }

    const taken = new Set(
      show.outputs.map((output) => connectorOf(output)).filter((key) => key !== null),
    );
    const connectors = this.#deps
      .connectors()
      .filter(
        (connector) =>
          connector.role !== 'SERVICE_DISPLAY' &&
          !taken.has(connectorKey(connector.gpu_pci_address, connector.connector)),
      );

    // A free socket when there is one, the network otherwise. The dialogue used
    // to refuse to open at all with every socket taken, which was right when a
    // socket was the only thing an output could be.
    let chosenKey =
      connectors.length > 0
        ? connectorKey(connectors[0]!.gpu_pci_address, connectors[0]!.connector)
        : NETWORK_OUTPUT;
    let chosenMode = 0;
    let stream = '';
    // Which of the published modes a stream would run. The composition's own
    // size when that is one of them, because a stream of the whole composition
    // is what most people want first, and 1920x1080p60 otherwise.
    const published = this.#deps.networkModes();
    let chosenStreamMode = Math.max(
      0,
      published.findIndex(
        (each) =>
          each.size.width === show.composition.size.width &&
          each.size.height === show.composition.size.height &&
          each.refresh.numerator === 60 &&
          each.refresh.denominator === 1,
      ),
    );

    const fill = (): void => {
      const sockets = this.#select(
        [
          { value: NETWORK_OUTPUT, label: 'NDI\u00AE \u2014 on the network' },
          ...connectors.map((connector) => {
            const key = connectorKey(connector.gpu_pci_address, connector.connector);
            return {
              value: key,
              label: `${key} \u2014 ${CONNECTOR_ROLE_LABELS[connector.role]}`,
            };
          }),
        ],
        chosenKey,
        (value) => {
          chosenKey = value;
          chosenMode = 0;
          fill();
        },
      );
      const fields: HTMLElement[] = [this.#field('Output', sockets)];

      if (chosenKey === NETWORK_OUTPUT) {
        // One name, which is the output's and the network's.
        //
        // On `input`, not `change` like every other text field here. Those
        // edit something; this one decides whether Add is usable, and an
        // operator typing a name and finding the button still greyed out would
        // have to click away to discover it had worked.
        const named = make('input');
        named.type = 'text';
        named.value = stream;
        named.autocomplete = 'off';
        named.addEventListener('input', () => {
          stream = named.value;
          this.#ui.addOutputConfirm.disabled = stream.trim() === '';
        });

        // Chosen, not typed: what a stream may run is what this server
        // publishes, and it is the same list the panel on the right offers.
        const modes = this.#select(
          published.map((each, position) => ({
            value: String(position),
            label: `${each.size.width}x${each.size.height}p${rateText(each.refresh)}`,
          })),
          String(chosenStreamMode),
          (value) => {
            chosenStreamMode = Number(value);
          },
        );
        modes.disabled = published.length === 0;

        fields.push(this.#field('Name', named), this.#field('Mode', modes));
        this.#ui.addOutputFields.replaceChildren(...fields);
        this.#ui.addOutputConfirm.disabled = stream.trim() === '' || published.length === 0;
        return;
      }

      const chosen = connectors.find(
        (connector) => connectorKey(connector.gpu_pci_address, connector.connector) === chosenKey,
      );
      const modes = chosen?.modes ?? [];
      if (chosenMode >= modes.length) {
        chosenMode = 0;
      }

      const modeChooser = this.#select(
        modes.map((mode, position) => ({ value: String(position), label: modeText(mode) })),
        String(chosenMode),
        (value) => {
          chosenMode = Number(value);
        },
      );
      modeChooser.disabled = modes.length === 0;

      fields.push(this.#field('Mode', modeChooser));
      this.#ui.addOutputFields.replaceChildren(...fields);
      this.#ui.addOutputConfirm.disabled = modes.length === 0;
    };

    fill();

    this.#ui.addOutputConfirm.onclick = (): void => {
      if (chosenKey === NETWORK_OUTPUT) {
        const wanted = published[chosenStreamMode];
        if (wanted !== undefined) {
          this.#edit(addNetworkOutput(show, stream, wanted));
        }
        this.#ui.addOutputDialog.close();
        return;
      }
      const chosen = connectors.find(
        (connector) => connectorKey(connector.gpu_pci_address, connector.connector) === chosenKey,
      );
      const mode = chosen?.modes?.[chosenMode];
      if (chosen && mode) {
        this.#edit(addOutput(show, chosen, mode));
      }
      this.#ui.addOutputDialog.close();
    };

    this.#ui.addOutputDialog.showModal();
  }

  /**
   * Every output, with the pieces that draw to it underneath.
   *
   * One flat list: an output is a heading row and its pieces are the rows
   * under it, in draw order. That relationship is the thing an operator holds
   * in their head -- this wall is made of these pieces -- and it is the whole
   * reason this is not a table of pieces with an output column.
   */
  #drawOutputs(show: Show | null): void {
    if (!show) {
      this.#ui.outputs.replaceChildren();
      return;
    }

    const rows: HTMLElement[] = [];
    for (const output of show.outputs) {
      rows.push(this.#outputRow(show, output));
      rows.push(...sequenceOf(show, output.id).map((piece) => this.#pieceRow(show, piece)));
    }
    this.#ui.outputs.replaceChildren(...rows);
  }

  /**
   * One output, as a heading: what it is called and what it is.
   *
   * The second line is the mode and the socket, which is what tells two
   * outputs apart at a glance, and it is a button: changing a wall's mode is
   * the same choice as choosing one when the output was made, so it opens the
   * same dialogue.
   */
  #outputRow(show: Show, output: OutputConfig): HTMLElement {
    const row = make('div', 'row-output');
    row.setAttribute('role', 'option');
    row.classList.toggle('selected', output.id === this.#selectedOutput);
    row.classList.toggle('off', !output.enabled);
    row.setAttribute('aria-selected', String(output.id === this.#selectedOutput));

    const base = outputPath(show, output.id) ?? '';
    const wrong = this.#deps.problems(base);
    row.classList.toggle('wrong', wrong.length > 0);
    if (wrong.length > 0) {
      row.title = wrong.join('; ');
    }

    // Kept, though the owner's drawing has no box on this row: without it an
    // output can no longer be taken out of the show without deleting it, and
    // switching a wall off for a soundcheck is a thing people do.
    const enabled = this.#toggle(output.enabled, (value) =>
      this.#edit(setOutputEnabled(show, output.id, value)),
    );
    enabled.title = 'whether this output is driven at all';

    const name = this.#text(output.name, (value) =>
      this.#edit(renameOutput(show, output.id, value)),
    );
    name.classList.add('row-name-field');
    name.addEventListener('focus', () => {
      this.#chooseOutput(output.id);
      this.draw();
    });

    const key = connectorOf(output);
    const says = `${output.mode.size.width}\u00d7${output.mode.size.height}px ${rateText(
      output.mode.refresh,
    )}Hz${key === null ? '' : ` @ ${key}`}`;
    const mode = button(
      says,
      () => {
        this.#chooseOutput(output.id);
        this.draw();
      },
      {
        className: 'row-says',
        title: 'what this output runs at; change it on the right',
      },
    );

    const head = make('div', 'row-output-head');
    head.append(enabled, make('span', 'row-icon', '\u25ad'), name);

    const body = make('div', 'row-output-body');
    body.append(head, mode);

    row.append(body);
    row.addEventListener('mousedown', (event) => {
      if (event.target === row || event.target === body) {
        this.#chooseOutput(output.id);
        this.draw();
      }
    });
    return row;
  }

  /**
   * Selects an output, or deselects it when it is already the selected one.
   *
   * Selecting it unselects whatever pieces were selected: the panel on the
   * right is about one thing.
   */
  #chooseOutput(outputId: string): void {
    if (this.#selectedOutput === outputId) {
      this.#clearSelection();
      return;
    }
    this.#selectedOutput = outputId;
    this.#selectedPiece = null;
    this.#alsoSelected.clear();
    this.#pendingMode = null;
  }

  #addPiece(show: Show, outputId: string): void {
    const before = show.pieces.map((piece) => piece.id);
    this.#edit(addPiece(show, outputId));
    const now = this.#deps.show();
    const added = now?.pieces.find((piece) => !before.includes(piece.id));
    if (added) {
      this.#selectedPiece = added.id;
      this.draw();
    }
  }

  // --- pieces ----------------------------------------------------------------

  /**
   * One piece, as one line: is it drawn, what is it, what is it called.
   *
   * No numbers and no buttons. The numbers are in the panel on the right,
   * which is about the selected piece and is where somebody changing them is
   * already looking; the buttons are over the list and act on the selection.
   */
  #pieceRow(show: Show, piece: Piece): HTMLElement {
    const row = make('div', 'row-piece');
    row.setAttribute('role', 'option');
    row.classList.toggle('selected', this.#alsoSelected.has(piece.id));
    row.classList.toggle('leading', piece.id === this.#selectedPiece);
    row.classList.toggle('off', !piece.enabled);
    row.setAttribute('aria-selected', String(piece.id === this.#selectedPiece));

    // A piece the server would refuse is marked, and says what is wrong when
    // asked. On the row rather than under it: a list of sentences under a list
    // of rows makes somebody match one to the other.
    const base = piecePath(show, piece.id) ?? '';
    const wrong = this.#deps.problems(base);
    row.classList.toggle('wrong', wrong.length > 0);
    if (wrong.length > 0) {
      row.title = wrong.join('; ');
    }

    const enabled = this.#toggle(piece.enabled, (value) =>
      this.#edit(setPiece(show, piece.id, { enabled: value })),
    );
    enabled.title = 'whether this piece is drawn';

    const name = make('button', 'row-name', piece.name);
    name.type = 'button';
    name.addEventListener('click', (event) => {
      // Ctrl or Shift adds, as on the canvas and as in every list anywhere.
      this.#choose(piece.id, event.ctrlKey || event.shiftKey || event.metaKey);
      this.draw();
    });

    row.append(enabled, make('span', 'row-icon', '\u25f0'), name);
    return row;
  }

  /**
   * What an output is: its name, its socket and its mode.
   *
   * The socket can be changed here rather than only when the output is made.
   * An output is forced onto a connector and stays there whatever is plugged
   * in, but which connector that is *is* an operator's decision -- a wall
   * moved to another card on a show day is an ordinary thing, and rebuilding
   * the output to say so would take its pieces with it.
   */
  #drawOutputFields(show: Show, output: OutputConfig): void {
    this.#ui.pieceTitle.textContent = 'Output';

    const name = this.#text(output.name, (value) =>
      this.#edit(renameOutput(show, output.id, value)),
    );

    const base = outputPath(show, output.id) ?? '';

    const key = connectorOf(output);
    const connectors = this.#deps.connectors();

    // Every socket this machine has that a wall may have, and the one this
    // output names even when the renderer is not reporting it: a wall
    // configured on a machine that is not running must not lose its socket by
    // being looked at.
    //
    // **Not the System screen's.** The server keeps that socket whatever a show
    // says, so offering it here was a control that took a choice and then
    // ignored it — which is worse than one that does not offer it. The dialogue
    // that adds an output has filtered it out since roles existed; this one,
    // which moves an existing output, never did.
    //
    // An output that already names it is still shown, for the same reason as
    // the unreported case below: a socket cannot be changed from a list it is
    // not in.
    const options = connectors
      .filter(
        (connector) =>
          connector.role !== 'SERVICE_DISPLAY' ||
          connectorKey(connector.gpu_pci_address, connector.connector) === key,
      )
      .map((connector) => {
        const each = connectorKey(connector.gpu_pci_address, connector.connector);
        return { value: each, label: `${each} \u2014 ${CONNECTOR_ROLE_LABELS[connector.role]}` };
      });
    if (key !== null && !options.some((option) => option.value === key)) {
      options.unshift({ value: key, label: `${key} \u2014 not reported by this server` });
    }

    // The network sits in the list with the sockets, so moving a wall onto it
    // and back is the same gesture as moving it between two cards.
    options.unshift({ value: NETWORK_OUTPUT, label: 'NDI\u00AE \u2014 on the network' });

    const chosenSocket = output.on_the_network === true ? NETWORK_OUTPUT : (key ?? '');
    const socket = this.#select(options, chosenSocket, (value) => {
      if (value === NETWORK_OUTPUT) {
        this.#edit(setOutputOnNetwork(show, output.id));
        return;
      }
      const [gpu, connector] = value.split('/');
      if (gpu !== undefined && connector !== undefined) {
        this.#edit(setOutputConnector(show, output.id, gpu, connector));
      }
    });

    const fields: HTMLElement[] = [
      this.#field('Name', name, `${base}.name`),
      this.#field('Output', socket, `${base}.identity`),
    ];
    fields.push(...this.#modeFields(show, output, base));

    this.#ui.pieceFields.replaceChildren(...fields);
  }

  /**
   * The mode, typed rather than only chosen.
   *
   * A display's mode list comes from its EDID, and one that presents none
   * leaves the driver with a handful of fallback timings. A wall that is
   * genuinely 1920x1080 is then unreachable through a chooser, which is the
   * whole reason these are fields. The chooser is still there when the display
   * did advertise something, because then it is the quick way.
   *
   * Whether the driver will build a mode it was not offered is the driver's
   * answer, and the renderer says which of the two happened in its log.
   */
  #modeFields(show: Show, output: OutputConfig, base: string): HTMLElement[] {
    const mode = output.mode;
    const network = output.on_the_network === true;
    const current = modeKey(mode.size, millihertzOf(mode.refresh) ?? 0);
    const says = (each: DisplayMode): string =>
      `${each.size.width}x${each.size.height}p${rateText(each.refresh)}`;

    // What can be chosen, from whichever list applies. A socket offers what its
    // panel advertised; a stream offers what this server publishes, which is
    // three broadcast rasters at eight rates and nothing hardware has a say in.
    const choosable: Array<{ key: string; label: string; mode: DisplayMode }> = network
      ? this.#deps.networkModes().map((each) => ({
          key: modeKey(each.size, millihertzOf(each.refresh) ?? 0),
          label: says(each),
          mode: each,
        }))
      : (connectorFor(output, this.#deps.connectors())?.modes ?? []).flatMap((each) => {
          const refresh = rateFromMillihertz(each.refresh_millihertz);
          return refresh === null
            ? []
            : [
                {
                  key: modeKey(each.size, each.refresh_millihertz),
                  label: modeText(each),
                  mode: { size: { ...each.size }, refresh },
                },
              ];
        });

    // The mode this output is already on, when nothing on offer is it. A show
    // configured on another machine, or one whose renderer has not reported
    // yet, must not lose its mode by being looked at.
    if (!choosable.some((each) => each.key === current)) {
      choosable.unshift({
        key: current,
        label: `${says(mode)} \u2014 not offered here`,
        mode,
      });
    }

    // What has been chosen and not set. Held on the panel rather than read off
    // the control, because a redraw between choosing and pressing Set replaces
    // it.
    const pending = this.#pendingMode?.outputId === output.id ? this.#pendingMode : null;
    const chooser = this.#select(
      choosable.map((each) => ({ value: each.key, label: each.label })),
      pending?.chosen ?? current,
      (value) => {
        const found = choosable.find((each) => each.key === value);
        if (found !== undefined) {
          this.#pendingMode = { outputId: output.id, chosen: value, mode: found.mode };
        }
      },
    );
    chooser.disabled = choosable.length < 2;
    // Named, so the audit can reach it. Every other control in this panel that
    // reaches the server has one.
    chooser.id = 'map-output-mode';

    // **The one control that reaches the cable.** Setting a mode takes a panel
    // dark while it resynchronises, so it happens when somebody asks for it and
    // not while they are still deciding what to ask for (owner, 2026-09-25).
    const set = button('Set', () => this.#setMode(show, output), {
      title: 'send this mode to the output',
    });
    set.id = 'map-set-mode';

    return [this.#field('Mode', chooser, `${base}.mode`), set];
  }

  /**
   * Sends the chosen mode to the output.
   *
   * Nothing happens when nothing was chosen since this output was last drawn,
   * or when what was chosen is what it is already running: a mode set again is
   * a panel dark for a second and a show marked changed, both for nothing.
   */
  #setMode(show: Show, output: OutputConfig): void {
    const pending = this.#pendingMode?.outputId === output.id ? this.#pendingMode : null;
    if (pending === null) {
      return;
    }
    const wanted = pending.mode;
    this.#pendingMode = null;

    if (
      wanted.size.width === output.mode.size.width &&
      wanted.size.height === output.mode.size.height &&
      millihertzOf(wanted.refresh) === millihertzOf(output.mode.refresh)
    ) {
      this.draw();
      return;
    }
    this.#edit(
      setOutputMode(show, output.id, {
        size: { ...wanted.size },
        refresh: { ...wanted.refresh },
      }),
    );
  }

  /**
   * The point a piece turns about, as the nine handles a corner offers.
   *
   * The nine come from the model (`AnchorPoint::HANDLES`) rather than from nine
   * numbers written into this screen. Both faces have one: the output anchor is
   * the point the placed rectangle swings about, and the input anchor is the
   * point the taken rectangle turns about in the composition (owner,
   * 2026-09-23).
   */
  #anchorChooser(show: Show, piece: Piece): HTMLElement {
    const input = this.#face === 'input';
    const anchor = (input ? piece.source_anchor : piece.anchor) ?? { x: 0.5, y: 0.5 };
    const chooser = make('select', 'piece-cell');
    for (const [label, point] of ANCHOR_HANDLES) {
      const option = make('option', undefined, label);
      option.value = `${point.x},${point.y}`;
      option.selected = near(anchor.x, point.x) && near(anchor.y, point.y);
      chooser.append(option);
    }
    if (![...chooser.options].some((option) => option.selected)) {
      // Somewhere between the handles, from a document written elsewhere.
      // Offered as itself rather than snapped to the nearest corner, which
      // would move a piece because somebody looked at it.
      const option = make('option', undefined, `${round2(anchor.x)}, ${round2(anchor.y)}`);
      option.value = `${anchor.x},${anchor.y}`;
      option.selected = true;
      chooser.append(option);
    }
    chooser.addEventListener('change', () => {
      const [x, y] = chooser.value.split(',').map(Number);
      const point = { x: x ?? 0.5, y: y ?? 0.5 };
      this.#edit(
        setPiece(show, piece.id, input ? { source_anchor: point } : { anchor: point }),
      );
    });
    return chooser;
  }

  /**
   * The selected piece, in numbers, for the face that is showing.
   *
   * INPUT is the rectangle of the composition this piece takes, how far that
   * rectangle is turned and about what point. OUTPUT is the same three about
   * where it lands. Both ends turn (owner, 2026-09-23).
   *
   * Every rectangle is committed as one edit — all four fields together —
   * because a rectangle is one thing and moving it a field at a time would walk
   * through positions the operator never asked for, each of which the editor
   * would have an opinion about.
   */
  #drawPiece(show: Show | null): void {
    const chosenOutput =
      show && this.#selectedOutput ? findOutput(show, this.#selectedOutput) : null;
    if (show && chosenOutput) {
      this.#ui.piecePanel.hidden = false;
      this.#ui.pieceNone.hidden = true;
      this.#drawOutputFields(show, chosenOutput);
      return;
    }

    const piece = show && this.#selectedPiece ? findPiece(show, this.#selectedPiece) : null;
    this.#ui.piecePanel.hidden = piece === null;
    this.#ui.pieceNone.hidden = piece !== null;
    if (!show || !piece) {
      return;
    }

    const input = this.#face === 'input';
    const base = piecePath(show, piece.id) ?? '';
    const output = findOutput(show, piece.output_id);
    // What the panel is about, not what the thing is called: the name is a
    // field two rows down, and a heading that repeated it said one thing
    // twice and told somebody where they were zero times.
    this.#ui.pieceTitle.textContent = 'Piece';

    const name = this.#text(piece.name, (value) =>
      this.#edit(renamePiece(show, piece.id, value)),
    );

    const outputChooser = this.#select(
      show.outputs.map((each) => ({ value: each.id, label: each.name })),
      piece.output_id,
      (value) => this.#edit(setPiece(show, piece.id, { output_id: value })),
    );

    const order = this.#number(piece.order, 0);
    order.addEventListener('change', () =>
      this.#edit(setPiece(show, piece.id, { order: Number(order.value) })),
    );

    const degrees = input ? piece.source_rotation_deg ?? 0 : piece.rotation_deg;
    const rotation = this.#number(degrees);
    rotation.step = 'any';
    rotation.addEventListener('change', () =>
      this.#edit(
        setPiece(
          show,
          piece.id,
          input
            ? { source_rotation_deg: Number(rotation.value) }
            : { rotation_deg: Number(rotation.value) },
        ),
      ),
    );

    // **Which picture this piece takes its rectangle out of** (owner,
    // 2026-09-23). The composition is the show as it leaves the server; a bus
    // is one of the three it is made of, and a wall fed from one is a monitor
    // of the look coming up rather than the one going out.
    const sourceChooser = make('select', 'piece-cell');
    sourceChooser.setAttribute('aria-label', 'What this piece takes its picture from');
    for (const entry of PIECE_SOURCES) {
      const option = make('option', undefined, entry.name);
      option.value = sourceKeyOf(entry.source);
      sourceChooser.append(option);
    }
    sourceChooser.value = sourceKeyOf(piece.source);
    sourceChooser.addEventListener('change', () =>
      this.#edit(setPiece(show, piece.id, { source: sourceFromKey(sourceChooser.value) })),
    );

    const fields: HTMLElement[] = [
      this.#field('Name', name, `${base}.name`),
      this.#field('Takes', sourceChooser, `${base}.source`),
      this.#field('Output', outputChooser, `${base}.output_id`),
      input
        ? this.#rectFields(
            'From the composition',
            piece.source_rect,
            piece.source_anchor ?? CENTRE_ANCHOR,
            `${base}.source_rect`,
            (rect) => this.#edit(setPieceSource(show, piece.id, rect)),
          )
        : this.#rectFields(
            `Onto ${output?.name ?? 'the output'}`,
            piece.dest_rect,
            piece.anchor ?? CENTRE_ANCHOR,
            `${base}.dest_rect`,
            (rect) => this.#edit(setPieceDest(show, piece.id, rect)),
          ),
    ];

    const turn = make('div', 'row');
    turn.append(
      this.#field(
        'Rotation °',
        rotation,
        input ? `${base}.source_rotation_deg` : `${base}.rotation_deg`,
      ),
      this.#field(
        'Anchor',
        this.#anchorChooser(show, piece),
        input ? `${base}.source_anchor` : `${base}.anchor`,
      ),
    );
    fields.push(turn);
    fields.push(this.#field('Draw order', order, `${base}.order`));

    this.#ui.pieceFields.replaceChildren(...fields);
  }

  /** Four numbers that are one rectangle, committed together. */
  /**
   * X, Y, W and H — all four about the piece's anchor.
   *
   * The owner, 2026-09-23: "Wenn ich den Anchor auf Center setze, ist mein xy
   * Nullpunkt immer noch oben Links und meine Breite und Höhe geht auch von
   * Oben Links aus als eigentlich von der Mitte aus."
   *
   * So X and Y are where the anchor is, not where the top-left corner is, and a
   * width typed here grows the piece about the anchor rather than off its left
   * edge. The document still holds a rectangle — that is what a renderer draws
   * and what a wall is built from — and this is the arithmetic between the two.
   */
  #rectFields(
    label: string,
    rect: RectPx,
    anchor: SnapPoint,
    path: string,
    onCommit: (rect: RectPx) => void,
  ): HTMLElement {
    const at = anchorOf(rect, anchor);
    const inputs: Record<keyof RectPx, HTMLInputElement> = {
      x: this.#number(Math.round(at.x)),
      y: this.#number(Math.round(at.y)),
      width: this.#number(rect.width, MIN_DIMENSION_PX),
      height: this.#number(rect.height, MIN_DIMENSION_PX),
    };
    const commit = (): void => {
      // Sized first and then placed, because the size decides where the anchor
      // sits inside the rectangle: doing it the other way round would put the
      // old anchor at the new position.
      const resized = sizedAbout(
        rect,
        anchor,
        Number(inputs.width.value),
        Number(inputs.height.value),
      );
      onCommit(
        placedAt(resized, anchor, {
          x: Number(inputs.x.value),
          y: Number(inputs.y.value),
        }),
      );
    };

    const row = make('div', 'row');
    for (const [key, caption] of [
      ['x', 'X px'],
      ['y', 'Y px'],
      ['width', 'W px'],
      ['height', 'H px'],
    ] as const) {
      inputs[key].addEventListener('change', commit);
      row.append(this.#field(caption, inputs[key]));
    }

    const wrap = make('div', 'field');
    wrap.dataset['path'] = path;
    const heading = make('label');
    heading.append(make('span', undefined, label));
    wrap.append(heading, row);
    for (const reason of this.#deps.problems(path)) {
      wrap.classList.add('wrong');
      wrap.append(make('span', 'why', reason));
    }
    return wrap;
  }

  // --- the surfaces ----------------------------------------------------------

  /**
   * One surface with rectangles on it, movable and resizable.
   *
   * The box gets its shape from the surface's own aspect ratio and every
   * rectangle is placed as a percentage of it, so the picture is honest at any
   * size and nothing has to be redrawn when the window changes.
   *
   * A gesture writes inline styles while it is happening and commits once, on
   * release, through the editing model — so what reaches the document is one
   * edit an operator can revert, and a rectangle the model refuses simply
   * springs back with the reason in the fault line.
   */
  #stage(surface: SizePx, rects: Drawn[], side: Side): HTMLElement {
    const stage = make('div', 'stage');
    stage.style.aspectRatio = `${surface.width} / ${surface.height}`;
    if (this.#zoom !== null) {
      // Output pixels times the scale, in screen pixels. At 1 the two are the
      // same number, which is the whole point of the scale being this one.
      stage.style.width = `${Math.round(surface.width * this.#zoom)}px`;
      stage.style.maxWidth = 'none';
      stage.style.margin = '0 0 10px';
    }

    // The picture behind it, if there is one for this face. Stretched to the
    // surface rather than fitted inside it: it is a photograph of the thing the
    // rectangles describe, so the two have to be in the same coordinates or
    // lining anything up against it would be a lie (owner, 2026-09-23).
    const backdrop = this.#backdrops.get(this.#face) ?? null;
    if (backdrop !== null) {
      const behind = make('img', 'stage-backdrop');
      behind.src = backdrop.source;
      behind.alt = '';
      behind.draggable = false;
      behind.style.opacity = String(backdrop.opacity);
      stage.append(behind);
    }
    for (const drawn of rects) {
      const box = make('div', 'stage-rect');
      box.classList.toggle('selected', this.#alsoSelected.has(drawn.pieceId));
      box.classList.toggle('leading', drawn.pieceId === this.#selectedPiece);
      box.classList.toggle('off', !drawn.drawn);
      place(box, drawn.rect, surface);
      if (drawn.rotationDeg !== 0) {
        // About the piece's own anchor. `rotate` turns about
        // `transform-origin`, which defaults to the middle of the box.
        box.style.transformOrigin = `${drawn.anchor.x * 100}% ${drawn.anchor.y * 100}%`;
        box.style.rotate = `${drawn.rotationDeg}deg`;
      }
      box.append(make('span', 'stage-label', drawn.label));
      box.title = `${drawn.label} · ${drawn.rect.width}×${drawn.rect.height} at ${drawn.rect.x}, ${drawn.rect.y}`;

      // Handles only on the one the panel is about. Eight on every member of a
      // group would be forty handles over four panels, and the gesture is the
      // group's anyway.
      if (drawn.pieceId === this.#selectedPiece) {
        for (const grip of GRIPS) {
          const handle = make('div', `grip grip-${grip}`);
          handle.addEventListener('pointerdown', (event) =>
            this.#gesture(event, box, stage, surface, drawn, side, grip),
          );
          box.append(handle);
        }
      }

      box.addEventListener('pointerdown', (event) => {
        if (event.target !== box && !(event.target as HTMLElement).classList.contains('stage-label')) {
          return;
        }
        // Ctrl or Shift adds to the selection; anything else replaces it. A
        // piece that is already in the selection keeps it, so a group can be
        // picked up by any of its members.
        const adding = event.ctrlKey || event.shiftKey || event.metaKey;
        if (adding || !this.#alsoSelected.has(drawn.pieceId)) {
          this.#choose(drawn.pieceId, adding);
        } else {
          this.#selectedPiece = drawn.pieceId;
        }
        this.#gesture(event, box, stage, surface, drawn, side, 'move');
      });

      stage.append(box);
    }

    if (rects.length === 0) {
      stage.append(make('span', 'stage-empty', 'Nothing mapped here yet.'));
    }
    return stage;
  }

  /**
   * One drag, from pointer down to pointer up.
   *
   * Bounded rather than free, and only where the model is: a source rectangle
   * is held inside the composition, because `Piece::validate` refuses one that
   * is not and a gesture that ends in a refusal every time is not a control. A
   * destination rectangle is left alone, because hanging off the edge of an
   * output is a legitimate mapping choice — what stops it is the model refusing
   * a rectangle that lies *entirely* outside, which the operator is told about
   * once, on release.
   */
  #gesture(
    event: PointerEvent,
    box: HTMLElement,
    stage: HTMLElement,
    surface: SizePx,
    drawn: Drawn,
    side: Side,
    grip: Grip,
  ): void {
    event.preventDefault();
    event.stopPropagation();

    const show = this.#deps.show();
    if (!show) {
      return;
    }

    const frame = stage.getBoundingClientRect();
    if (frame.width === 0 || frame.height === 0) {
      return;
    }
    const perPixelX = frame.width / surface.width;
    const perPixelY = frame.height / surface.height;
    const start = { x: event.clientX, y: event.clientY };

    // Everything that moves, and the rectangle each started from. A corner
    // scales them about the group's opposite corner, so four panels somebody
    // lined up are still lined up when they let go.
    const moving = this.#selection(show).map((piece) => ({
      id: piece.id,
      from: { ...(side === 'source' ? piece.source_rect : piece.dest_rect) },
    }));
    if (moving.length === 0) {
      moving.push({ id: drawn.pieceId, from: { ...drawn.rect } });
    }
    const group = boundsOf(moving.map((each) => each.from)) ?? { ...drawn.rect };
    // **The anchor is what a drag works about too** (owner, 2026-09-23). It was
    // the corner opposite the one being held, which is the familiar behaviour
    // and is only right when the anchor happens to be there: a piece anchored
    // at its centre and pulled by a corner grows both ways, which is what an
    // anchor at the centre means.
    const held = findPiece(show, drawn.pieceId);
    const grain =
      (side === 'source' ? held?.source_anchor : held?.anchor) ?? CENTRE_ANCHOR;
    const anchor = anchorOf(group, grain);
    const corner = grippedCorner(group, grip);

    let latest = new Map<string, RectPx>(moving.map((each) => [each.id, each.from]));

    // **What a dragged piece can snap to**: every other piece on this surface,
    // and the surface itself (owner, 2026-09-23). Gathered once, because none
    // of it moves while one piece is being dragged.
    //
    // Five *screen* pixels rather than five of the surface's own, so it feels
    // the same on a 1920 composition and a 7680 one: the number an operator
    // means by "close enough" is a number about their hand.
    const dragging = new Set(moving.map((each) => each.id));
    // On the INPUT face every piece shares the composition; on the OUTPUT face
    // only the ones on this wall are on the same surface at all.
    const on = held?.output_id ?? '';
    const fixed: SnapPoint[] = [
      ...surfacePoints(surface),
      ...show.pieces
        .filter((piece) => !dragging.has(piece.id) && (side === 'source' || piece.output_id === on))
        .flatMap((piece) =>
          side === 'source'
            ? snapPointsOf(piece.source_rect, piece.source_rotation_deg ?? 0, piece.source_anchor)
            : snapPointsOf(piece.dest_rect, piece.rotation_deg, piece.anchor),
        ),
    ];
    const within = SNAP_WITHIN_PX / Math.max(perPixelX, perPixelY, 0.0001);

    box.setPointerCapture(event.pointerId);
    box.classList.add('dragging');

    const move = (moved: PointerEvent): void => {
      const dx = Math.round((moved.clientX - start.x) / perPixelX);
      const dy = Math.round((moved.clientY - start.y) / perPixelY);

      const next = new Map<string, RectPx>();
      if (grip === 'move') {
        for (const each of moving) {
          next.set(each.id, nudged(each.from, dx, dy));
        }
      } else if (isCorner(grip)) {
        // **One scale for both axes**, so the shape survives (owner,
        // 2026-09-23). An LED wall's proportions are not the operator's to
        // change by a pixel while reaching for a size.
        const scale = proportionalScale(corner, anchor, dx, dy);
        for (const each of moving) {
          next.set(each.id, scaledAbout(each.from, anchor, scale));
        }
      } else {
        // An edge is one axis on purpose, and it is the one handle that can
        // change a shape — which is what an edge is for. About the anchor, so
        // an edge dragged on a centre-anchored piece moves both of them.
        for (const each of moving) {
          next.set(each.id, resizedAbout(each.from, grain, grip, dx, dy));
        }
      }

      // The last few pixels, taken by the piece rather than by the hand.
      // Alt turns it off, which is the one thing an operator needs when two
      // rectangles are meant to be a pixel apart.
      if (moved.altKey) {
        showSnap(stage, null, surface);
      } else {
        const turned = moving.map((each) => {
          const rect = next.get(each.id);
          const piece = show.pieces.find((candidate) => candidate.id === each.id);
          return rect === undefined
            ? []
            : side === 'source'
              ? snapPointsOf(rect, piece?.source_rotation_deg ?? 0, piece?.source_anchor)
              : snapPointsOf(rect, piece?.rotation_deg ?? 0, piece?.anchor);
        });
        const snap = snapGuides(turned.flat(), fixed, within);
        // Where it caught, drawn on the canvas so an operator can see that it
        // did (owner, 2026-09-23). A line for an edge, a dot for a corner.
        showSnap(stage, snap, surface);
        if (snap.dx !== 0 || snap.dy !== 0) {
          for (const [id, rect] of next) {
            // A move slides the whole rectangle; a grip that changes the shape
            // moves only the edges it is holding, which `resize` already knows
            // how to do from a delta.
            next.set(
              id,
              grip === 'move' ? nudged(rect, snap.dx, snap.dy) : resize(rect, grip, snap.dx, snap.dy),
            );
          }
        }
      }

      if (side === 'source') {
        for (const [id, rect] of next) {
          next.set(id, holdWithin(rect, surface));
        }
      }
      latest = next;

      // Only the one under the pointer follows live. The rest land on release,
      // which is one redraw rather than one per pointer event per piece.
      const own = latest.get(drawn.pieceId);
      if (own !== undefined) {
        place(box, own, surface);
        box.title = `${drawn.label} · ${own.width}×${own.height} at ${own.x}, ${own.y}`;
      }
    };

    const finish = (): void => {
      box.removeEventListener('pointermove', move);
      box.removeEventListener('pointerup', finish);
      box.removeEventListener('pointercancel', finish);
      box.classList.remove('dragging');
      showSnap(stage, null, surface);

      const moved = moving.some((each) => {
        const now = latest.get(each.id);
        return (
          now !== undefined &&
          (now.x !== each.from.x ||
            now.y !== each.from.y ||
            now.width !== each.from.width ||
            now.height !== each.from.height)
        );
      });
      if (!moved) {
        // A click rather than a drag: it selected the piece, which the redraw
        // shows, and the document is untouched.
        this.draw();
        return;
      }

      this.#commitRects(side, latest);
    };

    box.addEventListener('pointermove', move);
    box.addEventListener('pointerup', finish);
    box.addEventListener('pointercancel', finish);
  }

  /**
   * Stores a rectangle for each piece that moved, as one edit each.
   *
   * Each one is applied to the document the last one produced, because an edit
   * answers with a new show: applying them all to the document as it was
   * would keep only the last.
   */
  #commitRects(side: Side, rects: ReadonlyMap<string, RectPx>): void {
    for (const [id, rect] of rects) {
      const current = this.#deps.show();
      if (!current) {
        return;
      }
      const piece = findPiece(current, id);
      if (piece === null) {
        continue;
      }
      const was = side === 'source' ? piece.source_rect : piece.dest_rect;
      if (was.x === rect.x && was.y === rect.y && was.width === rect.width && was.height === rect.height) {
        continue;
      }
      this.#edit(
        side === 'source'
          ? setPieceSource(current, id, rect)
          : setPieceDest(current, id, rect),
      );
    }
  }

  /**
   * The arrow keys, on whatever is selected.
   *
   * An LED wall is patched to exact pixel boundaries, and the last few of those
   * are found by eye against a backdrop rather than typed (owner, 2026-09-23).
   * A pixel at a time is what that needs; ten with Shift is what crossing a
   * panel needs.
   *
   * Alt resizes instead of moving, from the top-left corner, so the position
   * an operator has just typed does not move while they adjust the size.
   *
   * Ignored while a text field has the keyboard: an arrow key in a number box
   * belongs to the number box.
   */
  #onArrow(event: KeyboardEvent): void {
    const steps: Record<string, [number, number]> = {
      ArrowLeft: [-1, 0],
      ArrowRight: [1, 0],
      ArrowUp: [0, -1],
      ArrowDown: [0, 1],
    };
    const step = steps[event.key];
    if (step === undefined) {
      return;
    }

    const focused = document.activeElement;
    if (
      focused instanceof HTMLInputElement ||
      focused instanceof HTMLTextAreaElement ||
      focused instanceof HTMLSelectElement
    ) {
      return;
    }

    const show = this.#deps.show();
    if (show === null || this.#alsoSelected.size === 0) {
      return;
    }

    event.preventDefault();
    const by = event.shiftKey ? 10 : 1;
    const dx = step[0] * by;
    const dy = step[1] * by;
    const side: Side = this.#face === 'input' ? 'source' : 'dest';
    const surface = this.#surfaceFor(show, side);

    const moved = new Map<string, RectPx>();
    for (const piece of this.#selection(show)) {
      const from = side === 'source' ? piece.source_rect : piece.dest_rect;
      // Alt resizes, about the anchor like everything else that changes a
      // size: a centred piece opens both ways and a top-left one opens right
      // and down (owner, 2026-09-23). It grew from the top-left corner
      // whatever the anchor said.
      const grain =
        (side === 'source' ? piece.source_anchor : piece.anchor) ?? CENTRE_ANCHOR;
      const next = event.altKey
        ? sizedAbout(from, grain, from.width + dx, from.height + dy)
        : nudged(from, dx, dy);
      moved.set(piece.id, side === 'source' && surface !== null ? holdWithin(next, surface) : next);
    }
    this.#commitRects(side, moved);
  }

  /**
   * What the rectangles of one face are measured against.
   *
   * The composition for INPUT; for OUTPUT, the wall the selected piece lands
   * on — and null when the show no longer has it, which is a piece that
   * reaches nothing and has nothing to be held inside.
   */
  #surfaceFor(show: Show, side: Side): SizePx | null {
    if (side === 'source') {
      return show.composition.size;
    }
    const piece = this.#selectedPiece === null ? null : findPiece(show, this.#selectedPiece);
    const output = piece === null ? null : findOutput(show, piece.output_id);
    return output?.mode.size ?? null;
  }

  // --- what the server will take, and the wall will still get wrong ----------

  // --- controls --------------------------------------------------------------
  //
  // The same shapes the rest of the editor uses, so a field here and a field on
  // a layer are the same control.

  #field(label: string, control: HTMLElement, path?: string): HTMLElement {
    const wrap = make('div', 'field');
    const labelNode = make('label');
    labelNode.append(make('span', undefined, label));

    if (control.id === '') {
      this.#controlSeq += 1;
      control.id = `mapping-control-${this.#controlSeq}`;
    }
    labelNode.htmlFor = control.id;
    wrap.append(labelNode, control);

    if (path !== undefined) {
      wrap.dataset['path'] = path;
      for (const reason of this.#deps.problems(path)) {
        wrap.classList.add('wrong');
        wrap.append(make('span', 'why', reason));
      }
    }
    return wrap;
  }

  #number(value: number, min?: number): HTMLInputElement {
    const input = make('input');
    input.type = 'number';
    input.step = '1';
    if (min !== undefined) {
      input.min = String(min);
    }
    input.value = String(value);
    return input;
  }

  #text(value: string, onCommit: (value: string) => void): HTMLInputElement {
    const input = make('input');
    input.type = 'text';
    input.value = value;
    input.autocomplete = 'off';
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
}

// --- geometry for the page ----------------------------------------------------

/** Places a rectangle on its surface, as a share of it. */
/**
 * Draws where a snap caught, or takes the drawing away.
 *
 * The owner, 2026-09-23: "Markiere bitte den Punkt oder die Linie wo ein Snap
 * greift… eine Magenta Linie für Linie an Linie oder Punkt für Ecke an Ecke."
 *
 * Magenta because nothing else on this canvas is: orange is selection, violet
 * is waiting for a grid, red is refused. A colour that means one thing means
 * it at a glance.
 *
 * A line per axis that caught, and a dot instead when both caught on the same
 * point — which is corner to corner, and is a different thing from two edges
 * that happen to cross.
 */
function showSnap(stage: HTMLElement, snap: SnapGuides | null, surface: SizePx): void {
  const existing = stage.querySelector('.snap-marks');
  if (snap === null || (snap.atX === null && snap.atY === null)) {
    existing?.remove();
    return;
  }

  const marks = existing instanceof HTMLElement ? existing : make('div', 'snap-marks');
  const drawn: HTMLElement[] = [];

  if (snap.corner !== null) {
    const dot = make('span', 'snap-corner');
    dot.style.left = `${(snap.corner.x / surface.width) * 100}%`;
    dot.style.top = `${(snap.corner.y / surface.height) * 100}%`;
    drawn.push(dot);
  } else {
    if (snap.atX !== null) {
      const line = make('span', 'snap-line vertical');
      line.style.left = `${(snap.atX / surface.width) * 100}%`;
      drawn.push(line);
    }
    if (snap.atY !== null) {
      const line = make('span', 'snap-line horizontal');
      line.style.top = `${(snap.atY / surface.height) * 100}%`;
      drawn.push(line);
    }
  }

  marks.replaceChildren(...drawn);
  if (existing === null) {
    stage.append(marks);
  }
}

function place(box: HTMLElement, rect: RectPx, surface: SizePx): void {
  box.style.left = `${(rect.x / surface.width) * 100}%`;
  box.style.top = `${(rect.y / surface.height) * 100}%`;
  box.style.width = `${(rect.width / surface.width) * 100}%`;
  box.style.height = `${(rect.height / surface.height) * 100}%`;
}

/** Applies one drag to a rectangle, keeping it at least one pixel each way. */
function resize(from: RectPx, grip: Grip, dx: number, dy: number): RectPx {
  if (grip === 'move') {
    return { ...from, x: from.x + dx, y: from.y + dy };
  }

  let { x, y, width, height } = from;
  if (grip.includes('w')) {
    const shift = Math.min(dx, width - MIN_DIMENSION_PX);
    x += shift;
    width -= shift;
  }
  if (grip.includes('e')) {
    width = Math.max(MIN_DIMENSION_PX, width + dx);
  }
  if (grip.includes('n')) {
    const shift = Math.min(dy, height - MIN_DIMENSION_PX);
    y += shift;
    height -= shift;
  }
  if (grip.includes('s')) {
    height = Math.max(MIN_DIMENSION_PX, height + dy);
  }
  return { x, y, width, height };
}

/**
 * Keeps a rectangle inside a surface while it is being dragged.
 *
 * Only for the composition side, where the model requires it. It shrinks rather
 * than refuses, because the alternative during a gesture is a rectangle that
 * springs back the moment it touches an edge.
 */
function holdWithin(rect: RectPx, surface: SizePx): RectPx {
  const width = Math.min(rect.width, surface.width);
  const height = Math.min(rect.height, surface.height);
  return {
    width,
    height,
    x: Math.max(0, Math.min(rect.x, surface.width - width)),
    y: Math.max(0, Math.min(rect.y, surface.height - height)),
  };
}

/** How one reported mode is named in a chooser's value. */
function modeKey(size: SizePx, millihertz: number): string {
  return `${size.width}x${size.height}@${millihertz}`;
}

/**
 * The nine points a corner, edge or centre handle stands for.
 *
 * The same nine `AnchorPoint::HANDLES` publishes, in the same order, so the
 * chooser offers what the model has rather than nine numbers written into a
 * screen.
 */
const ANCHOR_HANDLES: readonly (readonly [string, { x: number; y: number }])[] = [
  ['Top left', { x: 0, y: 0 }],
  ['Top', { x: 0.5, y: 0 }],
  ['Top right', { x: 1, y: 0 }],
  ['Left', { x: 0, y: 0.5 }],
  ['Centre', { x: 0.5, y: 0.5 }],
  ['Right', { x: 1, y: 0.5 }],
  ['Bottom left', { x: 0, y: 1 }],
  ['Bottom', { x: 0.5, y: 1 }],
  ['Bottom right', { x: 1, y: 1 }],
];

/** Whether two fractions are the same point, at the precision one is typed to. */
function near(one: number, other: number): boolean {
  return Math.abs(one - other) < 0.0005;
}

/** A fraction, as an operator would write it. */
function round2(value: number): number {
  return Math.round(value * 100) / 100;
}

/** Whether a grip is a corner, which scales, rather than an edge, which stretches. */
function isCorner(grip: Grip): boolean {
  return grip.length === 2;
}

/**
 * The corner a grip holds, in surface pixels.
 *
 * `move` has no corner of its own; it is given the top-left so that the
 * arithmetic has something finite to work with, and never reaches the scale
 * that would use it.
 */
function grippedCorner(bounds: RectPx, grip: Grip): { x: number; y: number } {
  return {
    x: grip.includes('e') ? bounds.x + bounds.width : bounds.x,
    y: grip.includes('s') ? bounds.y + bounds.height : bounds.y,
  };
}

/**
 * The rates a composition may be set to, and which of them it is set to now.
 *
 * The four the model names first, exact and rational so 60000/1001 does not
 * drift. **Then the rate of every wall in this show that is none of them.** A
 * wall's rate is not a number this client made up: it is the mode its
 * connector reported, which is the panel's own, and a composition that matches
 * it loses no frame to the difference. Without it the location server's
 * NovaStar MCTRL300, which reports 59.998 Hz, could only be fed 60.000 or
 * 59.94 (owner, 2026-10-18: "59,998 kann ich nicht auswählen").
 *
 * And the composition's own rate last, when it is none of those: a document
 * written elsewhere is shown as what it is and kept, never replaced.
 *
 * `selected` is the key of the choice that *is* the current rate, compared as
 * a value rather than as text, so 59998/1000 and 29999/500 are one choice.
 */
export function compositionRateChoices(show: Show): {
  choices: Array<{ value: string; label: string }>;
  selected: string;
} {
  const key = (rate: Show['composition']['render_rate']): string =>
    `${rate.numerator}/${rate.denominator}`;
  const same = (
    one: Show['composition']['render_rate'],
    other: Show['composition']['render_rate'],
  ): boolean => one.numerator * other.denominator === other.numerator * one.denominator;

  const choices = FRAME_RATES.map((named) => ({
    value: key(named.rate),
    label: `${named.label} fps`,
    rate: named.rate,
  }));

  for (const output of show.outputs) {
    const refresh = output.mode.refresh;
    if (refresh.numerator === 0 || refresh.denominator === 0) {
      continue;
    }
    if (choices.some((choice) => same(choice.rate, refresh))) {
      continue;
    }
    choices.push({
      value: key(refresh),
      label: `${rateText(refresh)} fps — ${output.name}`,
      rate: refresh,
    });
  }

  const current = show.composition.render_rate;
  let selected = choices.find((choice) => same(choice.rate, current))?.value;
  if (selected === undefined) {
    selected = key(current);
    choices.push({ value: selected, label: `${rateText(current)} fps (${selected})`, rate: current });
  }

  return { choices: choices.map(({ value, label }) => ({ value, label })), selected };
}

/**
 * Which wall the OUTPUT stage draws.
 *
 * **The selected wall first.** Selecting one clears the selected piece — the
 * panel on the right is about one thing — so reading the piece alone meant that
 * clicking a wall highlighted its row, filled the panel with its name and its
 * mode, and left the stage showing the first wall in the list (owner,
 * 2026-09-25). The two only ever disagree in the one case anybody would notice.
 *
 * Then the wall the selected piece is on, which is how an operator dragging a
 * piece gets the stage that piece lives on without choosing it separately.
 *
 * Then the first wall there is, so a show that has just been opened shows
 * something rather than an empty frame. `null` only when there are no walls at
 * all, which is what "No outputs" is for.
 */
export function stageOutput(
  show: Show,
  selectedOutput: string | null,
  selectedPiece: string | null,
): Show['outputs'][number] | null {
  const chosen = selectedOutput ? findOutput(show, selectedOutput) : null;
  if (chosen) {
    return chosen;
  }
  const piece = selectedPiece ? findPiece(show, selectedPiece) : null;
  const carrying = piece ? findOutput(show, piece.output_id) : null;
  return carrying ?? show.outputs[0] ?? null;
}
