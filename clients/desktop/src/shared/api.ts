// The HTTP half of talking to a server.
//
// Commands go over HTTP and state comes back over the socket,
// so this file is every button the operator can press and nothing they can see.
//
// It runs in Electron's main process. That is where the session token lives,
// and no request made from the window is made with it: the page has no origin a
// server would accept anyway, and every command it can ask for is named here.
// The token does leave this process in one shape — the first message of a
// preview socket, which `Session.streamCredential` mints and the page sends — and
// that one is argued out where it is minted.

import type {
  AddEffectRequest,
  AddEffectResponse,
  OrderEffectsRequest,
  OrderEffectsResponse,
  AddressSuggestionResponse,
  AssignSlotRequest,
  AssignSlotResponse,
  SetSlotRequest,
  CreateFolderRequest,
  CapabilitiesResponse,
  CreateUploadRequest,
  CreateUploadResponse,
  DmxProtocols,
  TestPatternMode,
  EffectCatalogueResponse,
  FileMediaRequest,
  FixtureCatalogueResponse,
  GeneratorCatalogueResponse,
  LoginResponse,
  SetLockRequest,
  SetLockResponse,
  WhoAmIResponse,
  MediaItem,
  FileShowRequest,
  MediaListItem,
  OutputConfig,
  Piece,
  SetMappingRequest,
  ShowExport,
  ImportShowResponse,
  ShowFoldersResponse,
  MediaListResponse,
  MediaUsageResponse,
  Show,
  ShowListEntry,
  RenameFolderRequest,
  SaveShowRequest,
  SelectSlotRequest,
  SetBanksRequest,
  MoveSlotRequest,
  MoveSlotResponse,
  SourceRef,
  StateSnapshot,
  StatusResponse,
  StreamTicketResponse,
  TapTempoResponse,
  UpdateLayerRequest,
  UpdateMasterRequest,
  UploadSession,
  ValidationError,
} from '@ovs/schema';

import { API_PREFIX, REALTIME_PATH } from './versions.js';

/** The `fetch` this client uses. Injected so the tests need no server. */
export type FetchLike = (url: string, init?: RequestInit) => Promise<Response>;

/**
 * The library as one answer: what is in it, and how it is organised.
 *
 * The folders travel with the items rather than being worked out from them,
 * because a folder exists while it is empty — an operator makes one in order to
 * put things in it (owner, 2026-09-23).
 */
export interface Library {
  /**
   * Every item, with how much disc its content takes.
   *
   * `MediaListItem` rather than `MediaItem`: the list endpoint sends the size
   * beside each item, flattened into the same object, and an operator deciding
   * what to delete before a show needs it (owner, 2026-09-23).
   */
  items: MediaListItem[];
  /** Every folder, in path order. Slash-separated; the root is not in it. */
  folders: string[];
}

/**
 * What a save answered.
 *
 * Written out here rather than imported, because `SaveShowResponse` is the
 * one response shape the schema generator does not publish a TypeScript type
 * for — `packages/ovs-schema-ts` has `SaveShowRequest` and no matching
 * response. Adding it is a change to a generated package this client does not
 * own, so it is named here and noted rather than invented there.
 */
export interface SaveResult {
  /** When the server recorded the save, in milliseconds. */
  updated_at: number;
}

/**
 * The live values of one effect instance, keyed as the manifest names them.
 *
 * Taken out of the generated `StateSnapshot` rather than written out, because
 * the map the server publishes and the map a client sends are the same map
 * (`EffectOverrides`). Declaring a second one here would be a copy that could
 * disagree with the schema the day a `ParameterValue` variant is added.
 */
export type EffectOverrides = NonNullable<StateSnapshot['effects']>[string];

/**
 * What `PUT /live/effects/{id}` takes.
 *
 * Written out here for the reason `SaveResult` is: `UpdateEffectRequest` is one
 * of the request shapes the schema generator does not publish a TypeScript type
 * for, and `packages/ovs-schema-ts` is a generated package this client does not
 * own. Only the two field names are named here; the value type they carry is
 * the generated one above.
 *
 * Both fields are optional and mean "do not". An absent `overrides` moves
 * nothing, and an absent `reset` keeps whatever live values the instance
 * already had — so `{ reset: true }` on its own is how an operator puts one
 * effect back to what the show configured.
 */
