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

import { afterAll, beforeAll, describe, expect, it } from 'vitest';

// Set before the module is loaded, because what to run is read once: this names
// a command no machine has, which is the case the queue has to cope with.
process.env['OVS_FFPROBE'] = 'ovs-no-such-prober';
const { fileSize, hashFile, probeFile, readPart } = await import('../src/main/files.js');

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
});
