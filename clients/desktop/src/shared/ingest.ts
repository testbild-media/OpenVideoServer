// Getting a file off this machine and into the library.
//
//   probe → hash → declare → send, resumably → verify
//
// The order is not arbitrary. The server opens an uploaded video and reads it,
// refusing one whose dimensions disagree with what was declared and one it
// cannot decode at all (`check_video` in upload_routes.rs), so the declaration
// has to describe what the file *is* rather than what its name suggests. A
// `.mov` holding a PNG sequence frame is a still. Extensions are what somebody
// typed; the probe is what is in the file.
//
// What is no longer here: converting everything on the way in. The server plays
// what it is given, so H.264, HEVC, AV1, MPEG-2 and the rest go
// across as they are.
//
// The hash comes before the declaration because the declaration carries it, and
// because that is what lets the server answer "I already have that" before a
// byte crosses the network.
//
// Everything here is plain logic with the disk and the network handed in, so a
// queue that resumes, a probe that disagrees with an extension and a server that
// forgot an upload are tested rather than hoped for. What actually reads a file
// and what actually reaches a server are in the main process, where they belong.

import type {
  CreateUploadRequest,
  CreateUploadResponse,
  FrameRate,
  MediaItem,
  MediaKind,
  UploadSession,
} from '@ovs/schema';

import { ApiFailure } from './api.js';
import type { ProbedVideo } from './profile.js';
import {
  planFor,
  tagsFor,
  type ColourTags,
  type HevcBackend,
} from './encoder.js';
import { MAX_SIDE } from './limits.js';
import { faults, passesWithoutReadingPackets, profileOf, type OvsProfile } from './profile.js';

// --- what a file turned out to be -------------------------------------------

/** What a probe found in a file. Measurements, not guesses. */
export interface Probe {
  /** Container format, as the prober names it. Diagnostics only. */
  container: string;
  /** Codec name, as the prober names it. This is what the server checks. */
  codec: string;
  /** Pixel dimensions. The server checks these too. */
  width: number;
  height: number;
  /** Length in milliseconds, when the file says. */
  durationMs: number | null;
  /** Exact rate, when the file says. */
  frameRate: FrameRate | null;
  /** Whether the pixel format carries an alpha channel. */
  hasAlpha: boolean;
  /**
   * The same file as the playback contract sees it, when it could be read.
   *
   * Beside the fields above rather than instead of them: those are what the
   * *library* records about a clip and are declared to the server, and this
   * is what decides whether the server will take it at all. They overlap and
   * they are not the same question.
   *
   * `null` on a machine with no prober, where nothing is known and nothing
   * is guessed. Its own `keyframeInterval` is null until something pays for
   * the packet pass — see `profile.ts`.
   */
  video: ProbedVideo | null;
}

/** What came of trying to read a file. */
export type ProbeOutcome =
  | { kind: 'read'; probe: Probe }
  /** There is no prober on this machine. Nothing is guessed from that. */
  | { kind: 'unavailable' }
  /** There is one, and it could not make sense of the file. */
  | { kind: 'failed'; reason: string };

/**
 * What an operator declares by hand when this machine cannot probe.
 *
 * Every field the upload endpoint requires and no more. There is no default for
 * any of them: a declaration this client filled in would be refused by a server
 * that can read the file, and accepted by one that cannot — which is the worse
 * of the two, because then the library holds a lie.
 */
export interface HandDeclaration {
  kind: MediaKind;
  width: number;
  height: number;
  /** Required for a video, refused for a still (`check` in upload_routes.rs). */
  durationMs: number | null;
  frameRate: FrameRate | null;
  hasAlpha: boolean;
}

/** The file being declared, as this machine measured it. */
export interface FileFacts {
  /** The name the operator sees, which is the file's own. */
  name: string;
  byteSize: number;
  /** Lowercase hex SHA-256 of the file as it will be sent. */
  sha256: string;
}

/** A declaration, or the reason there is none. */
export type Declared =
  | { ok: true; request: CreateUploadRequest }
  | { ok: false; reason: string };

/**
 * Pixel formats that carry an alpha channel.
 *
 * Written out rather than matched with a pattern, because the pattern that
 * looks right catches `ya8` and misses `gbrap`, and because getting this wrong
 * on a ProRes file picks 422 over 4444 — the same codec, so the upload is
 * accepted, and the library then says a clip has no alpha when it does.
 *
 * Only the prefix is compared: FFmpeg spells the same format `yuva444p10le`,
 * `yuva444p12be` and several other ways, and every one of them has alpha.
 */
const ALPHA_PIXEL_FORMAT_PREFIXES = [
  'yuva',
  'rgba',
  'bgra',
  'argb',
  'abgr',
  'gbrap',
  'ya8',
  'ya16',
  'rgba64',
  'bgra64',
  'ayuv',
] as const;

/** Whether a pixel format FFmpeg named carries alpha. */
export function carriesAlpha(pixelFormat: string): boolean {
  const name = pixelFormat.trim().toLowerCase();
  return ALPHA_PIXEL_FORMAT_PREFIXES.some((prefix) => name.startsWith(prefix));
}

