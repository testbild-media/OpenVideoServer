// Which encoder produces an OVS asset, and the arguments that make it one.
//
// # The operator never chooses
//
// The owner's decision of 2026-09-23, in their words:
//
// > The operator must not choose between NVENC and Media Foundation. Backend
// > selection is automatic and remains an implementation detail.
//
// So there is no setting, no dropdown and no mention of a backend anywhere an
// operator looks. What they see is that a file is being prepared, and how far
// along it is. Which encoder did it belongs in a log.
//
// # Backends are not profiles
//
// > Treat NVENC and Media Foundation as encoder backends for the same logical
// > HEVC ingest profile.
//
// One profile, several ways to reach it. `hevc_qsv` and `hevc_amf` are meant to
// slot in here later as two more backends and not as two more things an
// operator can pick — which is why `HevcBackend` is a list this file owns and
// `OvsProfile` is a contract `profile.ts` owns. Adding a backend touches this
// file and nothing else.
//
// # And if none of them can do it
//
// > If neither backend can produce an asset matching the required OVS playback
// > profile, do not silently produce a different/low-quality result; fall back
// > to another suitable OVS playback format such as HAP or report that
// > optimization is unavailable.
//
// Which is why every encode is *verified* against the contract afterwards
// rather than trusted. An encoder that quietly wrote 4:2:2, or dropped the
// colour tags, or ignored the keyframe interval, has produced a file this
// server will refuse — and finding that out here, with the source still on
// disc, is the difference between trying the next backend and an operator
// wondering why their upload failed.

import type { FrameRate } from '@ovs/schema';

import {
  faults,
  MOST_BITS_PER_SECOND,
  PIXEL_FORMAT,
  profileOf,
  targetFor,
  type OvsProfile,
  type ProbedVideo,
  type ProfileDepth,
} from './profile.js';

/**
 * A way of producing HEVC, in the order they are preferred.
 *
 * NVIDIA first because it is the fastest by a wide margin. Then the other two
 * vendors' own encoders — Quick Sync on an Intel machine, AMF on an AMD one.
 * Media Foundation after those, because it is Windows' own wrapper around
 * whichever of them the machine has, and the vendor's encoder is the shorter
 * path to the same silicon. Software last, and only ever last.
 *
 * **Quick Sync and AMF were in the binary all along and nothing asked for
 * them.** `tools/build-ffmpeg.sh` configures this client's FFmpeg
 * `--enable-libvpl --enable-amf`, so `hevc_qsv` and `hevc_amf` have been in it
 * since September. This list named two encoders, so a machine with an Intel
 * iGPU or an AMD card was told it had no way to make HEVC — and since every
 * file that is not already conforming has to be made so, that machine could
 * upload nothing at all (owner, 2026-10-10: "Ich konnte bisher kein Medium
 * hochladen, weil der Client nicht transcodiert").
 *
 * **Software last, which is new.** There was deliberately none (owner,
 * 2026-09-23), for two reasons: what it would do to the licence, and that
 * nobody waits for a software HEVC encode. The first is answered — libx265 is
 * GPL-2.0-or-later and cannot go inside an Apache-2.0 client, but Kvazaar is
 * BSD-3-Clause and can. The second stands, which is why it sits at the end of
 * this list -- hardware wherever there is any, software always (owner,
 * 2026-10-13: "Hardware Transcoding waere dann natuerlich immer der gewuenschte
 * weg, aber Software Transcoding muss als fallback immer gehen. Es koennte auf
 * low budget Laptops laufen der Client").
 *
 * **Eight bits only**, which is a limit of the wrapper rather than a choice:
 * FFmpeg's Kvazaar takes `yuv420p` and a ten-bit target asks for `p010le`. A
 * ten-bit source on a machine with no hardware encoder therefore still has no
 * way through, and says so by failing rather than by producing eight-bit
 * quietly.
 */
export const HEVC_BACKENDS = [
  'hevc_nvenc',
  'hevc_qsv',
  'hevc_amf',
  'hevc_mf',
  'libkvazaar',
] as const;

/** One of the ways of producing HEVC. */
export type HevcBackend = (typeof HEVC_BACKENDS)[number];