export interface UpdateEffectRequest {
  overrides?: EffectOverrides;
  reset?: boolean;
}

/**
 * A request the server refused.
 *
 * Carries the server's own code and message rather than a status number alone,
 * because the operator has to be told what went wrong in words, and the code is
 * what the client branches on.
 */
export class ApiFailure extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly details: ValidationError[] = [],
  ) {
    super(message);
    this.name = 'ApiFailure';
  }

  /** Whether signing in again would help. */
  get isUnauthenticated(): boolean {
    return this.status === 401;
  }
}

/**
 * Turns what an operator typed into an origin.
 *
 * They type `10.0.0.12`, or paste `http://ovs.local:7600/`, or include a path
 * because they copied it from the Service Console. All three mean the same
 * server, and refusing two of them would only teach them to type carefully.
 */
export function serverOrigin(typed: string): string {
  const trimmed = typed.trim();
  if (trimmed === '') {
    throw new Error('a server address is required');
  }
  const withScheme = /^https?:\/\//i.test(trimmed) ? trimmed : `http://${trimmed}`;
  const url = new URL(withScheme);
  if (url.port === '' && url.protocol === 'http:') {
    // What `ovsd` listens on unless OVS_BIND says otherwise. A client that
    // silently tried port 80 would report "connection refused" about a server
    // that is running perfectly well.
    url.port = '7420';
  }
  return url.origin;
}

/** Everything this client can ask a server to do. */
export class ServerApi {
  #token: string | null = null;

  constructor(
    readonly origin: string,
    private readonly fetchImpl: FetchLike,
  ) {}

  /** The token from the last successful sign-in, or null. */
  get token(): string | null {
    return this.#token;
  }

  /** Adopts a token obtained elsewhere, or forgets the current one. */
  setToken(token: string | null): void {
    this.#token = token;
  }

  /** The realtime socket's address for this server. */
  realtimeUrl(): string {
    const url = new URL(REALTIME_PATH, this.origin);
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    return url.toString();
  }

  // --- before signing in ---------------------------------------------------

  /**
   * What this server is and whether it has an operator account yet.
   *
   * Unauthenticated on purpose: a client has to be able to discover that a
   * server is in first-run setup before it can offer to sign in.
   */
  capabilities(): Promise<CapabilitiesResponse> {
    return this.#request('GET', '/server/capabilities');
  }

  /** Creates the operator account on a server that has none. */
  async createAccount(username: string, password: string): Promise<void> {
    await this.#request('POST', '/auth/setup', { username, password });
  }

