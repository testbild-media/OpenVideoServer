// The media page: what the server holds, and what is on its way there.
//
// Two halves of one screen, because they are two halves of one job. The library
// is a database view of what the server has — names, pictures, states
// and what the server measured, never a path and never a storage key. The import
// queue is files on their way into it.
//
// Nothing here decides anything. What a file is, whether it can be declared and
// where an interrupted upload continues are all in `shared/ingest.ts`; what an
// item's facts read as is in `shared/media.ts`. This file draws them and sends
// what the operator did back across the bridge.
//
// One deliberate absence, said out loud in the panel rather than passed over: a
// media item's size on disk and the profile it was prepared as are not shown,
// because `/media` does not send them. Both live on the asset, which is
// server-internal. Drawing a size the server never reported would be this
// client inventing a number an operator might act on.

import type { MediaItem } from '@ovs/schema';

import type { OvsBridge } from '../preload.js';
import {
  bytesLabel,
  fractionOf,
  stageLine,
  type HandDeclaration,
  type QueueItem,
} from '../shared/ingest.js';
import { MEDIA_KINDS, MEDIA_KIND_LABELS } from '../shared/limits.js';
import { browse, whereLabel, type Crumb, type FolderTile } from '../shared/browse.js';
import {
  ROOT_FOLDER,
  countsByFolder,
  folderNameProblem,
  folderTree,
  isInside,
  joinFolder,
  moveFolder,
  nameOf,
  parentOf,
  type FolderNode,
} from '../shared/folders.js';

const NEWLINE = String.fromCharCode(10);
import {
  facts,
  newestFirst,
  picture,
  refusedDeletion,
  stateLabel,
  usageLine,
} from '../shared/media.js';
import { button, element, make } from './dom.js';

/** What the media page needs from the rest of the window. */
export interface MediaDeps {
  ovs: OvsBridge;
  /** Says something to the operator in the fault line at the foot of the window. */
  note: (message: string) => void;
  /**
   * Says that the server's library is not what it was.
   *
   * Every other screen keeps its own copy and read it when it signed in, so
   * this is how they find out. Called after a change this screen made and the
   * server accepted — never after one it refused, because nothing changed.
   */
  changed: () => void;
}

/** The media library and the import queue. */
export class MediaPanel {
  readonly #ovs: OvsBridge;
  readonly #note: (message: string) => void;
  readonly #changed: () => void;

  #library: MediaItem[] = [];
  #queue: QueueItem[] = [];
  /** A picture per item: absent means nobody has asked, null means there is none. */
  readonly #thumbnails = new Map<string, string | null>();
  /** What the server said uses each item, for the ones that have been asked about. */
  readonly #usage = new Map<string, string[]>();
  #selected: string | null = null;
  /** The item a delete has been offered for once and is waiting to be confirmed. */
  #confirming: string | null = null;
  #connected = false;
  /** Every folder the server holds, in path order. */
  #folders: string[] = [];
  /** The folder whose contents are shown. Empty is the root. */
  #folder = ROOT_FOLDER;
  /** What is typed into the search, which reaches every folder. */
  #search = '';
  /** Which folders are open in the tree, by path. */
  readonly #open = new Set<string>();
  /** The item being dragged onto a folder, while a drag is in progress. */
  #dragging: string | null = null;
  /** The folder being dragged into another one, while a drag is in progress. */
  #draggingFolder: string | null = null;
  /**
   * A folder being named, either a new one or one being renamed.
   *
   * In the tree rather than in a dialogue, because **Electron has no
   * `window.prompt`** — Chromium removed it and Electron did not put it
   * back — and because a box typed into where the folder will appear says
   * more about where it is going than a modal over the whole screen does.
   */
  #naming: { of: 'new'; parent: string } | { of: 'rename'; path: string } | null = null;
  #libraryNote = 'Sign in to a server to see its library.';