/**
 * Which backends this machine's FFmpeg actually has, in preference order.
 *
 * Taken from `ffmpeg -encoders` rather than from what the build was meant to
 * include: a client running against a system FFmpeg, or one whose bundled
 * binary was replaced, is a real case and the honest answer is what the binary
 * says about itself.
 */
export function backendsAvailable(encoderNames: ReadonlySet<string>): HevcBackend[] {
  return HEVC_BACKENDS.filter((name) => encoderNames.has(name));
}

/**
 * What preparing a file would involve, or that it needs none.
 *
 * `unavailable` is the case the owner named: the file needs HEVC, this machine
 * has no way to make it, and saying so is better than producing something else
 * and calling it done.
 */
export type EncodePlan =
  /** The server plays this file as it stands. Send it. */
  | { kind: 'ready' }
  /** It has to be encoded first, to this profile, with these backends to try. */
  | { kind: 'encode'; to: OvsProfile; backends: HevcBackend[]; reason: string }
  /** Nothing here can produce what the server needs, and this says why. */
  | { kind: 'unavailable'; reason: string };

/**
 * The arguments that turn `from` into an OVS asset at `into`.
 *
 * Every one of these is a clause of the contract made into a flag, and the
 * mapping is worth reading beside `profile.ts`:
 *
 * - `-pix_fmt` is the pixel format clause. Named rather than left to the
 *   encoder, which would otherwise pick whatever its input already was.
 * - `-g` and the scene-cut switch are the keyframe clause. Both are needed:
 *   `-g` sets the *maximum* and a scene-cut detector inserts extra keyframes,
 *   which is harmless, but without a minimum an encoder may also skip one.
 * - `-color_*` are the four tagging clauses. BT.709 unless the source says
 *   otherwise, because that is what HD is and what every tool assumes of a file
 *   that does not say.
 * - `-maxrate` is the bitrate clause, and it is the whole of it. What decides how
 *   many bits a clip actually gets is a *quality* target — `-cq` on NVENC,
 *   `-rate_control quality` on Media Foundation — so a clip that needs eight
 *   megabits gets eight and the ceiling is only ever a cap.
 *
 *   This used to set `-b:v` to four fifths of the ceiling, reasoning that a rate
 *   control allowed sixty would spend sixty. It would; and `-b:v` under VBR is
 *   not a limit but the instruction for how much to spend, so the reasoning
 *   arrived at telling the encoder to spend forty-eight. A 70 MB VP9 file at
 *   7–10 Mbit/s came back as 342 MB at 44 (owner, 2026-09-25). `-b:v 0` is what
 *   hands the decision to `-cq`.
 * - `-r` carries the frame rate rather than choosing one. A clip that arrives
 *   at 29.97 is prepared at 29.97, because a show cut against it was cut
 *   against that.
 * - Audio is dropped. This server has no audio path at all.
 *
 * B-frames are not disabled. They were, until the clause was measured on the
 * owner's own material and bought nothing (narrowed); leaving them to
 * the encoder is a little quality for free.
 */
/**
 * The quality `hevc_nvenc` is asked for, on its own 0–51 scale.
 *
 * **A quality target, because the only number the owner ever gave is a ceiling**
 * (2026-09-25: "die einzige aussage war max. 60MBit bei HD und 80 Bei UHD.. es
 * gabe keine referenzierte Bitrate von mir genannt"). A clip that needs eight
 * megabits gets eight; the ceiling is what it may not exceed, not what it is
 * asked to fill.
 *
 * Measured on the reference machine — twenty seconds of 1080p25 from a 6.7 Mbit/s
 * source, SSIM against that source:
 *
 * | setting | bitrate | × source | SSIM |
 * |---------|---------|----------|------|
 * | `-b:v 48M`, as it was | 39.8 Mbit/s | 5.95 | 0.9961 |
 * | `-cq 17` | 17.8 Mbit/s | 2.66 | 0.9926 |
 * | `-cq 19` | 14.2 Mbit/s | 2.12 | 0.9911 |
 * | `-cq 21` | 11.4 Mbit/s | 1.70 | 0.9893 |
 * | `-cq 23` | 9.1 Mbit/s | 1.36 | 0.9874 |
 * | `-cq 25` | 7.1 Mbit/s | 1.07 | 0.9849 |
 *
 * The old target spent 2.8 times the bits of `-cq 19` for five thousandths of
 * SSIM. **Eighteen is the owner's choice** from that table, a little to the
 * quality side of the knee.
 *
 * One number for both depths, which is what a quality target is for: ten-bit
 * material takes more bits to reach the same quality, and it is allowed to take
 * them, up to the one ceiling everything shares.
 */
