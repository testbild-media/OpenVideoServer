// Which path the client runs its own FFmpeg from.
//
// The failure this exists for, measured inside the published 0.2.20 package on
// 2026-10-14 and reported from a venue before that as "nothing on this machine
// answered to ffmpeg":
//
//     in the archive       existsSync: true   spawn: ENOENT
//     unpacked beside it   existsSync: true   spawn: ffmpeg version 7.1.1
//
// Electron patches `fs` to read inside `app.asar`, so a lookup that asks
// whether the archive copy is there is told yes, and the one thing it cannot
// do with that file is the thing it wants to do.

import { describe, expect, it } from 'vitest';

import { bundledCandidates } from '../src/shared/bundled-binary.js';

describe('where the client runs its own binaries from', () => {
  it('leaves an ordinary path alone', () => {
    // Development, the tests, and a build whose binaries never came down.
    expect(bundledCandidates('D:\\work\\client\\dist\\bin\\ffmpeg.exe', '\\')).toEqual([
      'D:\\work\\client\\dist\\bin\\ffmpeg.exe',
    ]);
  });

  it('answers with the unpacked copy inside a packaged app, and only that', () => {
    const inside =
      'C:\\Users\\Someone With Spaces\\AppData\\Local\\Programs\\OpenVideoServer' +
      '\\resources\\app.asar\\dist\\bin\\ffmpeg.exe';
    const candidates = bundledCandidates(inside, '\\');
    expect(candidates).toEqual([
      'C:\\Users\\Someone With Spaces\\AppData\\Local\\Programs\\OpenVideoServer' +
        '\\resources\\app.asar.unpacked\\dist\\bin\\ffmpeg.exe',
    ]);
    // **Not merely first.** The archive path must not be a fallback: choosing a
    // file that cannot be run over admitting there is none is how an operator
    // was told to point `OVS_FFMPEG` at something that worked while the right
    // binary sat in the installation directory.
    expect(candidates).not.toContain(inside);
  });

  it('does the same where the separator is a slash', () => {
    expect(
      bundledCandidates('/opt/OpenVideoServer/resources/app.asar/dist/bin/ffprobe', '/'),
    ).toEqual(['/opt/OpenVideoServer/resources/app.asar.unpacked/dist/bin/ffprobe']);
  });

  it('is not fooled by a directory that merely ends in app.asar', () => {
    // No separator after it, so nothing inside an archive: left alone.
    expect(bundledCandidates('C:\\keep\\app.asar', '\\')).toEqual(['C:\\keep\\app.asar']);
  });
});