  readonly #ui = {
    drop: element('ingest-drop'),
    into: element('ingest-into'),
    pick: element<HTMLButtonElement>('ingest-pick'),
    clear: element<HTMLButtonElement>('ingest-clear'),
    stopAll: element<HTMLButtonElement>('ingest-stop'),
    queue: element('ingest-list'),
    refresh: element<HTMLButtonElement>('library-refresh'),
    newFolder: element<HTMLButtonElement>('library-new-folder'),
    search: element<HTMLInputElement>('library-search'),
    folders: element('library-folders'),
    path: element('library-path'),
    note: element('library-note'),
    grid: element('library-grid'),
    detail: element('media-detail'),
    detailTitle: element('media-title'),
    detailFields: element('media-fields'),
  };

  constructor(deps: MediaDeps) {
    this.#ovs = deps.ovs;
    this.#note = deps.note;
    this.#changed = deps.changed;

    this.#ui.pick.addEventListener('click', () => void this.#pick());
    this.#ui.refresh.addEventListener('click', () => void this.load());
    this.#ui.search.addEventListener('input', () => {
      this.#search = this.#ui.search.value;
      this.#drawLibrary();
    });
    this.#ui.newFolder.addEventListener('click', () => this.#newFolder());
    this.#ui.clear.addEventListener('click', () => void this.#ovs.clearIngest());
    // **Everything still on its way, in one press** (owner, 2026-10-20: "ich
    // kann die Vorgänge nichtmals abbrechen"). Remove stops the file being
    // prepared, and the next one in the queue starts at once — so with a dozen
    // files that all fail the same way, stopping them was a dozen presses, each
    // racing the next encode.
    this.#ui.stopAll.addEventListener('click', () => {
      for (const item of this.#queue) {
        if (item.stage.at !== 'ready') {
          void this.#ovs.removeIngest(item.id);
        }
      }
    });

    // Drag and drop. The page cannot read a file and would not know what to do
    // with one if it could; what it takes from a drop is where each file is on
    // this machine, and the main process does the rest.
    for (const event of ['dragenter', 'dragover'] as const) {
      this.#ui.drop.addEventListener(event, (dragged: DragEvent) => {
        dragged.preventDefault();
        this.#ui.drop.classList.add('over');
      });
    }
    for (const event of ['dragleave', 'dragend'] as const) {
      this.#ui.drop.addEventListener(event, () => this.#ui.drop.classList.remove('over'));
    }
    this.#ui.drop.addEventListener('drop', (dropped: DragEvent) => {
      dropped.preventDefault();
      this.#ui.drop.classList.remove('over');
      void this.#dropped(dropped);
    });

    // A file dropped anywhere else in the window is a file the operating system
    // would otherwise open in this window, which a page that is an application
    // must not do.
    for (const event of ['dragover', 'drop'] as const) {
      document.addEventListener(event, (anywhere: DragEvent) => {
        if (!this.#ui.drop.contains(anywhere.target as Node | null)) {
          anywhere.preventDefault();
        }
      });
    }
  }

  /** Whether the client is signed in, which is what the library needs. */
  setConnected(connected: boolean): void {
    const was = this.#connected;
    this.#connected = connected;
    if (connected && !was) {
      void this.load();
    }
    if (!connected) {
      // Let go of it rather than leave it on screen. The next sign-in may be to
      // a different server, and a library that still lists the last one's items
      // is a screen an operator would act on.
      this.#library = [];
      this.#thumbnails.clear();
      this.#usage.clear();
      this.#selected = null;
      this.#libraryNote = 'Sign in to a server to see its library.';
      this.draw();
    }
  }

  /** Asks the server what it holds. */
  async load(): Promise<void> {
    if (!this.#connected) {
      return;
    }
    this.#libraryNote = 'Reading the library…';
    this.draw();

    const answer = await this.#ovs.mediaLibrary();
    if (!answer.ok) {
      this.#libraryNote = answer.message;
      this.draw();
      return;
    }

    this.#library = newestFirst(answer.value.items);
    this.#folders = answer.value.folders;
    this.#libraryNote =
      this.#library.length === 0
        ? 'This server holds no media yet. Drop some files above.'
        : '';
    this.draw();
    void this.#fetchThumbnails();
  }

  /** The import queue moved. */
  setQueue(items: QueueItem[]): void {
    const finished = new Set(
      this.#queue
        .filter((item) => item.stage.at === 'ready')
        .map((item) => item.id),
    );
    this.#queue = items;
    // **The queue and nothing else.** This used to redraw the whole page, and
    // progress arrives many times a second during an upload — so the box a new
    // folder is named in was rebuilt under somebody's fingers, lost its focus,
    // and closed (owner, 2026-10-20: "immer sofort das Feld für die Eingabe
    // weg, als würde irgendwer daneben drücken"). Nothing else on the page
    // depends on where an upload has got to.
    this.#drawQueue();

    // A file that has just become part of the library is a reason to ask what
    // the library holds. Asked once per file rather than on every progress
    // report, which arrive by the hundred during an upload.
    const arrived = items.some((item) => item.stage.at === 'ready' && !finished.has(item.id));
    if (arrived) {
      void this.load();
      // And the other screens, for the same reason a deletion tells them: a
      // clip that has just arrived is one an operator wants to put in a slot,
      // and a list read at sign-in does not have it.
      this.#changed();
    }
  }

  draw(): void {
    this.#drawQueue();
    this.#drawInto();
    this.#drawFolders();
    this.#drawLibrary();
    this.#drawDetail();
  }

  /**
   * Where a dropped file will be filed.
   *
   * Said before the drop rather than discovered after it. An import goes into
   * the folder the operator has open (owner, 2026-09-23), and the folder they
   * have open is a click away up the tree, so the panel that takes the files
   * is the panel that has to name it.
   */
  #drawInto(): void {
    this.#ui.into.textContent =
      this.#folder === ROOT_FOLDER
        ? 'Filed in All media.'
        : `Filed in ${this.#folder}.`;
  }

  // --- the library's own structure -------------------------------------------

  /**
   * The folder tree down the left of the library.
   *
   * Everything is one root called "All media", which is where every item that
   * nothing has filed already is. A folder holds a count of what is under it,
   * including its own descendants, because the number beside a closed folder
   * answers "is there anything down there" and a zero beside a folder with
   * fifty clips two levels under it would be a lie.
   */
  #drawFolders(): void {
    // Not while a folder is being named in it. A library that reloads because
    // an upload finished must not take the box away from somebody typing: the
    // tree is drawn again the moment the box closes, which is when what it
    // shows can be read anyway.
    if (this.#naming !== null && this.#ui.folders.contains(document.activeElement)) {
      return;
    }
    const counts = countsByFolder(this.#library);
    const rows: HTMLElement[] = [
      this.#folderRow({ path: ROOT_FOLDER, name: 'All media', depth: -1, children: [] }, this.#library.length),
    ];

    if (this.#naming?.of === 'new' && this.#naming.parent === ROOT_FOLDER) {
      rows.push(this.#nameBox(0));
    }

    const walk = (nodes: readonly FolderNode[]) => {
      for (const node of nodes) {
        if (this.#naming?.of === 'rename' && this.#naming.path === node.path) {
          rows.push(this.#nameBox(node.depth, node.name));
        } else {
          rows.push(this.#folderRow(node, counts.get(node.path) ?? 0));
        }
        if (this.#naming?.of === 'new' && this.#naming.parent === node.path) {
          rows.push(this.#nameBox(node.depth + 1));
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

    this.#ui.folders.replaceChildren(...rows);
  }

  #folderRow(node: FolderNode, count: number): HTMLElement {
    const row = make('div', 'folder-row');
    row.style.setProperty('--depth', String(Math.max(0, node.depth)));
    row.classList.toggle('selected', node.path === this.#folder);

    // The root's children are always drawn, so it has nothing to twist open.
    // A control that does nothing is worse than no control.
    const hasChildren =
      node.path !== ROOT_FOLDER &&
      this.#folders.some((path) => parentOf(path) === node.path);
    const twist = button(
      hasChildren ? (this.#openAt(node.path) ? '\u25be' : '\u25b8') : '',
      () => {
        if (this.#open.has(node.path)) {
          this.#open.delete(node.path);
        } else {
          this.#open.add(node.path);
        }
        this.#drawFolders();
      },
      { className: 'folder-twist', disabled: !hasChildren },
    );

    const open = button(
      node.path === ROOT_FOLDER ? 'All media' : node.name,
      () => this.#openFolder(node.path),
      { className: 'folder-name', title: node.path === ROOT_FOLDER ? 'Everything nothing has filed' : node.path },
    );

    row.append(twist, open);
    if (count > 0) {
      row.append(make('span', 'folder-count', String(count)));
    }

    if (node.path !== ROOT_FOLDER) {
      row.append(
        button('Rename', () => this.#renameFolder(node.path), {
          className: 'folder-act',
          title: `Rename ${node.path}`,
        }),
        button('Delete', () => void this.#deleteFolder(node.path), {
          className: 'folder-act',
          title: `Delete ${node.path}, which has to be empty first`,
        }),
      );
    }

    // A clip dropped on a folder is filed there, and a folder dropped on one
    // goes inside it. Nothing on disk moves either way.
    this.#fileOnDrop(row, node.path);
    if (node.path !== ROOT_FOLDER) {
      this.#carriesFolder(row, node.path);
    }

    return row;
  }

  /**
   * Makes a folder's row or tile something that can be picked up and dropped
   * on another folder (owner, 2026-10-20: "ich kann keine Ordner in Ordner
   * verschieben via Drag and Drop wie bei Medien").
   */
  #carriesFolder(node: HTMLElement, path: string): void {
    node.draggable = true;
    node.addEventListener('dragstart', (event) => {
      this.#draggingFolder = path;
      this.#dragging = null;
      if (event.dataTransfer !== null) {
        event.dataTransfer.effectAllowed = 'move';
        event.dataTransfer.setData('text/plain', nameOf(path));
      }
    });
    node.addEventListener('dragend', () => {
      this.#draggingFolder = null;
      this.#clearDropTargets();
    });
  }

  #clearDropTargets(): void {
    for (const node of document.querySelectorAll('.drop-target')) {
      node.classList.remove('drop-target');
    }
  }

  /**
   * Moves a folder, and everything filed in and under it, into another one.
   *
   * A rename to a path under the new parent, which is what the server's rename
   * already is: it rewrites every path that begins with the old one.
   */
  async #moveFolderInto(path: string, into: string): Promise<void> {
    const move = moveFolder(path, into, this.#folders);
    if (!move.ok) {
      this.#note(move.because);
      return;
    }
    const answer = await this.#ovs.renameFolder(path, move.to);
    if (!answer.ok) {
      this.#note(answer.message);
      return;
    }
    if (isInside(this.#folder, path)) {
      this.#folder = move.to + this.#folder.slice(path.length);
    }
    this.#open.add(into);
    await this.load();
    this.#changed();
  }

  /** Whether a folder is drawn open, which the selected one's ancestors are. */
  #openAt(path: string): boolean {
    return this.#open.has(path) || isInside(this.#folder, path);
  }

  /**
   * The box a folder is named in, drawn where the folder will be.
   *
   * Enter accepts, Escape and losing focus abandon. No buttons: this is one
   * field, and an operator who has typed a name has already decided.
   */
  #nameBox(depth: number, value = ''): HTMLElement {
    const row = make('div', 'folder-row naming');
    row.style.setProperty('--depth', String(Math.max(0, depth)));

    const box = make('input', 'folder-input');
    box.type = 'text';
    box.value = value;
    box.placeholder = 'Folder name';
    box.addEventListener('keydown', (event) => {
      if (event.key === 'Enter') {
        event.preventDefault();
        void this.#acceptName(box.value);
      }
      if (event.key === 'Escape') {
        event.preventDefault();
        this.#naming = null;
        this.#drawFolders();
      }
    });
    // Abandoned rather than kept: a box left open over a tree that has since
    // been redrawn would be a name going somewhere the operator cannot see.
    box.addEventListener('blur', () => {
      if (this.#naming !== null) {
        this.#naming = null;
        this.#drawFolders();
      }
    });

    row.append(box);
    // After it is in the document, or the focus goes nowhere.
    queueMicrotask(() => {
      box.focus();
      box.select();
    });
    return row;
  }

  /** What Enter in the name box does, for whichever thing is being named. */
  async #acceptName(typed: string): Promise<void> {
    const naming = this.#naming;
    if (naming === null) {
      return;
    }
    const name = typed.trim();
    const problem = folderNameProblem(name);
    if (problem !== null) {
      this.#note(problem);
      return;
    }
    // Cleared before the request, so the blur that follows does not fire into
    // a second redraw while the first is still in flight.
    this.#naming = null;

    if (naming.of === 'new') {
      const path = joinFolder(naming.parent, name);
      const answer = await this.#ovs.createFolder(path);
      if (!answer.ok) {
        this.#note(answer.message);
        return;
      }
      this.#open.add(naming.parent);
      this.#folder = path;
      await this.load();
      return;
    }

    if (name === nameOf(naming.path)) {
      this.#drawFolders();
      return;
    }
    const to = joinFolder(parentOf(naming.path), name);
    const answer = await this.#ovs.renameFolder(naming.path, to);
    if (!answer.ok) {
      this.#note(answer.message);
      return;
    }
    // Everything filed under the old path followed it, and so does the view.
    if (isInside(this.#folder, naming.path)) {
      this.#folder = to + this.#folder.slice(naming.path.length);
    }
    await this.load();
  }

  /** Opens a box for a new folder inside the one that is open. */
  #newFolder(): void {
    this.#naming = { of: 'new', parent: this.#folder };
    this.#open.add(this.#folder);
    this.#drawFolders();
  }

  /** Opens a box over the folder's own name. */
  #renameFolder(path: string): void {
    this.#naming = { of: 'rename', path };
    this.#drawFolders();
  }

  async #deleteFolder(path: string): Promise<void> {
    // No "are you sure": the server refuses a folder that still holds anything,
    // so the only folder this can remove is an empty one, and an empty folder
    // is nothing to lose. What it cannot do is take somebody's clips with it.
    const answer = await this.#ovs.deleteFolder(path);
    if (!answer.ok) {
      this.#note(answer.message);
      return;
    }
    if (isInside(this.#folder, path)) {
      this.#folder = parentOf(path);
    }
    await this.load();
  }

  /** Files one item somewhere, or renames it. Neither touches the file. */
  async #fileItem(
    mediaItemId: string,
    folder?: string,
    displayName?: string,
  ): Promise<void> {
    const change: { displayName?: string; folder?: string } = {};
    if (folder !== undefined) {
      change.folder = folder;
    }
    if (displayName !== undefined) {
      change.displayName = displayName;
    }
    const answer = await this.#ovs.fileMedia(mediaItemId, change);
    if (!answer.ok) {
      this.#note(answer.message);
      return;
    }
    await this.load();
    // The other screens list the library by name, and a rename is a new name.
    this.#changed();
  }

  /**
   * Which folder the item is filed in. A fact, not a control.
   *
   * It was a list to pick from, and that made two ways of doing one thing
   * (owner, 2026-09-23). A clip is moved by dragging it onto a folder, which
   * is the gesture that works from the grid where the clips are; a dropdown on
   * a panel that only opens once something is selected is a second path to the
   * same edit and a worse one. So this says where the clip is and how to move
   * it, and nothing here can move it.
   */
  #folderField(item: MediaItem): string {
    const at = item.folder ?? ROOT_FOLDER;
    return at === ROOT_FOLDER ? 'All media' : at;
  }

  /**
   * The name an item is shown under, as a field that can be typed in.
   *
   * The library's name, not the file's. The filename it came in as is beside
   * it and never changes: it is what an operator orients by when two clips
   * have ended up with the same label.
   */
  #nameField(item: MediaItem): HTMLElement {
    const row = make('div', 'media-rename');
    const box = make('input', 'media-rename-box');
    box.type = 'text';
    box.value = item.display_name;
    box.addEventListener('keydown', (event) => {
      if (event.key === 'Enter') {
        event.preventDefault();
        box.blur();
      }
      if (event.key === 'Escape') {
        event.preventDefault();
        box.value = item.display_name;
        box.blur();
      }
    });
    box.addEventListener('blur', () => {
      const name = box.value.trim();
      if (name === '') {
        this.#note('A name cannot be empty.');
        box.value = item.display_name;
      } else if (name !== item.display_name) {
        // Filing reloads the library, which draws the panel again.
        void this.#fileItem(item.id, undefined, name);
        return;
      }
      // Whatever arrived while this was being typed in — `#drawDetail` waits
      // for nobody to be typing — now that nobody is.
      queueMicrotask(() => this.#drawDetail());
    });
    row.append(box);
    return row;
  }

  // --- the import queue ------------------------------------------------------

  #drawQueue(): void {
    this.#ui.clear.disabled = !this.#queue.some(
      (item) => item.stage.at === 'ready' || item.stage.at === 'stopped',
    );
    this.#ui.stopAll.disabled = !this.#queue.some((item) => item.stage.at !== 'ready');

    if (this.#queue.length === 0) {
      this.#ui.queue.replaceChildren();
      return;
    }
    this.#ui.queue.replaceChildren(...this.#queue.map((item) => this.#queueRow(item)));
  }

  #queueRow(item: QueueItem): HTMLElement {
    const row = make('div', `ingest-row ${item.stage.at}`);

    const head = make('div', 'ingest-head');
    head.append(make('span', 'ingest-name', item.name));
    if (item.byteSize !== null) {
      // This client's own measurement of a file on this machine, which is a
      // different thing from a number the server reported about a library item.
      head.append(make('span', 'ingest-size', bytesLabel(item.byteSize)));
    }
    head.append(make('span', 'ingest-stage', stageLine(item)));
    row.append(head);

    const fraction = fractionOf(item.stage);
    if (fraction !== null) {
      const track = make('div', 'ingest-bar');
      const fill = make('div', 'ingest-bar-fill');
      fill.style.width = `${Math.round(fraction * 100)}%`;
      track.append(fill);
      row.append(track);
    }

    const actions = make('div', 'ingest-actions');
    if (item.stage.at === 'stopped' && item.stage.resumable) {
      // Resume rather than restart: the server reported how much of this file it
      // already holds, and the endpoints take an offset.
      const from = item.upload === null ? null : bytesLabel(item.upload.receivedBytes);
      actions.append(
        button(
          from === null ? 'Resume' : `Resume from ${from}`,
          () => void this.#ovs.resumeIngest(item.id),
          { className: 'small' },
        ),
      );
    }
    actions.append(
      button('Remove', () => void this.#ovs.removeIngest(item.id), { className: 'small quiet' }),
    );
    row.append(actions);

    if (item.stage.at === 'undeclared') {
      row.append(this.#handForm(item, item.stage.reason));
    }

    return row;
  }

  /**
   * The form for a machine with no prober.
   *
   * Everything the upload endpoint requires and nothing more, with no field
   * filled in. A default here would be this client guessing, and the server
   * opens the file and refuses an upload whose size disagrees — so a guess is
   * not a small inaccuracy, it is a failed import after the bytes have crossed.
   *
   * It used to ask which internal profile the file was, which meant asking an
   * operator to recognise a codec. The server plays what it is given,
   * so the only question left is the one anybody can answer about their own
   * file: is it moving or is it a picture.
   */
  #handForm(item: QueueItem, why: string): HTMLElement {
    const form = make('div', 'ingest-hand');
    form.append(make('p', 'hint', why));

    const kind = document.createElement('select');
    for (const value of MEDIA_KINDS) {
      const option = document.createElement('option');
      option.value = value;
      option.textContent = MEDIA_KIND_LABELS[value];
      kind.append(option);
    }
    form.append(labelled('What it holds', kind));

    const width = numberField();
    const height = numberField();
    form.append(labelled('Width in pixels', width), labelled('Height in pixels', height));

    const seconds = numberField();
    seconds.step = '0.001';
    const durationRow = labelled('Length in seconds', seconds);
    form.append(durationRow);

    const alpha = document.createElement('input');
    alpha.type = 'checkbox';
    const alphaRow = labelled('Carries an alpha channel', alpha);
    form.append(alphaRow);

    /** A still has no duration; both kinds may carry alpha. */
    const sync = (): void => {
      durationRow.hidden = kind.value === 'image';
    };
    kind.addEventListener('change', sync);
    sync();

    form.append(
      button(
        'Declare and upload',
        () => {
          const chosen = kind.value as (typeof MEDIA_KINDS)[number];
          const length = Number(seconds.value);
          const hand: HandDeclaration = {
            kind: chosen,
            width: Number(width.value),
            height: Number(height.value),
            durationMs:
              chosen === 'image' || !Number.isFinite(length) || length <= 0
                ? null
                : Math.round(length * 1000),
            // Not asked for: the server takes the rate from the file when it can
            // read one, and a rate typed from memory is a number nobody measured.
            frameRate: null,
            hasAlpha: alpha.checked,
          };
          void this.#ovs.declareIngest(item.id, hand);
        },
        { className: 'small' },
      ),
    );

    return form;
  }

  async #pick(): Promise<void> {
    const chosen = await this.#ovs.pickMedia();
    if (chosen.length > 0) {
      await this.#ovs.ingest(chosen, this.#folder);
    }
  }

  async #dropped(event: DragEvent): Promise<void> {
    const dropped = [...(event.dataTransfer?.files ?? [])];
    const files = dropped
      .map((file) => ({ path: this.#ovs.pathForFile(file), name: file.name }))
      // A drop can carry something with no path at all — a selection out of
      // another application, or a directory on some platforms. Saying so beats
      // queueing a row that fails to open with no explanation.
      .filter((file) => file.path !== '');

    if (files.length === 0) {
      this.#note('Nothing in that drop was a file on this machine.');
      return;
    }
    await this.#ovs.ingest(files, this.#folder);
  }

  // --- the library -----------------------------------------------------------

  /**
   * Opens a folder, the way the tree, a folder tile and the path all do.
   *
   * The search goes with it: somebody who picked a folder out of the results
   * asked to see what is in it, which is a folder's contents and not the
   * results again.
   */
  #openFolder(path: string): void {
    this.#folder = path;
    this.#open.add(path);
    this.#search = '';
    this.#ui.search.value = '';
    this.draw();
  }

  #drawLibrary(): void {
    // Only what is filed in the open folder, not what is under it: a folder
    // that showed its descendants' contents would put the same clip in
    // several places, and renaming it in one of them would rename it in all
    // of them without anybody having been told. What is under it is there as
    // folders, which is the other half of the owner's ask of 2026-10-20.
    const view = browse(this.#library, (item) => item, this.#folders, this.#folder, this.#search);
    if (view.at !== this.#folder) {
      // Renamed or deleted under it. The nearest folder that is still there.
      this.#folder = view.at;
      this.#drawFolders();
      this.#drawInto();
    }

    this.#drawPath(view.crumbs, view.searching);

    const empty = view.folders.length === 0 && view.items.length === 0;
    this.#ui.note.textContent =
      this.#libraryNote !== ''
        ? this.#libraryNote
        : view.searching && empty
          ? `Nothing in any folder matches “${this.#search.trim()}”.`
          : empty && this.#folder !== ROOT_FOLDER
            ? 'Nothing is filed here yet. Drag a clip onto this folder to put it in.'
            : '';
    this.#ui.note.hidden = this.#ui.note.textContent === '';

    const tiles: HTMLElement[] = [];
    if (!view.searching && view.up !== null) {
      tiles.push(this.#upTile(view.up));
    }
    for (const folder of view.folders) {
      tiles.push(this.#folderTile(folder, view.searching));
    }
    for (const item of view.items) {
      tiles.push(this.#card(item, view.searching ? whereLabel(item.folder ?? ROOT_FOLDER) : null));
    }
    this.#ui.grid.replaceChildren(...tiles);
  }

  /**
   * Where the grid is, as a path somebody can go back up.
   *
   * During a search it says so, because the grid is then every folder's and
   * not the one the path names.
   */
  #drawPath(crumbs: readonly Crumb[], searching: boolean): void {
    const parts: HTMLElement[] = [];
    crumbs.forEach((crumb, index) => {
      if (index > 0) {
        parts.push(make('span', 'browse-path-gap', '›'));
      }
      const last = index === crumbs.length - 1 && !searching;
      const step = last
        ? make('span', 'browse-path-here', crumb.name)
        : button(crumb.name, () => this.#openFolder(crumb.path), {
            className: 'browse-path-step',
            title: `Open ${crumb.name}`,
          });
      // Each step is somewhere to drop as well: a folder dragged onto
      // "All media" comes out to the top.
      this.#fileOnDrop(step, crumb.path);
      parts.push(step);
    });
    if (searching) {
      parts.push(
        make('span', 'browse-path-gap', '›'),
        make('span', 'browse-path-here', `Searching every folder for “${this.#search.trim()}”`),
      );
    }
    this.#ui.path.replaceChildren(...parts);
  }

  /** One folder in the grid: opened by a press, filed into by a drop. */
  #folderTile(folder: FolderTile, searching: boolean): HTMLElement {
    const tile = button('', () => this.#openFolder(folder.path), {
      className: 'folder-tile',
      title: `Open ${whereLabel(folder.path)}`,
    });
    const glyph = make('div', 'folder-glyph-frame');
    glyph.append(make('span', 'folder-glyph'));
    tile.append(glyph, make('span', 'folder-tile-name', folder.name));
    tile.append(
      make(
        'span',
        'folder-tile-line',
        searching
          ? `in ${folder.where}`
          : folder.count === 0
            ? 'empty'
            : `${folder.count} item${folder.count === 1 ? '' : 's'}`,
      ),
    );
    this.#fileOnDrop(tile, folder.path);
    this.#carriesFolder(tile, folder.path);
    return tile;
  }

  /** The way back up, as the first tile in the grid. */
  #upTile(parent: string): HTMLElement {
    const tile = button('', () => this.#openFolder(parent), {
      className: 'folder-tile up',
      title: `Back up to ${whereLabel(parent)}`,
    });
    const glyph = make('div', 'folder-glyph-frame');
    glyph.append(make('span', 'folder-glyph up'));
    tile.append(glyph, make('span', 'folder-tile-name', '..'), make('span', 'folder-tile-line', whereLabel(parent)));
    this.#fileOnDrop(tile, parent);
    return tile;
  }

  /**
   * What lands on `target` goes into `folder`: a clip is filed there, and a
   * folder is moved inside it. Nothing on disk moves either way.
   *
   * A folder is only offered a drop it can take — not itself, not one of its
   * own folders, not the folder it is already in — so the highlight is a
   * promise rather than an invitation to be refused.
   */
  #fileOnDrop(target: HTMLElement, folder: string): void {
    const takes = (): boolean =>
      this.#dragging !== null ||
      (this.#draggingFolder !== null && moveFolder(this.#draggingFolder, folder, this.#folders).ok);
    target.addEventListener('dragover', (event) => {
      if (!takes()) {
        return;
      }
      event.preventDefault();
      if (event.dataTransfer !== null) {
        event.dataTransfer.dropEffect = 'move';
      }
      target.classList.add('drop-target');
    });
    target.addEventListener('dragleave', () => target.classList.remove('drop-target'));
    target.addEventListener('drop', (event) => {
      event.preventDefault();
      target.classList.remove('drop-target');
      const item = this.#dragging;
      const moving = this.#draggingFolder;
      this.#dragging = null;
      this.#draggingFolder = null;
      if (item !== null) {
        void this.#fileItem(item, folder);
      } else if (moving !== null) {
        void this.#moveFolderInto(moving, folder);
      }
    });
  }

  #card(item: MediaItem, where: string | null = null): HTMLElement {
    const card = make('button', `media-card ${item.state}`);
    card.type = 'button';
    // Dragged onto a folder to file it there. Nothing on disk moves.
    card.draggable = true;
    card.addEventListener('dragstart', (event) => {
      this.#dragging = item.id;
      this.#draggingFolder = null;
      if (event.dataTransfer !== null) {
        event.dataTransfer.effectAllowed = 'move';
        event.dataTransfer.setData('text/plain', item.display_name);
      }
    });
    card.addEventListener('dragend', () => {
      this.#dragging = null;
      this.#clearDropTargets();
    });
    card.classList.toggle('selected', item.id === this.#selected);

    const frame = make('div', 'media-picture');
    const drawn = picture(item, this.#thumbnails.get(item.id) ?? null);
    if (drawn.kind === 'picture') {
      const image = document.createElement('img');
      image.src = drawn.src;
      image.alt = drawn.alt;
      frame.append(image);
    } else {
      // No picture, so the name stands in for one. That is what every entry in
      // this library was before thumbnails existed, and it is not a fault.
      frame.append(make('span', 'media-picture-name', drawn.text));
    }
    card.append(frame);

    card.append(make('span', 'media-name', item.display_name));
    card.append(make('span', `media-state ${item.state}`, stateLabel(item.state)));

    // Whether it moves, beside its size and length: a library where a clip and
    // a still often share a name has to say which one this is without being
    // opened (owner, 2026-10-20).
    const line = facts(item)
      .filter(
        (fact) => fact.label === 'Type' || fact.label === 'Dimensions' || fact.label === 'Duration',
      )
      .map((fact) => fact.value)
      .join(' · ');
    if (line !== '') {
      card.append(make('span', 'media-line', line));
    }
    // During a search, which folder it is in: two of the same name in two
    // folders are otherwise the same card twice.
    if (where !== null) {
      card.append(make('span', 'media-where', where));
    }
    card.title = `${item.display_name}${NEWLINE}${item.original_filename}${NEWLINE}${whereLabel(item.folder ?? ROOT_FOLDER)}`;

    card.addEventListener('click', () => {
      this.#selected = item.id;
      this.#confirming = null;
      this.draw();
      void this.#fetchUsage(item.id);
    });
    return card;
  }

  /**
   * Fetches the pictures, one item at a time.
   *
   * One at a time because this runs against a machine that may be rendering a
   * show, and a library of two hundred asked for all at once is two hundred
   * reads competing with the one thing on that machine that must not be late.
   */
  async #fetchThumbnails(): Promise<void> {
    for (const item of this.#library) {
      if (this.#thumbnails.has(item.id)) {
        continue;
      }
      const png = await this.#ovs.mediaThumbnail(item.id);
      // Content-addressed, so a picture never changes and is asked for once.
      this.#thumbnails.set(item.id, png);
      this.#drawLibrary();
    }
  }

  async #fetchUsage(mediaItemId: string): Promise<void> {
    const answer = await this.#ovs.mediaUsage(mediaItemId);
    if (!answer.ok) {
      this.#note(answer.message);
      return;
    }
    this.#usage.set(mediaItemId, answer.value);
    this.#drawDetail();
  }

  // --- one item --------------------------------------------------------------

  #drawDetail(): void {
    // Not while the name is being typed into. Rebuilding the panel took the
    // box away mid-word, and the box files what it holds when it loses focus —
    // so a library reloading because an upload finished could rename a clip
    // to half of what somebody was typing. The box draws the panel again when
    // it closes.
    if (this.#ui.detailFields.contains(document.activeElement)) {
      return;
    }
    const item = this.#library.find((each) => each.id === this.#selected) ?? null;
    this.#ui.detail.hidden = item === null;
    if (item === null) {
      return;
    }

    this.#ui.detailTitle.textContent = item.display_name;

    const fields = make('div', 'media-facts');
    // The name first, because it is the one fact on this panel that can be
    // changed. Renaming is a label and never touches the file (owner,
    // 2026-09-23), which is why the filename it came in as sits under it and
    // does not follow.
    fields.append(labelled('Name', this.#nameField(item)));
    fields.append(
      field(
        'Folder',
        this.#folderField(item),
        'Drag the clip onto a folder to file it somewhere else. The file itself never moves.',
      ),
    );
    fields.append(field('Imported from', item.original_filename));
    fields.append(field('State', stateLabel(item.state)));
    for (const fact of facts(item)) {
      fields.append(field(fact.label, fact.value, fact.title));
    }

    const parts: HTMLElement[] = [fields];

    if (item.failure_reason) {
      parts.push(make('p', 'media-failure', item.failure_reason));
    }

    // What the server does not send, said once rather than left as a gap
    // somebody spends an afternoon looking for.

    const used = this.#usage.get(item.id);
    if (used === undefined) {
      parts.push(make('p', 'media-usage', 'Asking which shows use this…'));
    } else {
      parts.push(make('p', 'media-usage', usageLine(used)));
    }

    parts.push(this.#deleteControl(item, used));
    this.#ui.detailFields.replaceChildren(...parts);
  }

  /**
   * Delete, with the shows that would break named first.
   *
   * The server refuses while a show references the item (`media_in_use`), so
   * the button is not offered in that case: an operator pressing a button that
   * cannot work is being asked to read a refusal they could have been shown
   * instead. Two presses when it can work, because deleting media is not
   * something to do by brushing past a control (house rule §9).
   */
  #deleteControl(item: MediaItem, used: string[] | undefined): HTMLElement {
    const row = make('div', 'media-actions');

    if (used === undefined) {
      row.append(button('Delete', () => undefined, { className: 'small', disabled: true }));
      return row;
    }

    if (used.length > 0) {
      row.append(
        button('Delete', () => undefined, {
          className: 'small',
          disabled: true,
          title: usageLine(used),
        }),
      );
      row.append(
        make('span', 'media-blocked', 'This cannot be deleted while a show uses it.'),
      );
      return row;
    }

    const confirming = this.#confirming === item.id;
    row.append(
      button(
        confirming ? 'Delete — press again' : 'Delete',
        () => {
          if (!confirming) {
            this.#confirming = item.id;
            this.#drawDetail();
            return;
          }
          void this.#delete(item);
        },
        { className: confirming ? 'small danger' : 'small' },
      ),
    );
    if (confirming) {
      row.append(
        button(
          'Keep it',
          () => {
            this.#confirming = null;
            this.#drawDetail();
          },
          { className: 'small quiet' },
        ),
      );
    }
    return row;
  }

  async #delete(item: MediaItem): Promise<void> {
    this.#confirming = null;
    const answer = await this.#ovs.deleteMedia(item.id);
    if (answer.ok) {
      this.#usage.delete(item.id);
      this.#thumbnails.delete(item.id);
      this.#selected = null;
      await this.load();
      // Every other screen holds its own copy of this list and read it when it
      // signed in. A deletion nobody told them about leaves Live Mode's
      // BROWSER offering media the server no longer has.
      this.#changed();
      return;
    }

    // Refused. The server's sentence lists the shows inside prose; the usage
    // endpoint gives the same answer as a list, so the operator is told which
    // shows they would have broken rather than being handed a paragraph.
    const usage = await this.#ovs.mediaUsage(item.id);
    const used = usage.ok ? usage.value : [];
    if (usage.ok) {
      this.#usage.set(item.id, used);
    }
    this.#note(refusedDeletion({ code: answer.code, message: answer.message }, used));
    this.#drawDetail();
  }
}

// --- the small parts ---------------------------------------------------------

/** A label with a control under it, as the rest of the client draws one. */
function labelled(text: string, control: HTMLElement): HTMLElement {
  const row = make('label', 'field');
  row.append(make('span', 'field-label', text));
  row.append(control);
  return row;
}

/** One fact, as a name and what the server said. */
function field(label: string, value: string, title?: string): HTMLElement {
  const row = make('div', 'media-fact');
  row.append(make('span', 'media-fact-label', label));
  const said = make('span', 'media-fact-value', value);
  if (title !== undefined) {
    said.title = title;
  }
  row.append(said);
  return row;
}

/** A number input with nothing in it. */
function numberField(): HTMLInputElement {
  const input = document.createElement('input');
  input.type = 'number';
  input.min = '1';
  return input;
}
