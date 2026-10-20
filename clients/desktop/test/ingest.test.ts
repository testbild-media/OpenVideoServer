// Getting a file into the library: what it is declared as, and what happens
// when the sending stops half way.
//
// No disk and no server. The queue takes both as ports, so an upload that drops
// during a get-in, a server that has forgotten one and a file whose name says
// something its contents do not are all exercised here rather than found out
// during a show.

import type {
  CreateUploadRequest,
  CreateUploadResponse,
  MediaItem,
  UploadSession,
} from '@ovs/schema';
import { describe, expect, it } from 'vitest';

import { ApiFailure } from '../src/shared/api.js';
import {
  IngestQueue,
  bytesLabel,
  carriesAlpha,
  declareByHand,
  declareFromProbe,
  displayNameFor,
  fractionOf,
  kindFor,
  stageLine,
  type FileFacts,
  type IngestPorts,
  type Probe,
  type PrepareOutcome,
  type ProbeOutcome,
  type QueueItem,
} from '../src/shared/ingest.js';
import type { PreparedForm, Spending } from '../src/shared/encoder.js';
import type { ProbedVideo } from '../src/shared/profile.js';

/**
 * What a server built from this tree says it can decode.
 *
 * The queue asks before every file, and what it gets back decides whether
 * anything is converted at all. Most tests here want the ordinary
 * answer; the ones about preparing want a server that cannot play what is in
 * hand, which is what `plays` is for.
 */
const PLAYS_EVERYTHING = ['h264', 'hevc', 'av1', 'prores', 'hap', 'qtrle', 'png'];

/** A machine with both HEVC backends and a HAP encoder. */
const ENCODES_EVERYTHING = ['hevc_nvenc', 'hevc_mf', 'hap'];

/** A hash of the shape the endpoint insists on: 64 lowercase hex characters. */
const HASH = 'abcdef0123456789'.repeat(4);

const file = (name: string, byteSize = 10): FileFacts => ({ name, byteSize, sha256: HASH });

/**
 * The contract's reading of a file, which is what the queue decides from.
 *
 * Conforming by default, so that a test which cares about something else gets
 * a file the queue sends as it is. A test about preparing overrides the one
 * clause it is about.
 */
const reading = (over: Partial<ProbedVideo> = {}): ProbedVideo => ({
  codec: 'hevc',
  pixelFormat: 'yuv420p',
  hasAlpha: false,
  primaries: 'bt709',
  transfer: 'bt709',
  matrix: 'bt709',
  range: 'tv',
  frameRate: 60000 / 1001,
  nominalFrameRate: 60000 / 1001,
  keyframeInterval: 60,
  sampleAspect: [1, 1],
  interlaced: false,
  bitsPerSecond: 40_000_000,
  ...over,
});

/**
 * What a probe of a clip the server already takes reads like.
 *
 * It was a ProRes clip, when the server played anything. It is HEVC now,
 * because the point of most of these tests is what happens *after* a file is
 * accepted, and a file that needs preparing first would test the preparation
 * instead.
 */
const prores = (over: Partial<Probe> = {}): Probe => ({
  container: 'mov,mp4,m4a,3gp,3g2,mj2',
  codec: 'hevc',
  width: 1920,
  height: 1080,
  durationMs: 10_000,
  frameRate: { numerator: 60000, denominator: 1001 },
  hasAlpha: false,
  video: reading(),
  ...over,
});

/** A clip the server will not take: the right shape, the wrong codec. */
const needsPreparing = (over: Partial<Probe> = {}): Probe =>
  prores({ codec: 'vp9', video: reading({ codec: 'vp9' }), ...over });