/**
 * Codecs whose files are usually one picture rather than a sequence.
 *
 * A still is verified by decoding the file as an image rather than by name
 * (`check_content` in upload_routes.rs), so this only has to decide which of
 * the server's two checks a file should face.
 */
const STILL_CODECS = new Set(['png', 'apng', 'mjpeg', 'jpeg2000', 'bmp', 'gif', 'tiff', 'webp']);

/**
 * How long a file of a still codec may last and still be a still.
 *
 * Motion JPEG and an animated GIF are sequences held in codecs whose usual use
 * is one picture, and the codec name cannot tell them apart from a photograph.
 * The length can. Two frames at any ordinary rate is longer than this.
 */
const A_SINGLE_PICTURE_MS = 100;

/**
 * Whether a file is a moving or a still image.
 *
 * The one thing about a file the declaration still has to classify, because it
 * decides which check the server runs: a still is decoded and measured, a video
 * is probed. Everything else about the file — including its codec, which used
 * to be the whole question — the server reads for itself.
 */
export function kindFor(probe: Probe): MediaKind {
  const name = probe.codec.trim().toLowerCase();
  if (!STILL_CODECS.has(name)) {
    return 'video';
  }
  return probe.durationMs !== null && probe.durationMs > A_SINGLE_PICTURE_MS ? 'video' : 'image';
}

/** The name to show in the library: the file's, without the extension. */
export function displayNameFor(fileName: string): string {
  const withoutPath = fileName.split(/[\\/]/).pop() ?? fileName;
  const cut = withoutPath.replace(/\.[^.]+$/, '').trim();
  return cut === '' ? withoutPath : cut;
}

/** The extension, lowercase and without the dot, or the empty string. */
function extensionOf(fileName: string): string {
  const match = /\.([^.\\/]+)$/.exec(fileName);
  return (match?.[1] ?? '').toLowerCase();
}

/**
 * Declares a file as what the probe says it is.
 *
 * The extension is not consulted anywhere in here, on purpose. It decides
 * nothing: an operator's `.mov` full of H.264 and their `.mp4` full of ProRes
 * are both ordinary, and the one the server accepts is the one declared by what
 * a prober read out of it.
 */
export function declareFromProbe(file: FileFacts, probe: Probe): Declared {
  const kind = kindFor(probe);
  const still = kind === 'image';
  if (probe.width === 0 || probe.height === 0) {
    return { ok: false, reason: 'This file does not say what size it is, so nothing can be shown from it.' };
  }
  if (!still && probe.durationMs === null) {
    return { ok: false, reason: 'This file does not say how long it is, and a video upload must declare its duration.' };
  }

  return {
    ok: true,
    request: request(file, {
      kind,
      width: probe.width,
      height: probe.height,
      // A still has neither, and the server refuses one that declares either
      // (`invalid_duration`). A file that reports both — a single-frame `image2`
      // stream does — is still a still.
      durationMs: still ? null : probe.durationMs,
      frameRate: still ? null : probe.frameRate,
      hasAlpha: probe.hasAlpha,
      container: probe.container,
      codec: probe.codec,
    }),
  };
}

/**
 * Declares a file as what the operator says it is.
 *
 * For a machine with no prober. Only two things are being asserted now — that
 * this is a video or a still, and whether it carries alpha — because the server
 * reads the rest out of the file itself. The container and codec sent are the
 * file's extension, which is the only thing about it this client knows without
 * opening it; the server records what its own probe found instead.
 */
export function declareByHand(file: FileFacts, hand: HandDeclaration): Declared {
  const still = hand.kind === 'image';

  if (hand.width <= 0 || hand.height <= 0) {
    return { ok: false, reason: 'A width and a height are needed: the server refuses media with neither.' };
  }
  if (!still && (hand.durationMs === null || hand.durationMs <= 0)) {
    return { ok: false, reason: 'A duration is needed: the server refuses a video that does not declare one.' };
  }

  return {
    ok: true,
    request: request(file, {
      kind: hand.kind,
      width: hand.width,
      height: hand.height,
      durationMs: still ? null : hand.durationMs,
      frameRate: still ? null : hand.frameRate,
      hasAlpha: hand.hasAlpha,
      container: extensionOf(file.name),
      codec: '',
    }),
  };
}

/** The one place a `CreateUploadRequest` is built, so both paths agree. */
function request(
  file: FileFacts,
  what: {
    kind: MediaKind;
    width: number;
    height: number;
    durationMs: number | null;
    frameRate: FrameRate | null;
    hasAlpha: boolean;
    container: string;
    codec: string;
  },
): CreateUploadRequest {
  const out: CreateUploadRequest = {
    display_name: displayNameFor(file.name),
    original_filename: file.name,
    sha256: file.sha256,
    byte_size: file.byteSize,
    kind: what.kind,
    dimensions: { width: what.width, height: what.height },
    has_alpha: what.hasAlpha,
    container: what.container,
    codec: what.codec,
  };
  // Left out entirely rather than sent as null: the endpoint skips them when
  // they are absent, and a still that carries either field is refused.
  if (what.durationMs !== null) {
    out.duration_ms = what.durationMs;
  }
  if (what.frameRate !== null) {
    out.frame_rate = what.frameRate;
  }
  return out;
}

