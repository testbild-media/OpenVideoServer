// Reading a media file off this machine.
//
// The three operations the ingest queue asks the main process for, against a
// real file in a temporary directory. What is worth checking here is not the
// happy path but the answer this build gives when the machine has no prober:
// the server refuses an upload whose codec disagrees with the declaration, so
// "there is nothing here that can read this" has to come back as itself rather
// than as a guess from the file's name.

import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync } from 'node:fs';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';

// Set before the module is loaded, because what to run is read once: this names
// a command no machine has, which is the case the queue has to cope with.
process.env['OVS_FFPROBE'] = 'ovs-no-such-prober';
const { fileSize, hashFile, probeFile, readPart } = await import('../src/main/files.js');
import { preparedName } from '../src/shared/encoder.js';

// Where the build puts the pair it ships, and what the client computes from
// `__dirname` when it is running out of `dist`.
const bundled = (name: string) =>
  join(
    dirname(fileURLToPath(import.meta.url)),
    '..',
    'dist',
    'bin',
    `${name}${process.platform === 'win32' ? '.exe' : ''}`,
  );

let directory = '';
let path = '';
/** Big enough to be read in more than one go, and not a round number. */
const contents = Buffer.from('OpenVideoServer media ingest, one temporary file.');

beforeAll(async () => {
  directory = await mkdtemp(join(tmpdir(), 'ovs-ingest-'));
  path = join(directory, 'clip.mov');
  await writeFile(path, contents);
});

afterAll(async () => {
  await rm(directory, { recursive: true, force: true });
});

describe('reading a file for ingest', () => {
  it('measures the file the way the server will', async () => {
    expect(await fileSize(path)).toBe(contents.byteLength);
  });

  it('hashes the whole file, in the form the endpoint insists on', async () => {
    // 64 lowercase hex characters, or `check` in upload_routes.rs refuses the
    // declaration before a byte is sent.
    const hashed = await hashFile(path, () => undefined);

    expect(hashed).toBe(createHash('sha256').update(contents).digest('hex'));
    expect(hashed).toMatch(/^[0-9a-f]{64}$/);
  });

  it('says how far it has got, so a long file is not a frozen panel', async () => {
    const reported: number[] = [];
    await hashFile(path, (bytes) => reported.push(bytes));

    expect(reported[reported.length - 1]).toBe(contents.byteLength);
  });

  it('reads the part an upload chunk needs, and no more than the file has', async () => {
    expect(Buffer.from(await readPart(path, 0, 8))).toEqual(contents.subarray(0, 8));
    expect(Buffer.from(await readPart(path, 8, 8))).toEqual(contents.subarray(8, 16));

    // At the end of the file the last chunk is short, which is what the server
    // is told it holds afterwards.
    const tail = await readPart(path, contents.byteLength - 3, 8);
    expect(tail.byteLength).toBe(3);
    expect(Buffer.from(tail)).toEqual(contents.subarray(contents.byteLength - 3));
  });

  it('says plainly when this machine has no prober rather than guessing', async () => {
    // Not "failed": the difference matters. A file this build could not read is
    // a file the operator has to describe, and a file no prober exists for is
    // not a broken file.
    expect(await probeFile(path)).toEqual({ kind: 'unavailable' });
  });
});

