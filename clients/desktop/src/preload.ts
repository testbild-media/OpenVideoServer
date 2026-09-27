// The bridge between the window and the main process.
//
// Deliberately narrow: named calls and named events, nothing that forwards an
// arbitrary request. The page cannot ask the main process to fetch something it
// chose, and every way to reach a server from here has a name and a shape.
//
// There is one exception and it is `streamCredential` below: the preview socket
// is opened by the page, because only a page can decode H.264, and a server that
// is not this machine wants something in that socket's first message. What
// crosses now is a ticket — one surface, one minute, spent on use — minted in
// the main process per socket. Nothing here carries a session token any more.

import type {
  AddressSuggestionResponse,
  ImportShowResponse,
  AddEffectResponse,
  OrderEffectsResponse,
  AssignSlotResponse,
  OutputConfig,
  Piece,
  SetSlotRequest,
  ConnectorInfo,
  DisplayMode,
  DmxProtocols,
  TestPatternMode,
  Effect,
  Generator,
  MediaItem,
  Personality,
  Show,
  ShowListEntry,
  ServerTelemetry,
  SourceRef,
} from '@ovs/schema';
import { contextBridge, ipcRenderer, webUtils } from 'electron';

import type { Library, SaveResult } from './shared/api.js';
import type { DroppedFile, HandDeclaration, QueueItem } from './shared/ingest.js';
import type { Answer, Command, CommandResult } from './shared/types.js';
import type { FoundServer } from './main/discovery.js';
import type { ConnectionStatus } from './shared/connection.js';
import type { ShowView } from './shared/view.js';

/** What a sign-in attempt answered. */
export type ConnectResult = { ok: true } | { ok: false; code: string; message: string };

/** What a server said about itself. */
export type InspectResult =
  | { ok: true; origin: string; configured: boolean; version: string }
  | { ok: false; code: string; message: string };

/** Everything the window can do. */
export interface OvsBridge {
  inspect(address: string): Promise<InspectResult>;
  connect(address: string, username: string, password: string): Promise<ConnectResult>;
  createAccount(address: string, username: string, password: string): Promise<ConnectResult>;
  disconnect(): Promise<{ ok: true }>;
  /** What to draw immediately, for a window that has only just loaded. */
  snapshot(): Promise<{ status: ConnectionStatus; view: ShowView; ingest: QueueItem[] }>;
  run(command: Command): Promise<CommandResult>;

