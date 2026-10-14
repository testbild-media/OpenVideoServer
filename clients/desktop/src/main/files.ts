// Reading media files off this machine.
//
// The main process is the only half of this client with a filesystem and a
// child process, so probing, hashing and reading chunks happen here. What they
// are *for* is in `shared/ingest.ts`, which never touches a disk: this file is
// the four operations that do, and nothing decides anything.
//
// `ffprobe` may or may not be installed. When it is not, that is reported as
// such and nothing is guessed from a file extension — the server now opens an
// uploaded video and refuses one whose codec or size disagrees with what was
// declared, so a guess is not a small inaccuracy in the library any more, it is
// a refused upload after the bytes have crossed. The operator declares the file
// by hand instead, which is the honest failure.

import { execFile, spawn } from 'node:child_process';

import { bundledCandidates } from '../shared/bundled-binary.js';
import { probeFailure } from '../shared/probe-failure.js';
import { createHash } from 'node:crypto';
import { createReadStream, existsSync } from 'node:fs';
import { mkdtemp, open, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, sep } from 'node:path';

import type { FrameRate } from '@ovs/schema';

import {
  carriesAlpha,
  type IngestFiles,
  type PrepareOutcome,
  type Probe,
  type ProbeOutcome,
} from '../shared/ingest.js';
import type { ProbedVideo } from '../shared/profile.js';
import {
  hapArguments,
  hevcArguments,
  HEVC_BACKENDS,
  preparedName,
  progressFrom,
  whyItStopped,
  CONVENTIONAL_TAGS,
  type ColourTags,
  type HevcBackend,
} from '../shared/encoder.js';
import { depthOf, type OvsProfile } from '../shared/profile.js';

/**
 * What to run for one of FFmpeg's programs.
 *
 * Three answers, in this order.
 *
 * The environment variable, when somebody has said where it is. That is for a
 * machine with a build worth preferring — a hardware-accelerated one, or a
 * newer release than this client shipped with — and it wins over everything
 * because it is the only one of the three that was a decision.
 *
 * Then the copy the client brought. `build.mjs` puts `ffmpeg` and `ffprobe`
 * beside `main.cjs` in `bin/`, so ingest works on a machine where nobody has
 * installed a video toolkit and nobody should have to. This is preferred over
 * whatever is on the path deliberately: the client behaving one way here and
 * another there, depending on what somebody happened to install for some other
 * purpose, is worse than behaving the same way everywhere.
 *
 * Then the bare name, resolved by the operating system the way any other
 * command is — which is the answer during development, in the tests, and on a
 * build whose binaries did not come down.
 *
 * Resolved once when this module loads: it cannot change while the client runs,
 * and an operator waiting on an ingest should not pay for a search per file.
 */
function locate(name: string, variable: string): string {
    const said = process.env[variable];
    if (said !== undefined && said.trim() !== '') {
      return said;
    }
    const brought = beside(name);
    if (brought !== null) {
      return brought;
    }
    return name;
}

/**
 * The copy shipped beside this file, when there is one.
 *
 * `bundledCandidates` carries the reasoning, which is worth reading: inside a
 * packaged app this file lives in an archive, `existsSync` answers for paths
 * inside it, and the one thing that cannot be done with a file in there is run
 * it.
 */
function beside(name: string): string | null {
  const suffix = process.platform === 'win32' ? '.exe' : '';
  const here = join(__dirname, 'bin', `${name}${suffix}`);
  for (const candidate of bundledCandidates(here, sep)) {
    if (existsSync(candidate)) {
      return candidate;
    }
  }
  return null;
}

/** What to run to read a file. */
const FFPROBE = locate('ffprobe', 'OVS_FFPROBE');

/**
 * What to run to prepare a file.
 *
 * Looked up separately from the prober, because a machine may have one and not
 * the other: Debian's `ffmpeg` package ships both, a stripped build or a
 * hand-installed binary often does not, and a client that assumed the pair
 * would report "cannot prepare" as "cannot read".
 */
const FFMPEG = locate('ffmpeg', 'OVS_FFMPEG');

