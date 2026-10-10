// Builds the three halves of the client into dist/.
//
//   dist/main.cjs      the main process, with ws bundled in
//   dist/preload.cjs   the bridge, which must be CommonJS for a sandboxed
//                      preload script
//   dist/ui/           the page: index.html, app.js, style.css
//   dist/bin/          ffmpeg and ffprobe, so nobody has to install them,
//                      with the licence material that has to travel with them
//
// Bundled rather than shipped as node_modules, so what runs is one file per
// half and an install cannot be half-missing.
//
// The two executables are the exception, because an executable cannot be
// bundled into a JavaScript file. They are copied here as assets, the same way
// the page's HTML is, and the packages that supply them are build-time
// dependencies the finished client never loads. An operator preparing a clip
// should not first be told to install a video toolkit.

import { build } from 'esbuild';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { chmod, cp, mkdir, rm, stat } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { basename, dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);

const here = dirname(fileURLToPath(import.meta.url));
const dist = join(here, 'dist');

/**
 * Which commit this build came from.
 *
 * **Every build is `OpenVideoServer-0.2.0-setup.exe`**, because the version
 * comes out of package.json. Two installers with the same name can be
 * different software, and on 2026-10-10 that cost an install and an hour: a
 * fix was shipped, the installer was run, the fix was not in it, and the only
 * symptom was the error it was meant to replace.
 *
 * The environment first, because a workflow knows the commit without asking
 * git — and git in a container often refuses a checkout it does not own, which
 * would make every built client say `unknown` without saying why.
 */
function buildCommit() {
  const given = (process.env.OVS_BUILD_COMMIT ?? '').trim();
  if (given !== '') {
    return given;
  }
  try {
    const at = execFileSync('git', ['rev-parse', '--short=12', 'HEAD'], {
      cwd: here,
      encoding: 'utf8',
    }).trim();
    const dirty = execFileSync('git', ['status', '--porcelain', '--untracked-files=no'], {
      cwd: here,
      encoding: 'utf8',
    }).trim();
    // A build with uncommitted changes is not the commit it names, and that is
    // exactly the build somebody is holding when a fix seems to be missing.
    return dirty === '' ? at : `${at}-modified`;
  } catch {
    // A source tarball has no history to ask. An ordinary state, not a failure.
    return 'unknown';
  }
}

const COMMIT = buildCommit();
console.log(`building ${COMMIT}`);

await rm(dist, { recursive: true, force: true });
await mkdir(join(dist, 'ui'), { recursive: true });

/** Shared settings. Sourcemaps always: a stack trace during a show is evidence. */
const common = {
  bundle: true,
  sourcemap: true,
  logLevel: 'info',
  target: 'es2022',
  // Into all three bundles, so the main process can log it and the page can
  // show it without either of them reading a file at runtime.
  define: { OVS_BUILD_COMMIT: JSON.stringify(COMMIT) },
};

await build({
  ...common,
  entryPoints: [join(here, 'src', 'main', 'index.ts')],
  outfile: join(dist, 'main.cjs'),
  platform: 'node',
  format: 'cjs',
  // Electron is provided by the runtime. `ws` pulls in two optional native
  // speedups it works perfectly well without.
  external: ['electron', 'bufferutil', 'utf-8-validate'],
});

await build({
  ...common,
  entryPoints: [join(here, 'src', 'preload.ts')],
  outfile: join(dist, 'preload.cjs'),
  platform: 'node',
  format: 'cjs',
  external: ['electron'],
});

await build({
  ...common,
  entryPoints: [join(here, 'src', 'ui', 'app.ts')],
  outfile: join(dist, 'ui', 'app.js'),
  platform: 'browser',
  format: 'esm',
});

await cp(join(here, 'src', 'ui', 'index.html'), join(dist, 'ui', 'index.html'));
await cp(join(here, 'src', 'ui', 'style.css'), join(dist, 'ui', 'style.css'));

// The application's own icon, beside the built code rather than inside it: the
// main process hands Electron a path, and a bundler cannot hand it a PNG.
await cp(join(here, 'assets'), join(dist, 'assets'), { recursive: true });

