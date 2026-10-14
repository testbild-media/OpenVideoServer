// Asks the packaged client where it would run its own FFmpeg from, and runs it.
//
//   ELECTRON_RUN_AS_NODE=1 <app>/OpenVideoServer.exe \
//     tools/packaged-binary-check.cjs <app directory>
//
// **It has to be the packaged Electron, not node.** Electron patches `fs` to
// read inside `app.asar`, and that patch is the whole fault this checks for:
// `existsSync` says yes to a binary inside the archive, `spawn` says `ENOENT`,
// and a lookup that asked the archive first believed the first answer. Plain
// node sees no file in the archive at all, would take the unpacked path by
// default, and would pass this check while the product failed.
//
// Everything else in this repository's CI runs the client from its source tree,
// where `dist/bin` is an ordinary directory. Nothing exercised the package, so
// the client shipped from 0.2.0 to 0.2.20 with a bundled FFmpeg it could never
// start, telling operators "nothing on this machine answered to 'ffmpeg'" while
// the binary sat in the installation directory (reported from a venue
// 2026-10-13, found 2026-10-14).
//
// The function under test is read out of the asar rather than imported from the
// source tree, because what matters is the bytes that shipped.
'use strict';

const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');

const root = process.argv[2];
if (!root) {
  console.error('say which directory the packaged app is in');
  process.exit(2);
}

const asar = path.join(root, 'resources', 'app.asar');
let bundle;
try {
  bundle = fs.readFileSync(path.join(asar, 'dist', 'main.cjs'), 'utf8');
} catch (error) {
  console.error(`the packaged bundle could not be read: ${error.message}`);
  process.exit(2);
}

const found = bundle.match(/function bundledCandidates\([\s\S]*?\n\}/);
if (!found) {
  console.error('the shipped bundle contains no bundledCandidates');
  process.exit(1);
}
// As an expression, so its value comes back: a declaration evaluated in strict
// mode stays inside `eval`'s own scope and is not visible here.
// eslint-disable-next-line no-eval
const bundledCandidates = eval(`(${found[0]})`);

let failed = false;
for (const name of ['ffmpeg', 'ffprobe']) {
  // What `join(__dirname, 'bin', name)` comes to inside the package.
  const asked = path.join(asar, 'dist', 'bin', `${name}.exe`);
  const candidates = bundledCandidates(asked, path.sep);
  const chosen = candidates.find((candidate) => fs.existsSync(candidate)) ?? null;

  if (chosen === null) {
    console.error(`${name}: the packaged client would find nothing to run`);
    failed = true;
    continue;
  }
  const run = spawnSync(chosen, ['-hide_banner', '-version']);
  if (run.error) {
    console.error(`${name}: it chose ${chosen} and spawning that gave ${run.error.code}`);
    failed = true;
    continue;
  }
  const said = String(run.stdout).split('\n')[0].trim();
  console.log(`${name}: ${chosen}`);
  console.log(`  runs: ${said}`);
}

if (failed) {
  console.error('the packaged client cannot start the FFmpeg it ships with');
  process.exit(1);
}
console.log('the packaged client can start both of the binaries it ships');
