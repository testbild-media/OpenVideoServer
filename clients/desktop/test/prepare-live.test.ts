// Preparing a real file with a real FFmpeg, and checking the contract holds.
//
// `profile.test.ts` holds the decisions to account — which profile, which
// backend, which arguments — and never runs anything. This runs it: a file this
// test makes, through the encoder this machine has, back through the prober,
// and asks whether what came out is something the **server** would accept.
//
// It is the half that cannot be faked. The arguments can be right and the
// output still wrong, because what an encoder is told and what it writes are
// two different things — an encoder that quietly drops the colour tags, or
// writes 4:2:2, or ignores the keyframe interval, produces a file that fails
// the contract while every flag was correct. That is exactly the failure the
// owner's instruction of 2026-09-23 is about:
//
// > If neither backend can produce an asset matching the required OVS playback
// > profile, do not silently produce a different/low-quality result.
//
// Skipped where there is no FFmpeg. A media server has one; a build machine
// often does not, and an ordinary `npm test` must not turn red for it.
// Individual encodes are skipped where *that backend* is missing, because the
// machine that runs this may have a card or may not.

import { execFile } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';

import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import { CONVENTIONAL_TAGS, planFor, tagsFor } from '../src/shared/encoder.js';
import { faults, type OvsProfile, type ProbedVideo } from '../src/shared/profile.js';

const run = promisify(execFile);

const FFMPEG = process.env['OVS_FFMPEG'] ?? 'ffmpeg';

/** Whether this machine has an encoder to prepare anything with. */
async function hasFfmpeg(): Promise<boolean> {
  try {
    await run(FFMPEG, ['-hide_banner', '-loglevel', 'error', '-version']);
    return true;
  } catch {
    return false;
  }
}

const available = await hasFfmpeg();
const { discardFile, keyframeInterval, prepareFile, probeFile, usableEncoders } =
  await import('../src/main/files.js');
// What *works*, not what is listed. On a Windows machine with no HEVC encoder
// MFT installed, `hevc_mf` is listed and fails every frame — measured, 2026-09-23.
const encoders = available ? new Set(await usableEncoders()) : new Set<string>();

let directory = '';

beforeAll(async () => {
  if (available) {
    directory = await mkdtemp(join(tmpdir(), 'ovs-prepare-test-'));
  }
});

afterAll(async () => {
  if (directory !== '') {
    await rm(directory, { recursive: true, force: true });
  }
});

/** Renders a short clip the server will not take as it stands. */
async function makeSource(name: string, args: readonly string[]): Promise<string> {
  const path = join(directory, name);
  await run(FFMPEG, [
    '-nostdin',
    '-hide_banner',
    '-loglevel',
    'error',
    '-y',
    '-f',
    'lavfi',
    '-i',
    'testsrc2=size=320x240:rate=25:duration=2',
    ...args,
    path,
  ]);
  return path;
}

/** Reads a file the way the ingest queue does, packets and all. */
async function readFully(path: string): Promise<ProbedVideo> {
  const read = await probeFile(path);
  expect(read.kind).toBe('read');
  if (read.kind !== 'read') {
    throw new Error('the prober would not read it');
  }
  const video = read.probe.video;
  expect(video).not.toBeNull();
  if (video === null) {
    throw new Error('the prober said nothing about the contract');
  }
  return { ...video, keyframeInterval: await keyframeInterval(path) };
}