export const CONSTANT_QUALITY = 18;

/**
 * The quality `hevc_mf` is asked for, on Media Foundation's own 0–100 scale.
 *
 * **Not measured, and that is a to-do rather than a footnote.** `hevc_mf` cannot
 * be run on the development machine at all — every frame fails with `80004005`,
 * whatever the rate control — so nothing here has been checked against a picture.
 * It is reached only on a machine with no NVENC, where it is the only encoder
 * there is.
 *
 * What stops a wrong value shipping something bad is `ingest.ts`: every encode is
 * probed again and checked against the contract, and a backend that produced
 * something the server will refuse falls through to the next one. What that will
 * *not* catch is a file that conforms and looks worse than it should, because the
 * contract has no quality clause — which is why this needs measuring on a machine
 * where the encoder runs.
 */
export const MEDIA_FOUNDATION_QUALITY = 70;

/**
 * The most a prepared clip may be, in lines.
 *
 * **Why an operator is asked at all.** A clip is prepared once and played all
 * evening, and what reaches a wall is never the clip: it is the composition,
 * cut into pieces, each piece landing on a panel. On the owner's own wall the
 * largest piece is 840x280, taken from a 1366x444 region of a 1080p
 * composition — so a 720p clip already puts more than one source pixel behind
 * every pixel out there, and a 4K one carries 8.3 megapixels across the bus to
 * show 0.235 of them.
 *
 * That waste is not free: every frame is copied into a staging buffer and then
 * onto the card, on the render thread, and the copy at 2160 is **four times**
 * the copy at 1080 (measured 2026-10-14: about 8.7 ms a layer at 1080p). The
 * venue machine's processor is a six-core from 2016, where it costs more than
 * it does here.
 *
 * So: a house that wants 1080 sets 1080, a wall of small panels sets 720, and
 * material that has to stay as it is stays as it is.
 */
export type UploadCeiling = 'source' | 2160 | 1080 | 720;

/**
 * The ceiling a show asks for, in the terms this code works in.
 *
 * The document says `720P`; the arithmetic wants 720. Two vocabularies on
 * purpose — a show is a file people read and keep for years, and a number in a
 * filter argument is neither — with the one translation between them in one
 * place and a test on it.
 */
export function ceilingOf(settings: { upload_ceiling?: string | null } | null | undefined): UploadCeiling {
  switch (settings?.upload_ceiling) {
    case '2160P':
      return 2160;
    case '1080P':
      return 1080;
    case '720P':
      return 720;
    default:
      return 'source';
  }
}

/** The same the other way, for writing a choice back into a show. */
export function ceilingAsWritten(ceiling: UploadCeiling): string {
  switch (ceiling) {
    case 2160:
      return '2160P';
    case 1080:
      return '1080P';
    case 720:
      return '720P';
    default:
      return 'SOURCE';
  }
}

/** The ceilings an operator may choose, in the order they are offered. */
export const UPLOAD_CEILINGS: readonly UploadCeiling[] = ['source', 2160, 1080, 720];

/** What each ceiling is called where somebody picks one. */
export const UPLOAD_CEILING_LABELS: Readonly<Record<string, string>> = {
  source: 'As it is',
  2160: '2160p (UHD)',
  1080: '1080p (HD)',
  720: '720p',
};

/**
 * Picture sizes are kept to a multiple of this.
 *
 * Kvazaar refuses a width or height that is not a multiple of eight outright —
 * FFmpeg's wrapper answers `ENOSYS` before the encoder sees a frame — and no
 * hardware encoder minds one. Eight is the number that works everywhere rather
 * than the smallest that would work here.
 */
const PICTURE_STEP = 8;

/**
 * The size a clip should be prepared at, or `null` to leave it alone.
 *
 * Never upscales: a ceiling is a ceiling, and a 720p source asked for 2160
 * gains nothing but four times the bytes. The width follows the source's shape
 * and is rounded to a multiple of eight, which can move it by at most four
 * pixels — a thousandth of the picture, against an encoder that would otherwise
 * refuse the file.
 */