/** How long a probe may take before it is treated as a machine that cannot. */
const PROBE_TIMEOUT_MS = 20_000;

/** How much of a probe's output to read, in bytes. */
const PROBE_OUTPUT_LIMIT = 4 * 1024 * 1024;

/** How often hashing says how far it has got, in bytes. */
const HASH_REPORT_EVERY = 32 * 1024 * 1024;

/** What one stream of `ffprobe -show_streams` says, as far as this reads it. */
interface ProbeStream {
  codec_type?: string;
  codec_name?: string;
  width?: number;
  height?: number;
  pix_fmt?: string;
  r_frame_rate?: string;
  avg_frame_rate?: string;
  duration?: string;
  nb_frames?: string;
  // The rest of what the playback contract asks about. Everything here is
  // in the header, so it costs nothing beyond the call already being made
  // — unlike the keyframe interval, which is in the packets and has its own
  // function.
  color_primaries?: string;
  color_transfer?: string;
  color_space?: string;
  color_range?: string;
  field_order?: string;
  sample_aspect_ratio?: string;
  bit_rate?: string;
}

/** What `ffprobe -show_format` says, as far as this reads it. */
interface ProbeFormat {
  format_name?: string;
  duration?: string;
  bit_rate?: string;
}

/**
 * Asks `ffprobe` what is in a file.
 *
 * Only the first video stream is read. A file with two of them is not something
 * this pipeline prepares, and taking the second would be deciding which one the
 * show wants.
 */
export async function probeFile(path: string): Promise<ProbeOutcome> {
  let stdout: string;
  try {
    stdout = await run(FFPROBE, [
      '-v',
      'error',
      '-print_format',
      'json',
      '-show_streams',
      '-show_format',
      // Nothing after this is read as an option, so a file whose name begins
      // with a dash is a file rather than a flag somebody did not mean.
      '-i',
      path,
    ]);
  } catch (error) {
    if (isMissingCommand(error)) {
      return { kind: 'unavailable' };
    }
    return {
      kind: 'failed',
      reason: `this file could not be read: ${error instanceof Error ? error.message : String(error)}`,
    };
  }

  let parsed: { streams?: ProbeStream[]; format?: ProbeFormat };
  try {
    parsed = JSON.parse(stdout) as { streams?: ProbeStream[]; format?: ProbeFormat };
  } catch {
    return { kind: 'failed', reason: 'the prober said something this client could not read' };
  }

  const video = (parsed.streams ?? []).find((stream) => stream.codec_type === 'video');
  if (video === undefined) {
    return { kind: 'failed', reason: 'this file holds no picture, so there is nothing to show' };
  }

  const probe: Probe = {
    container: parsed.format?.format_name ?? '',
    codec: video.codec_name ?? '',
    width: video.width ?? 0,
    height: video.height ?? 0,
    durationMs: milliseconds(video.duration ?? parsed.format?.duration),
    frameRate: ratio(video.r_frame_rate ?? video.avg_frame_rate),
    hasAlpha: carriesAlpha(video.pix_fmt ?? ''),
    video: contractReading(video, parsed.format),
  };
  return { kind: 'read', probe };
}

/**
 * The same stream as the playback contract sees it.
 *
 * Everything here is in the header, so it costs nothing beyond the ffprobe
 * call that was being made anyway. The keyframe interval is not: it is in the
 * packets, and `measureKeyframes` is what pays for it.
 */
function contractReading(video: ProbeStream, format: ProbeFormat | undefined): ProbedVideo {
  const average = asNumber(video.avg_frame_rate);
  const nominal = asNumber(video.r_frame_rate);
  return {
    codec: video.codec_name ?? '',
    pixelFormat: video.pix_fmt ?? '',
    hasAlpha: carriesAlpha(video.pix_fmt ?? ''),
    primaries: tag(video.color_primaries),
    transfer: tag(video.color_transfer),
    matrix: tag(video.color_space),
    range: tag(video.color_range),
    frameRate: average,
    // A file with only one of the two readings is constant-rate by that
    // reading rather than variable by the absence of the other.
    nominalFrameRate: nominal === 0 ? average : nominal,
    keyframeInterval: null,
    sampleAspect: aspect(video.sample_aspect_ratio),
    // `progressive` and an absent field order are both progressive: an
    // untagged file is progressive until it says otherwise, which is what
    // every tool assumes of one.
    interlaced:
      video.field_order !== undefined &&
      video.field_order !== '' &&
      video.field_order !== 'progressive' &&
      video.field_order !== 'unknown',
    bitsPerSecond: bits(video, format),
  };
}

