// What an operator is told when the prober failed.
//
// Measured on the owner's laptop, 2026-10-10, with the installed client:
//
//   & "…\app.asar.unpacked\dist\bin\ffprobe.exe" -version
//   exit: -1073741515
//
// That is `0xC0000135`, `STATUS_DLL_NOT_FOUND`: the shipped binary needed
// libraries that were not beside it. Windows refused to start it before any of
// it ran, so there was no stderr — and every file he dropped came back as
// "Command failed", the command line, and nothing else at all.
//
// The number is the only evidence there is in that case, so it is pinned here.

import { describe, expect, it } from 'vitest';

import {
  MISSING_LIBRARY,
  MISSING_LIBRARY_SIGNED,
  probeFailure,
} from '../src/shared/probe-failure.js';

describe('what a failed probe says', () => {
  it('quotes the prober over anything written here', () => {
    // A real file that ffprobe read and disliked. Its own words name the
    // stream and the codec; nothing in this repository can do better.
    const said = probeFailure('ffprobe', 'moov atom not found\n', 1);
    expect(said).toBe('moov atom not found');
  });

  it('names a missing library for the exit code Windows gives', () => {
    // **The exact number from the owner's machine.** Node reported it signed.
    expect(MISSING_LIBRARY_SIGNED).toBe(-1073741515);
    const said = probeFailure('ffprobe', '', -1073741515);
    expect(said).toContain('needs a library that is not beside it');
    expect(said).toContain('PATH');
  });

  it('names it from the unsigned form too', () => {
    // The same status, as Node reports it on other paths. Both or neither.
    expect(MISSING_LIBRARY).toBe(3221225781);
    expect(probeFailure('ffprobe', '', MISSING_LIBRARY)).toContain(
      'needs a library that is not beside it',
    );
  });

  it('says a silent failure was silent, rather than nothing', () => {
    // Any other code with no output. It is not a guess about which library,
    // but it does say the one thing that leads somewhere: a prober that said
    // nothing usually never started.
    const said = probeFailure('ffprobe', '', 9009);
    expect(said).toContain('9009');
    expect(said).toContain('did not start');
  });

  it('copes with a failure that has no code at all', () => {
    const said = probeFailure('ffprobe', '', undefined);
    expect(said).toContain('undefined');
    expect(said).toContain('did not start');
  });

  it('prefers the prober even when the exit code is the library one', () => {
    // Belt and braces: if it managed to say something, that is the better
    // sentence whatever the status was.
    expect(probeFailure('ffprobe', 'no such file\n', -1073741515)).toBe('no such file');
  });
});
