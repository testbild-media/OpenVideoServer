// What the server plays, and therefore what this client has to produce.
//
// The same contract as `ovs_types::media::PlaybackProfile` and `faults`, in
// TypeScript, because the decision has to be made **before** a byte crosses the
// network. A client that uploaded first and asked afterwards would spend an
// operator's evening sending a gigabyte to be told no.
//
// # Two copies of one rule
//
// Which is a thing this repository otherwise refuses to do, and the reason it
// is allowed here is the same reason `limits.ts` is allowed to exist: the rule
// is arithmetic the server owns, the client needs it locally, and
// `test/profile.test.ts` reads the Rust and fails when the two drift. Nothing
// here is a number somebody chose in this client. Every one of them has a line
// in `crates/ovs-types/src/media/playback_profile.rs` that the drift test
// points at.
//
// # What it does not decide
//
// Which encoder produces the file. That is `encoder.ts`, it is automatic, and
// an operator never sees it (owner, 2026-09-23). This module only says what the
// output has to look like — which is the whole point of writing the contract
// down separately from the thing that satisfies it.

/** One of the three things the server plays. */
export type OvsProfile = 'hevc_8' | 'hevc_10' | 'hap';

/** Eight or ten bits a sample, for the HEVC profiles. */
export type ProfileDepth = 8 | 10;

/**
 * The pixel format each HEVC depth must be in.
 *
 * `BitDepth::pixel_format`. Named rather than inferred so that an encoder
 * cannot quietly pick 4:2:2 or a different endianness: those decode, and they
 * decode through a per-frame conversion on the processor, which is the cost
 * this whole policy exists to avoid.
 */
export const PIXEL_FORMAT: Readonly<Record<ProfileDepth, string>> = {
  8: 'yuv420p',
  10: 'yuv420p10le',
};

/**
 * The most bits a second a clip may carry.
 *
 * `MOST_BITS_PER_SECOND`. Sixty megabits, whatever the depth and whatever the
 * raster (owner, 2026-09-25: "einfach cq18 max. 60MBit immer"). A ceiling and
 * nothing else: what decides how many bits a clip actually gets is the encoder's
 * quality target, in `encoder.ts`.
 */
export const MOST_BITS_PER_SECOND = 60_000_000;

/**
 * The longest a clip may go without a keyframe, in seconds.
 *
 * `MOST_SECONDS_BETWEEN_KEYFRAMES`. A slot change is a seek, and a seek costs
 * the distance back to the last keyframe.
 */
export const MOST_SECONDS_BETWEEN_KEYFRAMES = 1;

/** The codec name a probe reports for each profile. */
export function codecOf(profile: OvsProfile): string {
  return profile === 'hap' ? 'hap' : 'hevc';
}

/** Which depth a HEVC profile is, or null for HAP. */
export function depthOf(profile: OvsProfile): ProfileDepth | null {
  if (profile === 'hevc_8') return 8;
  if (profile === 'hevc_10') return 10;
  return null;
}

/**
 * What a probe found, in the terms the contract is written in.
 *
 * Mirrors `ovs_types::media::ProbedVideo`. `keyframeInterval` is null when it
 * has not been measured — reading it costs a pass over the packets, so it is
 * asked for only when everything cheaper has already passed and the answer
 * could still make the file usable as it stands.
 */
export interface ProbedVideo {
  codec: string;
  pixelFormat: string;
  hasAlpha: boolean;
  primaries: string;
  transfer: string;
  matrix: string;
  range: string;
  frameRate: number;
  nominalFrameRate: number;
  keyframeInterval: number | null;
  sampleAspect: readonly [number, number];
  interlaced: boolean;
  bitsPerSecond: number;
}

/** Why a file is not something the server will play. */
export interface ProfileFault {
  /** What was wrong, in the operator's terms. */
  what: string;
  /** What the contract asks for. */
  wanted: string;
  /** What the file has. */
  found: string;
}

/**
 * Which of the three a file is claiming to be.
 *
 * `PlaybackProfile::of`. Not whether it *is* one — that is `faults` — but what
 * to ask it about, so the list an operator reads is the list of things to
 * change rather than "this is not the other thing".
 */
export function profileOf(video: ProbedVideo): OvsProfile {
  if (video.codec === 'hap') return 'hap';
  return video.pixelFormat === PIXEL_FORMAT[10] ? 'hevc_10' : 'hevc_8';
}

/**
 * Which profile a file should *become*, which is not the same question.
 *
 * The owner's routing of 2026-09-23: alpha goes to HAP because HAP was made for
 * exactly this and arrives as a texture already; everything else goes to HEVC,
 * at the depth the source already is. Ten-bit source stays ten-bit — widening
 * an eight-bit file buys nothing and costs a third more bitrate, and narrowing
 * a ten-bit one throws away what it was mastered for.
 */
export function targetFor(video: ProbedVideo): OvsProfile {
  if (video.hasAlpha) return 'hap';
  return isTenBit(video.pixelFormat) ? 'hevc_10' : 'hevc_8';
}