/** A colour tag, with FFmpeg's two ways of saying nothing read as nothing. */
function tag(value: string | undefined): string {
  const said = (value ?? '').trim();
  return said === 'unknown' || said === 'N/A' ? '' : said;
}

/** A rate as a plain number, and zero for one the file will not give. */
function asNumber(value: string | undefined): number {
  const parsed = ratio(value);
  if (parsed === null || parsed.denominator === 0) {
    return 0;
  }
  return parsed.numerator / parsed.denominator;
}

/**
 * A sample aspect, with unspecified read as square.
 *
 * A container that says nothing writes `0:1`, and square is what every tool
 * assumes of one. Reporting 0:1 would fail the contract for a file that is
 * perfectly square.
 */
function aspect(value: string | undefined): [number, number] {
  const parts = (value ?? '').split(':');
  const numerator = Number(parts[0]);
  const denominator = Number(parts[1]);
  if (!Number.isFinite(numerator) || !Number.isFinite(denominator)) {
    return [1, 1];
  }
  if (numerator <= 0 || denominator <= 0) {
    return [1, 1];
  }
  return [numerator, denominator];
}

/**
 * Bits a second, declared where the container says and the whole file where not.
 *
 * Zero only when neither is known. A bitrate that could not be read must not
 * become a file that escapes the ceiling, and the stream's own number is
 * preferred because the format's includes whatever else is in the container.
 */
function bits(video: ProbeStream, format: ProbeFormat | undefined): number {
  const declared = Number(video.bit_rate);
  if (Number.isFinite(declared) && declared > 0) {
    return Math.round(declared);
  }
  const whole = Number(format?.bit_rate);
  return Number.isFinite(whole) && whole > 0 ? Math.round(whole) : 0;
}
/**
 * What this machine's FFmpeg can encode, by name.
 *
 * From the binary rather than from what the build was meant to include: a
 * client running against a system FFmpeg, or one whose bundled binary was
 * replaced, is a real case and the honest answer is what the binary says about
 * itself.
 *
 * Asked once and remembered. The list does not change while the client runs,
 * and `ffmpeg -encoders` is a few hundred milliseconds nobody should pay per
 * file.
 */
export async function encoderNames(): Promise<string[]> {
  if (encodersRemembered !== null) {
    return encodersRemembered;
  }
  let listed: string;
  try {
    listed = await run(FFMPEG, ['-hide_banner', '-encoders']);
  } catch {
    // No FFmpeg here at all. An empty list, not a guess.
    encodersRemembered = [];
    return encodersRemembered;
  }
  // Lines look like ` V....D hevc_nvenc           NVIDIA NVENC hevc encoder`.
  // The name is the second field of any line whose flags begin with a medium.
  const names: string[] = [];
  for (const line of listed.split(/\r?\n/)) {
    const match = /^\s*[VAS][\w.]{5}\s+(\S+)/.exec(line);
    if (match?.[1] !== undefined) {
      names.push(match[1]);
    }
  }
  encodersRemembered = names;
  return names;
}

/** What `encoderNames` found, once it has looked. */
let encodersRemembered: string[] | null = null;

/**
 * The encoders that are listed **and actually work**, which is not the same set.
 *
 * Measured on a real machine, 2026-09-23: `hevc_mf` is in every Windows
 * FFmpeg's encoder list and on a machine with no HEVC encoder MFT installed it
 * fails every frame with `80004005` and writes a 258-byte file with no video in
 * it. Listing it and trying it are two different questions, and the second one
 * is the one that matters.
 *
 * Asking it costs one frame each, once per run of the client. The alternative
 * is discovering it after encoding a ten-minute clip — which the verification
 * step would catch, and which is a quarter of an hour of somebody's evening
 * spent finding out something a hundred milliseconds could have said.
 *
 * 720p rather than something tiny, because a Media Foundation encoder can
 * refuse a small picture and accept a real one; and one frame, because the
 * question is whether it can encode at all rather than how well.
 */
