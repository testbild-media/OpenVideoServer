// What to tell an operator when the prober failed.
//
// Its own module so a test can reach it: `main/files.ts` opens handles and
// looks for binaries the moment it is imported.
//
// The case this exists for, measured on the owner's laptop on 2026-10-10:
//
//   & "…\app.asar.unpacked\dist\bin\ffprobe.exe" -version
//   exit: -1073741515
//
// `0xC0000135`, `STATUS_DLL_NOT_FOUND`. The shipped binary needed libraries
// that were not beside it, so Windows refused to start it — before any of it
// ran, which is why there was no stderr and why every file he dropped failed
// with "Command failed" and a command line and nothing else.

/**
 * What Windows exits a process with when it cannot find a library it needs.
 *
 * `STATUS_DLL_NOT_FOUND`. Node reports it unsigned on some paths and as the
 * signed 32-bit value on others, so both are recognised.
 */
export const MISSING_LIBRARY = 0xc000_0135;

/** The same number as Node reports it when it reports it signed. */
export const MISSING_LIBRARY_SIGNED = MISSING_LIBRARY - 0x1_0000_0000;

/**
 * The sentence for a failed probe, given what it said and how it ended.
 *
 * `stderr` first and always, because the prober's own words about a file beat
 * anything written here. The rest is for the case where it had none — and a
 * process that says nothing is usually one that never started.
 */
export function probeFailure(
  command: string,
  stderr: string,
  status: number | string | undefined,
): string {
  const said = stderr.trim();
  if (said !== '') {
    return said;
  }
  if (status === MISSING_LIBRARY || status === MISSING_LIBRARY_SIGNED) {
    return (
      'the bundled ffprobe could not start: it needs a library that is not beside it. ' +
      'This build of the client is incomplete — install one from a newer release, or ' +
      'put an ffprobe on the PATH.'
    );
  }
  return (
    `${command} failed with ${String(status)} and said nothing. ` +
    'A prober that says nothing usually did not start at all.'
  );
}
