// The desktop client's main process.
//
// It owns the window and the session, and every request this client makes is
// made from here — bar the preview socket, which the window opens because the
// decoder lives there and which this process hands one message for. The window
// is sandboxed, isolated and not allowed to navigate: it draws what it is given
// and asks for named commands.

import type {
  DmxProtocols,
  SetLockRequest,
  TestPatternMode,
  ImportShowResponse,
  MediaManifestEntry,
  ShowExport,
  OutputConfig,
  Piece,
  Show,
  SetSlotRequest,
  SourceRef,
} from '@ovs/schema';
import { app, BrowserWindow, dialog, ipcMain, shell } from 'electron';

import { browse, type DiscoveryState, type FoundServer } from './discovery.js';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { basename, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import WebSocket from 'ws';

import type { DroppedFile, HandDeclaration } from '../shared/ingest.js';
import type { Answer, Command, CommandResult } from '../shared/types.js';
import type { OpenSocket, RealtimeSocket } from '../shared/connection.js';
import { files } from './files.js';
import { printSheet, type PrintHow } from './print.js';
import { Session, signInFailure } from './session.js';

/** The window, while there is one. */
let window: BrowserWindow | null = null;

/**
 * Opens a realtime socket.
 *
 * `ws` rather than the platform global: Electron's main process runs a Node
 * version where `WebSocket` is still behind a flag, and a client that silently
 * had no socket would look like a server that never answers.
 */
const openSocket: OpenSocket = (url, handlers): RealtimeSocket => {
  const socket = new WebSocket(url);
  socket.on('open', () => handlers.opened());
  socket.on('message', (data: Buffer | ArrayBuffer | Buffer[]) => {
    handlers.received(data.toString());
  });
  socket.on('error', (error: Error) => {
    // `ws` follows every error with a close, so this only records why.
    lastSocketError = error.message;
  });
  socket.on('close', () => {
    const reason = lastSocketError;
    lastSocketError = '';
    handlers.closed(reason);
  });

  return {
    send: (text) => {
      if (socket.readyState === WebSocket.OPEN) {
        socket.send(text);
      }
    },
    close: () => socket.close(),
  };
};

let lastSocketError = '';

// **Before anything else.** Every build is `OpenVideoServer-0.2.0-setup.exe`,
// so this is the only thing that tells one from another — and the question it
// answers, "is the fix I just installed in this one", cost an install and an
// hour on 2026-10-10 because nothing could answer it.
console.log(`OpenVideoServer client ${app.getVersion()} (${OVS_BUILD_COMMIT})`);

const session = new Session(
  {
    fetch: (url, init) => fetch(url, init),
    open: openSocket,
    clientVersion: app.getVersion(),
    files,
  },
  {
    status: (status) => send('ovs:status', status),
    view: (view) => send('ovs:view', view),
    telemetry: (telemetry) => send('ovs:telemetry', telemetry),
    ndiSources: (sources) => send('ovs:ndi-sources', sources),
    connectors: (connectors) => send('ovs:connectors-changed', connectors),
    fault: (code, message) => send('ovs:fault', { code, message }),
    ingest: (items) => send('ovs:ingest', items),
  },
);

// **Looking for servers, from the moment the app starts.** The connect screen is
// the first thing an operator sees, and a list that only began filling when they
// opened it would be empty for the second they look at it (owner, 2026-09-24).
//
// A browse that finds nothing costs one datagram every five seconds and changes
// nothing on screen: the address box is there either way, which is what makes
// this safe to run on a network that filters multicast.
const servers = browse((found, state) => {
  latestServers = found;
  latestDiscovery = state;
  send('ovs:servers', found, state);
});

/** The last thing the browse said about itself, for a window opening late. */
let latestDiscovery: DiscoveryState | null = null;

/** The last list, for a window that opens after the first answer arrived. */
let latestServers: FoundServer[] = [];

app.on('will-quit', () => servers.stop());

/** What this client has found, for a window asking on startup. */
export function foundServers(): FoundServer[] {
  return latestServers;
}

/** Sends to the window, if there is one to send to. */
function send(channel: string, ...payload: readonly unknown[]): void {
  if (window !== null && !window.isDestroyed()) {
    window.webContents.send(channel, ...payload);
  }
}

/**
 * The shape the window keeps, whatever an operator does to it.
 *
 * Sixteen by nine, always (owner, 2026-09-23). Everything inside this window
 * is laid out as a fraction of a 16:9 composition — the slot matrix, the
 * preview, the piece overlay — and at any other shape those fractions stop
 * agreeing with each other and the slots do visibly odd things. Locking the
 * frame means one set of proportions that simply scales.
 *
 * It is still resizable and still goes full screen; what it will not do is
 * become a shape nothing was drawn for.
 */
const ASPECT = 16 / 9;

/** The smallest the window may be and still be readable at arm's length. */
const MIN_WIDTH = 1280;

/**
 * Where the application's own files are.
 *
 * Beside the built code: `build.mjs` copies `assets/` into `dist/`, and
 * `__dirname` for `dist/main.cjs` is `dist/`. Reaching up a level would find
 * the source tree, which is there while developing and is not there once this
 * is packaged.
 */
function assetPath(name: string): string {
  return join(__dirname, 'assets', name);
}

function createWindow(): void {
  window = new BrowserWindow({
    width: 1600,
    height: Math.round(1600 / ASPECT),
    minWidth: MIN_WIDTH,
    minHeight: Math.round(MIN_WIDTH / ASPECT),
    backgroundColor: '#17171a',
    title: 'OpenVideoServer',
    // The owner's own icon: the mark alone,
    // without the wordmark, which is what an icon is.
    icon: assetPath(process.platform === 'win32' ? 'icon.ico' : 'icon.png'),
    // No File / Edit / View / Window / Help. Nothing in them belongs to this
    // product, and a menu bar above a show surface is a row of pixels spent on
    // commands nobody uses (owner, 2026-09-23).
    autoHideMenuBar: true,
    show: false,
    webPreferences: {
      preload: join(__dirname, 'preload.cjs'),
      // Everything here is a default worth stating: this window renders a page
      // that talks to a media server on a show network, and none of it needs
      // Node, another origin, or a second window.
      contextIsolation: true,
      sandbox: true,
      nodeIntegration: false,
      webviewTag: false,
      spellcheck: false,
    },
  });

  // Belt and braces with `autoHideMenuBar`: that hides it until Alt is
  // pressed, and this removes it, so Alt brings back nothing.
  window.setMenuBarVisibility(false);
  // Kept for the whole aspect: `setAspectRatio` is the platform's own
  // constraint, applied while the frame is being dragged rather than after,
  // so the window never flashes through a shape it will not keep.
  window.setAspectRatio(ASPECT);

  window.once('ready-to-show', () => window?.show());

  // What this client has already found, for a window that opened after the
  // answer arrived. Without it a window created a second late shows "looking…"
  // for ever, because the list only speaks when it changes.
  window.webContents.once('did-finish-load', () =>
    send('ovs:servers', latestServers, latestDiscovery ?? servers.state()),
  );
  window.on('closed', () => {
    window = null;
  });

  // The page is ours and stays ours. A link goes to the operator's browser
  // rather than turning this window into one.
  window.webContents.setWindowOpenHandler(({ url }) => {
    void shell.openExternal(url);
    return { action: 'deny' };
  });
  window.webContents.on('will-navigate', (event) => event.preventDefault());

  void window.loadURL(pathToFileURL(join(__dirname, 'ui', 'index.html')).toString());
}

app.on('ready', () => {
  registerHandlers();
  createWindow();
});

app.on('window-all-closed', () => {
  session.disconnect();
  if (process.platform !== 'darwin') {
    app.quit();
  }
});

app.on('activate', () => {
  if (BrowserWindow.getAllWindows().length === 0) {
    createWindow();
  }
});

app.on('before-quit', () => session.disconnect());

function registerHandlers(): void {
  ipcMain.handle('ovs:inspect', async (_event, address: string) => {
    try {
      return { ok: true as const, ...(await session.inspect(address)) };
    } catch (error) {
      return { ok: false as const, ...signInFailure(error) };
    }
  });

  ipcMain.handle(
    'ovs:connect',
    async (_event, address: string, username: string, password: string) => {
      try {
        await session.connect(address, username, password);
        return { ok: true as const };
      } catch (error) {
        return { ok: false as const, ...signInFailure(error) };
      }
    },
  );

  ipcMain.handle(
    'ovs:create-account',
    async (_event, address: string, username: string, password: string) => {
      try {
        await session.createAccount(address, username, password);
        return { ok: true as const };
      } catch (error) {
        return { ok: false as const, ...signInFailure(error) };
      }
    },
  );

  ipcMain.handle('ovs:disconnect', () => {
    session.disconnect();
    send('ovs:status', { state: 'offline' });
    return { ok: true as const };
  });

  ipcMain.handle('ovs:snapshot', () => session.snapshot);

  ipcMain.handle('ovs:command', (_event, command: Command): Promise<CommandResult> =>
    session.run(command),
  );

  // The show editor. Each of these is one named request; none of them can be
  // pointed at an endpoint of the window's choosing, and loading a show here
  // edits a document rather than changing what is on the wall.
  ipcMain.handle('ovs:shows', () => session.shows());
  ipcMain.handle('ovs:load-show', (_event, showId: string) => session.loadShow(showId));
  ipcMain.handle('ovs:run-show', (_event, showId: string) =>
    session.runShow(showId),
  );
  ipcMain.handle('ovs:create-show', (_event, name: string) => session.createShow(name));
  ipcMain.handle('ovs:save-show', (_event, show: Show) => session.saveShow(show));
  ipcMain.handle('ovs:capture-live', (_event, showId: string) =>
    session.captureLiveState(showId),
  );
  ipcMain.handle(
    'ovs:assign-slot',
    (_event, showId: string, layerId: string, slotIndex: number, source: SourceRef) =>
      session.assignSlot(showId, layerId, slotIndex, source),
  );
  ipcMain.handle(
    'ovs:clear-slot',
    (_event, showId: string, layerId: string, slotIndex: number) =>
      session.clearSlot(showId, layerId, slotIndex),
  );
  ipcMain.handle(
    'ovs:add-effect',
    (
      _event,
      showId: string,
      layerId: string,
      slotIndex: number | null,
      typeId: string,
      version: number,
    ) => session.addEffect(showId, layerId, slotIndex, typeId, version),
  );
  ipcMain.handle('ovs:folder-create', (_event, path: string) => session.createFolder(path));
  ipcMain.handle('ovs:folder-rename', (_event, from: string, to: string) =>
    session.renameFolder(from, to),
  );
  ipcMain.handle('ovs:folder-delete', (_event, path: string) => session.deleteFolder(path));
  ipcMain.handle(
    'ovs:media-file',
    (_event, mediaItemId: string, change: { displayName?: string; folder?: string }) =>
      session.fileMedia(mediaItemId, change),
  );
  ipcMain.handle(
    'ovs:set-mapping',
    (_event, showId: string, outputs: OutputConfig[], pieces: Piece[]) =>
      session.setMapping(showId, outputs, pieces),
  );
  ipcMain.handle(
    'ovs:set-slot',
    (
      _event,
      showId: string,
      layerId: string,
      slotIndex: number,
      change: SetSlotRequest,
    ) => session.setSlot(showId, layerId, slotIndex, change),
  );
  ipcMain.handle('ovs:show-folders', () => session.showFolders());
  ipcMain.handle('ovs:show-folder-create', (_event, path: string) =>
    session.createShowFolder(path),
  );
  ipcMain.handle('ovs:show-folder-delete', (_event, path: string) =>
    session.deleteShowFolder(path),
  );
  ipcMain.handle('ovs:show-delete', (_event, showId: string) =>
    session.deleteShow(showId),
  );
  ipcMain.handle('ovs:show-folder-rename', (_event, from: string, to: string) =>
    session.renameShowFolder(from, to),
  );
  ipcMain.handle('ovs:show-file', (_event, showId: string, folder: string) =>
    session.fileShow(showId, folder),
  );
  ipcMain.handle(
    'ovs:set-banks',
    (_event, showId: string, layerId: string, banks: number) =>
      session.setBanks(showId, layerId, banks),
  );
  ipcMain.handle(
    'ovs:move-slot',
    (_event, showId: string, layerId: string, from: number, to: number, toLayer: string) =>
      session.moveSlot(showId, layerId, from, to, toLayer),
  );
  ipcMain.handle(
    'ovs:remove-effect',
    (
      _event,
      showId: string,
      layerId: string,
      slotIndex: number | null,
      effectInstanceId: string,
    ) => session.removeEffect(showId, layerId, slotIndex, effectInstanceId),
  );
  ipcMain.handle(
    'ovs:order-effects',
    (
      _event,
      showId: string,
      layerId: string,
      slotIndex: number | null,
      order: readonly string[],
    ) => session.orderEffects(showId, layerId, slotIndex, order),
  );
  ipcMain.handle('ovs:set-lock', (_event, showId: string, request: SetLockRequest) =>
    session.setLock(showId, request),
  );
  ipcMain.handle('ovs:dmx-protocols', () => session.dmxProtocols());
  ipcMain.handle('ovs:dmx-set', (_event, protocols: DmxProtocols) =>
    session.setDmxProtocols(protocols),
  );
  ipcMain.handle(
    'ovs:test-pattern',
    (_event, mode: TestPatternMode, highlighted: string | null) =>
      session.setTestPattern(mode, highlighted),
  );
  ipcMain.handle('ovs:catalogues', () => session.catalogues());
  // What the mapping editor may offer as an output: the sockets this machine
  // actually has, with the modes each one reported.
  ipcMain.handle('ovs:connectors', () => session.connectors());
  ipcMain.handle('ovs:network-modes', () => session.networkModes());
  // What the patch editor may offer as a fixture: the personalities this server
  // publishes, and where the server says one fits.
  ipcMain.handle('ovs:fixtures', () => session.fixtures());
  ipcMain.handle(
    'ovs:suggest-address',
    (
      _event,
      showId: string,
      universe: number,
      footprint: number,
      moving: string | null,
    ) => session.suggestAddress(showId, universe, footprint, moving),
  );

  // The media library. A database view, not a filesystem one: nothing here
  // names a path on the server, and nothing in the window could ask for one.
  ipcMain.handle('ovs:media', () => session.mediaLibrary());
  ipcMain.handle('ovs:media-usage', (_event, mediaItemId: string) =>
    session.mediaUsage(mediaItemId),
  );
  ipcMain.handle('ovs:media-delete', (_event, mediaItemId: string) =>
    session.deleteMedia(mediaItemId),
  );
  ipcMain.handle('ovs:media-thumbnail', (_event, mediaItemId: string) =>
    session.mediaThumbnail(mediaItemId),
  );
  ipcMain.handle('ovs:media-strip', (_event, mediaItemId: string) =>
    session.mediaStrip(mediaItemId),
  );
  ipcMain.handle('ovs:show-export', (_event, showId: string, suggested: string) =>
    saveShowPackage(showId, suggested),
  );
  ipcMain.handle('ovs:show-import', () => loadShowPackage());
  ipcMain.handle('ovs:show-bundle', (_event, showId: string, suggested: string) =>
    saveShowBundle(showId, suggested),
  );
  // The DMX patch table, printed or kept as a PDF. The window builds the sheet;
  // this is only the printer.
  ipcMain.handle('ovs:print-sheet', (_event, html: string, how: PrintHow, suggested: string) =>
    printSheet(String(html), how === 'pdf' ? 'pdf' : 'print', String(suggested), window),
  );

  // Media ingest. The window sends paths and the main process does everything
  // else: it is the half with a filesystem, and the only half that may reach a
  // server.
  ipcMain.handle('ovs:pick-image', () => pickImage());
  ipcMain.handle('ovs:ingest-pick', () => pickMedia());
  ipcMain.handle('ovs:ingest-add', (_event, dropped: DroppedFile[], folder?: string) => {
    session.ingest(dropped, folder ?? '');
    return { ok: true as const };
  });
  ipcMain.handle('ovs:ingest-declare', (_event, id: string, hand: HandDeclaration) => {
    session.declareIngest(id, hand);
    return { ok: true as const };
  });
  ipcMain.handle('ovs:ingest-resume', (_event, id: string) => {
    session.resumeIngest(id);
    return { ok: true as const };
  });
  ipcMain.handle('ovs:ingest-remove', (_event, id: string) => {
    session.removeIngest(id);
    return { ok: true as const };
  });
  ipcMain.handle('ovs:ingest-clear', () => {
    session.clearIngest();
    return { ok: true as const };
  });

  // One credential for one socket, minted on demand. The window asks when a
  // preview socket has just opened and sends what comes back as its first
  // message; what it gets is a ticket for that one surface, good for a minute
  // and spent on use, so there is nothing else it could do with it even if it
  // tried.
  ipcMain.handle('ovs:stream-credential', (_event, surface: string) =>
    session.streamCredential(surface),
  );

  /**
   * The file picker, for an operator who would rather not drag anything.
   *
   * No filter by extension. The extension decides nothing here — the file is
   * probed and declared by what is in it — and a filter would hide a ProRes
   * clip somebody had named `.mp4` from the person looking for it.
   */
  /**
   * Asks for one picture and hands back its bytes as a data URL.
   *
   * For the mapping canvas's backdrop. The window is sandboxed and cannot read
   * a file; this does not give it one either — what crosses is an image the
   * operator chose, as bytes the page can draw, and nothing about where it
   * came from.
   *
   * It is read here rather than pointed at, because a `file://` URL in a page
   * that is not allowed to navigate is a permission this client does not want
   * to hand out for a working aid.
   */
  /**
   * Writes a show's package to a file the operator chooses.
   *
   * **A show that exists only on one server is a show that can be lost**
   * (owner, 2026-09-23): "Alles ist nur auf dem Server und kann nicht extra
   * gesichert werden." This is the document and an inventory of what it needs,
   * as JSON, which is something an operator can read, keep, and put on another
   * server.
   *
   * The media is not in it, and the package says so by listing what it wants.
   * Putting tens of gigabytes inside would make one transfer that fails as a
   * unit and re-sends clips the destination already has.
   */
  async function saveShowPackage(
    showId: string,
    suggested: string,
  ): Promise<Answer<string | null>> {
    const answer = await session.exportShow(showId);
    if (!answer.ok) {
      return answer;
    }

    const options: Electron.SaveDialogOptions = {
      defaultPath: `${suggested}.ovsshow.json`,
      filters: [{ name: 'OVS show package', extensions: ['json'] }],
    };
    const chosen = await (window === null || window.isDestroyed()
      ? dialog.showSaveDialog(options)
      : dialog.showSaveDialog(window, options));
    if (chosen.canceled || chosen.filePath === undefined) {
      return { ok: true, value: null };
    }

    try {
      await writeFile(chosen.filePath, JSON.stringify(answer.value, null, 2), 'utf8');
      return { ok: true, value: chosen.filePath };
    } catch (error) {
      return {
        ok: false,
        code: 'write_failed',
        message: `the package could not be written: ${String(error)}`,
        details: [],
      };
    }
  }

  /**
   * Writes a show and every clip it uses into a folder the operator chooses.
   *
   * **The server never builds the archive** (owner, 2026-09-23): "Export alles
   * müsste, um auf dem server nicht zu viel Platz zu brauchen, einzeln an den
   * Clienten schicken und der muss daraus eine Zip oder ähnlich machen." Each
   * item is asked for on its own and written as it arrives, so the server holds
   * one copy of a show and this machine holds the other.
   *
   * A folder rather than one archive file, and that is a choice worth stating:
   * a show's clips run to tens of gigabytes and past four of them a ZIP needs
   * ZIP64, which is a format to implement against a reader rather than blind. A
   * folder has no such limit, can be zipped by whoever wants it zipped, and an
   * interrupted export leaves the files that did arrive rather than an archive
   * that opens nowhere.
   */
  async function saveShowBundle(
    showId: string,
    suggested: string,
  ): Promise<Answer<string | null>> {
    const answer = await session.exportShow(showId);
    if (!answer.ok) {
      return answer;
    }

    const chosen = await (window === null || window.isDestroyed()
      ? dialog.showOpenDialog({ properties: ['openDirectory', 'createDirectory'] })
      : dialog.showOpenDialog(window, { properties: ['openDirectory', 'createDirectory'] }));
    const into = chosen.canceled ? undefined : chosen.filePaths[0];
    if (into === undefined) {
      return { ok: true, value: null };
    }

    const folder = join(into, `${safeName(suggested)} (export)`);
    const clips = join(folder, 'media');
    try {
      await mkdir(clips, { recursive: true });
      await writeFile(
        join(folder, `${safeName(suggested)}.ovsshow.json`),
        JSON.stringify(answer.value, null, 2),
        'utf8',
      );
    } catch (error) {
      return failed('write_failed', `${folder} could not be written: ${String(error)}`);
    }

    // One at a time, written as each arrives. A clip that cannot be read stops
    // the export rather than leaving a folder that looks complete: an operator
    // carrying this to another building has to be able to trust it.
    for (const entry of answer.value.media) {
      const content = await session.mediaContent(entry.media_item_id);
      if (!content.ok) {
        return failed(
          'media_unreadable',
          `${entry.display_name} could not be read from the server: ${content.message}`,
        );
      }
      try {
        await writeFile(join(clips, fileNameFor(entry)), content.value);
      } catch (error) {
        return failed('write_failed', `${entry.display_name} could not be written: ${String(error)}`);
      }
    }

    return { ok: true, value: folder };
  }

  /** A failure in the shape every other answer has. */
  function failed(code: string, message: string): Answer<never> {
    return { ok: false, code, message, details: [] };
  }

  /**
   * Reads a package the operator chooses and puts it on this server.
   *
   * Parsed here rather than sent as bytes, so a file that is not a package is
   * refused before anything reaches the server — and the refusal names the file
   * rather than a line number in a JSON parser.
   */
  async function loadShowPackage(): Promise<Answer<ImportShowResponse | null>> {
    const options: Electron.OpenDialogOptions = {
      properties: ['openFile'],
      filters: [{ name: 'OVS show package', extensions: ['json'] }],
    };
    const chosen = await (window === null || window.isDestroyed()
      ? dialog.showOpenDialog(options)
      : dialog.showOpenDialog(window, options));
    const path = chosen.canceled ? undefined : chosen.filePaths[0];
    if (path === undefined) {
      return { ok: true, value: null };
    }

    let pkg: ShowExport;
    try {
      pkg = JSON.parse(await readFile(path, 'utf8')) as ShowExport;
    } catch (error) {
      return {
        ok: false,
        code: 'unreadable_package',
        message: `${path} is not a package this client can read: ${String(error)}`,
        details: [],
      };
    }
    if (pkg === null || typeof pkg !== 'object' || !('show' in pkg)) {
      return {
        ok: false,
        code: 'not_a_package',
        message: `${path} is not an OVS show package`,
        details: [],
      };
    }

    const answer = await session.importShow(pkg);
    return answer.ok ? { ok: true, value: answer.value } : answer;
  }

  async function pickImage(): Promise<Answer<string | null>> {
    const options: Electron.OpenDialogOptions = {
      properties: ['openFile'],
      filters: [{ name: 'Pictures', extensions: ['png', 'jpg', 'jpeg', 'webp', 'gif', 'bmp'] }],
    };
    const chosen = await (window === null || window.isDestroyed()
      ? dialog.showOpenDialog(options)
      : dialog.showOpenDialog(window, options));
    const path = chosen.canceled ? undefined : chosen.filePaths[0];
    if (path === undefined) {
      return { ok: true, value: null };
    }

    try {
      const bytes = await readFile(path);
      // Held in one window for as long as it is open. A backdrop the size of a
      // wall photograph is a few megabytes, which is nothing beside the media
      // this client already moves.
      const type = mediaTypeOf(path);
      return { ok: true, value: `data:${type};base64,${bytes.toString('base64')}` };
    } catch (error) {
      return {
        ok: false,
        code: 'unreadable',
        message: `that picture could not be read: ${String(error)}`,
        details: [],
      };
    }
  }

  /** What an image file's extension says it is. */
  /**
 * A name a filesystem will take, from a name an operator typed.
 *
 * Everything Windows refuses and everything that would walk out of the folder.
 * A show called `AC/DC — 22:00` is a show, and a path separator in the middle
 * of it is a directory nobody asked for.
 */
function safeName(name: string): string {
  const cleaned = name.replace(/[\\/:*?"<>|]/g, '-').replace(/\s+/g, ' ').trim();
  return cleaned === '' ? 'show' : cleaned.slice(0, 80);
}

/**
 * What one clip is called inside a bundle.
 *
 * The operator's own name, the item's identifier, and an extension from the
 * container. The identifier is in there because two clips may be called the
 * same thing and a bundle that silently kept one of them would be a bundle that
 * lost a clip.
 */
function fileNameFor(entry: MediaManifestEntry): string {
  const ending = entry.original_filename.includes('.')
    ? (entry.original_filename.split('.').pop() ?? 'bin')
    : (entry.container.split(',')[0] ?? 'bin');
  return `${safeName(entry.display_name)} [${entry.media_item_id}].${safeName(ending)}`;
}

function mediaTypeOf(path: string): string {
    const extension = path.slice(path.lastIndexOf('.') + 1).toLowerCase();
    switch (extension) {
      case 'png':
        return 'image/png';
      case 'jpg':
      case 'jpeg':
        return 'image/jpeg';
      case 'webp':
        return 'image/webp';
      case 'gif':
        return 'image/gif';
      case 'bmp':
        return 'image/bmp';
      default:
        // The dialog only offers the list above, so this is a file somebody
        // typed a name for. Named as bytes rather than guessed at.
        return 'application/octet-stream';
    }
  }

  async function pickMedia(): Promise<DroppedFile[]> {
    const chosen = await (window === null || window.isDestroyed()
      ? dialog.showOpenDialog({ properties: ['openFile', 'multiSelections'] })
      : dialog.showOpenDialog(window, { properties: ['openFile', 'multiSelections'] }));
    if (chosen.canceled) {
      return [];
    }
    return chosen.filePaths.map((path) => ({ path, name: basename(path) }));
  }

  ipcMain.handle('ovs:preview', async (_event, surface: string) => {
    const png = await session.preview(surface);
    // As a data URL, because an <img> cannot carry a bearer token and the page
    // has no session of its own to fetch with.
    return png === null ? null : `data:image/png;base64,${Buffer.from(png).toString('base64')}`;
  });
}