// FFmpeg and FFprobe, for the platform this build is for.
//
// **Two sources, and only one of them may be published.**
//
// `vendor/ffmpeg` is what `tools/build-ffmpeg.sh --profile client` produces:
// configured `--disable-gpl --disable-nonfree --disable-version3`, with the
// LGPL licence text and the configuration line beside it, and checked by that
// script against the finished binary rather than against its own command line.
// That is the build NOTICE describes and the only one that may be shipped.
//
// The npm packages are the other source, and they are GPL. `ffmpeg-static`
// declares GPL-3.0-or-later and its binary reports `--enable-gpl
// --enable-version3` — measured on 2026-09-27 with `tools/build-ffmpeg.sh
// --check` over the two of them. They stay because a working copy needs an
// ffmpeg and building one takes half an hour; they are refused in a release
// build, where the difference stops being academic.
//
// A build with neither says so and carries on. The client then looks for one
// on the path as it did before, and says what to do if there is none.
// Either way of saying it. The environment variable is what a workflow
// sets; the flag is what an npm script can pass on every platform without
// a package whose whole job is setting an environment variable.
const RELEASE = process.env.OVS_RELEASE === '1' || process.argv.includes('--release');
const EXE = process.platform === 'win32' ? '.exe' : '';
const vendor = join(here, 'vendor', 'ffmpeg');

await mkdir(join(dist, 'bin'), { recursive: true });

/** Copies one file into `dist/bin`, keeping it runnable. */
async function bundle(from, as) {
  const to = join(dist, 'bin', as ?? basename(from));
  await cp(from, to);
  // Copying does not carry the executable bit everywhere, and a binary that
  // cannot be run is the same as one that is not there.
  await chmod(to, 0o755);
  const { size } = await stat(to);
  return size;
}

let vendored = true;
for (const name of ['ffmpeg', 'ffprobe']) {
  const from = join(vendor, `${name}${EXE}`);
  if (!existsSync(from)) {
    vendored = false;
    break;
  }
}

if (vendored) {
  for (const name of ['ffmpeg', 'ffprobe']) {
    const size = await bundle(join(vendor, `${name}${EXE}`));
    console.log(`bundled ${name}: from vendor/ffmpeg, ${Math.round(size / 1048576)} MB`);
  }
  // **The licence material travels with the binaries.** The LGPL asks for the
  // licence text and for what the thing was built with, and a copy of the
  // binaries without them is the part of shipping this that is easy to forget.
  for (const file of ['COPYING.LGPLv2.1', 'CONFIGURATION.txt']) {
    const from = join(vendor, file);
    if (existsSync(from)) {
      await bundle(from);
    } else {
      console.warn(`vendor/ffmpeg has no ${file}, which the LGPL asks to travel with it`);
    }
  }
} else if (RELEASE) {
  // Refused rather than quietly published. The whole reason this file
  // distinguishes the two sources is that one of them may not be given to
  // anybody, and a release that fell back to it would be a licence breach
  // nobody would notice until somebody read `ffmpeg -version`.
  console.error(
    'OVS_RELEASE=1 and vendor/ffmpeg is not there.\n' +
      'A release may not carry the npm FFmpeg: ffmpeg-static is GPL-3.0-or-later\n' +
      'and its binary reports --enable-gpl --enable-version3.\n' +
      'Build the shippable one first:  ./tools/build-ffmpeg.sh --profile client',
  );
  process.exit(1);
} else {
  console.warn(
    'no vendor/ffmpeg, so this build uses the npm FFmpeg, which is GPL and must not be shipped.',
  );
  console.warn('build the shippable one with: ./tools/build-ffmpeg.sh --profile client');
  for (const [name, resolve] of [
    ['ffmpeg', () => require('ffmpeg-static')],
    ['ffprobe', () => require('@ffprobe-installer/ffprobe').path],
  ]) {
    let from;
    try {
      from = resolve();
    } catch (error) {
      console.warn(`no bundled ${name}: ${error.message}`);
      continue;
    }
    if (typeof from !== 'string' || from === '') {
      console.warn(`no bundled ${name}: the package named no file`);
      continue;
    }
    const size = await bundle(from);
    console.log(`bundled ${name}: ${basename(from)}, ${Math.round(size / 1048576)} MB`);
  }
}

console.log(`built to ${dist}`);