  /** Exchanges credentials for a session token, and keeps it. */
  async logIn(username: string, password: string): Promise<LoginResponse> {
    const response = await this.#request<LoginResponse>('POST', '/auth/login', {
      username,
      password,
    });
    this.#token = response.token;
    return response;
  }

  /** Ends the session, whether or not the server agrees. */
  async logOut(): Promise<void> {
    try {
      await this.#request('POST', '/auth/logout');
    } finally {
      // A token the server has already forgotten is still a token this client
      // must stop using.
      this.#token = null;
    }
  }

  /**
   * The account this session belongs to, and its role.
   *
   * Asked so that an interface offers the lock on an effect only to somebody
   * the server will let lock one. The server still decides.
   */
  whoAmI(): Promise<WhoAmIResponse> {
    return this.#request('GET', '/auth/me');
  }

  // --- the server ----------------------------------------------------------

  status(): Promise<StatusResponse> {
    return this.#request('GET', '/server/status');
  }

  fixtures(): Promise<FixtureCatalogueResponse> {
    return this.#request('GET', '/fixtures');
  }

  generators(): Promise<GeneratorCatalogueResponse> {
    return this.#request('GET', '/generators');
  }

  effects(): Promise<EffectCatalogueResponse> {
    return this.#request('GET', '/effects');
  }

  // --- shows ------------------------------------------------------------

  shows(): Promise<ShowListEntry[]> {
    return this.#request('GET', '/shows');
  }

  show(showId: string): Promise<Show> {
    return this.#request('GET', `/shows/${encodeURIComponent(showId)}`);
  }

  createShow(name: string): Promise<Show> {
    return this.#request('POST', '/shows', { name });
  }

  /**
   * Saves a show whole, and reports when the server says it wrote it.
   *
   * A show is saved whole, on purpose: it cannot be left internally
   * inconsistent by a half-applied edit (`show_routes::save`). `assignSlot`
   * below is the one edit that does not send the whole document, and it is
   * still applied to one — the server reads the show, changes the slot and
   * validates and stores the result.
   *
   * **A save never interrupts the show** (owner, 2026-09-23). It used to:
   * storing the show the server had open reloaded it there, which put
   * every layer back to its opening slot in the middle of a night. The
   * server now installs the stored document over the running one without
   * applying the restart policy; only opening a show reloads one.
   *
   * This stores the document exactly as it is sent. `captureLiveState` is the
   * other gesture — “store what I am looking at” — and it sends no document at
   * all, because the server already holds one.
   *
   * The timestamp is the server's own. An editor that stamped the document
   * itself would be putting a number in it that nothing measured.
   */
  saveShow(show: Show): Promise<SaveResult> {
    const body: SaveShowRequest = { show };
    return this.#request('PUT', `/shows/${encodeURIComponent(show.id)}`, body);
  }

  /**
   * Stores what is on stage into the open show.
   *
   * The owner's Save from the live screen (2026-09-23): each layer's bus,
   * opacity, blend mode, trigger grid and whether it is on; the master's
   * tempo, dimmer, crossfader and speed; and every effect and generator
   * parameter a control source has moved.
   *
   * **The show does not stop.** No document is sent: the server holds the one
   * it has open and the live state running on top of it, and a copy from here
   * could only be staler than the original.
   */
  captureLiveState(showId: string): Promise<SaveResult> {
    return this.#request('POST', `/shows/${encodeURIComponent(showId)}/capture`);
  }

  async openShow(showId: string): Promise<void> {
    await this.#request('POST', `/shows/${encodeURIComponent(showId)}/open`);
  }

  /**
   * Puts a source in one slot of one layer.
   *
   * The one show edit that is not a whole-document save, and the reason
   * is the show: storing a show the server has open reloads it there, so
   * `saveShow` would put every layer back to its opening slot because
   * somebody filled an empty pad. This route edits the document the server
   * already holds and leaves the show running (`slot_routes`).
   *
   * Everything else about the slot — how it loops, its direction, its beat
   * sync, its effects — is kept by the server. This changes what plays.
   *
   * The answer says what was there before. The asking happens in front of
   * the operator, before this is called; what comes back is the record of
   * what was actually replaced, which is not always what the client thought.
   */
  assignSlot(
    showId: string,
    layerId: string,
    slotIndex: number,
    source: SourceRef,
  ): Promise<AssignSlotResponse> {
    const body: AssignSlotRequest = { source };
    return this.#request('PUT', this.#slotPath(showId, layerId, slotIndex), body);
  }

  /**
   * Empties one slot.
   *
   * A slot that is already empty is not an error: the pad the operator is
   * looking at is empty either way, and the server stores nothing.
   */
  clearSlot(
    showId: string,
    layerId: string,
    slotIndex: number,
  ): Promise<AssignSlotResponse> {
    return this.#request('DELETE', this.#slotPath(showId, layerId, slotIndex));
  }

  #slotPath(showId: string, layerId: string, slotIndex: number): string {
    return (
      `/shows/${encodeURIComponent(showId)}` +
      `/layers/${encodeURIComponent(layerId)}` +
      `/slots/${encodeURIComponent(String(slotIndex))}`
    );
  }

  /**
   * Stacks an effect on a layer, or on one slot of it.
   *
   * Appended, never replacing: a chain is an order, and dropping a second
   * effect is not a request to undo the first (owner, 2026-09-23).
   *
   * No parameters are sent. Every one has a default in the effect's own
   * manifest, and copies written into the document stop following it.
   */
  addEffect(
    showId: string,
    layerId: string,
    slotIndex: number | null,
    typeId: string,
    version: number,
  ): Promise<AddEffectResponse> {
    const body: AddEffectRequest = { type_id: typeId, version };
    return this.#request('POST', `${this.#chainPath(showId, layerId, slotIndex)}`, body);
  }

  /** Takes one effect off a layer's chain, or off one slot's. */
  removeEffect(
    showId: string,
    layerId: string,
    slotIndex: number | null,
    effectInstanceId: string,
  ): Promise<AddEffectResponse> {
    return this.#request(
      'DELETE',
      `${this.#chainPath(showId, layerId, slotIndex)}/${encodeURIComponent(effectInstanceId)}`,
    );
  }

  /**
   * Sets the order of a layer's chain, or of one slot's.
   *
   * The whole order, not a move: see `OrderEffectsRequest`. Every instance in
   * the chain must appear exactly once, so this cannot take one off by leaving
   * it out — that is `removeEffect`, where it says so.
   */
  orderEffects(
    showId: string,
    layerId: string,
    slotIndex: number | null,
    order: readonly string[],
  ): Promise<OrderEffectsResponse> {
    const body: OrderEffectsRequest = { order: [...order] };
    return this.#request('PUT', `${this.#chainPath(showId, layerId, slotIndex)}/order`, body);
  }

  /**
   * Locks one effect or generator where it is, or unlocks it.
   *
   * An administrator's, and the only way a lock changes: a show sent whole
   * carries the locks it already has (`SetLockRequest`).
   */
  setLock(showId: string, request: SetLockRequest): Promise<SetLockResponse> {
    return this.#request('PUT', `/shows/${encodeURIComponent(showId)}/lock`, request);
  }

  /** The chain a slot has, or the layer's own when no slot is named. */
  #chainPath(showId: string, layerId: string, slotIndex: number | null): string {
    const base =
      slotIndex === null
        ? `/shows/${encodeURIComponent(showId)}/layers/${encodeURIComponent(layerId)}`
        : this.#slotPath(showId, layerId, slotIndex);
    return `${base}/effects`;
  }

  /**
   * Sets how many banks of twelve one layer's matrix shows.
   *
   * Stored on the layer rather than worked out from its slots, because a bank
   * is added in order to put something in it: a derived count would collapse
   * back to what is filled on the next redraw (owner, 2026-09-23).
   *
   * Asking for fewer than the layer already uses is refused rather than
   * hiding slots.
   */
  setBanks(showId: string, layerId: string, banks: number): Promise<AssignSlotResponse> {
    const body: SetBanksRequest = { banks };
    return this.#request(
      'PUT',
      `/shows/${encodeURIComponent(showId)}/layers/${encodeURIComponent(layerId)}/banks`,
      body,
    );
  }

  /**
   * Moves a slot to another pad of the same layer, with everything it carries.
   *
   * A pad dragged onto another (owner, 2026-10-21). A pad that is taken swaps
   * with the one that moves, and a layer showing either goes with what it was
   * showing.
   */
  moveSlot(showId: string, layerId: string, from: number, to: number): Promise<MoveSlotResponse> {
    const body: MoveSlotRequest = { to };
    return this.#request('POST', `${this.#slotPath(showId, layerId, from)}/move`, body);
  }

  /**
   * Changes how the clip in one slot runs.
   *
   * Loop or once, forwards or backwards, its own rate or the show's beat.
   * These are the document rather than live state — they describe the clip and
   * not what the show is doing with it — but they are decided while watching
   * the wall, so they go through the same kind of targeted edit a slot
   * assignment does and nothing restarts (owner, 2026-09-23).
   *
   * An absent field is left alone, so the one control the operator moved is
   * the only thing that travels.
   */
  setSlot(
    showId: string,
    layerId: string,
    slotIndex: number,
    change: SetSlotRequest,
  ): Promise<AssignSlotResponse> {
    return this.#request(
      'PUT',
      `${this.#slotPath(showId, layerId, slotIndex)}/settings`,
      change,
    );
  }

  /**
   * Puts a mapping on the wall without saving the show.
   *
   * Outputs and pieces together, because they refer to each other. It is the
   * same kind of targeted edit a slot assignment is: the document the server
   * holds is changed and installed under the running show, and nothing goes
   * back to its opening slot (owner, 2026-09-23).
   */
  setMapping(
    showId: string,
    outputs: OutputConfig[],
    pieces: Piece[],
  ): Promise<AssignSlotResponse> {
    const body: SetMappingRequest = { outputs, pieces };
    return this.#request('PUT', `/shows/${encodeURIComponent(showId)}/mapping`, body);
  }

  /**
   * Where a fixture fits in a universe, according to the server.
   *
   * The patch editor's own arithmetic would be a second opinion about the one
   * question this endpoint exists to settle, so the answer is taken as given.
   * `moving` is the entry being re-addressed, if this is a move rather than an
   * add: a fixture does not collide with where it already is, and without this
   * the server could not suggest the address a fixture already has.
   *
   * It answers from the show on disk rather than the one being edited
   * (`show_routes::suggest_address`), which is why the panel says so.
   */
  suggestAddress(
    showId: string,
    universe: number,
    footprint: number,
    moving?: string | null,
  ): Promise<AddressSuggestionResponse> {
    const query = new URLSearchParams({ universe: String(universe), footprint: String(footprint) });
    if (moving !== undefined && moving !== null) {
      query.set('moving', moving);
    }
    return this.#request(
      'GET',
      `/shows/${encodeURIComponent(showId)}/patch/address?${query.toString()}`,
    );
  }

  // --- the show ------------------------------------------------------------

  async selectSlot(layerId: string, slotIndex: number): Promise<void> {
    const body: SelectSlotRequest = { slot_index: slotIndex };
    await this.#request('POST', `/live/layers/${encodeURIComponent(layerId)}/slot`, body);
  }

  async updateLayer(layerId: string, change: UpdateLayerRequest): Promise<void> {
    await this.#request('PUT', `/live/layers/${encodeURIComponent(layerId)}`, change);
  }

  async updateMaster(change: UpdateMasterRequest): Promise<void> {
    await this.#request('PUT', '/live/master', change);
  }

  /**
   * Moves the live values on one effect instance, or puts them all back.
   *
   * 204 and nothing else on success, including when the request changed nothing
   * — a value the instance was already at, or a fader held still. The state
   * comes back over the socket as `EffectChanged`, and only when something
   * actually moved, which is why the caller must not wait for one.
   *
   * A refusal is the server quoting the manifest: `invalid_parameter` carries
   * the range or the type in the manifest's own words, and it is shown to the
   * operator as it arrived rather than paraphrased.
   */
  async updateEffect(effectInstanceId: string, change: UpdateEffectRequest): Promise<void> {
    await this.#request('PUT', `/live/effects/${encodeURIComponent(effectInstanceId)}`, change);
  }

  /**
   * Moves the live values on one generator instance.
   *
   * Everything [`updateEffect`](ServerApi#updateEffect) is, for the other kind
   * of module, down to the request body: the server publishes one route each
   * because the identities are different types, and one parameter model behind
   * them because an effect and a generator are both plugins with declared
   * parameters (owner, 2026-09-23). A console moving a generator's colour and
   * this moving it arrive at the renderer the same way.
   */
  async updateGenerator(
    generatorInstanceId: string,
    change: UpdateEffectRequest,
  ): Promise<void> {
    await this.#request(
      'PUT',
      `/live/generators/${encodeURIComponent(generatorInstanceId)}`,
      change,
    );
  }

  tap(): Promise<TapTempoResponse> {
    return this.#request('POST', '/live/tap');
  }

  async resetPhase(): Promise<void> {
    await this.#request('POST', '/live/phase/reset');
  }

  // --- media and previews --------------------------------------------------

  media(): Promise<MediaListResponse> {
    return this.#request('GET', '/media');
  }

  /**
   * Which shows reference one item.
   *
   * Asked before offering to delete, so an operator is warned rather than
   * silently breaking an archived show.
   */
  mediaUsage(mediaItemId: string): Promise<MediaUsageResponse> {
    return this.#request('GET', `/media/${encodeURIComponent(mediaItemId)}/usage`);
  }

  /** Deletes one item. The server refuses while a show uses it. */
  /**
   * Which DMX-over-IP protocols reach the show.
   *
   * Two switches rather than one: a venue's house desk may speak sACN while a
   * visiting console brings Art-Net, and switching one off has to leave the
   * other driving (owner, 2026-09-23).
   *
   * A server setting rather than a show's, because which protocols a
   * building speaks is a fact about the building.
   */
  async dmxProtocols(): Promise<DmxProtocols> {
    const status = await this.#request<StatusResponse>('GET', '/server/status');
    return status.dmx_protocols ?? { artnet: true, sacn: true };
  }

  /**
   * Sets which DMX-over-IP protocols reach the show.
   *
   * Switched off means it does not drive anything, not that it is not listened
   * for: the sockets stay open and the status keeps saying what is on the wire,
   * so an operator can still tell "I switched it off" from "the cable is out".
   */
  async setDmxProtocols(protocols: DmxProtocols): Promise<void> {
    await this.#request('PUT', '/server/dmx', protocols);
  }

  /**
   * Puts a test pattern on the walls, or takes it off.
   *
   * `highlighted` is the piece to draw as the chosen one and is only read in
   * `PIECES` mode. It travels with the mode because it is part of the same
   * question, and because two calls can be half-applied where one cannot.
   */
  async setTestPattern(
    mode: TestPatternMode,
    highlighted: string | null = null,
  ): Promise<void> {
    await this.#request('PUT', '/server/test-pattern', {
      mode,
      ...(highlighted === null ? {} : { highlighted }),
    });
  }

  // --- the show list's own structure -------------------------------------

  /**
   * The folders the show list is organised into.
   *
   * Its own call rather than a field on the list: `GET /shows` answers with
   * an array, and this API is additive within a major version.
   */
  async showFolders(): Promise<string[]> {
    const answer = await this.#request<ShowFoldersResponse>('GET', '/shows/folders');
    return answer.folders ?? [];
  }

  /** Makes a show folder, and every folder above it that is not there. */
  async createShowFolder(path: string): Promise<void> {
    const body: CreateFolderRequest = { path };
    await this.#request('POST', '/shows/folders', body);
  }

  /**
   * Deletes a show.
   *
   * The server closes it first when it is the open one, so the renderer is
   * never left holding a show that has gone.
   */
  async deleteShow(showId: string): Promise<void> {
    await this.#request('DELETE', `/shows/${encodeURIComponent(showId)}`);
  }

  /** Removes a show folder that holds nothing. */
  async deleteShowFolder(path: string): Promise<void> {
    await this.#request('DELETE', `/shows/folders?path=${encodeURIComponent(path)}`);
  }

  /** Renames a show folder, which moves everything under it. */
  async renameShowFolder(from: string, to: string): Promise<void> {
    const body: RenameFolderRequest = { from, to };
    await this.#request('PUT', '/shows/folders', body);
  }

  /** Files one show in a folder. The document is untouched. */
  async fileShow(showId: string, folder: string): Promise<void> {
    const body: FileShowRequest = { folder };
    await this.#request('PUT', `/shows/${encodeURIComponent(showId)}/folder`, body);
  }

  // --- the library's own structure -----------------------------------------

  /**
   * Makes a folder in the library.
   *
   * Every folder above it that is not there yet is made with it, because a tree
   * with a hole in it cannot be drawn. Making one that already exists changes
   * nothing and is not an error.
   *
   * Virtual, always: no file moves and no object key changes (owner,
   * 2026-09-23).
   */
  async createFolder(path: string): Promise<void> {
    const body: CreateFolderRequest = { path };
    await this.#request('POST', '/media/folders', body);
  }

  /** Renames a folder, which moves everything filed under it. */
  async renameFolder(from: string, to: string): Promise<void> {
    const body: RenameFolderRequest = { from, to };
    await this.#request('PUT', '/media/folders', body);
  }

  /**
   * Removes a folder that has nothing in it.
   *
   * The server refuses one that still holds an item or another folder, which is
   * the answer that cannot lose somebody's media because they tidied up.
   */
  async deleteFolder(path: string): Promise<void> {
    const query = new URLSearchParams({ path });
    await this.#request('DELETE', `/media/folders?${query.toString()}`);
  }

  /**
   * Renames one item, or moves it to another folder, or both.
   *
   * A field left out means "leave this alone", so a rename need not send a
   * folder and a move need not send a name. Neither touches the file.
   */
  fileMedia(
    mediaItemId: string,
    change: { displayName?: string; folder?: string },
  ): Promise<MediaItem> {
    const body: FileMediaRequest = {
      display_name: change.displayName ?? null,
      folder: change.folder ?? null,
    };
    return this.#request('PUT', `/media/${encodeURIComponent(mediaItemId)}`, body);
  }

  /**
   * A show as a package: the document and an inventory of what it needs.
   *
   * The media is not in it. A package is something an operator can read and a
   * server can validate before a byte of video moves, and the files follow
   * through the ordinary upload path — only the ones the receiving server does
   * not already hold, resumable like any other upload.
   */
  exportShow(showId: string): Promise<ShowExport> {
    return this.#request('GET', `/shows/${encodeURIComponent(showId)}/export`);
  }

  /** Puts a package on this server, and says what it had to relink. */
  importShow(pkg: ShowExport): Promise<ImportShowResponse> {
    return this.#request('POST', '/shows/import', pkg);
  }

  /**
   * One media item's content, as bytes.
   *
   * The whole file, streamed by the server and assembled here. One item per
   * request on purpose (owner, 2026-09-23): a server that built an archive of a
   * show's media would need room for a second copy of it, which is the cost
   * this avoids.
   */
  mediaContent(mediaItemId: string): Promise<Uint8Array> {
    return this.#bytes(`/media/${encodeURIComponent(mediaItemId)}/content`);
  }

  async deleteMedia(mediaItemId: string): Promise<void> {
    await this.#request('DELETE', `/media/${encodeURIComponent(mediaItemId)}`);
  }

  /**
   * One item's picture, as PNG bytes, or null when there is none.
   *
   * A 404 is ordinary rather than a fault: an item registered before thumbnails
   * existed has none, and so has one nothing could be read from. Everything else
   * is thrown, because a library that drew a name for an item whose picture the
   * server refused to serve would be hiding a refusal.
   */
  async mediaThumbnail(mediaItemId: string): Promise<Uint8Array | null> {
    try {
      return await this.#bytes(`/media/${encodeURIComponent(mediaItemId)}/thumbnail`);
    } catch (error) {
      if (error instanceof ApiFailure && error.status === 404) {
        return null;
      }
      throw error;
    }
  }

  /**
   * One clip's moving preview, as JPEG bytes, or null when there is none.
   *
   * Ten frames of the clip side by side, earliest first. A 404 is ordinary and
   * means there is nothing to animate: a still has none, and so has a clip
   * registered before this existed or one nothing could be read from.
   */
  async mediaStrip(mediaItemId: string): Promise<Uint8Array | null> {
    try {
      return await this.#bytes(`/media/${encodeURIComponent(mediaItemId)}/strip`);
    } catch (error) {
      if (error instanceof ApiFailure && error.status === 404) {
        return null;
      }
      throw error;
    }
  }

  // --- media ingest --------------------------------------------------------

  /** Says what is coming, and is told whether to send it at all. */
  createUpload(request: CreateUploadRequest): Promise<CreateUploadResponse> {
    return this.#request('POST', '/media/uploads', request);
  }

  /**
   * Sends one chunk, at the offset it belongs at.
   *
   * The offset is in the query string and the bytes are the body. The server
   * refuses a chunk that is not where the upload continues (`out_of_step`), and
   * answers with what it now holds — which is where the next one goes, and where
   * a resume starts.
   */
  appendChunk(uploadId: string, offset: number, bytes: Uint8Array): Promise<UploadSession> {
    const query = new URLSearchParams({ offset: String(offset) });
    return this.#sendBytes(`/media/uploads/${encodeURIComponent(uploadId)}?${query.toString()}`, bytes);
  }

  /** Asks the server to hash what arrived, check it and register it. */
  verifyUpload(uploadId: string): Promise<MediaItem> {
    return this.#request('POST', `/media/uploads/${encodeURIComponent(uploadId)}/verify`);
  }

  /** Abandons an upload, so the staged bytes go now rather than in an hour. */
  async cancelUpload(uploadId: string): Promise<void> {
    await this.#request('DELETE', `/media/uploads/${encodeURIComponent(uploadId)}`);
  }

  // --- previews ------------------------------------------------------------

  /**
   * Mints a permission to watch one surface.
   *
   * Good for that surface, for a minute, once. This needs a session and gives
   * back something far smaller than one, which is the whole point: the window
   * opens the preview socket, and a window has no business holding a credential
   * that could also stop the show.
   */
  streamTicket(surface: string): Promise<StreamTicketResponse> {
    return this.#request('POST', `/preview/stream/${encodeURIComponent(surface)}/ticket`);
  }

  /** The composition preview, as PNG bytes. */
  compositionPreview(): Promise<Uint8Array> {
    return this.#bytes('/preview/composition');
  }

  /** One output's preview, as PNG bytes. */
  outputPreview(outputId: string): Promise<Uint8Array> {
    return this.#bytes(`/preview/${encodeURIComponent(outputId)}`);
  }

  // --- the wire ------------------------------------------------------------

  async #request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const response = await this.#send(method, path, body, 'application/json');
    if (response.status === 204 || response.headers.get('content-length') === '0') {
      return undefined as T;
    }
    const text = await response.text();
    return text === '' ? (undefined as T) : (JSON.parse(text) as T);
  }

  async #bytes(path: string): Promise<Uint8Array> {
    const response = await this.#send('GET', path, undefined, 'image/png');
    return new Uint8Array(await response.arrayBuffer());
  }

  /**
   * Sends raw bytes, which is what an upload chunk is.
   *
   * Separate from `#send` because that one turns a body into JSON, and a chunk
   * of a ProRes file encoded as JSON would be a third larger and would arrive as
   * something the server has to decode before it can write it.
   */
  async #sendBytes<T>(path: string, bytes: Uint8Array): Promise<T> {
    const headers: Record<string, string> = {
      accept: 'application/json',
      'content-type': 'application/octet-stream',
    };
    if (this.#token !== null) {
      headers['authorization'] = `Bearer ${this.#token}`;
    }

    const response = await this.fetchImpl(`${this.origin}${API_PREFIX}${path}`, {
      method: 'PUT',
      headers,
      // Copied into a buffer of its own. `fetch` takes a view whose backing
      // store is an `ArrayBuffer` rather than any `ArrayBufferLike`, and this
      // says so by being one instead of asserting it. A memcpy of one chunk is
      // nothing beside sending it.
      body: bytes.slice().buffer,
    });
    if (!response.ok) {
      throw await failureFrom(response, `PUT ${path}`);
    }
    return JSON.parse(await response.text()) as T;
  }

  async #send(
    method: string,
    path: string,
    body: unknown,
    accept: string,
  ): Promise<Response> {
    const headers: Record<string, string> = { accept };
    if (this.#token !== null) {
      headers['authorization'] = `Bearer ${this.#token}`;
    }
    const init: RequestInit = { method, headers };
    if (body !== undefined) {
      headers['content-type'] = 'application/json';
      init.body = JSON.stringify(body);
    }

    const response = await this.fetchImpl(`${this.origin}${API_PREFIX}${path}`, init);
    if (!response.ok) {
      throw await failureFrom(response, `${method} ${path}`);
    }
    return response;
  }
}