describe('declaring what a file is', () => {
  it('declares a file as what the probe found, not as what its name suggests', () => {
    // The whole reason the probe comes first. The server opens the file and
    // refuses an upload whose size disagrees, so a declaration taken from
    // `.mp4` would be a failed import after the bytes had crossed.
    const declared = declareFromProbe(file('conference-opener.mp4'), prores());

    expect(declared.ok).toBe(true);
    expect(declared.ok && declared.request).toMatchObject({
      kind: 'video',
      codec: 'hevc',
      dimensions: { width: 1920, height: 1080 },
      duration_ms: 10_000,
      original_filename: 'conference-opener.mp4',
      display_name: 'conference-opener',
      sha256: HASH,
    });
  });

  it('declares a still as a still, whatever the file is called', () => {
    const declared = declareFromProbe(
      file('logo.mov'),
      prores({ codec: 'png', container: 'png_pipe', durationMs: 40, hasAlpha: true }),
    );

    expect(declared.ok && declared.request.kind).toBe('image');
    // The server refuses a still that declares either (`invalid_duration`), so
    // the fields are absent rather than null.
    expect(declared.ok && 'duration_ms' in declared.request).toBe(false);
    expect(declared.ok && 'frame_rate' in declared.request).toBe(false);
  });

  it('carries alpha through from the pixel format', () => {
    const declared = declareFromProbe(file('key.mov'), prores({ hasAlpha: true }));

    expect(declared.ok && declared.request.has_alpha).toBe(true);
  });

  it('tells a photograph from a sequence held in the same codec', () => {
    // Motion JPEG and an animated GIF are sequences in codecs whose usual use
    // is one picture, and the codec name cannot tell them apart. The length
    // can, and it decides which of the server's two checks the file faces.
    expect(kindFor(prores({ codec: 'png', durationMs: null }))).toBe('image');
    expect(kindFor(prores({ codec: 'png', durationMs: 40 }))).toBe('image');
    expect(kindFor(prores({ codec: 'mjpeg', durationMs: 8_000 }))).toBe('video');
    expect(kindFor(prores({ codec: 'gif', durationMs: 3_000 }))).toBe('video');
    expect(kindFor(prores({ codec: 'h264', durationMs: null }))).toBe('video');
  });

  it('reads alpha off the pixel formats that have one and not off the ones that do not', () => {
    for (const format of ['yuva444p10le', 'rgba', 'bgra', 'gbrap12le', 'ya8']) {
      expect(carriesAlpha(format), format).toBe(true);
    }
    for (const format of ['yuv422p10le', 'rgb24', 'gbrp', 'yuv420p', '']) {
      expect(carriesAlpha(format), format).toBe(false);
    }
  });

  it('declares a phone clip as what it is rather than refusing it', () => {
    // At the one line where it used to be refused.
    const declared = declareFromProbe(file('from-a-phone.mov'), prores({ codec: 'h264' }));

    expect(declared.ok).toBe(true);
    expect(declared.ok && declared.request).toMatchObject({ kind: 'video', codec: 'h264' });
  });

  it('refuses a video that does not say how long it is', () => {
    // `check` in upload_routes.rs rejects it, so it is caught before the hash
    // and the upload rather than after them.
    const declared = declareFromProbe(file('clip.mov'), prores({ durationMs: null }));

    expect(declared.ok).toBe(false);
    expect(!declared.ok && declared.reason).toContain('duration');
  });

  it('declares by hand what a machine with no prober cannot read', () => {
    const declared = declareByHand(file('key.mov'), {
      kind: 'video',
      width: 3840,
      height: 2160,
      durationMs: 5_000,
      frameRate: null,
      hasAlpha: true,
    });

    expect(declared.ok && declared.request).toMatchObject({
      kind: 'video',
      has_alpha: true,
      // Nothing is claimed about the codec: this machine could not read the
      // file, and the server reads it for itself.
      codec: '',
      container: 'mov',
      dimensions: { width: 3840, height: 2160 },
      duration_ms: 5_000,
    });
    expect(declared.ok && 'frame_rate' in declared.request).toBe(false);
  });

  it('refuses a hand declaration with nothing in the fields the server requires', () => {
    const declared = declareByHand(file('clip.mov'), {
      kind: 'video',
      width: 0,
      height: 0,
      durationMs: null,
      frameRate: null,
      hasAlpha: false,
    });

    expect(declared.ok).toBe(false);
  });

  it('names an item after the file without its extension', () => {
    expect(displayNameFor('conference-opener.mp4')).toBe('conference-opener');
    expect(displayNameFor('no-extension')).toBe('no-extension');
    expect(displayNameFor('.hidden')).toBe('.hidden');
  });
});

// --- the queue ---------------------------------------------------------------

/**
 * The upload endpoints, as far as the queue can tell.
 *
 * It keeps the rule the real one keeps: a chunk that is not where the upload
 * continues is refused. A fake that accepted any offset would let a resume that
 * started from the beginning pass this suite.
 */
class FakeServer {
  /** Every append that was attempted, including the ones that failed. */
  readonly attempts: number[] = [];
  readonly created: CreateUploadRequest[] = [];
  readonly verified: string[] = [];
  readonly cancelled: string[] = [];
  readonly reads: Array<{ offset: number; length: number }> = [];

  /** Offsets the next append at should fail at, once each. */
  readonly failAt = new Set<number>();
  /** Offsets at which the server has forgotten the upload, once each. */
  readonly forgetAt = new Set<number>();
  /** Offsets at which the disk where media is kept is full, once each. */
  readonly fullAt = new Set<number>();
  /** Whether there is no room for the file being declared. */
  noRoom: string | null = null;
  /** Whether the server already holds the content being declared. */
  alreadyPresent = false;
  chunkSize = 4;

  readonly #uploads = new Map<string, { received: number; total: number }>();
  #next = 0;

  createUpload = (request: CreateUploadRequest): Promise<CreateUploadResponse> => {
    this.created.push(request);
    if (this.noRoom !== null) {
      return Promise.reject(new ApiFailure(507, 'out_of_space', this.noRoom));
    }
    if (this.alreadyPresent) {
      return Promise.resolve({ result: 'already_present', media_item_id: 'ITEM-HELD' });
    }
    this.#next += 1;
    const upload_id = `UPLOAD-${this.#next}`;
    this.#uploads.set(upload_id, { received: 0, total: request.byte_size });
    return Promise.resolve({
      result: 'accepted',
      upload_id,
      media_item_id: 'ITEM-1',
      byte_size: request.byte_size,
      received_bytes: 0,
      chunk_size: this.chunkSize,
    });
  };

  append = (uploadId: string, offset: number, bytes: Uint8Array): Promise<UploadSession> => {
    this.attempts.push(offset);

    if (this.forgetAt.delete(offset)) {
      return Promise.reject(new ApiFailure(404, 'not_found', 'no such upload'));
    }
    if (this.failAt.delete(offset)) {
      return Promise.reject(new Error('the network went away'));
    }
    if (this.fullAt.delete(offset)) {
      return Promise.reject(
        new ApiFailure(507, 'out_of_space', 'could not write this chunk: the disk holding the media store is full'),
      );
    }

    const upload = this.#uploads.get(uploadId);
    if (upload === undefined) {
      return Promise.reject(new ApiFailure(404, 'not_found', 'no such upload'));
    }
    if (offset !== upload.received) {
      return Promise.reject(
        new ApiFailure(409, 'out_of_step', `this upload continues at byte ${upload.received}`),
      );
    }

    upload.received += bytes.byteLength;
    return Promise.resolve({
      upload_id: uploadId,
      media_item_id: 'ITEM-1',
      byte_size: upload.total,
      received_bytes: upload.received,
      chunk_size: this.chunkSize,
    });
  };