export async function usableEncoders(): Promise<string[]> {
  if (usableRemembered !== null) {
    return usableRemembered;
  }
  const listed = new Set(await encoderNames());
  // Only the ones a decision rests on. Trialling every encoder FFmpeg has
  // would be a hundred encodes to answer three questions.
  // **Every backend a decision can rest on**, which is all four of them and
  // not the two that were being asked about. `hevc_qsv` and `hevc_amf` were
  // passed through as listed, so a machine with neither was handed one of them
  // as its encoder and found out by failing. A trial is one frame.
  const worthTrying = [...HEVC_BACKENDS, 'hap'].filter((name) => listed.has(name));
  const working: string[] = [];
  for (const name of worthTrying) {
    if (await encodesOneFrame(name)) {
      working.push(name);
    }
  }
  // Everything else is passed through as listed: nothing decides on it, and a
  // client that hid an encoder it had not trialled would be lying about its own
  // binary.
  for (const name of listed) {
    if (!worthTrying.includes(name)) {
      working.push(name);
    }
  }
  usableRemembered = working;
  return working;
}

/** What `usableEncoders` found, once it has looked. */
let usableRemembered: string[] | null = null;

/**
 * What the system said about a failure, short enough to put in front of
 * somebody.
 *
 * `ENOENT` and the rest are what Node reports and what a person can search for;
 * the stack is not, and the message alone reads as a sentence.
 */
function systemSaid(error: unknown): string {
  if (error instanceof Error) {
    const code = (error as NodeJS.ErrnoException).code;
    return code === undefined ? error.message : `${code} (${error.message})`;
  }
  return String(error);
}