// --- the queue ---------------------------------------------------------------

/** Where one file has got to. */
export type IngestStage =
  /** In the queue, waiting for the one in front of it. */
  | { at: 'waiting' }
  /** Being read, to find out what it is. */
  | { at: 'examining' }
  /**
   * Being re-encoded into something this server can play.
   *
   * `seconds` is how far into the file the encoder has got and `ofSeconds` how
   * long it is, so a bar can be honest about a job that takes minutes. Both are
   * the file's own numbers, not an estimate of how long the machine will take.
   */
  | { at: 'preparing'; seconds: number; ofSeconds: number | null; to: OvsProfile }
  /** Being hashed, which is the long part for a large file. */
  | { at: 'hashing'; hashed: number; byteSize: number }
  /** Asking the server whether it wants the bytes at all. */
  | { at: 'asking' }
  | { at: 'uploading'; sent: number; byteSize: number }
  /** Sent whole; the server is checking it before registering it. */
  | { at: 'verifying' }
  | { at: 'ready'; mediaItemId: string; alreadyHeld: boolean }
  /** Nothing here could say what the file is. The operator must. */
  | { at: 'undeclared'; reason: string }
  /** It stopped. `resumable` says whether carrying on is the thing to do. */
  | { at: 'stopped'; reason: string; resumable: boolean };

/** An upload the server has accepted and is waiting for the rest of. */
export interface UploadProgress {
  uploadId: string;
  mediaItemId: string;
  /** What the server last said it holds. A resume starts here, never at zero. */
  receivedBytes: number;
  /** The chunk size the server asked for. Its number, not one this client chose. */
  chunkSize: number;
}

/** One file on its way into the library. */
export interface QueueItem {
  /** This client's own handle for the row. Never sent anywhere. */
  id: string;
  /** Where the file is on this machine. Never sent anywhere either. */
  path: string;
  name: string;
  /** Size on disk, once this client has measured it. */
  byteSize: number | null;
  stage: IngestStage;
  upload: UploadProgress | null;
  /**
   * Where the prepared file is, when this one had to be re-encoded.
   *
   * Everything that reads bytes reads this instead of `path` once it is set:
   * the hash, the declaration and the upload are all about what will reach the
   * server. `path` stays the file the operator chose, because that is what they
   * recognise in the list.
   */
  prepared: string | null;
  /**
   * Stops a preparation that is running, when there is one.
   *
   * Not part of what a caller sees — `items` copies rows for the outside and
   * this is a live handle — but it lives on the row because that is what a
   * removal has in its hand. Without it, removing a file mid-preparation would
   * leave FFmpeg encoding to the end of a clip nobody wants, on a machine that
   * is running a show.
   */
  stop: AbortController | null;
  /** What was declared, once there is something to declare. */
  declaration: CreateUploadRequest | null;
  /** What the operator said the file is, on a machine with no prober. */
  hand: HandDeclaration | null;
  /**
   * Where in the library it is to be filed.
   *
   * The folder the operator had open when they dropped it, kept on the row so
   * that a file waiting in a queue is still filed where it was dropped after
   * they have walked off to another folder (owner, 2026-09-23). Empty is the
   * root, which is what a drop in All media means.
   */
  folder: string;
}

/**
 * What the queue needs from the world.
 *
 * Injected, so the tests need neither a disk nor a server. The implementations
 * live in the main process: it is the only half of this client with a
 * filesystem, and the only half that may reach a server.
 */
export interface IngestPorts {
  /** What is in the file, as far as this machine can tell. */
  probe(path: string): Promise<ProbeOutcome>;
  /** The file's size in bytes. */
  size(path: string): Promise<number>;
  /** SHA-256, lowercase hex, saying how far it has got as it goes. */
  hash(path: string, onProgress: (bytesHashed: number) => void): Promise<string>;
  /**
   * The codecs the server this client is signed in to can decode.
   *
   * `media_codecs` from `/server/capabilities`. Empty means the server did not
   * say, which is a build with no FFmpeg, and every file is then sent as it is
   * rather than converted on a guess.
   */
  playableCodecs(): Promise<readonly string[]>;
  /**
   * What this machine's FFmpeg can encode, by name.
   *
   * From `ffmpeg -encoders`, so a client running against a replaced binary
   * answers from what the binary has. An empty set means there is no FFmpeg
   * here at all, and nothing can be prepared.
   */
  encoders(): Promise<readonly string[]>;
  /**
   * The longest run of frames between two keyframes.
   *
   * A pass over the packets, which is the only place the answer is: a container
   * that declares a GOP size declares what an encoder was asked for. Asked only
   * when everything cheaper has already passed and this could still make the
   * file usable as it stands.
   *
   * `null` when it could not be measured, which is not the same as "fine".
   */
  keyframeInterval(path: string): Promise<number | null>;
  /**
   * Re-encodes a file into something this server can play.
   *
   * Given where the file is, what it should become and which backend to use,
   * it answers with where the prepared one is. The caller deletes that
   * afterwards, whatever happened: an ingest that leaves gigabytes of working
   * files behind is one nobody uses twice.
   *
   * `backend` is null for HAP, which has one encoder and no choice to make.
   */
  prepare(
    path: string,
    to: OvsProfile,
    backend: HevcBackend | null,
    rate: FrameRate,
    tags: ColourTags,
    onProgress: (seconds: number) => void,
    stop: AbortSignal,
  ): Promise<PrepareOutcome>;
  /** Removes a file this client made. Best effort, and never the operator's. */
  discard(path: string): Promise<void>;
  /** `length` bytes from `offset`, or fewer at the end of the file. */
  read(path: string, offset: number, length: number): Promise<Uint8Array>;
  createUpload(request: CreateUploadRequest): Promise<CreateUploadResponse>;
  append(uploadId: string, offset: number, bytes: Uint8Array): Promise<UploadSession>;
  verify(uploadId: string): Promise<MediaItem>;
  /** Tells the server an upload is not coming. Best effort. */
  cancel(uploadId: string): Promise<void>;
}