  // The show editor. A show is one document: it is loaded whole, edited
  // in the window, and saved whole, because the server has no per-field
  // endpoint (`show_routes::save`). These are still named calls with named
  // shapes — the page cannot compose a request of its own — and the document
  // that crosses on a save is the one the operator has been editing.
  shows(): Promise<Answer<ShowListEntry[]>>;
  /**
   * Asks the operator for one picture and hands it back as a data URL.
   *
   * For the mapping canvas's backdrop. The page cannot read a file and this
   * does not give it one: what crosses is an image the operator chose, as
   * bytes the page can draw, and nothing about where it came from.
   *
   * Null when they chose nothing.
   */
  pickImage(): Promise<Answer<string | null>>;
  /** Loads a show for editing. Changes nothing on stage. */
  loadShow(showId: string): Promise<Answer<Show>>;
  /**
   * Makes a show the running show.
   *
   * **This is the one thing that reloads.** The server opens the document,
   * applies the restart policy and starts the media, effects and generators
   * it names. Everything else — saving, assigning a slot, stacking an
   * effect — leaves the show running, which is why this is a separate,
   * deliberate press (owner, 2026-09-23).
   */
  runShow(showId: string): Promise<Answer<null>>;
  createShow(name: string): Promise<Answer<Show>>;
  /** Saves a show whole. Never interrupts the show. */
  saveShow(show: Show): Promise<Answer<SaveResult>>;
  /**
   * Stores what is on stage into the open show, without stopping anything.
   *
   * The live screen's Save. It sends no document: the server holds the one it
   * has open and the live state on top of it.
   */
  captureLiveState(showId: string): Promise<Answer<SaveResult>>;
  /**
   * Puts a source in one slot of one layer.
   *
   * The one show edit that is not a whole-document save, and the only one
   * the live screen makes. Storing a show the server has open reloads it
   * there, so `saveShow` would restart the show because an operator filled
   * an empty pad; this route edits the document the server already holds and
   * leaves the show running (`slot_routes`).
   *
   * Still a named call with a named shape: the page names a show, a layer,
   * a slot and a source, and cannot compose a request of its own.
   */
  assignSlot(
    showId: string,
    layerId: string,
    slotIndex: number,
    source: SourceRef,
  ): Promise<Answer<AssignSlotResponse>>;
  /** Empties one slot. A slot that is already empty is not an error. */
  clearSlot(
    showId: string,
    layerId: string,
    slotIndex: number,
  ): Promise<Answer<AssignSlotResponse>>;
  /**
   * Stacks an effect on a layer, or on one slot of it.
   *
   * `slotIndex` is null for the layer's own chain. Appended, never replacing
   * (owner, 2026-09-23).
   */
  addEffect(
    showId: string,
    layerId: string,
    slotIndex: number | null,
    typeId: string,
    version: number,
  ): Promise<Answer<AddEffectResponse>>;
  /** Takes one effect off a chain. */
  removeEffect(
    showId: string,
    layerId: string,
    slotIndex: number | null,
    effectInstanceId: string,
  ): Promise<Answer<AddEffectResponse>>;
  /** Sets the order of a chain, naming every instance in it exactly once. */
  orderEffects(
    showId: string,
    layerId: string,
    slotIndex: number | null,
    order: readonly string[],
  ): Promise<Answer<OrderEffectsResponse>>;
  /**
   * Puts a mapping on the wall without saving the show.
   *
   * Lining a wall up is the one job that cannot be done blind, so this is a
   * targeted edit like a slot assignment: the document changes under the
   * running show and nothing restarts (owner, 2026-09-23).
   */
  setMapping(
    showId: string,
    outputs: OutputConfig[],
    pieces: Piece[],
  ): Promise<Answer<AssignSlotResponse>>;
  /**
   * Changes how the clip in one slot runs.
   *
   * A document edit made with the show running, like putting a source in a
   * slot: nothing restarts (owner, 2026-09-23).
   */
  setSlot(
    showId: string,
    layerId: string,
    slotIndex: number,
    change: SetSlotRequest,
  ): Promise<Answer<AssignSlotResponse>>;
  /**
   * The show list's own folders, and the four things done to one.
   *
   * Filing a show moves nothing and changes no document: it is a label on
   * this server's list (owner, 2026-09-23).
   */
  showFolders(): Promise<Answer<string[]>>;
  createShowFolder(path: string): Promise<Answer<null>>;
  /**
   * Deletes a show.
   *
   * It does not come back. The server closes it first when it is the open one,
   * so the renderer is never left holding a show that has gone.
   */
  deleteShow(showId: string): Promise<Answer<null>>;
  deleteShowFolder(path: string): Promise<Answer<null>>;
  renameShowFolder(from: string, to: string): Promise<Answer<null>>;
  fileShow(showId: string, folder: string): Promise<Answer<null>>;
  /** Sets how many banks of twelve one layer's matrix shows. */
  setBanks(
    showId: string,
    layerId: string,
    banks: number,
  ): Promise<Answer<AssignSlotResponse>>;
  /**
   * Which DMX-over-IP protocols reach the show, and setting them.
   *
   * A server setting rather than a show's: which protocols a building
   * speaks is a fact about the building (owner, 2026-09-23).
   */
  dmxProtocols(): Promise<Answer<DmxProtocols>>;
  setDmxProtocols(protocols: DmxProtocols): Promise<Answer<null>>;
  /** Puts a test pattern on the walls, or takes it off. */
  setTestPattern(mode: TestPatternMode, highlighted: string | null): Promise<Answer<null>>;
  /** What this server publishes, which is what the editor may offer. */
  catalogues(): Promise<Answer<{ effects: Effect[]; generators: Generator[] }>>;
  /**
   * The fixture personalities this server publishes.
   *
   * The whole of what the patch editor may offer: channel counts, footprints
   * and personality names all come from here rather than from anything written
   * into this client.
   */
  fixtures(): Promise<Answer<Personality[]>>;
  /**
   * Where the server says a fixture fits in a universe.
   *
   * Asked rather than worked out in the window: the server knows what is
   * already patched. `moving` is the entry being re-addressed, if this is a
   * move, because a fixture does not collide with where it already is.
   */
  suggestAddress(
    showId: string,
    universe: number,
    footprint: number,
    moving: string | null,
  ): Promise<Answer<AddressSuggestionResponse>>;
  mediaLibrary(): Promise<Answer<Library>>;
  /**
   * The sockets this machine has, and what each is for.
   *
   * The only thing an output may be built from. An empty list means no renderer
   * has said yet, which is not the same as a machine with no connectors, so the
   * mapping editor says which of the two it is rather than offering something.
   */
  connectors(): Promise<Answer<ConnectorInfo[]>>;
  /**
   * Every mode an output published on the network may run.
   *
   * Three broadcast rasters at eight rates, progressive only. The server's
   * list: a stream has no panel to satisfy, so what it may run is what the
   * build allows rather than what any hardware offers.
   */
  networkModes(): Promise<Answer<DisplayMode[]>>;