export function preparedSize(
  source: { width: number; height: number },
  ceiling: UploadCeiling,
): { width: number; height: number } | null {
  if (ceiling === 'source') {
    return null;
  }
  const { width, height } = source;
  if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) {
    // Nothing is known about the picture, so nothing is decided about it.
    return null;
  }
  if (height <= ceiling) {
    return null;
  }
  const scaled = Math.round((width * ceiling) / height);
  const stepped = Math.max(PICTURE_STEP, Math.round(scaled / PICTURE_STEP) * PICTURE_STEP);
  return { width: stepped, height: ceiling };
}

/** The filter that puts a picture at `size`, for an encode that needs one. */
function scaleArguments(size: { width: number; height: number } | null): string[] {
  if (size === null) {
    return [];
  }
  // Lanczos, because this only ever makes a picture smaller, and a soft
  // downscale of a hard-edged graphic is where "the VJ loop looks mushy" comes
  // from.
  return ['-vf', `scale=${size.width}:${size.height}:flags=lanczos`];
}

export function hevcArguments(
  from: string,
  into: string,
  backend: HevcBackend,
  depth: ProfileDepth,
  rate: FrameRate,
  tags: ColourTags,
  readAs: readonly string[] = [],
  size: { width: number; height: number } | null = null,
): string[] {
  const keyframes = keyframeArguments(backend, rate);

  // **Said twice, to the decoder and to the encoder.** Since FFmpeg 7 an
  // encoder takes its colour properties from the frame the filter graph hands
  // it, not from the options on its own command line. The graph negotiates the
  // two that swscale can convert — the matrix and the range — and leaves the two
  // it cannot: a frame decoded from an untagged file arrives with its primaries
  // and transfer still "unspecified", and that is what the encoder writes.
  //
  // Measured on this machine, 2026-10-14, 1080p25 through `hevc_nvenc` with the
  // binary 0.2.19 ships, reading the four tags back off what came out:
  //
  // | source untagged, tags given        | primaries | transfer | matrix | range |
  // |------------------------------------|-----------|----------|--------|-------|
  // | on the output only, as it was      | *missing* | *missing*| bt709  | tv    |
  // | on the input as well               | bt709     | bt709    | bt709  | tv    |
  //
  // The first of those is a file the server refuses, so **every untagged source
  // on a machine with a hardware encoder** — a render, a screen recording,
  // anything out of a tool that does not tag — failed the contract after a
  // successful encode. Kvazaar escaped it only because its tags go in its own
  // parameter string, which is the one thing FFmpeg passes through untouched.
  //
  // Telling the decoder restates what a tagged file already says and gives an
  // untagged one the convention, which is `tagsFor`'s decision and not a new
  // one made here.
  const colour = [
    '-color_range',
    tags.range,
    '-colorspace',
    tags.matrix,
    '-color_primaries',
    tags.primaries,
    '-color_trc',
    tags.transfer,
  ];

  return [
    ...common(from, [...readAs, ...colour]),
    ...scaleArguments(size),
    '-c:v',
    backend,
    ...qualityArguments(backend, MOST_BITS_PER_SECOND, rate, tags),
    '-pix_fmt',
    PIXEL_FORMAT[depth],
    ...keyframes,
    ...colour,
    '-r',
    rateAsArgument(rate),
    // `hvc1` rather than `hev1`, which is the tag every player outside this
    // system expects of HEVC in an MP4 and costs nothing to write.
    '-tag:v',
    'hvc1',
    into,
  ];
}

/**
 * The arguments that turn `from` into a HAP Alpha asset at `into`.
 *
 * Snappy because the renderer decompresses it and it is most of the reason HAP
 * is worth having: a HAP frame is a texture the card samples as it is, and
 * Snappy is the difference between that texture being three megabytes on disc
 * and being eight.
 *
 * Four chunks so the decoder can decompress them in parallel. One is the
 * default and is a single-threaded decompress of the whole frame; more than
 * four buys nothing on the cores a show has spare.
 */