/**
 * The half of the ports that needs a filesystem rather than a server.
 *
 * Separated because the two halves come from different places: this one is the
 * same for the whole life of the process, and the other one only exists while
 * there is a session.
 */
export type IngestFiles = Pick<
  IngestPorts,
  | 'probe'
  | 'size'
  | 'hash'
  | 'read'
  | 'prepare'
  | 'discard'
  | 'encoders'
  | 'keyframeInterval'
>;

/** What came of preparing a file. */
export type PrepareOutcome =
  /** It worked, and the prepared file is here. */
  | { kind: 'prepared'; path: string }
  /** There is no encoder on this machine. Nothing is guessed from that. */
  | { kind: 'unavailable' }
  /** There is one, and it could not do it. */
  | { kind: 'failed'; reason: string }
  /** It was stopped part way, because nobody wants the file any more. */
  | { kind: 'stopped' };

/** A file an operator dropped or picked. */
export interface DroppedFile {
  path: string;
  name: string;
}

/**
 * Files on their way into the library, one at a time.
 *
 * One at a time on purpose. Somebody will drop twenty files at once and every
 * one of them wants the disk and the network; a machine that is running a show
 * has both spoken for already, and twenty ingests in parallel finish no sooner
 * than twenty in a row while making the show's own reads wait behind them.
 *
 * A failure stops one file and leaves the queue running. What stopped keeps
 * whatever the server said it holds, so carrying on means carrying on: the
 * endpoints take an offset (`PUT /media/uploads/{id}?offset=`), and a client
 * that started again from zero would be re-sending gigabytes the server has.
 */
export class IngestQueue {
  #items: QueueItem[] = [];
  #busy = false;
  #counter = 0;

  constructor(
    private readonly ports: IngestPorts,
    private readonly onChange: (items: QueueItem[]) => void,
  ) {}