  /** Whether the server reads the file and finds it is not what was declared. */
  verificationFails = false;

  verify = (uploadId: string): Promise<MediaItem> => {
    this.verified.push(uploadId);
    if (this.verificationFails) {
      return Promise.reject(
        new ApiFailure(400, 'verification_failed', 'the video is 1280x720, not the 1920x1080 that was declared'),
      );
    }
    return Promise.resolve({
      id: 'ITEM-1',
      display_name: 'clip',
      original_filename: 'clip.mov',
      kind: 'video',
      state: 'ready',
      has_alpha: false,
      created_at: 1,
    });
  };

  cancel = (uploadId: string): Promise<void> => {
    this.cancelled.push(uploadId);
    return Promise.resolve();
  };
}

/** A queue wired to the fake server and a file this machine pretends to hold. */
function harness(
  options: {
    probe?: ProbeOutcome;
    /** What a second probe finds, once the file has been prepared. */
    afterPreparing?: ProbeOutcome;
    /**
     * What each preparation in turn reads back as, for a test about a second
     * encode. Past its end, `afterPreparing` again.
     */
    afterEachPreparing?: readonly ProbeOutcome[];
    byteSize?: number;
    prepare?: PrepareOutcome;
    /** What each backend in turn comes to, for a test about falling through. */
    prepareByBackend?: Readonly<Record<string, PrepareOutcome>>;
    /** Whether preparing hangs until it is told to stop. */
    prepareSlowly?: boolean;
    /** What the server says it can decode. Everything, unless a test narrows it. */
    plays?: readonly string[];
    /** What this machine can encode. Everything, unless a test narrows it. */
    encoders?: readonly string[];
    /** What the packet pass finds. One, which is every frame a keyframe. */
    keyframeInterval?: number | null;
  } = {},
) {
  const server = new FakeServer();
  const byteSize = options.byteSize ?? 10;
  const outcome: ProbeOutcome = options.probe ?? { kind: 'read', probe: prores() };

  /** Every file this machine was asked to prepare, and what it was asked for. */
  const prepared: Array<{ path: string; to: PreparedForm; backend: string | null }> = [];
  /** The size each of those was asked to come out at, in the same order. */
  const sizes: Array<{ width: number; height: number } | null> = [];
  /** And how each was to spend its bits, in the same order. */
  const spendings: Spending[] = [];
  /** Every working file this machine was asked to remove. */
  const discarded: string[] = [];
  let probes = 0;

  const ports: IngestPorts = {
    playableCodecs: () => Promise.resolve(options.plays ?? PLAYS_EVERYTHING),
    probe: () => {
      probes += 1;
      // The second read is of the prepared file, which is a different file and
      // may be a different answer.
      const nth = options.afterEachPreparing?.[probes - 2];
      if (probes > 1 && nth !== undefined) {
        return Promise.resolve(nth);
      }
      if (probes > 1 && options.afterPreparing !== undefined) {
        return Promise.resolve(options.afterPreparing);
      }
      return Promise.resolve(outcome);
    },
    encoders: () => Promise.resolve(options.encoders ?? ENCODES_EVERYTHING),
    keyframeInterval: () => Promise.resolve(options.keyframeInterval ?? 1),
    prepare: (path, to, backend, _rate, _tags, onProgress, stop, size, spending) => {
      prepared.push({ path, to, backend });
      sizes.push(size ?? null);
      spendings.push(spending);
      onProgress(1);
      onProgress(4);
      if (options.prepareSlowly === true) {
        // Never finishes on its own, so a test can take the file away from
        // under it and see what happens.
        return new Promise((resolve) => {
          stop.addEventListener('abort', () => resolve({ kind: 'stopped' }), { once: true });
        });
      }
      const named = options.prepareByBackend?.[backend ?? ''];
      return Promise.resolve(
        named ??
          options.prepare ?? { kind: 'prepared', path: '/tmp/ovs-prepare-x/clip.hevc.mp4' },
      );
    },
    discard: (path) => {
      discarded.push(path);
      return Promise.resolve();
    },
    size: () => Promise.resolve(byteSize),
    hash: (_path, onProgress) => {
      onProgress(byteSize);
      return Promise.resolve(HASH);
    },
    read: (_path, offset, length) => {
      server.reads.push({ offset, length });
      return Promise.resolve(new Uint8Array(Math.min(length, byteSize - offset)));
    },
    createUpload: server.createUpload,
    append: server.append,
    verify: server.verify,
    cancel: server.cancel,
  };

  const reports: QueueItem[][] = [];
  const queue = new IngestQueue(ports, (items) => reports.push(items));
  return { queue, server, reports, byteSize, prepared, sizes, spendings, discarded };
}

/** Runs the queue until nothing in it is still moving. */
async function settled(queue: IngestQueue): Promise<QueueItem[]> {
  for (let attempt = 0; attempt < 500; attempt += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
    const items = queue.items;
    const moving = items.some(
      (item) =>
        item.stage.at !== 'ready' &&
        item.stage.at !== 'stopped' &&
        item.stage.at !== 'undeclared',
    );
    if (!moving) {
      return items;
    }
  }
  throw new Error(`the queue never settled: ${JSON.stringify(queue.items)}`);
}