export function hapArguments(
  from: string,
  into: string,
  rate: FrameRate,
  readAs: readonly string[] = [],
  size: { width: number; height: number } | null = null,
): string[] {
  return [
    ...common(from, readAs),
    ...scaleArguments(size),
    '-c:v',
    'hap',
    '-format',
    'hap_alpha',
    '-compressor',
    'snappy',
    '-chunks',
    '4',
    '-r',
    rateAsArgument(rate),
    into,
  ];
}

/** What a prepared file should be called, beside the one it came from. */
export function preparedName(fileName: string, to: OvsProfile): string {
  const withoutExtension = fileName.replace(/\.[^.\\/]+$/, '');
  if (to === 'hap') return `${withoutExtension}.hap.mov`;
  return `${withoutExtension}.${to === 'hevc_10' ? 'hevc10' : 'hevc'}.mp4`;
}

/**
 * The colour tags to write, which are the source's own where it has them.
 *
 * A file that says nothing is tagged BT.709 limited, which is what HD is and
 * what every tool assumes of an untagged file. Carrying a wrong tag forward
 * would be worse than choosing the conventional one, and inventing BT.2020 for
 * a file that never claimed it would be worse still.
 */
export interface ColourTags {
  primaries: string;
  transfer: string;
  matrix: string;
  range: string;
}

/** BT.709, limited range: the convention for anything HD that does not say. */
export const CONVENTIONAL_TAGS: ColourTags = {
  primaries: 'bt709',
  transfer: 'bt709',
  matrix: 'bt709',
  range: 'tv',
};

/** The tags to write for a source, which are its own where it has them. */
export function tagsFor(source: Partial<ColourTags>): ColourTags {
  const kept = (value: string | undefined, fallback: string): string =>
    value === undefined || value === '' || value === 'unknown' ? fallback : value;
  return {
    primaries: kept(source.primaries, CONVENTIONAL_TAGS.primaries),
    transfer: kept(source.transfer, CONVENTIONAL_TAGS.transfer),
    matrix: kept(source.matrix, CONVENTIONAL_TAGS.matrix),
    range: kept(source.range, CONVENTIONAL_TAGS.range),
  };
}

/**
 * A frame rate as FFmpeg wants it on the command line.
 *
 * As a fraction, not a decimal. `30000/1001` is exact and `29.97` is not, and a
 * clip prepared at the rounded rate drifts against the one it was cut with.
 */
export function rateAsArgument(rate: FrameRate): string {
  const denominator = rate.denominator === 0 ? 1 : rate.denominator;
  return denominator === 1 ? `${rate.numerator}` : `${rate.numerator}/${denominator}`;
}

/** The frame rate as a plain number, for working out a keyframe interval. */
function ratePerSecond(rate: FrameRate): number {
  const denominator = rate.denominator === 0 ? 1 : rate.denominator;
  const perSecond = rate.numerator / denominator;
  return Number.isFinite(perSecond) && perSecond > 0 ? perSecond : 25;
}

/** How many frames a second, as a whole number, which is the keyframe period. */
function framesPerKeyframe(rate: FrameRate): string {
  return String(Math.max(1, Math.round(ratePerSecond(rate))));
}

/**
 * The keyframe flags, which the backends spell differently.
 *
 * NVENC takes `-no-scenecut`; the rest have no scene-cut detector to switch off
 * and take the GOP size alone. All of them take `-g`, and all are given
 * `-keyint_min` so that an encoder cannot decide a scene is dull enough to skip
 * one — which is exactly the file that seeks badly on the night.
 *
 * Kvazaar is the exception and gets nothing here: it takes every setting it has
 * as one string, the keyframe period included, and `-g` beside that string
 * would be a second way of saying the same thing that could disagree with it.
 */
function keyframeArguments(backend: HevcBackend, rate: FrameRate): string[] {
  if (backend === 'libkvazaar') {
    return [];
  }
  const every = framesPerKeyframe(rate);
  const shared = ['-g', every, '-keyint_min', every];
  return backend === 'hevc_nvenc' ? [...shared, '-no-scenecut', '1'] : shared;
}