describe.skipIf(!available)('preparing a file with the FFmpeg on this machine', () => {
  it('reads a file closely enough to say whether the server would take it', async () => {
    // The whole policy starts here. An H.264 file is not one of the three, and
    // saying so is a codec fault and nothing else.
    const source = await makeSource('opener.mp4', ['-c:v', 'libx264', '-pix_fmt', 'yuv420p']);
    const video = await readFully(source);

    expect(video.codec).toBe('h264');
    expect(faults(video, 'hevc_8').map((fault) => fault.what)).toEqual(['codec']);

    const decided = planFor(video, encoders);
    if (encoders.has('hevc_nvenc') || encoders.has('hevc_mf')) {
      expect(decided).toMatchObject({ kind: 'encode', to: 'hevc_8' });
    } else {
      expect(decided.kind).toBe('unavailable');
    }
  });

  for (const backend of ['hevc_nvenc', 'hevc_mf'] as const) {
    it.skipIf(!encoders.has(backend))(
      `produces a file the server accepts, with ${backend}`,
      async () => {
        const source = await makeSource(`source-${backend}.mp4`, [
          '-c:v',
          'libx264',
          '-pix_fmt',
          'yuv420p',
        ]);

        const seen: number[] = [];
        const outcome = await prepareFile(
          source,
          'hevc_8',
          backend,
          { numerator: 25, denominator: 1 },
          CONVENTIONAL_TAGS,
          (seconds) => seen.push(seconds),
        );
        expect(outcome.kind).toBe('prepared');
        if (outcome.kind !== 'prepared') {
          return;
        }

        try {
          // The point of the whole exercise: every clause, on the file that
          // actually came out. A flag being right is not the same as an
          // encoder having obeyed it.
          const after = await readFully(outcome.path);
          expect(faults(after, 'hevc_8')).toEqual([]);

          // And the clauses that are easiest to get silently wrong, named, so
          // a failure says which one rather than "faults: 1".
          expect(after.codec).toBe('hevc');
          expect(after.pixelFormat).toBe('yuv420p');
          expect(after.primaries).toBe('bt709');
          expect(after.transfer).toBe('bt709');
          expect(after.matrix).toBe('bt709');
          expect(after.range).toBe('tv');
          expect(after.keyframeInterval).not.toBeNull();
          expect(after.keyframeInterval ?? 0).toBeLessThanOrEqual(25);
          expect(after.interlaced).toBe(false);
          expect(after.sampleAspect).toEqual([1, 1]);
        } finally {
          await discardFile(outcome.path);
        }
      },
      120_000,
    );
  }

  it.skipIf(!encoders.has('hap'))('turns alpha into HAP Alpha, which the server takes', async () => {
    // The other half of the routing. A file with alpha has nowhere to go but
    // HAP: HEVC would drop it, and dropping alpha silently is the kind of
    // conversion this whole policy exists to prevent.
    const source = await makeSource('alpha.mov', [
      '-c:v',
      'qtrle',
      '-pix_fmt',
      'argb',
    ]);
    const video = await readFully(source);
    expect(video.hasAlpha).toBe(true);

    const decided = planFor(video, encoders);
    expect(decided).toMatchObject({ kind: 'encode', to: 'hap' });
    if (decided.kind !== 'encode') {
      return;
    }
    expect(decided.backends).toEqual([]);

    const outcome = await prepareFile(
      source,
      'hap',
      null,
      { numerator: 25, denominator: 1 },
      tagsFor(video),
      () => undefined,
    );
    expect(outcome.kind).toBe('prepared');
    if (outcome.kind !== 'prepared') {
      return;
    }

    try {
      const after = await readFully(outcome.path);
      expect(after.codec).toBe('hap');
      expect(faults(after, 'hap')).toEqual([]);
      // Every frame a keyframe, which is what makes HAP seek for free.
      expect(after.keyframeInterval).toBe(1);
    } finally {
      await discardFile(outcome.path);
    }
  }, 120_000);

  it('measures a keyframe interval a header would not give', async () => {
    // The one clause no header carries, and the one most "already HEVC" files
    // fail. Two seconds at 25 with a five-second GOP is one keyframe: the
    // whole file.
    const long = await makeSource('long-gop.mp4', [
      '-c:v',
      'libx264',
      '-pix_fmt',
      'yuv420p',
      '-g',
      '125',
      '-keyint_min',
      '125',
      '-sc_threshold',
      '0',
    ]);
    expect(await keyframeInterval(long)).toBeGreaterThan(25);

    const short = await makeSource('short-gop.mp4', [
      '-c:v',
      'libx264',
      '-pix_fmt',
      'yuv420p',
      '-g',
      '25',
      '-keyint_min',
      '25',
      '-sc_threshold',
      '0',
    ]);
    expect(await keyframeInterval(short)).toBeLessThanOrEqual(25);
  }, 60_000);

  it('knows what this machine can encode, from the binary rather than the build', async () => {
    const names = await usableEncoders();
    expect(names.length).toBeGreaterThan(0);
    // Whatever else it has, an FFmpeg has these.
    expect(names).toContain('mjpeg');
  });

  it.skipIf(encoders.has('hevc_nvenc') || encoders.has('hevc_mf'))(
    'says so rather than producing something else, on a machine with no HEVC encoder',
    async () => {
      const source = await makeSource('no-backend.mp4', ['-c:v', 'libx264', '-pix_fmt', 'yuv420p']);
      const video = await readFully(source);
      const decided: ReturnType<typeof planFor> = planFor(video, encoders);
      expect(decided.kind).toBe('unavailable');
      if (decided.kind !== 'unavailable') {
        return;
      }
      expect(decided.reason).toContain('HEVC');
    },
  );
});

/** Keeps the unused import honest when every encode is skipped. */
export type _Profile = OvsProfile;