describe('the ingest queue', () => {
  it('probes, hashes, sends and verifies one file', async () => {
    const { queue, server } = harness();
    queue.add([{ path: '/media/clip.mov', name: 'clip.mov' }]);

    const items = await settled(queue);

    expect(server.attempts).toEqual([0, 4, 8]);
    expect(server.verified).toEqual(['UPLOAD-1']);
    expect(items[0]?.stage).toEqual({ at: 'ready', mediaItemId: 'ITEM-1', alreadyHeld: false });
  });

  it('resumes an upload from where the server got to rather than from the beginning', async () => {
    // The endpoints take an offset for exactly this. A client that started
    // again would re-send everything the server already holds, which on a show
    // file is the difference between a minute and an hour.
    const { queue, server } = harness();
    server.failAt.add(4);
    queue.add([{ path: '/media/clip.mov', name: 'clip.mov' }]);

    const stopped = await settled(queue);
    expect(stopped[0]?.stage).toMatchObject({ at: 'stopped', resumable: true });
    expect(stopped[0]?.upload?.receivedBytes).toBe(4);

    queue.resume(stopped[0]!.id);
    const items = await settled(queue);

    // The failed attempt at 4, then 4 again, then 8. Never a second 0.
    expect(server.attempts).toEqual([0, 4, 4, 8]);
    expect(server.attempts.filter((offset) => offset === 0)).toHaveLength(1);
    expect(server.reads.map((read) => read.offset)).toEqual([0, 4, 4, 8]);
    // One declaration: resuming is not re-importing, so the item the server
    // made is the item the content ends up in.
    expect(server.created).toHaveLength(1);
    expect(items[0]?.stage).toMatchObject({ at: 'ready' });
  });

  it('sends nothing at all when the server already holds the content', async () => {
    // What hashing before declaring buys: the operator who dropped the same
    // show file twice waits for a hash rather than for an upload.
    const { queue, server } = harness();
    server.alreadyPresent = true;
    queue.add([{ path: '/media/clip.mov', name: 'clip.mov' }]);

    const items = await settled(queue);

    expect(server.attempts).toEqual([]);
    expect(items[0]?.stage).toEqual({ at: 'ready', mediaItemId: 'ITEM-HELD', alreadyHeld: true });
  });

  it('declares the upload again into the same item when the server has forgotten it', async () => {
    // A server sweeps an upload nothing has arrived for in an hour, and the
    // item it was for is still there with a show possibly pointing at it. A
    // second item would leave that show waiting for content that never comes.
    const { queue, server } = harness();
    server.forgetAt.add(4);
    queue.add([{ path: '/media/clip.mov', name: 'clip.mov' }]);

    const items = await settled(queue);

    expect(server.created).toHaveLength(2);
    expect(server.created[1]?.media_item_id).toBe('ITEM-1');
    expect(items[0]?.stage).toMatchObject({ at: 'ready' });
  });

  it('asks the operator to say what a file is when there is no prober', async () => {
    // Nothing is guessed from the extension. A wrong declaration is a refused
    // upload now that the server reads the file, which is the honest failure.
    const { queue, server } = harness({ probe: { kind: 'unavailable' } });
    queue.add([{ path: '/media/clip.mov', name: 'clip.mov' }]);

    const waiting = await settled(queue);
    expect(waiting[0]?.stage.at).toBe('undeclared');
    expect(server.created).toEqual([]);

    queue.declare(waiting[0]!.id, {
      kind: 'video',
      width: 1920,
      height: 1080,
      durationMs: 10_000,
      frameRate: null,
      hasAlpha: false,
    });
    const items = await settled(queue);

    expect(server.created[0]).toMatchObject({ kind: 'video', codec: '' });
    expect(items[0]?.stage).toMatchObject({ at: 'ready' });
  });

  it('sends a long-GOP clip as it is, because the server plays it', async () => {
    // What this whole change is for. A folder of H.264 used to be a folder of
    // re-encodes; it is now a folder of uploads.
    const { queue, server, prepared } = harness({
      probe: {
        kind: 'read',
        probe: {
          container: 'mov,mp4',
          codec: 'h264',
          width: 1920,
          height: 1080,
          durationMs: 8_000,
          frameRate: { numerator: 60, denominator: 1 },
          hasAlpha: false,
          video: null,
        },
      },
    });
    queue.add([{ path: '/media/opener.mp4', name: 'opener.mp4' }]);

    const items = await settled(queue);

    expect(prepared).toEqual([]);
    expect(server.created[0]).toMatchObject({ kind: 'video', codec: 'h264' });
    expect(items[0]?.stage).toMatchObject({ at: 'ready' });
  });

  it('prepares a file this server cannot decode, and sends what came out', async () => {
    // The ordinary case since the owner narrowed the policy on 2026-09-23: a
    // file that is not one of the three is prepared here, before a byte crosses
    // the network.
    const { queue, server, prepared, discarded } = harness({
      probe: {
        kind: 'read',
        probe: needsPreparing({ container: 'mov,mp4', width: 1920, height: 1080 }),
      },
      afterPreparing: { kind: 'read', probe: prores() },
    });
    queue.add([{ path: '/media/opener.mp4', name: 'opener.mp4' }]);

    const items = await settled(queue);

    // The first backend in preference order, chosen without asking anybody.
    expect(prepared).toEqual([
      { path: '/media/opener.mp4', to: 'hevc_8', backend: 'hevc_nvenc' },
    ]);
    expect(items[0]?.stage).toMatchObject({ at: 'ready' });
    // Declared as what the encoder produced, read back off the prepared file —
    // not as what the plan intended. The server checks the file, so a
    // declaration written from an intention is this client guessing about its
    // own output.
    expect(server.created[0]).toMatchObject({ kind: 'video', codec: 'hevc' });
    // And the working file is gone.
    expect(discarded).toEqual(['/tmp/ovs-prepare-x/clip.hevc.mp4']);
  });

  it('prepares a picture whose pixels are not square at the width it is shown at', async () => {
    // The sample aspect is in the probe and the size is decided in the queue;
    // the encoder only squares what it is given. Leaving the aspect out of the
    // queue's question would square a PAL widescreen clip by squashing it.
    const { queue, sizes } = harness({
      probe: {
        kind: 'read',
        probe: needsPreparing({
          width: 720,
          height: 576,
          video: reading({ codec: 'vp9', sampleAspect: [64, 45] }),
        }),
      },
      afterPreparing: { kind: 'read', probe: prores() },
    });
    queue.add([{ path: '/media/pal.mov', name: 'pal.mov' }]);

    await settled(queue);

    expect(sizes).toEqual([{ width: 1024, height: 576 }]);
  });

  it('prepares a file whose only fault is its sample aspect', async () => {
    // The owner's ProRes, 2026-10-20, as the contract read it after Quick Sync
    // had re-encoded it: everything right but 112301:112320. Not scaled — a
    // third of a pixel is not worth resampling every frame for.
    const { queue, prepared, sizes } = harness({
      probe: { kind: 'read', probe: prores({ video: reading({ sampleAspect: [112301, 112320] }) }) },
      afterPreparing: { kind: 'read', probe: prores() },
    });
    queue.add([{ path: '/media/blockbuster.mov', name: 'blockbuster.mov' }]);

    const items = await settled(queue);

    expect(prepared.map((attempt) => attempt.to)).toEqual(['hevc_8']);
    expect(sizes).toEqual([null]);
    expect(items[0]?.stage).toMatchObject({ at: 'ready' });
  });

  it('stops a preparation when the file is taken out of the queue', async () => {
    // FFmpeg on a clip of any length is minutes of a core. An operator who
    // removes a file has decided they do not want it, and a machine running a
    // show has better uses for that core than finishing it.
    const { queue } = harness({
      probe: {
        kind: 'read',
        probe: needsPreparing({ container: 'mov,mp4', durationMs: 600_000 }),
      },
      prepareSlowly: true,
    });
    queue.add([{ path: '/media/long.mp4', name: 'long.mp4' }]);

    // Let it get as far as preparing, which is where it will sit.
    for (let attempt = 0; attempt < 50; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 0));
      if (queue.items[0]?.stage.at === 'preparing') {
        break;
      }
    }
    expect(queue.items[0]?.stage.at).toBe('preparing');

    const id = queue.items[0]!.id;
    queue.remove(id);

    // The queue lets go at once, and the encoder is told to stop rather than
    // being left to finish something nobody will look at.
    expect(queue.items).toEqual([]);
    const items = await settled(queue);
    expect(items).toEqual([]);
  });

  it('never prepares a file the server can already play', async () => {
    // Every re-encode is a generation of quality gone for nothing and a
    // gigabyte a minute of disc to arrive back where it started.
    const { queue, prepared, discarded } = harness();
    queue.add([{ path: '/media/clip.mov', name: 'clip.mov' }]);

    await settled(queue);

    expect(prepared).toEqual([]);
    expect(discarded).toEqual([]);
  });

  it('keeps alpha by preparing into the format that has it', async () => {
    const { queue, prepared } = harness({
      probe: {
        kind: 'read',
        probe: needsPreparing({
          container: 'mov',
          codec: 'qtrle',
          durationMs: 4_000,
          hasAlpha: true,
          video: reading({ codec: 'qtrle', pixelFormat: 'argb', hasAlpha: true }),
        }),
      },
      afterPreparing: { kind: 'read', probe: prores() },
    });
    queue.add([{ path: '/media/logo.mov', name: 'logo.mov' }]);

    await settled(queue);

    expect(prepared[0]?.to).toBe('hap');
  });

  it('sends a PNG or a JPEG exactly as it is, alpha or not', async () => {
    // The owner, 2026-10-20: "PNGs und Bilder sollten ja so durchgehen ohne
    // Konvertierung". A PNG with alpha used to come out as HAP.
    for (const codec of ['png', 'mjpeg']) {
      const { queue, prepared, server } = harness({
        probe: {
          kind: 'read',
          probe: prores({
            codec,
            container: 'image2',
            durationMs: 40,
            video: reading({ codec, pixelFormat: 'rgba', hasAlpha: true }),
          }),
        },
      });
      queue.add([{ path: '/media/logo.png', name: 'logo.png' }]);

      const items = await settled(queue);

      expect(prepared, codec).toEqual([]);
      expect(server.created[0], codec).toMatchObject({ kind: 'image' });
      expect(items[0]?.stage.at, codec).toBe('ready');
    }
  });

  it('turns a picture the server cannot read into a PNG, and nothing else', async () => {
    const { queue, prepared, server } = harness({
      probe: {
        kind: 'read',
        probe: prores({
          codec: 'tiff',
          container: 'tiff_pipe',
          durationMs: null,
          video: reading({ codec: 'tiff', pixelFormat: 'rgb48le' }),
        }),
      },
      afterPreparing: {
        kind: 'read',
        probe: prores({
          codec: 'png',
          container: 'png_pipe',
          durationMs: null,
          video: reading({ codec: 'png', pixelFormat: 'rgb48be' }),
        }),
      },
    });
    queue.add([{ path: '/media/plan.tif', name: 'plan.tif' }]);

    const items = await settled(queue);

    expect(prepared.map((attempt) => [attempt.to, attempt.backend])).toEqual([['png', null]]);
    expect(server.created[0]).toMatchObject({ kind: 'image', original_filename: 'plan.tif' });
    expect(items[0]?.stage.at).toBe('ready');
  });

  it('prepares a ten-bit clip as eight-bit and sends it', async () => {
    // The owner's three clips of 2026-10-20, each encoded to the end and then
    // refused before it was sent, because the target was ten-bit and the
    // encoder that finished writes eight.
    const { queue, prepared, server } = harness({
      probe: {
        kind: 'read',
        probe: needsPreparing({
          codec: 'prores',
          video: reading({ codec: 'prores', pixelFormat: 'yuv422p10le' }),
        }),
      },
      afterPreparing: { kind: 'read', probe: prores() },
      encoders: ['hevc_nvenc', 'libkvazaar'],
      prepareByBackend: {
        hevc_nvenc: { kind: 'failed', reason: 'No capable devices found' },
        libkvazaar: { kind: 'prepared', path: '/tmp/ovs-prepare-x/clip.hevc.mp4' },
      },
    });
    queue.add([{ path: '/media/visual.mov', name: 'visual.mov' }]);

    const items = await settled(queue);

    expect(prepared.map((attempt) => attempt.to)).toEqual(['hevc_8', 'hevc_8']);
    expect(server.created[0]).toMatchObject({ kind: 'video', codec: 'hevc' });
    expect(items[0]?.stage.at).toBe('ready');
  });

  it('says so when there is nothing on this machine to prepare with', async () => {
    // A machine with ffprobe and no ffmpeg is an ordinary machine, and telling
    // somebody to prepare it elsewhere is more use than an exit code.
    const { queue } = harness({
      probe: {
        kind: 'read',
        probe: needsPreparing({ container: 'mov,mp4' }),
      },
      prepare: { kind: 'unavailable' },
    });
    queue.add([{ path: '/media/opener.mp4', name: 'opener.mp4' }]);

    const items = await settled(queue);

    expect(items[0]?.stage.at).toBe('undeclared');
    expect(items[0]?.stage.at === 'undeclared' && items[0].stage.reason).toContain('ffmpeg');
  });

  it('stops with the encoder\u2019s own words when preparing fails', async () => {
    const { queue, discarded } = harness({
      probe: {
        kind: 'read',
        probe: needsPreparing({ container: 'mov,mp4' }),
      },
      prepare: { kind: 'failed', reason: 'Preparing this file failed: Invalid data found' },
    });
    queue.add([{ path: '/media/opener.mp4', name: 'opener.mp4' }]);

    const items = await settled(queue);

    expect(items[0]?.stage.at).toBe('stopped');
    const stage = items[0]?.stage;
    expect(stage?.at === 'stopped' && stage.reason).toContain(
      'Preparing this file failed: Invalid data found',
    );
    // Nothing was made, so nothing is removed.
    expect(discarded).toEqual([]);
  });

  it('tries the next backend when one fails outright', async () => {
    // **The behaviour that makes listing an encoder safe.** `ffmpeg -encoders`
    // names `hevc_qsv` and `hevc_amf` on any FFmpeg built with them, silicon
    // or no silicon, so the first backend failing to open a device is an
    // ordinary evening rather than a fault. It used to end the file.
    const { queue, prepared } = harness({
      probe: { kind: 'read', probe: needsPreparing({ container: 'mov,mp4' }) },
      // What the second backend wrote, read back: this one the server takes.
      afterPreparing: { kind: 'read', probe: prores({ container: 'mov,mp4' }) },
      encoders: ['hevc_nvenc', 'hevc_mf'],
      prepareByBackend: {
        hevc_nvenc: { kind: 'failed', reason: 'Cannot load nvcuda.dll' },
        hevc_mf: { kind: 'prepared', path: '/tmp/ovs-prepare-x/clip.hevc.mp4' },
      },
    });
    queue.add([{ path: '/media/opener.mp4', name: 'opener.mp4' }]);

    const items = await settled(queue);

    expect(prepared.map((attempt) => attempt.backend)).toEqual(['hevc_nvenc', 'hevc_mf']);
    expect(items[0]?.stage.at).not.toBe('stopped');
  });

  describe('a clip that comes out over the bitrate ceiling', () => {
    // The owner's 1.4 GB clip, 2026-10-20: Quick Sync made it 73.5 Mbit/s and
    // Kvazaar 65.0, at the quality every clip is asked for, and the server
    // takes sixty. Neither takes a peak in its quality mode.
    const tooMuch = (bitsPerSecond: number): ProbeOutcome => ({
      kind: 'read',
      probe: prores({ video: reading({ bitsPerSecond }) }),
    });
    const fits: ProbeOutcome = { kind: 'read', probe: prores() };

    it('is encoded again by the same backend, at an average under the ceiling', async () => {
      const { queue, prepared, spendings, reports, discarded } = harness({
        probe: { kind: 'read', probe: needsPreparing() },
        afterEachPreparing: [tooMuch(73_500_000), fits],
        encoders: ['hevc_qsv', 'libkvazaar'],
      });
      queue.add([{ path: '/media/gold.mov', name: 'gold.mov' }]);

      const items = await settled(queue);

      expect(prepared.map((attempt) => attempt.backend)).toEqual(['hevc_qsv', 'hevc_qsv']);
      expect(spendings).toEqual(['quality', 'capped']);
      expect(items[0]?.stage.at).toBe('ready');
      // Both working files are removed: the first before the second is made,
      // the second once it has been sent.
      expect(discarded).toHaveLength(2);
      // And the row says why its bar started again.
      const second = reports
        .flat()
        .find((item) => item.stage.at === 'preparing' && item.stage.capped === true);
      expect(second).toBeDefined();
      expect(stageLine(second!)).toMatch(/^Preparing as HEVC again, at a lower bitrate/);
    });

    it('is not encoded again for a fault a bitrate will not fix', async () => {
      // Over the ceiling *and* untagged: a capped encode by the same backend
      // would be untagged too, so the next backend is the one to ask.
      const { queue, prepared, spendings } = harness({
        probe: { kind: 'read', probe: needsPreparing() },
        afterEachPreparing: [
          {
            kind: 'read',
            probe: prores({ video: reading({ bitsPerSecond: 73_500_000, primaries: '' }) }),
          },
          fits,
        ],
        encoders: ['hevc_qsv', 'libkvazaar'],
      });
      queue.add([{ path: '/media/gold.mov', name: 'gold.mov' }]);

      const items = await settled(queue);

      expect(prepared.map((attempt) => attempt.backend)).toEqual(['hevc_qsv', 'libkvazaar']);
      expect(spendings).toEqual(['quality', 'quality']);
      expect(items[0]?.stage.at).toBe('ready');
    });

    it('goes on to the next backend when even the capped encode is over', async () => {
      const { queue, prepared, spendings } = harness({
        probe: { kind: 'read', probe: needsPreparing() },
        afterEachPreparing: [tooMuch(73_500_000), tooMuch(61_000_000), fits],
        encoders: ['hevc_qsv', 'libkvazaar'],
      });
      queue.add([{ path: '/media/gold.mov', name: 'gold.mov' }]);

      const items = await settled(queue);

      expect(prepared.map((attempt) => attempt.backend)).toEqual([
        'hevc_qsv',
        'hevc_qsv',
        'libkvazaar',
      ]);
      expect(spendings).toEqual(['quality', 'capped', 'quality']);
      expect(items[0]?.stage.at).toBe('ready');
    });

    it('says what both encodes of every backend came to when none fits', async () => {
      const { queue } = harness({
        probe: { kind: 'read', probe: needsPreparing() },
        afterPreparing: tooMuch(65_000_000),
        encoders: ['hevc_qsv', 'libkvazaar'],
      });
      queue.add([{ path: '/media/gold.mov', name: 'gold.mov' }]);

      const items = await settled(queue);

      const stage = items[0]?.stage;
      const reason = stage?.at === 'stopped' ? stage.reason : '';
      expect(reason).toContain('hevc_qsv made a file');
      expect(reason).toContain('hevc_qsv at a capped bitrate made a file');
      expect(reason).toContain('libkvazaar at a capped bitrate made a file');
    });
  });

  it('says what every backend said when every backend fails', async () => {
    // **Every one, not the last one.** On 2026-10-20 the last one was the
    // software encoder reporting eight bits, and why each hardware encoder
    // before it had failed \u2014 the half that said what was wrong \u2014 was gone.
    const { queue, prepared } = harness({
      probe: { kind: 'read', probe: needsPreparing({ container: 'mov,mp4' }) },
      afterPreparing: {
        kind: 'read',
        probe: prores({ container: 'mov,mp4', video: reading({ interlaced: true }) }),
      },
      encoders: ['hevc_nvenc', 'libkvazaar'],
      prepareByBackend: {
        hevc_nvenc: { kind: 'failed', reason: 'OpenEncodeSessionEx failed: out of memory' },
        libkvazaar: { kind: 'prepared', path: '/tmp/ovs-prepare-x/clip.hevc.mp4' },
      },
    });
    queue.add([{ path: '/media/opener.mp4', name: 'opener.mp4' }]);

    const items = await settled(queue);

    expect(prepared).toHaveLength(2);
    const stage = items[0]?.stage;
    expect(stage?.at).toBe('stopped');
    const reason = stage?.at === 'stopped' ? stage.reason : '';
    expect(reason).toContain('hevc_nvenc: OpenEncodeSessionEx failed: out of memory');
    expect(reason).toContain('libkvazaar made a file the server would refuse');
    expect(reason).toContain('scan: wanted progressive, got interlaced');
  });

  it('refuses a picture too large for the server before preparing it', async () => {
    // Refused here rather than after minutes of encoding and a rejection from
    // the far end.
    const { queue, prepared } = harness({
      probe: {
        kind: 'read',
        probe: needsPreparing({ container: 'mov,mp4', width: 16_384, height: 8_640 }),
      },
    });
    queue.add([{ path: '/media/huge.mp4', name: 'huge.mp4' }]);

    const items = await settled(queue);

    expect(prepared).toEqual([]);
    expect(items[0]?.stage.at).toBe('stopped');
  });

  it('shows a disk with no room for the file in the server\u2019s own words', async () => {
    // A 507 before the first byte: the server measured the file against what is
    // free and would not start. There is nothing to resume — no upload was ever
    // begun — so the operator is told how much room there is rather than
    // offered a button that asks the same question again.
    const { queue, server } = harness();
    server.noRoom =
      'this file is 4.0 GB, and 800.0 MB is free where media is kept; an upload is refused while less than 1.0 GB would be left over';
    queue.add([{ path: '/media/clip.mov', name: 'clip.mov' }]);

    const items = await settled(queue);

    expect(items[0]?.stage).toEqual({
      at: 'stopped',
      reason: server.noRoom,
      resumable: false,
    });
    expect(server.attempts).toEqual([]);
  });

  it('offers to carry on when the disk filled up part way through', async () => {
    // A 507 on a chunk is the other case, and the opposite answer. The bytes
    // that did not land were not counted, so the upload is exactly where it
    // was: an operator who frees some room carries on from there rather than
    // sending the whole file again.
    const { queue, server } = harness();
    server.fullAt.add(4);
    queue.add([{ path: '/media/clip.mov', name: 'clip.mov' }]);

    const items = await settled(queue);

    expect(items[0]?.stage).toMatchObject({
      at: 'stopped',
      reason: 'could not write this chunk: the disk holding the media store is full',
      resumable: true,
    });
    expect(items[0]?.upload).not.toBeNull();
    expect(server.attempts).toEqual([0, 4]);
  });

  it('does not offer to carry on when the server refused what arrived', async () => {
    // The server read the file, found it was not what was declared, discarded
    // the staged bytes and marked the item failed with the reason. Sending the
    // same bytes under the same declaration would fail the same way, so the
    // operator is shown the reason rather than a button that repeats it.
    const { queue, server } = harness();
    server.verificationFails = true;
    queue.add([{ path: '/media/clip.mov', name: 'clip.mov' }]);

    const items = await settled(queue);

    expect(items[0]?.stage).toEqual({
      at: 'stopped',
      reason: 'the video is 1280x720, not the 1920x1080 that was declared',
      resumable: false,
    });
    expect(items[0]?.upload).toBeNull();
  });

  it('stops a file no encoder here can prepare, without offering to carry on', async () => {
    // Nothing to resume: sending the same bytes again would fail the same way,
    // and this machine has no way to turn them into something else.
    //
    // Which is the owner's third case of 2026-09-23 reached from the other
    // side: the file needs HEVC and there is no HEVC encoder here, so this says
    // so rather than producing something the server would refuse.
    const { queue, server } = harness({
      encoders: [],
      probe: { kind: 'read', probe: needsPreparing({ codec: 'cinepak' }) },
    });
    queue.add([{ path: '/media/from-1994.mov', name: 'from-1994.mov' }]);

    const items = await settled(queue);

    expect(items[0]?.stage).toMatchObject({ at: 'stopped', resumable: false });
    expect(server.created).toEqual([]);
  });

  it('works through the files one at a time', async () => {
    // Twenty files dropped at once all want the disk and the network, and the
    // machine has a show to run.
    const { queue, server } = harness();
    queue.add([
      { path: '/media/one.mov', name: 'one.mov' },
      { path: '/media/two.mov', name: 'two.mov' },
    ]);

    const items = await settled(queue);

    expect(items.map((item) => item.stage.at)).toEqual(['ready', 'ready']);
    expect(server.created).toHaveLength(2);
    expect(server.verified).toEqual(['UPLOAD-1', 'UPLOAD-2']);
  });

  it('cancels an upload the operator took out of the queue', async () => {
    // The staged bytes go now rather than in an hour, and the item says it was
    // cancelled instead of sitting at "importing" for ever.
    const { queue, server } = harness();
    server.failAt.add(4);
    queue.add([{ path: '/media/clip.mov', name: 'clip.mov' }]);
    const stopped = await settled(queue);

    queue.remove(stopped[0]!.id);
    await settled(queue);

    expect(server.cancelled).toEqual(['UPLOAD-1']);
    expect(queue.items).toEqual([]);
  });

  it('reports what it is doing as it goes, so a long file is not a frozen panel', async () => {
    const { queue, reports } = harness();
    queue.add([{ path: '/media/clip.mov', name: 'clip.mov' }]);
    await settled(queue);

    const stages = reports.flatMap((items) => items.map((item) => item.stage.at));
    expect(stages).toContain('hashing');
    expect(stages).toContain('uploading');
    expect(stages).toContain('verifying');
    expect(stages).toContain('ready');
  });
});