/**
 * The length of Kvazaar's B-frame group, said here rather than inherited.
 *
 * Its presets each set one — `veryfast` sets `--gop=8` — and a keyframe period
 * that is not a multiple of it is refused outright rather than rounded:
 *
 *     Input error: intra period (25) not a multiple of B-gop length (8)
 *
 * The encoder then does not open, FFmpeg's wrapper says so, the scheduler
 * flattens that to `-22` two layers further out, and the client used to keep
 * only those outer layers — which is how the software path failed on every
 * machine without a hardware encoder from 0.2.16 to 0.2.18 and never once said
 * why. The client asks for a keyframe a second, a second at 25 fps is 25
 * frames, and 25 is not a multiple of 8.
 *
 * Stating the GOP length here puts both halves of that arithmetic in one place,
 * so a preset that chose a different one cannot quietly break it again.
 */
const KVAZAAR_GOP = 8;

/**
 * The names Kvazaar knows a colour tag by, where they differ from FFmpeg's.
 *
 * Both tools use H.273's short names, so nearly everything passes through
 * unchanged. These four do not: FFmpeg calls the two old transfer curves
 * `gamma22` and `gamma28` where Kvazaar uses the recommendation numbers, and
 * the identity matrix is `gbr` to ffprobe and `GBR` here.
 *
 * A name Kvazaar does not know — `smpte2084`, `arib-std-b67`, anything HDR — is
 * passed through as it is rather than replaced with a guess. Kvazaar then
 * ignores that one setting, the file comes out untagged in that respect, and
 * `ingest.ts` refuses it and names the tag. Mislabelling a file would be worse,
 * and quieter.
 */
const KVAZAAR_TAGS: Readonly<Record<string, string>> = {
  gamma22: 'bt470m',
  gamma28: 'bt470bg',
  gbr: 'GBR',
  rgb: 'GBR',
  ycgco: 'YCgCo',
};

/** One colour tag, under the name Kvazaar knows it by. */
function kvazaarTag(tag: string): string {
  return KVAZAAR_TAGS[tag] ?? tag;
}

/**
 * The keyframe period Kvazaar will accept, with the B-gop it has to match.
 *
 * Rounded **down** to a multiple of the GOP length, because the contract is a
 * ceiling: `MOST_SECONDS_BETWEEN_KEYFRAMES` is the longest a clip may go
 * without a keyframe, so at 25 fps 24 frames keeps it and 32 would break it.
 *
 * Under one GOP there is no multiple to round to — a clip at 5 fps may go five
 * frames — so the B-gop is switched off instead, which is the other case
 * Kvazaar exempts from the rule.
 */
function kvazaarKeyframes(rate: FrameRate): { gop: number; period: number } {
  const most = Number(framesPerKeyframe(rate));
  if (most < KVAZAAR_GOP) {
    return { gop: 0, period: most };
  }
  return { gop: KVAZAAR_GOP, period: most - (most % KVAZAAR_GOP) };
}

/**
 * The rate-control flags, which the two backends spell entirely differently.
 *
 * NVENC's `p5` is the middle of its seven presets: the point past which more time
 * buys very little, measured rather than assumed. `-cq` with `-b:v 0` is its
 * constant-quality mode — the zero matters, because a bitrate left set would
 * override the quality target and is how this came to spend forty-eight megabits
 * on an eight-megabit clip. `-maxrate` and `-bufsize` keep the cap: a second's
 * worth of buffer, so the ceiling is a ceiling on any given second rather than on
 * the average of the whole clip.
 *
 * Media Foundation has no preset worth naming and no way to cap a quality-mode
 * encode, so it gets the quality target alone. Nothing caps it, and what catches a
 * result over the ceiling is the probe `ingest.ts` runs on every prepared file
 * before it is sent.
 */
