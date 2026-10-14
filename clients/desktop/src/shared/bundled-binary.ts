// Where a binary that ships with the client really is.
//
// Its own module so a test can reach it: `main/files.ts` resolves these paths
// the moment it is imported, which is too early to arrange anything.
//
// # The archive, and why asking does not help
//
// Electron serves a packaged app out of `resources/app.asar`, and an executable
// inside an archive cannot be run — the operating system has no path to it. The
// packaging leaves `dist/bin` unpacked (`asarUnpack` in `electron-builder.yml`),
// and the real file sits at the same path with `.unpacked` on the archive's
// name.
//
// **Electron patches `fs`, so `existsSync` answers for the copy inside the
// archive as well.** It is readable there; it is only not executable. A lookup
// that tried the archive path first therefore got "yes", returned it, and every
// `spawn` afterwards failed with `ENOENT` on a file that is plainly there —
// which is what the client did in every packaged build it has ever had, and why
// an operator was told "nothing on this machine answered to ffmpeg" while the
// binary sat in the installation directory. Measured inside the published
// 0.2.20 package on 2026-10-14:
//
//     in the archive       existsSync: true   spawn: ENOENT
//     unpacked beside it   existsSync: true   spawn: ffmpeg version 7.1.1
//
// So when the path leads into an archive, the unpacked copy is not merely tried
// first — it is the *only* candidate. Falling back to the archive path would
// mean choosing a file that cannot be run over admitting there is none, and
// "no FFmpeg here" is a sentence an operator can act on.

/**
 * The paths worth trying for `path`, in the order to try them.
 *
 * One path outside a packaged app, which is development, the tests, and a
 * machine where the binaries never came down. Inside one, the unpacked path and
 * nothing else.
 */
export function bundledCandidates(path: string, separator: string): string[] {
  const archive = `app.asar${separator}`;
  if (!path.includes(archive)) {
    return [path];
  }
  return [path.replace(archive, `app.asar.unpacked${separator}`)];
}