/**
 * Reads the server's error shape, and copes when there isn't one.
 *
 * Every endpoint answers with an `ApiError`, but a proxy in the way answers
 * with whatever it likes, and "Unexpected token < in JSON" is not something to
 * put in front of an operator during a show.
 */
async function failureFrom(response: Response, call?: string): Promise<ApiFailure> {
  let code = `http_${response.status}`;
  // **Which call.** A bare “Not Found” in the fault line is a message an
  // operator cannot act on and nobody can diagnose from (owner, 2026-09-23):
  // it is the same four words whether a show is missing, a clip has been
  // deleted, or this client is newer than the server it is talking to and has
  // asked for a route that build does not publish. Saying which one turns it
  // into a fact.
  const said = response.statusText || `refused with ${response.status}`;
  let message = call === undefined ? said : `${said} — ${call}`;
  let fromServer = false;
  let details: ValidationError[] = [];

  try {
    const body = (await response.json()) as {
      code?: unknown;
      message?: unknown;
      details?: unknown;
    };
    if (typeof body.code === 'string') {
      code = body.code;
    }
    if (typeof body.message === 'string') {
      message = body.message;
      fromServer = true;
    }
    if (Array.isArray(body.details)) {
      details = body.details as ValidationError[];
    }
  } catch {
    // Not the server's error shape. The status and reason above stand.
  }

  // A 404 with no body at all is a route this server does not publish, which
  // is a different thing from a thing that is not there — and the one an
  // operator running a client newer than their server will meet.
  if (!fromServer && response.status === 404 && call !== undefined) {
    message = `this server has no ${call}. It may be an older build than this client.`;
  }

  return new ApiFailure(response.status, code, message, details);
}