  /** The queue as it stands, copied, so nothing outside can edit it. */
  get items(): QueueItem[] {
    return this.#items.map((item) => ({
      ...item,
      upload: item.upload === null ? null : { ...item.upload },
    }));
  }

  /**
   * Puts files in the queue and starts working through it.
   *
   * `folder` is where they are to be filed: the one the operator had open when
   * they dropped them. It is taken here rather than read when the upload is
   * declared, because by then they may be looking somewhere else.
   */
  add(files: readonly DroppedFile[], folder = ''): void {
    for (const file of files) {
      this.#counter += 1;
      this.#items.push({
        id: `file-${this.#counter}`,
        path: file.path,
        name: file.name,
        byteSize: null,
        stage: { at: 'waiting' },
        upload: null,
        prepared: null,
        stop: null,
        declaration: null,
        hand: null,
        folder,
      });
    }
    this.#report();
    this.#pump();
  }

  /**
   * Says by hand what a file is, on a machine that cannot probe.
   *
   * The file is hashed and sent as declared. If the server can read it and it is
   * not what was said, the upload is refused with the server's own reason, which
   * is the honest failure: better than a library that holds something nothing
   * can play.
   */
  declare(id: string, hand: HandDeclaration): void {
    const item = this.#find(id);
    if (item === null || item.stage.at !== 'undeclared') {
      return;
    }
    item.hand = hand;
    item.stage = { at: 'waiting' };
    this.#report();
    this.#pump();
  }

  /** Carries on with a file that stopped, from wherever the server got to. */
  resume(id: string): void {
    const item = this.#find(id);
    if (item === null || item.stage.at !== 'stopped') {
      return;
    }
    item.stage = { at: 'waiting' };
    this.#report();
    this.#pump();
  }

  /**
   * Takes a file out of the queue.
   *
   * An upload the server has accepted is cancelled, so the staged bytes go now
   * rather than in an hour when the server sweeps it, and so the item it would
   * have become says it was cancelled instead of sitting at "importing" forever.
   */
  remove(id: string): void {
    const item = this.#find(id);
    if (item === null) {
      return;
    }
    this.#items = this.#items.filter((other) => other.id !== id);
    item.stop?.abort();
    const upload = item.upload;
    if (upload !== null && item.stage.at !== 'ready') {
      void this.ports.cancel(upload.uploadId).catch(() => undefined);
    }
    this.#forgetWorkingFile(item);
    this.#report();
  }

  /** Forgets everything that has finished, one way or the other. */
  clearFinished(): void {
    const finished = (item: QueueItem) =>
      item.stage.at === 'ready' || (item.stage.at === 'stopped' && !item.stage.resumable);
    for (const item of this.#items.filter(finished)) {
      this.#forgetWorkingFile(item);
    }
    this.#items = this.#items.filter((item) => !finished(item));
    this.#report();
  }

  /**
   * Removes the prepared file a row was carrying, if it still has one.
   *
   * A row that stopped part way through an upload keeps its prepared file on
   * purpose, because resuming needs the bytes it was sending. When the row
   * itself goes there is nobody left to resume, and without this the file stays
   * — which for a prepared ProRes clip is gigabytes of somebody's disc, left
   * behind by an ingest they thought they had cleaned up.
   */
  #forgetWorkingFile(item: QueueItem): void {
    const working = item.prepared;
    if (working === null) {
      return;
    }
    item.prepared = null;
    void this.ports.discard(working).catch(() => undefined);
  }

  // --- the pump --------------------------------------------------------------

  #pump(): void {
    if (this.#busy) {
      return;
    }
    const next = this.#items.find((item) => item.stage.at === 'waiting');
    if (next === undefined) {
      return;
    }
    this.#busy = true;
    void this.#run(next).finally(() => {
      this.#busy = false;
      this.#pump();
    });
  }

  async #run(item: QueueItem): Promise<void> {
    try {
      if (item.declaration === null && !(await this.#examine(item))) {
        return;
      }
      await this.#send(item);
    } catch (error) {
      // Resumable when the server has already taken some of it: that is the
      // case where starting again would re-send what it holds.
      this.#set(item, { at: 'stopped', reason: reasonFor(error), resumable: item.upload !== null });
    } finally {
      // A working file goes whatever happened, unless the upload can still be
      // carried on — which needs the bytes it was sending. Nothing here ever
      // deletes the file the operator chose.
      const stage = item.stage;
      const keep = stage.at === 'stopped' && stage.resumable;
      if (item.prepared !== null && !keep) {
        const working = item.prepared;
        item.prepared = null;
        await this.ports.discard(working).catch(() => undefined);
      }
    }
  }

  /** The file whose bytes will reach the server. */
  #source(item: QueueItem): string {
    return item.prepared ?? item.path;
  }

  /**
   * Re-encodes a file into `to`, and says what the result turned out to be.
   *
   * Answers with a fresh probe of the prepared file rather than with the plan's
   * intention. What the encoder produced is what the server will check, and a
   * declaration written from the intention would be this client guessing about
   * its own output — which is the mistake the whole probe-first order exists to
   * avoid.
   *
   * `null` when it could not be done, having already said why.
   */
  async #prepare(
    item: QueueItem,
    from: Probe,
    to: OvsProfile,
    backend: HevcBackend | null,
  ): Promise<Probe | null> {
    const rate = from.frameRate ?? { numerator: 25, denominator: 1 };
    const ofSeconds = from.durationMs === null ? null : from.durationMs / 1000;
    const tags = tagsFor(from.video ?? {});

    this.#set(item, { at: 'preparing', seconds: 0, ofSeconds, to });
    const stop = new AbortController();
    item.stop = stop;
    const outcome = await this.ports
      .prepare(
        item.path,
        to,
        backend,
        rate,
        tags,
        (seconds) => {
          if (item.stage.at === 'preparing') {
            this.#set(item, { at: 'preparing', seconds, ofSeconds, to });
          }
        },
        stop.signal,
      )
      .finally(() => {
        item.stop = null;
      });

    if (outcome.kind === 'stopped') {
      // Asked to stop, so there is nothing to say about it: whoever asked is
      // the one who already knows.
      return null;
    }
    if (!this.#holds(item)) {
      // Cancelled while it was running. Whatever the encoder wrote is this
      // client's to clear up, and `#run` does it.
      if (outcome.kind === 'prepared') {
        item.prepared = outcome.path;
      }
      return null;
    }

    if (outcome.kind === 'unavailable') {
      this.#set(item, {
        at: 'undeclared',
        reason:
          'This file is not in a form the server plays, and nothing on this machine answered to “ffmpeg” to convert it. Install it so it is on the path, or point OVS_FFMPEG at it — then this file prepares itself. Otherwise convert it to HEVC or HAP elsewhere, or say what it is and it will be sent as that.',
      });
      return null;
    }
    if (outcome.kind === 'failed') {
      this.#set(item, { at: 'stopped', reason: outcome.reason, resumable: false });
      return null;
    }

    item.prepared = outcome.path;

    // Read again, because what the encoder produced is what the server checks.
    const read = await this.ports.probe(outcome.path);
    if (!this.#holds(item)) {
      return null;
    }
    if (read.kind !== 'read') {
      this.#set(item, {
        at: 'stopped',
        reason:
          read.kind === 'failed'
            ? `The prepared file could not be read back: ${read.reason}`
            : 'The prepared file could not be read back, because the prober went away between preparing it and checking it.',
        resumable: false,
      });
      return null;
    }
    return read.probe;
  }

  /**
   * Turns a file into something the server will take, or says why not.
   *
   * Answers the probe of whatever is going to be uploaded — the file itself
   * when it already conforms, and the prepared one when it did not.
   *
   * # The order of the questions
   *
   * Cheap before expensive, because the expensive one is a pass over the
   * packets. A file that already fails something a header answers is being
   * re-encoded whatever its keyframes are, so the interval is measured only
   * when it is the last thing that could still make the file usable as it
   * stands — which is exactly the case where it is worth paying for.
   *
   * # Why every encode is verified
   *
   * The owner's instruction of 2026-09-23: **do not silently produce a
   * different or low-quality result.** An encoder that wrote 4:2:2, or dropped
   * the colour tags, or ignored the keyframe interval has produced a file the
   * server will refuse — and finding that out here, with the source still on
   * disc, is the difference between trying the next backend and an operator
   * wondering why their upload failed at the far end.
   */
  async #conform(item: QueueItem, probe: Probe): Promise<Probe | null> {
    // Before anything else, and before any encoding: a picture the server
    // cannot hold as a texture cannot be prepared into one either, and finding
    // that out after minutes of encoding and a rejection from the far end is
    // the worst of the three places to find it out.
    if (probe.width > MAX_SIDE || probe.height > MAX_SIDE) {
      this.#set(item, {
        at: 'stopped',
        reason: `This picture is ${probe.width}x${probe.height}, and the server cannot hold a side longer than ${MAX_SIDE} pixels. It has to be made smaller before it can be prepared.`,
        resumable: false,
      });
      return null;
    }
    if (probe.width === 0 || probe.height === 0) {
      this.#set(item, {
        at: 'stopped',
        reason: 'This file has no picture size, so there is nothing to prepare.',
        resumable: false,
      });
      return null;
    }

    const video = probe.video;
    if (video === null) {
      // No prober, so nothing is known and nothing is guessed. The file goes as
      // it is and the server says what it thinks of it, which is a better
      // outcome than this client re-encoding on no evidence.
      return probe;
    }

    const measured = await this.#measure(item, video);
    if (measured === null) {
      return null;
    }

    const encoders = new Set(
      (await this.ports.encoders().catch(() => [])).map((name) => name.trim().toLowerCase()),
    );
    if (!this.#holds(item)) {
      return null;
    }

    const decided = planFor(measured, encoders);
    if (decided.kind === 'ready') {
      return { ...probe, video: measured };
    }
    if (decided.kind === 'unavailable') {
      this.#set(item, { at: 'stopped', reason: decided.reason, resumable: false });
      return null;
    }

    // HAP has one encoder and no choice to make; HEVC has the backends in
    // preference order, and each is tried until one produces a file that
    // actually conforms.
    const attempts: (HevcBackend | null)[] =
      decided.to === 'hap' ? [null] : decided.backends;
    let lastFault = '';

    for (const backend of attempts) {
      const prepared = await this.#prepare(item, probe, decided.to, backend);
      if (prepared === null) {
        // Stopped, cancelled, or failed outright. `#prepare` has already said.
        return null;
      }
      const check = await this.#measure(item, prepared.video);
      if (check === null) {
        return null;
      }
      const problems = faults(check, decided.to);
      if (problems.length === 0) {
        item.byteSize = await this.ports.size(this.#source(item));
        if (!this.#holds(item)) {
          return null;
        }
        return { ...prepared, video: check };
      }

      lastFault = problems
        .map((fault) => `${fault.what}: wanted ${fault.wanted}, got ${fault.found}`)
        .join('; ');
      // This backend produced something the server would refuse. Throw it away
      // and try the next one; the source is still where it was.
      const working = item.prepared;
      item.prepared = null;
      if (working !== null) {
        await this.ports.discard(working).catch(() => undefined);
      }
    }

    this.#set(item, {
      at: 'stopped',
      reason:
        lastFault === ''
          ? 'This file could not be prepared into anything the server will take.'
          : `This file was prepared, and what came out is still not something the server will take (${lastFault}). Nothing on this machine can produce it.`,
      resumable: false,
    });
    return null;
  }

  /**
   * Fills in the keyframe interval, when it is worth paying for.
   *
   * `null` means the item went away or the measurement failed and has been
   * reported. A reading whose interval stays null is one where everything
   * cheaper already failed, and the interval would change nothing.
   */
  async #measure(item: QueueItem, video: ProbedVideo | null): Promise<ProbedVideo | null> {
    if (video === null) {
      this.#set(item, {
        at: 'stopped',
        reason: 'The prepared file could not be read back closely enough to check it.',
        resumable: false,
      });
      return null;
    }
    if (!passesWithoutReadingPackets(video, profileOf(video))) {
      return video;
    }

    // Whatever is going to be uploaded, which is the prepared file once
    // there is one and the operator's own until then.
    const interval = await this.ports.keyframeInterval(this.#source(item));
    if (!this.#holds(item)) {
      return null;
    }
    return { ...video, keyframeInterval: interval };
  }
  /** Works out what the file is, and hashes it. Returns false when it cannot. */
  async #examine(item: QueueItem): Promise<boolean> {
    this.#set(item, { at: 'examining' });

    const byteSize = await this.ports.size(item.path);
    if (!this.#holds(item)) {
      return false;
    }
    item.byteSize = byteSize;

    // Settled before the hash, because the hash is the long part and a file
    // nothing here can describe should not be read end to end first.
    const hand = item.hand;
    let describe: (facts: FileFacts) => Declared;
    if (hand !== null) {
      describe = (facts) => declareByHand(facts, hand);
    } else {
      const outcome = await this.ports.probe(item.path);
      if (!this.#holds(item)) {
        return false;
      }
      if (outcome.kind === 'unavailable') {
        this.#set(item, {
          at: 'undeclared',
          reason:
            'Nothing on this machine answered to “ffprobe”, so nothing here can say what this file holds. Install it so it is on the path, or point OVS_FFPROBE at it. Otherwise say what the file is and it will be sent as that — the server reads it and refuses an upload that disagrees.',
        });
        return false;
      }
      if (outcome.kind === 'failed') {
        this.#set(item, { at: 'stopped', reason: outcome.reason, resumable: false });
        return false;
      }
      let probe = outcome.probe;

      // A file the server has no profile for is prepared rather than refused,
      // which is the other half of the rule. What is already suitable is
      // never touched: re-encoding an all-intra codec costs a generation of
      // quality and a gigabyte a minute to arrive back where it started.
      // Asked of the server rather than assumed, and asked per file rather
      // than once, because an operator can sign out of one server and into
      // another without this queue emptying.
      const readied = await this.#conform(item, probe);
      if (readied === null) {
        return false;
      }
      probe = readied;

      const settled = probe;
      describe = (facts) => declareFromProbe(facts, settled);
    }

    const bytes = item.byteSize ?? byteSize;
    this.#set(item, { at: 'hashing', hashed: 0, byteSize: bytes });
    const sha256 = await this.ports.hash(this.#source(item), (hashed) => {
      if (item.stage.at === 'hashing') {
        this.#set(item, { at: 'hashing', hashed, byteSize: bytes });
      }
    });
    if (!this.#holds(item)) {
      return false;
    }

    const declared = describe({ name: item.name, byteSize: item.byteSize ?? byteSize, sha256 });
    if (!declared.ok) {
      this.#set(item, { at: 'stopped', reason: declared.reason, resumable: false });
      return false;
    }

    item.declaration = declared.request;
    // Where it was dropped, which is not necessarily where the operator is
    // looking by the time the hash finishes. Left off the request entirely for
    // the root, because the field is skipped when it is empty.
    if (item.folder !== '') {
      item.declaration.folder = item.folder;
    }
    return true;
  }

  /** Sends the bytes, from wherever the server says it has got to. */
  async #send(item: QueueItem): Promise<void> {
    const declaration = item.declaration;
    if (declaration === null) {
      return;
    }

    if (item.upload === null) {
      this.#set(item, { at: 'asking' });
      const answer = await this.ports.createUpload(declaration);
      if (!this.#holds(item)) {
        return;
      }
      if (answer.result === 'already_present') {
        // The server holds this exact content already. Nothing crosses the
        // network, which is the whole point of hashing first.
        this.#set(item, { at: 'ready', mediaItemId: answer.media_item_id, alreadyHeld: true });
        return;
      }
      item.upload = {
        uploadId: answer.upload_id,
        mediaItemId: answer.media_item_id,
        receivedBytes: answer.received_bytes,
        chunkSize: answer.chunk_size,
      };
    }

    const total = declaration.byte_size;
    this.#set(item, { at: 'uploading', sent: item.upload.receivedBytes, byteSize: total });

    while (item.upload !== null && item.upload.receivedBytes < total) {
      const offset = item.upload.receivedBytes;
      const length = Math.min(item.upload.chunkSize, total - offset);
      const bytes = await this.ports.read(this.#source(item), offset, length);
      if (!this.#holds(item) || item.upload === null) {
        return;
      }

      let session: UploadSession;
      try {
        session = await this.ports.append(item.upload.uploadId, offset, bytes);
      } catch (error) {
        if (!isForgotten(error)) {
          throw error;
        }
        // The server swept the upload — nothing arrived for an hour
        // (`ABANDONED_AFTER`). The item it was for is still there and a show
        // may already point at it, so the upload is declared again *into that
        // item* rather than as a new one, which is what `media_item_id` on the
        // request is for.
        if (!(await this.#declareAgain(item, declaration))) {
          return;
        }
        continue;
      }

      if (!this.#holds(item) || item.upload === null) {
        return;
      }
      if (session.received_bytes <= offset) {
        throw new Error(
          `the server took no bytes at offset ${offset}, so this upload cannot go on`,
        );
      }
      item.upload.receivedBytes = session.received_bytes;
      this.#set(item, { at: 'uploading', sent: session.received_bytes, byteSize: total });
    }

    if (item.upload === null) {
      return;
    }
    this.#set(item, { at: 'verifying' });
    let registered: MediaItem;
    try {
      registered = await this.ports.verify(item.upload.uploadId);
    } catch (error) {
      if (error instanceof ApiFailure && error.status === 400) {
        // The server read the file and it is not what was declared. It has
        // discarded the staged bytes and marked the item failed with the
        // reason, so there is nothing left to carry on from — and sending the
        // same bytes under the same declaration would fail the same way.
        item.upload = null;
      }
      throw error;
    }
    if (!this.#holds(item)) {
      return;
    }
    item.upload = null;
    this.#set(item, { at: 'ready', mediaItemId: registered.id, alreadyHeld: false });
  }

  /** Asks for the upload again, into the item that is already waiting for it. */
  async #declareAgain(item: QueueItem, declaration: CreateUploadRequest): Promise<boolean> {
    const previous = item.upload;
    if (previous === null) {
      return false;
    }

    const answer = await this.ports.createUpload({
      ...declaration,
      media_item_id: previous.mediaItemId,
    });
    if (!this.#holds(item)) {
      return false;
    }
    if (answer.result === 'already_present') {
      this.#set(item, { at: 'ready', mediaItemId: answer.media_item_id, alreadyHeld: true });
      return false;
    }

    item.upload = {
      uploadId: answer.upload_id,
      mediaItemId: answer.media_item_id,
      receivedBytes: answer.received_bytes,
      chunkSize: answer.chunk_size,
    };
    this.#set(item, {
      at: 'uploading',
      sent: answer.received_bytes,
      byteSize: declaration.byte_size,
    });
    return true;
  }

  // --- bookkeeping -----------------------------------------------------------

  #find(id: string): QueueItem | null {
    return this.#items.find((item) => item.id === id) ?? null;
  }

  /** Whether this item is still in the queue, after something was awaited. */
  #holds(item: QueueItem): boolean {
    return this.#items.includes(item);
  }

  #set(item: QueueItem, stage: IngestStage): void {
    if (!this.#holds(item)) {
      return;
    }
    item.stage = stage;
    this.#report();
  }

  #report(): void {
    this.onChange(this.items);
  }
}

