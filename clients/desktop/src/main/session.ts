// One operator, one server, one session.
//
// Everything that touches the network lives here, in the main process, and the
// window is told what to draw. That is not only Electron hygiene: the page has
// no origin a server would accept, and a window that can only ask for named
// commands cannot be talked into reaching an endpoint nobody meant it to reach.
//
// One thing leaves here in a form the page can use, and only one: the first
// message of a preview socket, minted per socket by `streamCredential`, because
// that socket has to be opened where the decoder is. It is a ticket — one
// surface, one minute, spent on use — rather than the session token it used to
// be, so what the window holds can no longer do anything but watch. Since
// 2026-09-22 the ticket is the only thing that socket accepts, so there is no
// path left by which a token could reach a window at all.

import type {
  AddressSuggestionResponse,
  AddEffectResponse,
  OrderEffectsResponse,
  AssignSlotResponse,
  MoveSlotResponse,
  ImportShowResponse,
  ShowExport,
  OutputConfig,
  Piece,
  SetLockRequest,
  SetLockResponse,
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

import {
  ApiFailure,
  ServerApi,
  serverOrigin,
  type FetchLike,
  type Library,
  type SaveResult,
  type UpdateEffectRequest,
} from '../shared/api.js';
import { Connection, type ConnectionStatus, type OpenSocket } from '../shared/connection.js';
import {
  IngestQueue,
  type DroppedFile,
  type HandDeclaration,
  type IngestFiles,
  type QueueItem,
} from '../shared/ingest.js';
import { ceilingOf } from '../shared/encoder.js';
import { COMPOSITION_SURFACE, MASKED_SURFACE } from '../shared/stream.js';
import { emptyView, toView, type ShowView } from '../shared/view.js';
import type { Answer, Command, CommandResult, TapResult } from '../shared/types.js';
import { streamTicket } from '../shared/versions.js';

/** What a session reports to whoever is drawing it. */
export interface SessionListener {
  status(status: ConnectionStatus): void;
  view(view: ShowView): void;
  telemetry(telemetry: ServerTelemetry): void;
  /** The NDI senders the server can see, whenever the set changes. */
  ndiSources(sources: readonly string[]): void;
  /** What the machine is wired to, whenever that changes. */
  connectors(connectors: readonly ConnectorInfo[]): void;
  fault(code: string, message: string): void;
  /** The ingest queue, whenever anything in it moves. */
  ingest(items: QueueItem[]): void;
}

/** How a session reaches the world. Injected, so the tests need neither. */
export interface SessionEnvironment {
  fetch: FetchLike;
  open: OpenSocket;
  clientVersion: string;
  /** Reading files off this machine, for media ingest. */
  files: IngestFiles;
}

/** Signing in went wrong, in a way worth showing the operator. */
export interface SignInFailure {
  code: string;
  message: string;
}

/** One connection to one server, from typing an address to running a show. */
export class Session {
  #api: ServerApi | null = null;
  #connection: Connection | null = null;
  #view: ShowView = emptyView();
  #status: ConnectionStatus = { state: 'offline' };
  /** Whether the account signed in may administer the server. */
  #mayAdminister = false;

  /**
   * Files on their way into the library.
   *
   * It outlives a connection on purpose. A dropped socket in the middle of a
   * twenty-file import is a reason to stop sending, not a reason to throw away
   * what has been hashed and what the server already holds: the endpoints take
   * an offset, so signing back in and carrying on is the whole point.
   */
  readonly #ingest: IngestQueue;

  constructor(
    private readonly environment: SessionEnvironment,
    private readonly listener: SessionListener,
  ) {
    this.#ingest = new IngestQueue(
      {
        ...environment.files,
        // Looked up when they are called rather than captured, because the
        // session they belong to is replaced every time an operator signs in.
        playableCodecs: () => this.#withApi(async (api) => (await api.capabilities()).media_codecs),
        createUpload: (request) => this.#withApi((api) => api.createUpload(request)),
        append: (uploadId, offset, bytes) =>
          this.#withApi((api) => api.appendChunk(uploadId, offset, bytes)),
        verify: (uploadId) => this.#withApi((api) => api.verifyUpload(uploadId)),
        cancel: (uploadId) => this.#withApi((api) => api.cancelUpload(uploadId)),
      },
      (items) => this.listener.ingest(items),
    );
  }

  /** What the window should be showing right now. */
  get snapshot(): { status: ConnectionStatus; view: ShowView; ingest: QueueItem[] } {
    return { status: this.#status, view: this.#view, ingest: this.#ingest.items };
  }

  /** Whether a server has an operator account yet, before offering to sign in. */
  async inspect(address: string): Promise<{ origin: string; configured: boolean; version: string }> {
    const api = new ServerApi(serverOrigin(address), this.environment.fetch);
    const capabilities = await api.capabilities();
    return {
      origin: api.origin,
      configured: capabilities.configured,
      version: capabilities.server_version,
    };
  }

  /** Creates the operator account on a server that has none, then signs in. */
  async createAccount(address: string, username: string, password: string): Promise<void> {
    const api = new ServerApi(serverOrigin(address), this.environment.fetch);
    await api.createAccount(username, password);
    await this.connect(address, username, password);
  }

  /** Signs in and opens the realtime connection. */
  async connect(address: string, username: string, password: string): Promise<void> {
    this.disconnect();

    const api = new ServerApi(serverOrigin(address), this.environment.fetch);
    await api.logIn(username, password);
    this.#api = api;
    // Whether to offer the lock on an effect or a generator at all. Asked once
    // per sign-in, because a role is changed by signing in again; a server
    // that cannot say is one this client offers nothing it may refuse.
    this.#mayAdminister = await api
      .whoAmI()
      .then((me) => me?.account?.role === 'admin')
      .catch(() => false);

    this.#connection = new Connection({
      url: api.realtimeUrl(),
      token: api.token ?? '',
      clientVersion: this.environment.clientVersion,
      open: this.environment.open,
      listener: {
        status: (status) => {
          this.#status = status;
          if (status.state === 'offline' || status.state === 'refused') {
            // Nothing is arriving any more, so what is on screen is no longer
            // the show. Better an empty view than a stale one that still looks
            // live.
            this.#view = emptyView();
            this.listener.view(this.#view);
          }
          this.listener.status(status);
        },
        state: (state) => {
          this.#view = toView(state, this.#mayAdminister);
          // **The ceiling comes from the show, not from this laptop.** It is a
          // property of the wall the material is for, so every client uploading
          // to this server agrees about it without anybody being told twice.
          this.#ingest.setCeiling(ceilingOf(state.show?.settings));
          this.listener.view(this.#view);
        },
        telemetry: (telemetry) => this.listener.telemetry(telemetry),
        ndiSources: (sources) => this.listener.ndiSources(sources),
        connectors: (connectors) => this.listener.connectors(connectors),
        fault: (code, message) => this.listener.fault(code, message),
      },
    });
    this.#connection.start();
  }

  /** Ends the session. Safe to call when there is none. */
  disconnect(): void {
    this.#connection?.stop();
    this.#connection = null;

    const api = this.#api;
    this.#api = null;
    // Best effort: a server that cannot be reached cannot be told, and the
    // token is forgotten here either way.
    void api?.logOut().catch(() => undefined);

    this.#mayAdminister = false;
    this.#view = emptyView();
    this.#status = { state: 'offline' };
  }

  /** Performs one operator command. */
  async run(command: Command): Promise<CommandResult> {
    const api = this.#api;
    if (!api) {
      return { ok: false, code: 'not_connected', message: 'this client is not connected to a server' };
    }

    try {
      // A tap answers with the tempo it produced and whether it produced one;
      // everything else answers with "carried out" and nothing more.
      const tap = await this.#dispatch(api, command);
      return tap === null ? { ok: true } : { ok: true, tap };
    } catch (error) {
      if (error instanceof ApiFailure) {
        return { ok: false, code: error.code, message: error.message };
      }
      return {
        ok: false,
        code: 'unreachable',
        message: error instanceof Error ? error.message : 'the server could not be reached',
      };
    }
  }

  /**
   * The first message for one preview socket: a ticket, per socket.
   *
   * This is what closes the hole this file's header describes. A ticket opens
   * one surface for one minute and is spent the first time it is used, so what
   * crosses the
   * bridge is no longer a credential that could also load a show, move a
   * fader or stop the machine — it is a permission to watch one thing, and a
   * window that has been subverted can do nothing with it but watch that thing
   * for the next minute.
   *
   * Minted here rather than in the window because minting one needs the session,
   * and asked for per socket because a ticket is spent by being used: the second
   * socket's first message has to be the second ticket.
   *
   * There is no fallback. A server that does not know the route is one built
   * before tickets existed, and its socket no longer accepts anything else
   * either (owner decision Q10, closed 2026-09-22) — so sending it a
   * `ClientHello` carrying the session token would hand a window the wide
   * credential and still not get a picture. A server that cannot mint a ticket
   * is a server this client cannot watch, and it says so rather than
   * downgrading.
   */
  async streamCredential(surface: string): Promise<string | null> {
    const api = this.#api;
    if (api === null) {
      return null;
    }

    try {
      const minted = await api.streamTicket(surface);
      return streamTicket(minted.ticket);
    } catch (error) {
      // Said out loud rather than swallowed: the panel would otherwise show five
      // seconds of nothing and then a socket the server closed without a word.
      const failure = error instanceof ApiFailure ? error : null;
      this.listener.fault(
        failure?.code ?? 'unreachable',
        `this client could not get permission to watch ${surface}: ${
          failure?.message ?? (error instanceof Error ? error.message : 'the server could not be reached')
        }`,
      );
      return null;
    }
  }

  // --- the media library ----------------------------------------------------

  /** Which shows reference one item. Asked before offering to delete. */
  mediaUsage(mediaItemId: string): Promise<Answer<string[]>> {
    return this.#answer(async (api) => (await api.mediaUsage(mediaItemId)).used_by_shows);
  }

  /** Makes a folder in the library, and every folder above it. */
  createFolder(path: string): Promise<Answer<null>> {
    return this.#answer(async (api) => {
      await api.createFolder(path);
      return null;
    });
  }

  /** Renames a folder, which moves everything filed under it. */
  renameFolder(from: string, to: string): Promise<Answer<null>> {
    return this.#answer(async (api) => {
      await api.renameFolder(from, to);
      return null;
    });
  }

  /** Removes a folder that has nothing in it. */
  deleteFolder(path: string): Promise<Answer<null>> {
    return this.#answer(async (api) => {
      await api.deleteFolder(path);
      return null;
    });
  }

  /** Renames one item, moves it to another folder, or both. */
  fileMedia(
    mediaItemId: string,
    change: { displayName?: string; folder?: string },
  ): Promise<Answer<MediaItem>> {
    return this.#answer((api) => api.fileMedia(mediaItemId, change));
  }

  /** Deletes one item. The server refuses while a show uses it. */
  deleteMedia(mediaItemId: string): Promise<Answer<null>> {
    return this.#answer(async (api) => {
      await api.deleteMedia(mediaItemId);
      return null;
    });
  }

  /**
   * One item's picture, as a data URL, or null when it has none.
   *
   * A data URL because an `<img>` in the window cannot carry a bearer token and
   * the page has no session of its own to fetch with — the same reason the still
   * composition preview crosses this way.
   */
  /** A show as a package, ready to be written to a file. */
  exportShow(showId: string): Promise<Answer<ShowExport>> {
    return this.#answer((api) => api.exportShow(showId));
  }

  /** One media item's content, as bytes. */
  mediaContent(mediaItemId: string): Promise<Answer<Uint8Array>> {
    return this.#answer((api) => api.mediaContent(mediaItemId));
  }

  /** Puts a package on this server. */
  importShow(pkg: ShowExport): Promise<Answer<ImportShowResponse>> {
    return this.#answer((api) => api.importShow(pkg));
  }

  /** One clip's moving preview, as a data URL, or null when there is none. */
  async mediaStrip(mediaItemId: string): Promise<string | null> {
    const api = this.#api;
    if (api === null) {
      return null;
    }
    try {
      const jpeg = await api.mediaStrip(mediaItemId);
      return jpeg === null
        ? null
        : `data:image/jpeg;base64,${Buffer.from(jpeg).toString('base64')}`;
    } catch {
      // A pad that does not move is a pad. Nothing on stage depends on this.
      return null;
    }
  }

  async mediaThumbnail(mediaItemId: string): Promise<string | null> {
    const api = this.#api;
    if (api === null) {
      return null;
    }
    try {
      const png = await api.mediaThumbnail(mediaItemId);
      return png === null ? null : `data:image/png;base64,${Buffer.from(png).toString('base64')}`;
    } catch {
      // An item whose picture could not be fetched is drawn as its name, which
      // is what an item with no picture is drawn as. Nothing on stage depends on
      // this and a library that failed to load is not a show that stopped.
      return null;
    }
  }

  // --- media ingest ---------------------------------------------------------

  /**
   * Puts files in the ingest queue, to be filed in `folder`.
   *
   * The folder the operator had open when they dropped them. Empty is the
   * root — which is what dropping into All media means, and what every caller
   * meant before there were folders.
   */
  ingest(files: readonly DroppedFile[], folder = ''): void {
    this.#ingest.add(files, folder);
  }

  /** Says by hand what a file is, on a machine with no prober. */
  declareIngest(id: string, hand: HandDeclaration): void {
    this.#ingest.declare(id, hand);
  }

  /** Carries on with a file that stopped, from wherever the server got to. */
  resumeIngest(id: string): void {
    this.#ingest.resume(id);
  }

  /** Takes a file out of the queue, cancelling its upload if there is one. */
  removeIngest(id: string): void {
    this.#ingest.remove(id);
  }

  /** Forgets the rows that have finished, one way or the other. */
  clearIngest(): void {
    this.#ingest.clearFinished();
  }

  /** Runs one request against the session there is, or refuses in so many words. */
  #withApi<T>(work: (api: ServerApi) => Promise<T>): Promise<T> {
    const api = this.#api;
    if (api === null) {
      return Promise.reject(new Error('this client is not connected to a server'));
    }
    return work(api);
  }

  // --- the show editor ---------------------------------------------------
  //
  // The editor asks for documents rather than pressing buttons, so these answer
  // with what came back. Each of them is a named request with a named shape:
  // the window still cannot compose one of its own.

  /** Every show on the server, including ones this build cannot open. */
  shows(): Promise<Answer<ShowListEntry[]>> {
    return this.#answer((api) => api.shows());
  }

  /**
   * One show, for editing.
   *
   * Loading a document changes nothing on stage. Making a show the running
   * show is a separate thing an operator does deliberately.
   */
  loadShow(showId: string): Promise<Answer<Show>> {
    return this.#answer((api) => api.show(showId));
  }

  /**
   * Makes a show the running show.
   *
   * The one call that reloads: the server opens the document and applies the
   * restart policy, which starts the media, effects and generators it names.
   */
  runShow(showId: string): Promise<Answer<null>> {
    return this.#answer(async (api) => {
      await api.openShow(showId);
      return null;
    });
  }

  /** Creates an empty show and hands it back to be edited. */
  createShow(name: string): Promise<Answer<Show>> {
    return this.#answer((api) => api.createShow(name));
  }

  /**
   * Saves a document whole.
   *
   * A refusal carries the server's field-level problems through `details`, so
   * the editor can put each one beside the field it names.
   *
   * Never interrupts the show: the server installs the stored document over
   * the running one without applying the restart policy.
   */
  saveShow(show: Show): Promise<Answer<SaveResult>> {
    return this.#answer((api) => api.saveShow(show));
  }

  /** Stores what is on stage into the open show, without stopping it. */
  captureLiveState(showId: string): Promise<Answer<SaveResult>> {
    return this.#answer((api) => api.captureLiveState(showId));
  }

  /**
   * Puts a source in one slot, without reloading the show.
   *
   * A refusal comes back as it arrived. The three that matter are a clip the
   * library does not have, a slot index a layer cannot hold, and a show
   * this server does not have — and all three are things the operator has to
   * be told in the server's own words rather than have paraphrased.
   */
  assignSlot(
    showId: string,
    layerId: string,
    slotIndex: number,
    source: SourceRef,
  ): Promise<Answer<AssignSlotResponse>> {
    return this.#answer((api) => api.assignSlot(showId, layerId, slotIndex, source));
  }

  /** Empties one slot. */
  clearSlot(
    showId: string,
    layerId: string,
    slotIndex: number,
  ): Promise<Answer<AssignSlotResponse>> {
    return this.#answer((api) => api.clearSlot(showId, layerId, slotIndex));
  }

  /** Stacks an effect on a layer, or on one slot of it. */
  addEffect(
    showId: string,
    layerId: string,
    slotIndex: number | null,
    typeId: string,
    version: number,
  ): Promise<Answer<AddEffectResponse>> {
    return this.#answer((api) => api.addEffect(showId, layerId, slotIndex, typeId, version));
  }

  /** Takes one effect off a chain. */
  removeEffect(
    showId: string,
    layerId: string,
    slotIndex: number | null,
    effectInstanceId: string,
  ): Promise<Answer<AddEffectResponse>> {
    return this.#answer((api) =>
      api.removeEffect(showId, layerId, slotIndex, effectInstanceId),
    );
  }

  /** Sets the order of a chain, naming every instance in it exactly once. */
  orderEffects(
    showId: string,
    layerId: string,
    slotIndex: number | null,
    order: readonly string[],
  ): Promise<Answer<OrderEffectsResponse>> {
    return this.#answer((api) => api.orderEffects(showId, layerId, slotIndex, order));
  }

  /** Locks one effect or generator where it is, or unlocks it. Administrators only. */
  setLock(showId: string, request: SetLockRequest): Promise<Answer<SetLockResponse>> {
    return this.#answer((api) => api.setLock(showId, request));
  }

  /** Sets how many banks of twelve one layer's matrix shows. */
  setBanks(
    showId: string,
    layerId: string,
    banks: number,
  ): Promise<Answer<AssignSlotResponse>> {
    return this.#answer((api) => api.setBanks(showId, layerId, banks));
  }

  /** Moves a slot to another pad of the same layer. */
  moveSlot(
    showId: string,
    layerId: string,
    from: number,
    to: number,
  ): Promise<Answer<MoveSlotResponse>> {
    return this.#answer((api) => api.moveSlot(showId, layerId, from, to));
  }

  /** Puts a mapping on the wall without saving the show. */
  setMapping(
    showId: string,
    outputs: OutputConfig[],
    pieces: Piece[],
  ): Promise<Answer<AssignSlotResponse>> {
    return this.#answer((api) => api.setMapping(showId, outputs, pieces));
  }

  /** Changes how the clip in one slot runs. Nothing restarts. */
  setSlot(
    showId: string,
    layerId: string,
    slotIndex: number,
    change: SetSlotRequest,
  ): Promise<Answer<AssignSlotResponse>> {
    return this.#answer((api) => api.setSlot(showId, layerId, slotIndex, change));
  }

  /** The folders the show list is organised into. */
  showFolders(): Promise<Answer<string[]>> {
    return this.#answer((api) => api.showFolders());
  }

  /** Makes a show folder, and every folder above it that is not there. */
  createShowFolder(path: string): Promise<Answer<null>> {
    return this.#answer(async (api) => {
      await api.createShowFolder(path);
      return null;
    });
  }

  /** Removes a show folder that holds nothing. */
  /** Deletes a show, and everything the server keeps with it. */
  deleteShow(showId: string): Promise<Answer<null>> {
    return this.#answer(async (api) => {
      await api.deleteShow(showId);
      return null;
    });
  }

  deleteShowFolder(path: string): Promise<Answer<null>> {
    return this.#answer(async (api) => {
      await api.deleteShowFolder(path);
      return null;
    });
  }

  /** Renames a show folder, which moves everything under it. */
  renameShowFolder(from: string, to: string): Promise<Answer<null>> {
    return this.#answer(async (api) => {
      await api.renameShowFolder(from, to);
      return null;
    });
  }

  /** Files one show in a folder. The document is untouched. */
  fileShow(showId: string, folder: string): Promise<Answer<null>> {
    return this.#answer(async (api) => {
      await api.fileShow(showId, folder);
      return null;
    });
  }

  /** Which DMX-over-IP protocols reach the show. */
  dmxProtocols(): Promise<Answer<DmxProtocols>> {
    return this.#answer((api) => api.dmxProtocols());
  }

  /** Sets which DMX-over-IP protocols reach the show. */
  setDmxProtocols(protocols: DmxProtocols): Promise<Answer<null>> {
    return this.#answer(async (api) => {
      await api.setDmxProtocols(protocols);
      return null;
    });
  }

  /** Puts a test pattern on the walls, or takes it off. */
  setTestPattern(mode: TestPatternMode, highlighted: string | null): Promise<Answer<null>> {
    return this.#answer(async (api) => {
      await api.setTestPattern(mode, highlighted);
      return null;
    });
  }

  /** What this server publishes: the effects and generators an editor offers. */
  catalogues(): Promise<Answer<{ effects: Effect[]; generators: Generator[] }>> {
    return this.#answer(async (api) => {
      const [effects, generators] = await Promise.all([api.effects(), api.generators()]);
      return { effects: effects.effects, generators: generators.generators };
    });
  }

  /**
   * The fixture personalities this server publishes.
   *
   * The whole of what the patch editor may offer: the type, the footprint,
   * what a personality may be patched to and its channels. A client
   * that carried its own copy of any of that would patch the next fixture on top
   * of the last one the day a personality changed.
   */
  fixtures(): Promise<Answer<Personality[]>> {
    return this.#answer(async (api) => (await api.fixtures()).fixtures);
  }

  /**
   * Where the server says a fixture fits.
   *
   * Asked rather than worked out here, because the server knows what is already
   * patched. It answers from the show on disk, so an unsaved patch change is
   * not in the answer — which the panel says, rather than this client quietly
   * doing the arithmetic itself.
   */
  suggestAddress(
    showId: string,
    universe: number,
    footprint: number,
    moving: string | null,
  ): Promise<Answer<AddressSuggestionResponse>> {
    return this.#answer((api) => api.suggestAddress(showId, universe, footprint, moving));
  }

  /** The media library, which is what a slot can be pointed at. */
  mediaLibrary(): Promise<Answer<Library>> {
    return this.#answer(async (api) => {
      const response = await api.media();
      return {
        items: response.items,
        // Sent whole rather than worked out from the items, because a folder
        // exists while it is empty: an operator makes one in order to put
        // things in it (owner, 2026-09-23).
        folders: response.folders ?? [],
      };
    });
  }

  /**
   * The physical connectors this server found, and what each one is for.
   *
   * What the mapping editor builds an output from, and the only place it may:
   * an output is forced onto a socket by GPU and connector, so both halves have
   * to be ones the machine reported. An empty list is a real
   * answer — it means no renderer has connected and said yet — and is different
   * from a machine with no connectors, which is why nothing here substitutes
   * anything for it.
   */
  connectors(): Promise<Answer<ConnectorInfo[]>> {
    return this.#answer(async (api) => (await api.capabilities()).connectors);
  }

  /**
   * Every mode an output published on the network may run.
   *
   * The server's list, not this client's. A stream has no panel to satisfy, so
   * what it may run is what this build allows — and a second copy of that list
   * here would be the one that went out of date.
   */
  networkModes(): Promise<Answer<DisplayMode[]>> {
    return this.#answer(async (api) => (await api.capabilities()).network_output_modes);
  }

  /** Performs one request, turning anything thrown into an answer. */
  async #answer<T>(work: (api: ServerApi) => Promise<T>): Promise<Answer<T>> {
    const api = this.#api;
    if (!api) {
      return {
        ok: false,
        code: 'not_connected',
        message: 'this client is not connected to a server',
        details: [],
      };
    }
    try {
      return { ok: true, value: await work(api) };
    } catch (error) {
      if (error instanceof ApiFailure) {
        return { ok: false, code: error.code, message: error.message, details: error.details };
      }
      return {
        ok: false,
        code: 'unreachable',
        message: error instanceof Error ? error.message : 'the server could not be reached',
        details: [],
      };
    }
  }

  /**
   * One surface's still preview, as PNG bytes, or null when there is none.
   *
   * `composition`, or an output's identifier, which is the same vocabulary the
   * stream sockets use. The surface has to cross, because this is the fallback
   * the panel falls back *to*: a panel pointed at OUTPUT 2 that quietly showed
   * the composition instead would be labelling one picture as another.
   *
   * The masked composition has no still of its own and answers nothing rather
   * than answering with the unmasked one, for the same reason. It is not a gap
   * worth a second readback on the renderer: the piece outlines are drawn over
   * the plain composition either way, so a server that will not stream still
   * shows an operator which regions are taken — as lines instead of as a mask.
   */
  async preview(surface: string): Promise<Uint8Array | null> {
    if (!this.#api || surface === MASKED_SURFACE) {
      return null;
    }
    try {
      return surface === COMPOSITION_SURFACE
        ? await this.#api.compositionPreview()
        : await this.#api.outputPreview(surface);
    } catch {
      // A preview that is not available is not an error worth interrupting an
      // operator for; the panel simply shows nothing.
      return null;
    }
  }

  /** Performs one command, answering with the tap's tempo when it was a tap. */
  async #dispatch(api: ServerApi, command: Command): Promise<TapResult | null> {
    switch (command.do) {
      case 'select_slot':
        await api.selectSlot(command.layerId, command.slotIndex);
        return null;
      case 'set_layer':
        // A field left out means "leave this alone", on the wire as well as
        // here: a client that sent a default for something it was not thinking
        // about would move it on stage (`UpdateLayerRequest`).
        await api.updateLayer(command.layerId, {
          route: command.route ?? null,
          opacity: command.opacity ?? null,
          transform: command.transform ?? null,
          blend_mode: command.blendMode ?? null,
          transport: command.transport ?? null,
          enabled: command.enabled ?? null,
        });
        return null;
      case 'set_master':
        await api.updateMaster({
          crossfader: command.crossfader ?? null,
          dimmer: command.dimmer ?? null,
          bpm: command.bpm ?? null,
          speed_scale: command.speedScale ?? null,
          freeze: command.freeze ?? null,
          dmx_enabled: command.dmxEnabled ?? null,
        });
        return null;
      case 'set_effect': {
        // Built field by field rather than with `?? null`, as the two above
        // are. Those fields are `Option` on the wire and a null is a legitimate
        // "leave this alone"; these two are a map and a bool with serde
        // defaults, so an absent field is the only way to say it. A request that
        // only puts an effect back carries `reset` and no values at all.
        const body: UpdateEffectRequest = {};
        if (command.overrides !== undefined) {
          body.overrides = command.overrides;
        }
        if (command.reset === true) {
          body.reset = true;
        }
        await api.updateEffect(command.effectInstanceId, body);
        return null;
      }
      case 'set_generator': {
        // The same body, for the same reason: one parameter model, two
        // identities. `UpdateEffectRequest` is the shape both routes take.
        const body: UpdateEffectRequest = {};
        if (command.overrides !== undefined) {
          body.overrides = command.overrides;
        }
        if (command.reset === true) {
          body.reset = true;
        }
        await api.updateGenerator(command.generatorInstanceId, body);
        return null;
      }
      case 'tap': {
        const tapped = await api.tap();
        return { bpm: tapped.bpm, changed: tapped.changed };
      }
      case 'reset_phase':
        await api.resetPhase();
        return null;
      case 'open_show':
        await api.openShow(command.showId);
        return null;
    }
  }
}

/** Turns anything thrown while signing in into something an operator can read. */
export function signInFailure(error: unknown): SignInFailure {
  if (error instanceof ApiFailure) {
    // Whether the server needs setting up at all is not something a refused
    // sign-in can say: an unknown username and a wrong password answer
    // identically, on purpose, because the difference would enumerate
    // accounts. `Session.inspect` asks the server that question directly.
    return { code: error.code, message: error.message };
  }
  if (error instanceof Error) {
    return { code: 'unreachable', message: error.message };
  }
  return { code: 'unreachable', message: 'the server could not be reached' };
}