function qualityArguments(
  backend: HevcBackend,
  ceiling: number,
  rate: FrameRate,
  tags: ColourTags,
): string[] {
  if (backend === 'hevc_nvenc') {
    return [
      '-preset',
      'p5',
      '-rc',
      'vbr',
      '-cq',
      String(CONSTANT_QUALITY),
      // Zero, not absent: a bitrate that is set beats the quality target.
      '-b:v',
      '0',
      '-maxrate',
      String(ceiling),
      '-bufsize',
      String(ceiling),
    ];
  }
  if (backend === 'hevc_qsv') {
    // Intel's constant-quality mode, and the zero matters for the same reason
    // it does on NVENC: a bitrate that is set beats the quality target.
    return ['-global_quality', String(CONSTANT_QUALITY), '-b:v', '0'];
  }
  if (backend === 'hevc_amf') {
    // AMF's constant quantiser, given to both frame kinds so that a quiet
    // stretch is not held to a different standard from a busy one.
    return [
      '-rc',
      'cqp',
      '-qp_i',
      String(CONSTANT_QUALITY),
      '-qp_p',
      String(CONSTANT_QUALITY),
    ];
  }
  if (backend === 'libkvazaar') {
    // Everything Kvazaar takes, in the one string it takes it in: the preset,
    // the quantiser, the B-gop, the keyframe period and the colour tags.
    // `veryfast` because this is the path a machine with no encoder at all goes
    // down, where what an operator cares about is whether it finishes before
    // doors — a slower preset buys picture quality nobody is comparing against
    // anything.
    //
    // **The colour tags have to be in here too.** FFmpeg's wrapper hands
    // Kvazaar the dimensions, the frame rate and this string, and nothing else:
    // `-color_primaries` and `-color_trc` never reach it, so the stream it
    // writes carries no colour description at all. A file like that is one the
    // server refuses — "colour primaries: wanted tagged, found untagged",
    // measured on a release runner on 2026-10-13, after the encode itself had
    // finally started working — so the whole encode would be thrown away at the
    // last step. HEVC's VUI gates all three of primaries, transfer and matrix
    // on one flag, which is why all three are named rather than only the two
    // that were missing.
    const keyframes = kvazaarKeyframes(rate);
    return [
      '-kvazaar-params',
      [
        'preset=veryfast',
        `qp=${CONSTANT_QUALITY}`,
        `gop=${keyframes.gop}`,
        `period=${keyframes.period}`,
        `colorprim=${kvazaarTag(tags.primaries)}`,
        `transfer=${kvazaarTag(tags.transfer)}`,
        `colormatrix=${kvazaarTag(tags.matrix)}`,
        `range=${tags.range === 'pc' || tags.range === 'full' ? 'pc' : 'tv'}`,
      ].join(','),
    ];
  }
  return ['-rate_control', 'quality', '-quality', String(MEDIA_FOUNDATION_QUALITY)];
}

/**
 * How far an encode has got, from one line of FFmpeg's progress output.
 *
 * FFmpeg writes `key=value` a line at a time and `progress=continue` at the
 * end of each group. The only field worth reading is `out_time_us`, which is
 * where the output has got to; dividing it by what the probe said the file
 * lasts gives a fraction that is right rather than a spinner that is not.
 *
 * Returns `null` for a line that says nothing about time, which is most of
 * them.
 */
export function progressFrom(line: string): number | null {
  const [key, value] = line.split('=', 2);
  if (key === undefined || value === undefined) {
    return null;
  }
  if (key.trim() !== 'out_time_us' && key.trim() !== 'out_time_ms') {
    return null;
  }
  const raw = Number.parseInt(value.trim(), 10);
  if (!Number.isFinite(raw) || raw < 0) {
    return null;
  }
  // `out_time_ms` is a misnomer in FFmpeg: it is microseconds, the same as
  // `out_time_us`. Treating it as milliseconds would report a preparation as
  // a thousand times further along than it is.
  return raw / 1_000_000;
}

/**
 * Lines that are FFmpeg tidying up rather than FFmpeg saying what went wrong.
 *
 * Each of these is written by whichever part noticed the failure *after* the
 * part that caused it, so each one pushes the cause further out of view.
 */
const BOOKKEEPING = [
  /Task finished with error code/,
  /Terminating thread with return code/,
  /Nothing was written into output file/,
  /^Conversion failed!?$/,
];

/**
 * How many of the telling lines are worth putting in front of an operator.
 *
 * Six, because the cause can sit above three consequences and still not be the
 * last thing said. The Kvazaar refusal read, in order: Kvazaar's own `Input
 * error: intra period (25) not a multiple of B-gop length (8)`, then "Could not
 * open kvazaar encoder", then FFmpeg's guess at why, then a filter graph
 * reporting an internal bug, then "Could not open encoder before EOF". Four
 * lines kept the diagnosis and lost the reason by one line (2026-10-13).
 */
const LINES_KEPT = 6;