/** Whether one encoder can write a single frame on this machine. */
async function encodesOneFrame(encoder: string): Promise<boolean> {
  const directory = await mkdtemp(join(tmpdir(), 'ovs-encoder-trial-'));
  const into = join(directory, encoder === 'hap' ? 'trial.mov' : 'trial.mp4');
  // **A raw frame from a file, not a generated one.** It was
  // `-f lavfi -i testsrc2=...`, and the FFmpeg this client ships is built with
  // `--disable-everything`: it has neither that input nor that filter, so every
  // trial failed with "Unknown input format: 'lavfi'" and every encoder was
  // recorded as not working here -- on every machine, whatever hardware it had
  // (found 2026-10-13). What this uses instead is the `rawvideo` demuxer and
  // the `file` protocol, which are in the build because the pipeline needs
  // them, so the trial asks about the encoder and nothing else.
  const source = join(directory, 'trial.yuv');
  const TRIAL = { width: 1280, height: 720 };
  const rate = { numerator: 25, denominator: 1 };
  try {
    // **A gradient rather than flat grey, because what it looks like turned
    // out to matter.** A uniform picture compresses to almost nothing: the
    // trial wrote a one-kilobyte container, and the size check below — which
    // exists because Media Foundation writes a container with no video in it
    // and then fails — rejected a perfectly good encoder for it. Measured
    // against FFmpeg 6.1 on 2026-10-14, where it cost `hevc_nvenc` its place
    // on a machine that has one. A gradient costs a loop to make and leaves
    // that threshold nowhere near the edge.
    //
    // Exactly one frame, which is what makes `-frames:v` unnecessary: the
    // rawvideo demuxer stops when the file does.
    const luma = Buffer.alloc(TRIAL.width * TRIAL.height);
    for (let row = 0; row < TRIAL.height; row += 1) {
      for (let column = 0; column < TRIAL.width; column += 1) {
        luma[row * TRIAL.width + column] = (column * 7 + row * 3) & 0xff;
      }
    }
    // Mid grey in Cb and Cr: a gradient in luma alone is a legal picture and
    // the colour of it is not what is being asked about.
    await writeFile(
      source,
      Buffer.concat([luma, Buffer.alloc((TRIAL.width * TRIAL.height) / 2, 128)]),
    );
    // **The arguments a real encode would use, not a reduced set of them.**
    // This asked `-c:v libkvazaar` and nothing else, so Kvazaar passed the
    // trial and then refused every file the client gave it: the settings it
    // objected to were in the string the trial left out. A trial that asks an
    // easier question than the job is worse than no trial, because its answer
    // is believed (2026-10-13).
    //
    // Eight bits, because that is the one depth every backend here can do. A
    // machine whose encoder manages eight and not ten still passes this and
    // fails a ten-bit file -- which `ingest.ts` catches by probing what came
    // out, and which no one-frame trial can tell apart from a refusal to work
    // at all.
    const readAs = [
      '-f',
      'rawvideo',
      '-pix_fmt',
      'yuv420p',
      '-s',
      `${TRIAL.width}x${TRIAL.height}`,
      '-r',
      '25',
    ];
    await run(
      FFMPEG,
      encoder === 'hap'
        ? hapArguments(source, into, rate, readAs)
        : hevcArguments(source, into, encoder as HevcBackend, 8, rate, CONVENTIONAL_TAGS, readAs),
    );
    // A zero exit is not enough. Media Foundation writes a container with no
    // video in it and *then* fails, and some builds report that as success.
    const written = await stat(into).catch(() => null);
    return written !== null && written.size > 1024;
  } catch {
    return false;
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

/**
 * The longest run of frames between two keyframes.
 *
 * A pass over the packets, which is the only place the answer is: a container
 * that declares a GOP size declares what an encoder was asked for, and what it
 * wrote is another matter. `-show_entries packet=flags` reads the flags
 * without decoding anything, which is a few hundred megabytes a second.
 *
 * `null` when it could not be measured, which callers must not read as fine.
 */
export async function keyframeInterval(path: string): Promise<number | null> {
  let listed: string;
  try {
    listed = await run(FFPROBE, [
      '-v',
      'error',
      '-select_streams',
      'v:0',
      '-show_entries',
      'packet=flags',
      '-of',
      'csv=p=0',
      '-i',
      path,
    ]);
  } catch {
    return null;
  }

  let frames = 0;
  let since = 0;
  let longest = 0;
  let seenKey = false;
  for (const line of listed.split(/\r?\n/)) {
    const flags = line.trim();
    if (flags === '') {
      continue;
    }
    frames += 1;
    // `K_` for a keyframe, `__` for anything else.
    if (flags.startsWith('K')) {
      longest = Math.max(longest, since);
      since = 1;
      seenKey = true;
    } else {
      since += 1;
    }
  }
  longest = Math.max(longest, since);

  if (frames === 0) {
    return null;
  }
  // A file of one frame has no interval to measure, and a stream with no
  // keyframe at all reports its whole length — which fails the contract, as it
  // should.
  if (frames === 1) {
    return 1;
  }
  return seenKey ? Math.max(1, longest) : frames;
}
/** The file's size in bytes. */
export async function fileSize(path: string): Promise<number> {
  return (await stat(path)).size;
}

/**
 * SHA-256 of the whole file, lowercase hex.
 *
 * Streamed rather than read whole: a show file is gigabytes, and a client that
 * held one in memory to hash it would be the reason an import failed on the
 * machine it was most needed on.
 */
export function hashFile(
  path: string,
  onProgress: (bytesHashed: number) => void,
): Promise<string> {
  return new Promise((resolve, reject) => {
    const hash = createHash('sha256');
    const stream = createReadStream(path);
    let read = 0;
    let reported = 0;

    stream.on('data', (chunk: string | Buffer) => {
      const bytes = typeof chunk === 'string' ? Buffer.from(chunk) : chunk;
      hash.update(bytes);
      read += bytes.byteLength;
      // Not per read: a 64 KiB read on a 40 GB file is six hundred thousand
      // messages across a process boundary to move a bar by nothing.
      if (read - reported >= HASH_REPORT_EVERY) {
        reported = read;
        onProgress(read);
      }
    });
    stream.on('error', reject);
    stream.on('end', () => {
      onProgress(read);
      resolve(hash.digest('hex'));
    });
  });
}

/** `length` bytes from `offset`, or fewer at the end of the file. */
export async function readPart(
  path: string,
  offset: number,
  length: number,
): Promise<Uint8Array> {
  const handle = await open(path, 'r');
  try {
    const buffer = Buffer.alloc(length);
    const { bytesRead } = await handle.read(buffer, 0, length, offset);
    return new Uint8Array(buffer.buffer, buffer.byteOffset, bytesRead);
  } finally {
    await handle.close();
  }
}

/**
 * Re-encodes a file into something the server has a profile for.
 *
 * The output goes in a directory of its own under the system's temporary one,
 * so that a name collision is impossible and clearing up is removing one
 * directory. The caller removes it — `discardFile` — because only the caller
 * knows whether the bytes are still wanted.
 *
 * FFmpeg is asked for machine-readable progress on its standard output and for
 * errors on standard error, so that what goes in front of an operator when this
 * fails is FFmpeg's own last words rather than an exit code.
 */
export async function prepareFile(
  path: string,
  to: OvsProfile,
  backend: HevcBackend | null,
  rate: FrameRate,
  tags: ColourTags,
  onProgress: (seconds: number) => void,
  stop?: AbortSignal,
): Promise<PrepareOutcome> {
  const directory = await mkdtemp(join(tmpdir(), 'ovs-prepare-'));
  const into = join(directory, preparedName(basenameOf(path), to));
  const depth = depthOf(to);
  // HAP has one encoder and no depth; HEVC has a backend the caller chose and
  // a depth the profile names. A HEVC profile with no backend is a caller
  // mistake rather than a machine without one, which `planFor` has already
  // reported — so the honest answer is the same as having no FFmpeg.
  const args =
    depth === null
      ? hapArguments(path, into, rate)
      : backend === null
        ? null
        : hevcArguments(path, into, backend, depth, rate, tags);
  if (args === null) {
    await rm(directory, { recursive: true, force: true });
    return { kind: 'unavailable' };
  }

  return new Promise((resolve) => {
    let child;
    try {
      child = spawn(FFMPEG, args, {
        stdio: ['ignore', 'pipe', 'pipe'],
      });
    } catch (error) {
      void rm(directory, { recursive: true, force: true });
      resolve({ kind: 'unavailable', reason: systemSaid(error) });
      return;
    }

    // Stopped rather than left running. An operator who removes a file from
    // the queue has decided they do not want it, and a machine running a show
    // has better uses for a core than encoding a clip nobody will ask for.
    // SIGTERM rather than SIGKILL: FFmpeg closes its output on it, so the
    // partial file is a file and the directory can be removed.
    let cancelled = false;
    const onAbort = () => {
      cancelled = true;
      child.kill('SIGTERM');
    };
    if (stop !== undefined) {
      if (stop.aborted) {
        onAbort();
      } else {
        stop.addEventListener('abort', onAbort, { once: true });
      }
    }

    // Kept because a failure's reason is the last thing FFmpeg said, and it
    // says it on standard error. Bounded, because a file it hates can produce a
    // line per frame and none of them are more useful than the last few.
    let said = '';
    child.stderr?.on('data', (chunk: Buffer) => {
      said = `${said}${chunk.toString()}`.slice(-4096);
    });

    // Progress arrives as `key=value` lines, which may be split across reads.
    let pending = '';
    child.stdout?.on('data', (chunk: Buffer) => {
      pending += chunk.toString();
      const lines = pending.split('\n');
      pending = lines.pop() ?? '';
      for (const line of lines) {
        const seconds = progressFrom(line);
        if (seconds !== null) {
          onProgress(seconds);
        }
      }
    });

    child.on('error', (error) => {
      stop?.removeEventListener('abort', onAbort);
      void rm(directory, { recursive: true, force: true });
      // A command that cannot be spawned at all is a machine without one, which
      // is a different answer from a file it could not handle — and *why* it
      // could not be spawned is the difference between "install FFmpeg" and
      // "something on this machine is holding it".
      resolve({ kind: 'unavailable', reason: systemSaid(error) });
    });

    child.on('close', (code) => {
      stop?.removeEventListener('abort', onAbort);
      if (cancelled) {
        void rm(directory, { recursive: true, force: true });
        resolve({ kind: 'stopped' });
        return;
      }
      if (code === 0) {
        resolve({ kind: 'prepared', path: into });
        return;
      }
      void rm(directory, { recursive: true, force: true });
      const tail = whyItStopped(said);
      resolve({
        kind: 'failed',
        reason:
          tail === ''
            ? `Preparing this file failed, and ffmpeg said nothing about why (it stopped with ${code}).`
            : `Preparing this file failed: ${tail}`,
      });
    });
  });
}

/**
 * Removes a file this client made, and the directory it was made in.
 *
 * Deliberately not a general delete. It removes the *directory* `prepareFile`
 * created, and only when the path looks like one of its own — a mistake here
 * would be deleting somebody's media, so the check is worth the three lines.
 */
export async function discardFile(path: string): Promise<void> {
  const directory = path.slice(0, Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\')));
  if (directory === '' || !basenameOf(directory).startsWith('ovs-prepare-')) {
    return;
  }
  await rm(directory, { recursive: true, force: true });
}

/** The last part of a path, whichever kind of separator it uses. */
function basenameOf(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

/** The operations the ingest queue needs from this machine. */
export const files: IngestFiles = {
  probe: probeFile,
  size: fileSize,
  hash: hashFile,
  read: readPart,
  prepare: prepareFile,
  discard: discardFile,
  encoders: usableEncoders,
  keyframeInterval,
};

// --- the small parts ---------------------------------------------------------

/**
 * What Windows exits a process with when it cannot find a library it needs.
 *
 * `STATUS_DLL_NOT_FOUND`. Node reports it unsigned on some paths and signed on
 * others, which is why both are checked where this is used.
 */
/** Runs a command and gives back what it printed. */
function run(command: string, args: string[]): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile(
      command,
      args,
      { timeout: PROBE_TIMEOUT_MS, maxBuffer: PROBE_OUTPUT_LIMIT, windowsHide: true },
      (error, stdout, stderr) => {
        if (error) {
          // `code` is the exit status when the process ran, and a string like
          // `ENOENT` when it could not be started at all.
          // `NodeJS.ErrnoException` only knows about the second.
          const status: number | string | undefined = (
            error as Error & { code?: number | string }
          ).code;
          reject(
            Object.assign(new Error(probeFailure(command, stderr, status)), {
              code: (error as NodeJS.ErrnoException).code,
            }),
          );
          return;
        }
        resolve(stdout);
      },
    );
  });
}

/** Whether a command failed because there is no such command on this machine. */
function isMissingCommand(error: unknown): boolean {
  const code = (error as NodeJS.ErrnoException | null)?.code;
  // ENOENT is a missing executable everywhere; EACCES is one that is there and
  // cannot be run, which is the same thing from the operator's side.
  return code === 'ENOENT' || code === 'EACCES';
}

/** Seconds as the prober prints them, in whole milliseconds. */
function milliseconds(seconds: string | undefined): number | null {
  if (seconds === undefined || seconds === 'N/A') {
    return null;
  }
  const value = Number(seconds);
  if (!Number.isFinite(value) || value <= 0) {
    return null;
  }
  return Math.round(value * 1000);
}

/**
 * `60000/1001` as the rational the model stores.
 *
 * Kept as the two numbers rather than divided out: 60000/1001 is not 59.94, and
 * a scheduler handed the second accumulates drift.
 */
function ratio(rate: string | undefined): { numerator: number; denominator: number } | null {
  if (rate === undefined) {
    return null;
  }
  const [numerator, denominator] = rate.split('/');
  const top = Number(numerator);
  const bottom = Number(denominator ?? '1');
  if (!Number.isFinite(top) || !Number.isFinite(bottom) || top <= 0 || bottom <= 0) {
    return null;
  }
  return { numerator: top, denominator: bottom };
}