describe('what the queue panel says', () => {
  it('measures progress against the size, and says nothing where there is none to say', () => {
    expect(fractionOf({ at: 'uploading', sent: 5, byteSize: 10 })).toBe(0.5);
    expect(fractionOf({ at: 'hashing', hashed: 10, byteSize: 10 })).toBe(1);
    expect(fractionOf({ at: 'waiting' })).toBeNull();
    expect(fractionOf({ at: 'verifying' })).toBeNull();

    // Preparing measures against the file's own length, which is known, rather
    // than against how long the machine will take, which is not.
    expect(
      fractionOf({ at: 'preparing', seconds: 4, ofSeconds: 8, to: 'hevc_8' }),
    ).toBe(0.5);
    expect(
      fractionOf({ at: 'preparing', seconds: 4, ofSeconds: null, to: 'hevc_8' }),
    ).toBeNull();
    // A file that says it lasts no time cannot have a fraction, and must not
    // produce one by dividing by zero.
    expect(
      fractionOf({ at: 'preparing', seconds: 4, ofSeconds: 0, to: 'hevc_8' }),
    ).toBeNull();
    // Nor one past the end, when an encoder reports a little more than the
    // file claimed to hold.
    expect(
      fractionOf({ at: 'preparing', seconds: 9, ofSeconds: 8, to: 'hevc_8' }),
    ).toBe(1);
  });

  it('writes a size the way an operator reads one', () => {
    expect(bytesLabel(512)).toBe('512 B');
    expect(bytesLabel(1024)).toBe('1.0 kB');
    expect(bytesLabel(5 * 1024 * 1024)).toBe('5.0 MB');
  });
});