/** Whether the server has forgotten an upload this client still has. */
function isForgotten(error: unknown): boolean {
  return error instanceof ApiFailure && error.status === 404;
}

/** Whatever was thrown, as something to put in front of an operator. */
function reasonFor(error: unknown): string {
  if (error instanceof ApiFailure) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

// --- what the panel says -----------------------------------------------------

/**
 * A size in bytes, as an operator reads one.
 *
 * These numbers are this client's own: the size of a file on this machine, and
 * the `received_bytes` the server reported. Nothing here is estimated.
 */
export function bytesLabel(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  const units = ['kB', 'MB', 'GB', 'TB'] as const;
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(1)} ${units[unit]}`;
}

/** How far along one file is, between 0 and 1, or null when that has no meaning. */
export function fractionOf(stage: IngestStage): number | null {
  switch (stage.at) {
    case 'preparing':
      // Against the file's own length, which is known, rather than against how
      // long the machine will take, which is not.
      if (stage.ofSeconds === null || stage.ofSeconds <= 0) {
        return null;
      }
      return Math.min(stage.seconds / stage.ofSeconds, 1);
    case 'hashing':
      return stage.byteSize === 0 ? 1 : Math.min(stage.hashed / stage.byteSize, 1);
    case 'uploading':
      return stage.byteSize === 0 ? 1 : Math.min(stage.sent / stage.byteSize, 1);
    case 'ready':
      return 1;
    default:
      return null;
  }
}

/** What one row of the queue says, in the operator's words rather than the wire's. */
export function stageLine(item: QueueItem): string {
  const stage = item.stage;
  switch (stage.at) {
    case 'waiting':
      return 'Waiting';
    case 'examining':
      return 'Reading the file…';
    case 'preparing': {
      // The format, never the encoder. Which backend produced it is an
      // implementation detail and an operator never sees one.
      const as =
        stage.to === 'hap' ? 'HAP Alpha' : stage.to === 'hevc_10' ? 'HEVC 10-bit' : 'HEVC';
      if (stage.ofSeconds === null || stage.ofSeconds <= 0) {
        return `Preparing as ${as}…`;
      }
      const done = Math.round((stage.seconds / stage.ofSeconds) * 100);
      return `Preparing as ${as} — ${Math.min(done, 100)}%`;
    }
    case 'hashing':
      return `Processing — ${bytesLabel(stage.hashed)} of ${bytesLabel(stage.byteSize)}`;
    case 'asking':
      return 'Asking the server…';
    case 'uploading':
      return `Uploading — ${bytesLabel(stage.sent)} of ${bytesLabel(stage.byteSize)}`;
    case 'verifying':
      return 'Verifying…';
    case 'ready':
      return stage.alreadyHeld ? 'Ready — the server already held this' : 'Ready';
    case 'undeclared':
      // Short, because the reason is long and belongs beside the form that acts
      // on it rather than in the line that says where the file has got to.
      return 'Waiting to be told what it is';
    case 'stopped':
      return stage.resumable ? `Stopped — ${stage.reason}` : stage.reason;
  }
}