/**
 * Whether a pixel format carries more than eight bits a sample.
 *
 * By name, because that is what a prober reports and the alternative is a table
 * of every format FFmpeg knows. Anything with a bit depth in its name above
 * eight counts: `yuv420p10le`, `yuv422p12le`, `p010le`, `gbrp16le`.
 */
export function isTenBit(pixelFormat: string): boolean {
  const bits = /(\d+)(?:le|be)$/.exec(pixelFormat.trim().toLowerCase());
  return bits !== null && Number(bits[1]) > 8;
}

/**
 * The most frames a clip at this rate may go without a keyframe.
 *
 * `keyframes_at_most`. At least one, because a rate a container reported as
 * zero must not turn into "no keyframes required".
 */
export function keyframesAtMost(frameRate: number): number {
  if (!Number.isFinite(frameRate) || frameRate <= 0) return 1;
  return Math.max(1, Math.round(frameRate * MOST_SECONDS_BETWEEN_KEYFRAMES));
}

/**
 * Whether two reported rates are the same rate.
 *
 * `rates_agree`. Within a thousandth, because a container stores a rate as a
 * fraction and two ways of asking it can disagree in the last place.
 */
export function ratesAgree(one: number, other: number): boolean {
  if (!Number.isFinite(one) || !Number.isFinite(other)) return false;
  return Math.abs(one - other) <= 0.001 * Math.max(Math.abs(one), Math.abs(other), 1);
}

/**
 * Checks a probed file against the profile it claims to be.
 *
 * `faults`. Every fault rather than the first, because an operator fixing a
 * file wants the whole list and this client wants to know whether one fault or
 * eight stand between it and a usable asset.
 *
 * Empty for a file the server will play as it stands.
 */
export function faults(video: ProbedVideo, profile: OvsProfile): ProfileFault[] {
  const out: ProfileFault[] = [];
  const codec = codecOf(profile);

  if (video.codec !== codec) {
    // And nothing else. A file in the wrong codec fails every question below it
    // as well, and listing those would bury the one fault that matters.
    return [{ what: 'codec', wanted: codec, found: video.codec }];
  }

  const depth = depthOf(profile);
  if (depth === null) {
    // A HAP file is a texture already: no pixel format to get wrong, no matrix
    // to leave out, every frame a keyframe and nothing reordered.
    return out;
  }

  if (video.pixelFormat !== PIXEL_FORMAT[depth]) {
    out.push({ what: 'pixel format', wanted: PIXEL_FORMAT[depth], found: video.pixelFormat });
  }
  if (video.hasAlpha) {
    out.push({ what: 'alpha', wanted: 'none; material with alpha is HAP', found: 'present' });
  }

  // Tagged, not merely plausible. An untagged file is one the renderer has to
  // guess about, and two renderers guessing differently is a picture that
  // changes when the machine does.
  for (const [what, value] of [
    ['colour primaries', video.primaries],
    ['transfer characteristics', video.transfer],
    ['colour matrix', video.matrix],
    ['colour range', video.range],
  ] as const) {
    if (value === '' || value === 'unknown') {
      out.push({ what, wanted: 'tagged', found: 'untagged' });
    }
  }

  if (!ratesAgree(video.frameRate, video.nominalFrameRate)) {
    out.push({
      what: 'frame rate',
      wanted: 'constant',
      found: `variable: ${video.frameRate.toFixed(3)} average against ${video.nominalFrameRate.toFixed(3)} nominal`,
    });
  }

  // Null is "not measured", which is not "fine". A caller that has not paid for
  // the packet pass gets no verdict on this clause rather than a pass.
  if (video.keyframeInterval !== null) {
    const most = keyframesAtMost(video.nominalFrameRate);
    if (video.keyframeInterval > most) {
      out.push({
        what: 'keyframe interval',
        wanted: `at most ${most} frames, which is one second`,
        found: `${video.keyframeInterval} frames`,
      });
    }
  }

  const [numerator, denominator] = video.sampleAspect;
  if (numerator !== 1 || denominator !== 1) {
    out.push({ what: 'sample aspect', wanted: '1:1', found: `${numerator}:${denominator}` });
  }

  if (video.interlaced) {
    out.push({ what: 'scan', wanted: 'progressive', found: 'interlaced' });
  }

  const ceiling = MOST_BITS_PER_SECOND;
  if (video.bitsPerSecond > ceiling) {
    out.push({
      what: 'bitrate',
      wanted: `at most ${ceiling / 1_000_000} Mbit/s`,
      found: `${(video.bitsPerSecond / 1_000_000).toFixed(1)} Mbit/s`,
    });
  }

  return out;
}

/**
 * Whether the cheap half of the contract passes.
 *
 * Everything a header answers. A file that fails any of these is being
 * re-encoded whatever its keyframes are, so there is no point paying for the
 * packet pass to find out — which is the only reason this exists separately.
 */
export function passesWithoutReadingPackets(video: ProbedVideo, profile: OvsProfile): boolean {
  return faults({ ...video, keyframeInterval: null }, profile).length === 0;
}