describe('the FFmpeg the client brings with it', () => {
  // An operator preparing a clip should not first be told to install a video
  // toolkit, so `build.mjs` copies both programs into `dist/bin`. This checks
  // the build did it and that what it copied runs — a binary that is there and
  // will not start is the same as one that is missing, and harder to notice.
  const run = promisify(execFile);

  it.skipIf(!existsSync(bundled('ffmpeg')))('ships an ffmpeg that runs', async () => {
    const { stdout } = await run(bundled('ffmpeg'), ['-hide_banner', '-version']);
    expect(stdout).toContain('ffmpeg version');
  });

  it.skipIf(!existsSync(bundled('ffprobe')))('ships an ffprobe that runs', async () => {
    const { stdout } = await run(bundled('ffprobe'), ['-hide_banner', '-version']);
    expect(stdout).toContain('ffprobe version');
  });

  // **That it runs was never the question.** It ran, it had every encoder, and
  // no file could be prepared with it on any machine: the build has
  // `--disable-everything`, the client named its output `.mp4`, and `mp4` was
  // not among the muxers. "Unable to choose an output format" is what an
  // operator at a venue saw as "nothing on this machine answered to ffmpeg"
  // (2026-10-13).
  it.skipIf(!existsSync(bundled('ffmpeg')))(
    'can write the containers this client names',
    async () => {
      const { stdout } = await run(bundled('ffmpeg'), ['-hide_banner', '-muxers']);
      // What `preparedName` produces, asked of the binary rather than of the
      // build script: the two disagreed, and the binary is the one that runs.
      for (const profile of ['hevc_8', 'hevc_10', 'hap'] as const) {
        const extension = preparedName('clip.mov', profile).split('.').pop();
        const muxer = extension === 'mp4' ? 'mp4' : 'mov';
        expect(stdout, `${profile} is written as .${extension}`).toMatch(
          new RegExp(`^\\s*E\\s+${muxer}\\b`, 'm'),
        );
      }
    },
  );

  // **The whole path, with the binary that will ship.** Probe a file the server
  // refuses, decide what to do with it, do it, and read back what came out --
  // which is what the queue does per file and what nothing was testing. Both
  // defects found at a venue would have failed here: a build with no mp4 muxer
  // cannot write the output, and a trial the binary cannot run leaves no
  // backend to choose.
  //
  // On a machine with no hardware encoder this goes down the software path,
  // which is the one that has to work everywhere (owner, 2026-10-13: "Software
  // Weg muss immer vorbereitet sein").
  //
  // **And with pixels that are not square**, which the server refuses and which
  // neither the scale nor the encoder clears. The owner's ProRes at
  // 112301:112320 came out of Quick Sync and Kvazaar exactly as it went in
  // (2026-10-20), and the fix -- `setsar=1` -- was a filter this build did not
  // have: against a developer's FFmpeg it passed, against this one it refused
  // every clip. One nearly square, which is only relabelled, and one anamorphic,
  // which is widened to the width it is shown at.
  //
  // **And at a capped bitrate**, which is the second encode a clip gets when
  // its first came out over the ceiling (the owner's 1.4 GB clip, 2026-10-20).
  // On a runner with no GPU that is Kvazaar with `bitrate=` in place of `qp=`,
  // a parameter string nothing else in this suite gives the shipped binary.
  for (const { fixture, width, spending } of [
    { fixture: 'refused-h264.mp4', width: 320, spending: 'quality' },
    { fixture: 'refused-nearly-square-h264.mp4', width: 320, spending: 'quality' },
    { fixture: 'refused-anamorphic-h264.mp4', width: 424, spending: 'quality' },
    { fixture: 'refused-h264.mp4', width: 320, spending: 'capped' },
  ] as const) {
    it.skipIf(!existsSync(bundled('ffmpeg')) || !existsSync(bundled('ffprobe')))(
      `turns a file the server refuses into one it accepts: ${fixture}, ${spending}`,
      async (ctx) => {
        vi.resetModules();
        process.env['OVS_FFMPEG'] = bundled('ffmpeg');
        process.env['OVS_FFPROBE'] = bundled('ffprobe');
        const client = await import('../src/main/files.js');
        const { planFor, preparedSize } = await import('../src/shared/encoder.js');
        const { faults } = await import('../src/shared/profile.js');

        // H.264 with a fifteen-frame GOP: refused by the policy, and the shape of
        // every clip an operator drops in. 320x240 rather than the renderer's
        // 64x64 fixtures, because NVENC refuses a picture that small and a test
        // whose first backend cannot take the input is measuring the input.
        const clip = join(dirname(fileURLToPath(import.meta.url)), 'media', fixture);

        const read = await client.probeFile(clip);
        if (read.kind !== 'read') {
          throw new Error(`the fixture could not be probed: ${JSON.stringify(read)}`);
        }
        const measured = read.probe.video;
        if (measured === null) {
          throw new Error('the prober said nothing about the fixture it was given');
        }
        const video = { ...measured, keyframeInterval: await client.keyframeInterval(clip) };

        const plan = planFor(video, new Set(await client.usableEncoders()));
        if (plan.kind === 'unavailable') {
          // **Not a skip when the binary has a software encoder in it.**
          // `tools/build-ffmpeg.sh` refuses to produce one without `libkvazaar`,
          // so on the build this client ships there is always a way through, and
          // "this machine has no encoder" is only ever true of a developer's own
          // FFmpeg. Skipping here would have turned the release that could
          // prepare nothing into a green run (2026-10-13).
          const { stdout } = await run(bundled('ffmpeg'), ['-hide_banner', '-encoders']);
          if (/libkvazaar/.test(stdout)) {
            throw new Error(
              `the shipped FFmpeg has libkvazaar and the client cannot use it: ${plan.reason}`,
            );
          }
          ctx.skip();
          return;
        }
        expect(plan.kind, 'an H.264 clip is not something the server takes').toBe('encode');
        if (plan.kind !== 'encode') {
          return;
        }

        // Every backend in turn, which is what the queue does: one that cannot
        // take this picture is not the end of the file, and on a machine with no
        // hardware encoder the last one standing is the software path.
        const attempts = plan.to === 'hap' ? [null] : plan.backends;
        // Decided as the queue decides it, from the probe and with no ceiling.
        const size = preparedSize(
          { width: read.probe.width, height: read.probe.height, sampleAspect: measured.sampleAspect },
          'source',
        );
        let outcome: Awaited<ReturnType<typeof client.prepareFile>> = { kind: 'unavailable' };
        const refused: string[] = [];
        for (const backend of attempts) {
          outcome = await client.prepareFile(
            clip,
            plan.to,
            backend,
            read.probe.frameRate ?? { numerator: 25, denominator: 1 },
            { range: 'tv', matrix: 'bt709', primaries: 'bt709', transfer: 'bt709' },
            () => undefined,
            undefined,
            size,
            spending,
          );
          if (outcome.kind === 'prepared') {
            break;
          }
          refused.push(`${backend ?? 'hap'}: ${JSON.stringify(outcome)}`);
        }
        expect(outcome.kind, `every backend refused it -- ${refused.join('; ')}`).toBe('prepared');
        if (outcome.kind !== 'prepared') {
          return;
        }

        try {
          const again = await client.probeFile(outcome.path);
          if (again.kind !== 'read') {
            throw new Error(`what was written could not be read back: ${JSON.stringify(again)}`);
          }
          const came = again.probe.video;
          if (came === null) {
            throw new Error('what was written told the prober nothing about its picture');
          }
          const written = {
            ...came,
            keyframeInterval: await client.keyframeInterval(outcome.path),
          };
          // The server's own verdict, asked of the file that would be sent to it.
          expect(faults(written, plan.to)).toEqual([]);
          expect(again.probe.width, 'the width it is shown at').toBe(width);
        } finally {
          await rm(dirname(outcome.path), { recursive: true, force: true });
        }
      },
      120_000,
    );
  }

  // The trial the client runs before it trusts an encoder. It fed
  // `-f lavfi -i testsrc2=...`, which this build has neither of, so every
  // encoder on every machine was recorded as broken -- including `hap`, which
  // needs no hardware at all and is the one this can insist on here.
  it.skipIf(!existsSync(bundled('ffmpeg')))(
    'can run the trial the client uses to decide what works',
    async (ctx) => {
      // Only against the build this client ships. A developer with their own
      // FFmpeg in `dist/bin` has one without a HAP encoder, and the trial's
      // answer there says nothing about ours.
      const { stdout } = await run(bundled('ffmpeg'), ['-hide_banner', '-encoders']);
      if (!/^\s*V[\w.]{5}\s+hap\b/m.test(stdout)) {
        ctx.skip();
        return;
      }

      // The module reads what to run once, when it loads, and it was loaded at
      // the top of this file with no `OVS_FFMPEG` set -- so it found the one on
      // the path. Against that binary this proves nothing: the question is
      // about the one the client ships. A fresh registry is what lets the
      // variable be read.
      vi.resetModules();
      process.env['OVS_FFMPEG'] = bundled('ffmpeg');
      const fresh = await import('../src/main/files.js');

      expect(await fresh.usableEncoders()).toContain('hap');
    },
    // Five encoders, each a process that has to start, fail and be waited on:
    // on a runner with no hardware at all that is slow rather than quick.
    120_000,
  );
});