/**
 * The part of what FFmpeg said that says why it stopped.
 *
 * **The last three lines are the wrong three lines.** FFmpeg 7 runs each
 * output stream as a task, and when one fails, the end of standard error is
 * the scheduler's account of the funeral rather than the cause of death:
 *
 *     [vost#0:0/libkvazaar] Task finished with error code: -22 (Invalid argument)
 *     [vost#0:0/libkvazaar] Terminating thread with return code -22 (Invalid argument)
 *     [out#0/mp4] Nothing was written into output file, because at least one of
 *     its streams received no packets.
 *
 * Three lines, not one of them the reason, and the encoder's own complaint
 * pushed out above them. That is the message an operator reads at a venue, and
 * it is also what came back from CI -- so a software encoder that refused every
 * frame cost two round trips and said nothing either time (2026-10-13).
 *
 * So: the bookkeeping dropped as long as anything else remains, the last few of
 * what is left, and the bookkeeping on its own if that is genuinely all there
 * was -- an operator is better served by "error code: -22" than by silence.
 */
export function whyItStopped(said: string): string {
  const lines = said
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line !== '');
  const telling = lines.filter((line) => !BOOKKEEPING.some((pattern) => pattern.test(line)));
  return (telling.length > 0 ? telling : lines).slice(-LINES_KEPT).join('; ');
}

/**
 * The flags every encode takes, whatever it is producing.
 *
 * `readAs` is what to put before `-i`, which is the only place FFmpeg takes an
 * input's own options. Empty for a file that says what it is; the encoder trial
 * passes the rawvideo demuxer's, because its one frame is a headerless file.
 */
function common(from: string, readAs: readonly string[] = []): string[] {
  return [
    '-nostdin',
    '-hide_banner',
    // **Warnings as well as errors.** What is kept of this is the last four
    // kilobytes, and only when the encode failed -- so a successful one costs
    // nothing and a failed one says why. An encoder that refused one of its own
    // options says so at warning level and then fails with a number
    // ("Task finished with error code: -22"), and the number on its own sent me
    // reading FFmpeg's source instead of its output (2026-10-13).
    '-loglevel',
    'warning',
    // Progress as parseable lines rather than the status line FFmpeg writes for
    // a person.
    '-progress',
    'pipe:1',
    '-nostats',
    '-y',
    ...readAs,
    '-i',
    from,
    '-an',
    '-map',
    '0:v:0',
  ];
}

/**
 * What to do with a file, given what it is and what this machine can encode.
 *
 * `video` must be **fully measured**, keyframe interval included. A verdict of
 * `ready` on a file whose packets were never read would be this client telling
 * the server a file conforms when nobody checked the one clause that costs
 * anything — see `passesWithoutReadingPackets`, which is how to decide whether
 * paying for that measurement is worth it.
 *
 * `encoders` is what `ffmpeg -encoders` reported, so that a client running
 * against a replaced binary answers from what the binary has rather than from
 * what the build was meant to include.
 */
export function planFor(video: ProbedVideo, encoders: ReadonlySet<string>): EncodePlan {
  const already = profileOf(video);
  if (faults(video, already).length === 0) {
    return { kind: 'ready' };
  }

  const to = targetFor(video);

  if (to === 'hap') {
    if (!encoders.has('hap')) {
      return {
        kind: 'unavailable',
        reason:
          'This file carries alpha, so it has to become HAP before the server will take it, and this copy of FFmpeg has no HAP encoder.',
      };
    }
    return {
      kind: 'encode',
      to,
      backends: [],
      reason: 'This file carries alpha. It will be prepared as HAP Alpha.',
    };
  }

  const backends = backendsAvailable(encoders);
  if (backends.length === 0) {
    // The owner's third case, 2026-09-23: do not silently produce something
    // else. HAP is named because it is the one alternative that would work, and
    // it is named rather than chosen because a HAP file is several times the
    // size and nobody should find that out afterwards.
    return {
      kind: 'unavailable',
      reason:
        'This file has to be re-encoded as HEVC before the server will take it, and this machine has no HEVC encoder — neither an NVIDIA card nor Media Foundation. It could be prepared as HAP instead, which plays just as well and is several times the size.',
    };
  }

  return {
    kind: 'encode',
    to,
    backends,
    reason: `This file is not in a form the server plays. It will be prepared as ${
      to === 'hevc_10' ? 'HEVC 10-bit' : 'HEVC 8-bit'
    }.`,
  };
}