  // The media library. A database view of what the server holds: names, states
  // and what it measured, never a path and never a storage key.
  /** Which shows use one item. Asked before offering to delete it. */
  mediaUsage(mediaItemId: string): Promise<Answer<string[]>>;
  /** Deletes one item. The server refuses while a show uses it. */
  deleteMedia(mediaItemId: string): Promise<Answer<null>>;
  /**
   * The library's own structure. Virtual, always: organising media never
   * moves the real files (owner, 2026-09-23).
   */
  createFolder(path: string): Promise<Answer<null>>;
  renameFolder(from: string, to: string): Promise<Answer<null>>;
  deleteFolder(path: string): Promise<Answer<null>>;
  /** Renames one item, moves it to another folder, or both. */
  fileMedia(
    mediaItemId: string,
    change: { displayName?: string; folder?: string },
  ): Promise<Answer<MediaItem>>;
  /**
   * One item's picture as a data URL, or null when it has none.
   *
   * Null is ordinary: `GET /media/{id}/thumbnail` answers 404 for an item
   * nothing could be read from, and the library draws the name instead.
   */
  mediaThumbnail(mediaItemId: string): Promise<string | null>;
  /**
   * One clip's moving preview as a data URL, or null when it has none.
   *
   * Ten frames of the clip side by side, earliest first, which the window
   * steps through to animate a pad (owner, 2026-09-23). Null for a still and
   * for anything registered before this existed.
   */
  mediaStrip(mediaItemId: string): Promise<string | null>;
  /**
   * Writes a show's package to a file the operator chooses.
   *
   * Answers with the path it was written to, or null when they chose nothing.
   * A show that exists only on one server is a show that can be lost (owner,
   * 2026-09-23).
   */
  exportShow(showId: string, suggested: string): Promise<Answer<string | null>>;
  /**
   * Reads a package the operator chooses and puts it on this server.
   *
   * Answers with what the server had to relink, or null when they chose
   * nothing.
   */
  importShow(): Promise<Answer<ImportShowResponse | null>>;
  /**
   * Writes a show and every clip it uses into a folder.
   *
   * The clips are asked for one at a time and written as they arrive, so the
   * server never builds an archive and never needs room for a second copy of a
   * show (owner, 2026-09-23).
   */
  exportShowWithMedia(showId: string, suggested: string): Promise<Answer<string | null>>;

  // Media ingest. The window hands over paths and is told what is happening to
  // them; probing, hashing, uploading and verifying are the main process's, as
  // everything that touches a disk or a server is.
  /** Opens the file picker. Empty when the operator picked nothing. */
  pickMedia(): Promise<DroppedFile[]>;
  /**
   * Where a dropped file is on this machine.
   *
   * Electron stopped putting a path on `File` in version 32, and a sandboxed
   * page has no other way to say which file was dropped on it. This is the
   * supported replacement and it is local: it reads a path out of a drop the
   * operator performed and reaches nothing.
   */
  pathForFile(file: File): string;
  /**
   * Puts files in the queue, filed in the folder the operator has open.
   *
   * Empty is the root. A clip imported while standing in a folder belongs in
   * that folder (owner, 2026-09-23).
   */
  ingest(files: DroppedFile[], folder?: string): Promise<{ ok: true }>;
  /** Says by hand what a file is, on a machine with no prober. */
  declareIngest(id: string, hand: HandDeclaration): Promise<{ ok: true }>;
  /** Carries on with a file that stopped, from wherever the server got to. */
  resumeIngest(id: string): Promise<{ ok: true }>;
  removeIngest(id: string): Promise<{ ok: true }>;
  clearIngest(): Promise<{ ok: true }>;

  /**
   * One surface's still preview as a data URL, or null when there is none.
   *
   * `composition`, or an output's identifier — the same vocabulary the stream
   * sockets use. The panel falls back to this when a server will not stream, so
   * it has to be the surface the panel is pointed at rather than always the
   * composition: one picture labelled as another is worse than no picture.
   */
  preview(surface: string): Promise<string | null>;
  /**
   * The first message for one preview socket, or null when there is nothing to
   * prove it with.
   *
   * Only a page can decode H.264, so the page has to open that socket; a server
   * that is not this machine wants proof in the socket's first message and will
   * not read one out of a URL. So the message is minted in the main process,
   * per socket — a ticket for this one surface, good for a minute and spent the
   * first time it is used — and the window sends it without reading it.
   *
   * It used to be a `ClientHello` carrying the session token, which meant a
   * window that had been subverted held something that could also load a
   * show or stop the machine. It no longer does. A server too old to know
   * the ticket route still gets the hello, because the alternative is refusing
   * to show an operator their wall.
   */
  streamCredential(surface: string): Promise<string | null>;
  onStatus(listener: (status: ConnectionStatus) => void): void;
  /**
   * Servers this client has found on the network.
   *
   * Called again whenever the list changes, which on a venue network is when
   * the rack is switched on. A list that stays empty is not a fault: a network
   * that filters multicast is ordinary, and the address box is still there.
   */
  onServers(listener: (servers: FoundServer[]) => void): void;
  onView(listener: (view: ShowView) => void): void;
  /** What the server measures, about once a second, including its outputs. */
  onTelemetry(listener: (telemetry: ServerTelemetry) => void): void;
  /**
   * The NDI senders on the server's network, whenever the set changes.
   *
   * The whole list every time, so a listener replaces rather than merges. Sent
   * once when the session opens, so an operator who connects to a rig that is
   * already running sees what is there without waiting for something to move.
   */
  onNdiSources(listener: (sources: readonly string[]) => void): void;
  /**
   * What this machine is wired to, whenever it changes.
   *
   * Not the same as [`connectors`](OvsBridge.connectors), which asks once. A
   * monitor swapped after signing in changes what an output may be assigned to,
   * and a chooser filled in at sign-in would go on offering what was there
   * before.
   */
  onConnectors(listener: (connectors: readonly ConnectorInfo[]) => void): void;
  onFault(listener: (fault: { code: string; message: string }) => void): void;
  /** The ingest queue, whenever anything in it moves. */
  onIngest(listener: (items: QueueItem[]) => void): void;
}

const bridge: OvsBridge = {
  inspect: (address) => ipcRenderer.invoke('ovs:inspect', address),
  connect: (address, username, password) =>
    ipcRenderer.invoke('ovs:connect', address, username, password),
  createAccount: (address, username, password) =>
    ipcRenderer.invoke('ovs:create-account', address, username, password),
  disconnect: () => ipcRenderer.invoke('ovs:disconnect'),
  snapshot: () => ipcRenderer.invoke('ovs:snapshot'),
  run: (command) => ipcRenderer.invoke('ovs:command', command),
  shows: () => ipcRenderer.invoke('ovs:shows'),
  pickImage: () => ipcRenderer.invoke('ovs:pick-image'),
  loadShow: (showId) => ipcRenderer.invoke('ovs:load-show', showId),
  runShow: (showId) => ipcRenderer.invoke('ovs:run-show', showId),
  createShow: (name) => ipcRenderer.invoke('ovs:create-show', name),
  saveShow: (show) => ipcRenderer.invoke('ovs:save-show', show),
  captureLiveState: (showId) => ipcRenderer.invoke('ovs:capture-live', showId),
  assignSlot: (showId, layerId, slotIndex, source) =>
    ipcRenderer.invoke('ovs:assign-slot', showId, layerId, slotIndex, source),
  clearSlot: (showId, layerId, slotIndex) =>
    ipcRenderer.invoke('ovs:clear-slot', showId, layerId, slotIndex),
  addEffect: (showId, layerId, slotIndex, typeId, version) =>
    ipcRenderer.invoke('ovs:add-effect', showId, layerId, slotIndex, typeId, version),
  removeEffect: (showId, layerId, slotIndex, effectInstanceId) =>
    ipcRenderer.invoke('ovs:remove-effect', showId, layerId, slotIndex, effectInstanceId),
  orderEffects: (showId, layerId, slotIndex, order) =>
    ipcRenderer.invoke('ovs:order-effects', showId, layerId, slotIndex, order),
  setBanks: (showId, layerId, banks) =>
    ipcRenderer.invoke('ovs:set-banks', showId, layerId, banks),
  showFolders: () => ipcRenderer.invoke('ovs:show-folders'),
  createShowFolder: (path) => ipcRenderer.invoke('ovs:show-folder-create', path),
  deleteShow: (showId) => ipcRenderer.invoke('ovs:show-delete', showId),
  deleteShowFolder: (path) => ipcRenderer.invoke('ovs:show-folder-delete', path),
  renameShowFolder: (from, to) => ipcRenderer.invoke('ovs:show-folder-rename', from, to),
  fileShow: (showId, folder) => ipcRenderer.invoke('ovs:show-file', showId, folder),
  setSlot: (showId, layerId, slotIndex, change) =>
    ipcRenderer.invoke('ovs:set-slot', showId, layerId, slotIndex, change),
  setMapping: (showId, outputs, pieces) =>
    ipcRenderer.invoke('ovs:set-mapping', showId, outputs, pieces),
  dmxProtocols: () => ipcRenderer.invoke('ovs:dmx-protocols'),
  setDmxProtocols: (protocols) => ipcRenderer.invoke('ovs:dmx-set', protocols),
  setTestPattern: (mode, highlighted) =>
    ipcRenderer.invoke('ovs:test-pattern', mode, highlighted),
  catalogues: () => ipcRenderer.invoke('ovs:catalogues'),
  fixtures: () => ipcRenderer.invoke('ovs:fixtures'),
  suggestAddress: (showId, universe, footprint, moving) =>
    ipcRenderer.invoke('ovs:suggest-address', showId, universe, footprint, moving),
  mediaLibrary: () => ipcRenderer.invoke('ovs:media'),
  connectors: () => ipcRenderer.invoke('ovs:connectors'),
  networkModes: () => ipcRenderer.invoke('ovs:network-modes'),
  mediaUsage: (mediaItemId) => ipcRenderer.invoke('ovs:media-usage', mediaItemId),
  deleteMedia: (mediaItemId) => ipcRenderer.invoke('ovs:media-delete', mediaItemId),
  createFolder: (path) => ipcRenderer.invoke('ovs:folder-create', path),
  renameFolder: (from, to) => ipcRenderer.invoke('ovs:folder-rename', from, to),
  deleteFolder: (path) => ipcRenderer.invoke('ovs:folder-delete', path),
  fileMedia: (mediaItemId, change) => ipcRenderer.invoke('ovs:media-file', mediaItemId, change),
  mediaThumbnail: (mediaItemId) => ipcRenderer.invoke('ovs:media-thumbnail', mediaItemId),
  mediaStrip: (mediaItemId) => ipcRenderer.invoke('ovs:media-strip', mediaItemId),
  exportShow: (showId, suggested) =>
    ipcRenderer.invoke('ovs:show-export', showId, suggested),
  importShow: () => ipcRenderer.invoke('ovs:show-import'),
  exportShowWithMedia: (showId, suggested) =>
    ipcRenderer.invoke('ovs:show-bundle', showId, suggested),
  pickMedia: () => ipcRenderer.invoke('ovs:ingest-pick'),
  pathForFile: (file) => webUtils.getPathForFile(file),
  ingest: (files, folder) => ipcRenderer.invoke('ovs:ingest-add', files, folder ?? ''),
  declareIngest: (id, hand) => ipcRenderer.invoke('ovs:ingest-declare', id, hand),
  resumeIngest: (id) => ipcRenderer.invoke('ovs:ingest-resume', id),
  removeIngest: (id) => ipcRenderer.invoke('ovs:ingest-remove', id),
  clearIngest: () => ipcRenderer.invoke('ovs:ingest-clear'),
  preview: (surface) => ipcRenderer.invoke('ovs:preview', surface),
  streamCredential: (surface) => ipcRenderer.invoke('ovs:stream-credential', surface),
  onStatus: (listener) => {
    ipcRenderer.on('ovs:status', (_event, status: ConnectionStatus) => listener(status));
  },
  onServers: (listener) => {
    ipcRenderer.on('ovs:servers', (_event, servers: FoundServer[]) => listener(servers));
  },
  onView: (listener) => {
    ipcRenderer.on('ovs:view', (_event, view: ShowView) => listener(view));
  },
  onTelemetry: (listener) => {
    ipcRenderer.on('ovs:telemetry', (_event, telemetry: ServerTelemetry) => listener(telemetry));
  },
  onNdiSources: (listener) => {
    ipcRenderer.on('ovs:ndi-sources', (_event, sources: readonly string[]) => listener(sources));
  },
  onConnectors: (listener) => {
    ipcRenderer.on('ovs:connectors-changed', (_event, connectors: readonly ConnectorInfo[]) =>
      listener(connectors),
    );
  },
  onFault: (listener) => {
    ipcRenderer.on('ovs:fault', (_event, fault: { code: string; message: string }) =>
      listener(fault),
    );
  },
  onIngest: (listener) => {
    ipcRenderer.on('ovs:ingest', (_event, items: QueueItem[]) => listener(items));
  },
};

contextBridge.exposeInMainWorld('ovs', bridge);
