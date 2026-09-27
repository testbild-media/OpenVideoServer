// The contract this client has to produce, and the encoders that produce it.
//
// Two kinds of test here. The first kind checks the arithmetic: that a file
// which conforms is left alone and one that does not is described accurately.
// The second kind reads `crates/ovs-types/src/media/playback_profile.rs` and
// fails when the two copies drift — the same arrangement `limits.test.ts` uses,
// and for the same reason: a client that guessed a ceiling would spend an
// operator's evening producing a file the server then refuses.

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import {
  backendsAvailable,
  hapArguments,
  hevcArguments,
  planFor,
  preparedName,
  progressFrom,
  rateAsArgument,
  tagsFor,
  CONSTANT_QUALITY,
  CONVENTIONAL_TAGS,
  HEVC_BACKENDS,
  MEDIA_FOUNDATION_QUALITY,
} from '../src/shared/encoder.js';
import {
  faults,
  isTenBit,
  keyframesAtMost,
  passesWithoutReadingPackets,
  profileOf,
  ratesAgree,
  targetFor,
  MOST_BITS_PER_SECOND,
  MOST_SECONDS_BETWEEN_KEYFRAMES,
  PIXEL_FORMAT,
  type ProbedVideo,
} from '../src/shared/profile.js';

const here = dirname(fileURLToPath(import.meta.url));
const profileRs = readFileSync(
  join(here, '../../../crates/ovs-types/src/media/playback_profile.rs'),
  'utf8',
);

/**
 * The owner's `HEVC.mp4` after it has been prepared: every clause met.
 *
 * The same numbers the Rust fixture uses, read off a real file on 2026-09-23.
 */
function conforming(): ProbedVideo {
  return {
    codec: 'hevc',
    pixelFormat: 'yuv420p',
    hasAlpha: false,
    primaries: 'bt709',
    transfer: 'bt709',
    matrix: 'bt709',
    range: 'tv',
    frameRate: 50,
    nominalFrameRate: 50,
    keyframeInterval: 50,
    sampleAspect: [1, 1],
    interlaced: false,
    bitsPerSecond: 48_600_000,
  };
}

describe('what the server will play', () => {
  it('accepts a file that meets every clause', () => {
    expect(faults(conforming(), 'hevc_8')).toEqual([]);
  });

  it('refuses the owner’s own HEVC file, and for the keyframes alone', () => {
    // The point of the whole policy on a real file: it is HEVC, 8-bit, tagged,
    // progressive, constant-rate and inside the ceiling, and its keyframes are
    // 253 frames apart, which at fifty a second is five seconds.
    const delivered: ProbedVideo = { ...conforming(), keyframeInterval: 253 };
    expect(faults(delivered, 'hevc_8').map((fault) => fault.what)).toEqual([
      'keyframe interval',
    ]);
  });

  it('says only the codec when the codec is wrong', () => {
    // A ProRes file has no HEVC pixel format and no HEVC anything; listing the
    // consequences would bury the one fault that matters.
    const prores: ProbedVideo = { ...conforming(), codec: 'prores' };
    expect(faults(prores, 'hevc_8').map((fault) => fault.what)).toEqual(['codec']);
  });

  it('asks a HAP file none of the HEVC questions', () => {
    const hap: ProbedVideo = {
      ...conforming(),
      codec: 'hap',
      pixelFormat: 'rgba',
      hasAlpha: true,
      primaries: '',
      transfer: '',
      range: '',
      keyframeInterval: 1,
    };
    expect(faults(hap, 'hap')).toEqual([]);
  });

  it('treats an unmeasured keyframe interval as unanswered rather than as fine', () => {
    // The clause costs a pass over the packets. A caller that has not paid for
    // it gets no verdict on it — it must not silently pass.
    const unmeasured: ProbedVideo = { ...conforming(), keyframeInterval: null };
    expect(faults(unmeasured, 'hevc_8')).toEqual([]);
    expect(passesWithoutReadingPackets({ ...conforming(), keyframeInterval: 253 }, 'hevc_8')).toBe(
      true,
    );
    // And a file that fails something cheaper needs no packet pass at all.
    expect(passesWithoutReadingPackets({ ...conforming(), range: '' }, 'hevc_8')).toBe(false);
  });

  it('names every fault at once, because one refusal per re-encode is cruel', () => {
    const bad: ProbedVideo = {
      ...conforming(),
      pixelFormat: 'yuv422p',
      matrix: '',
      keyframeInterval: 250,
      sampleAspect: [16, 15],
      interlaced: true,
      bitsPerSecond: 120_000_000,
    };
    expect(faults(bad, 'hevc_8').map((fault) => fault.what)).toEqual([
      'pixel format',
      'colour matrix',
      'keyframe interval',
      'sample aspect',
      'scan',
      'bitrate',
    ]);
  });

  it('accepts B-frames, because the measurement said to', () => {
    // There is nothing to assert about them any more, which is the assertion:
    // no clause mentions frame order.
    expect(profileRs).not.toContain('frame order');
  });
});

describe('which of the three a file is, and which it should become', () => {
  it('asks a file about the profile it is nearest to', () => {
    expect(profileOf(conforming())).toBe('hevc_8');
    expect(profileOf({ ...conforming(), pixelFormat: 'yuv420p10le' })).toBe('hevc_10');
    expect(profileOf({ ...conforming(), codec: 'hap' })).toBe('hap');
  });

  it('routes alpha to HAP and everything else to HEVC at its own depth', () => {
    expect(targetFor({ ...conforming(), codec: 'prores', hasAlpha: true })).toBe('hap');
    expect(targetFor({ ...conforming(), codec: 'prores', pixelFormat: 'yuv420p' })).toBe('hevc_8');
    // Ten-bit source stays ten-bit: narrowing throws away what it was mastered
    // for, and widening costs a third more bitrate for nothing.
    expect(targetFor({ ...conforming(), codec: 'vp9', pixelFormat: 'yuv420p10le' })).toBe('hevc_10');
    expect(targetFor({ ...conforming(), codec: 'prores', pixelFormat: 'yuv422p12le' })).toBe(
      'hevc_10',
    );
  });

  it('reads a bit depth out of a pixel format’s name', () => {
    expect(isTenBit('yuv420p')).toBe(false);
    expect(isTenBit('rgba')).toBe(false);
    expect(isTenBit('yuv420p10le')).toBe(true);
    expect(isTenBit('p010le')).toBe(true);
    expect(isTenBit('yuva444p12le')).toBe(true);
    expect(isTenBit('gbrp16le')).toBe(true);
  });

  it('never asks for no keyframes at all', () => {
    expect(keyframesAtMost(50)).toBe(50);
    expect(keyframesAtMost(59.94)).toBe(60);
    expect(keyframesAtMost(0)).toBe(1);
    expect(keyframesAtMost(Number.NaN)).toBe(1);
  });

  it('calls two readings of one rate the same rate', () => {
    expect(ratesAgree(59.94, 60000 / 1001)).toBe(true);
    expect(ratesAgree(50, 50)).toBe(true);
    expect(ratesAgree(50, 25)).toBe(false);
    expect(ratesAgree(Number.NaN, 50)).toBe(false);
  });
});

describe('which encoder, and what it is told', () => {
  const everything = new Set(['hevc_nvenc', 'hevc_mf', 'hap']);

  it('leaves a conforming file alone', () => {
    expect(planFor(conforming(), everything)).toEqual({ kind: 'ready' });
  });

  it('prefers NVIDIA and falls back to Media Foundation, never asking anybody', () => {
    const plan = planFor({ ...conforming(), codec: 'vp9' }, everything);
    expect(plan.kind).toBe('encode');
    if (plan.kind !== 'encode') return;
    expect(plan.backends).toEqual(['hevc_nvenc', 'hevc_mf']);
    // And the reason an operator reads names the format, never the backend.
    expect(plan.reason).not.toContain('nvenc');
    expect(plan.reason).not.toContain('Media Foundation');

    const windowsOnly = planFor({ ...conforming(), codec: 'vp9' }, new Set(['hevc_mf', 'hap']));
    expect(windowsOnly.kind === 'encode' && windowsOnly.backends).toEqual(['hevc_mf']);
  });

  it('sends alpha to HAP whatever HEVC encoders are about', () => {
    const plan = planFor({ ...conforming(), codec: 'prores', hasAlpha: true }, everything);
    expect(plan.kind === 'encode' && plan.to).toBe('hap');
    expect(plan.kind === 'encode' && plan.backends).toEqual([]);
  });

  it('says so rather than producing something else, when it cannot produce HEVC', () => {
    // The owner's third case. Not a silent fall back to HAP: a HAP file is
    // several times the size and nobody should find that out afterwards.
    const plan = planFor({ ...conforming(), codec: 'vp9' }, new Set(['hap']));
    expect(plan.kind).toBe('unavailable');
    if (plan.kind !== 'unavailable') return;
    expect(plan.reason).toContain('HEVC');
    expect(plan.reason).toContain('HAP');
  });

  it('says so when it cannot produce HAP either', () => {
    const plan = planFor({ ...conforming(), codec: 'prores', hasAlpha: true }, new Set(['hevc_mf']));
    expect(plan.kind).toBe('unavailable');
  });

  it('only offers backends the binary actually has', () => {
    expect(backendsAvailable(new Set())).toEqual([]);
    expect(backendsAvailable(new Set(['hevc_nvenc']))).toEqual(['hevc_nvenc']);
    expect(backendsAvailable(new Set(['hevc_mf', 'hevc_nvenc']))).toEqual(HEVC_BACKENDS.slice());
  });
});

describe('the arguments that satisfy the contract', () => {
  const rate = { numerator: 50, denominator: 1 };

  it('write every clause the contract asks about', () => {
    const args = hevcArguments('in.mov', 'out.mp4', 'hevc_nvenc', 8, rate, CONVENTIONAL_TAGS);
    const after = (flag: string): string | undefined => args[args.indexOf(flag) + 1];

    expect(after('-c:v')).toBe('hevc_nvenc');
    expect(after('-pix_fmt')).toBe(PIXEL_FORMAT[8]);
    expect(after('-g')).toBe('50');
    expect(after('-keyint_min')).toBe('50');
    expect(after('-no-scenecut')).toBe('1');
    expect(after('-color_primaries')).toBe('bt709');
    expect(after('-color_trc')).toBe('bt709');
    expect(after('-colorspace')).toBe('bt709');
    expect(after('-color_range')).toBe('tv');
    expect(after('-r')).toBe('50');
    expect(after('-tag:v')).toBe('hvc1');
    // **A ceiling and a quality target, not a bitrate to spend.** `-b:v` has to be
    // exactly zero: a bitrate left set overrides `-cq`, and that is how a
    // 7 Mbit/s source came back at 44 (owner, 2026-09-25).
    expect(after('-cq')).toBe(String(CONSTANT_QUALITY));
    expect(after('-b:v')).toBe('0');
    expect(Number(after('-maxrate'))).toBe(MOST_BITS_PER_SECOND);
    expect(Number(after('-bufsize'))).toBe(MOST_BITS_PER_SECOND);
    // No audio path anywhere in this system.
    expect(args).toContain('-an');
  });

  it('give ten-bit its own pixel format and the same ceiling as everything else', () => {
    // The ceiling stopped being a per-depth number on 2026-09-25. Ten-bit
    // material takes more bits to reach the same quality and is allowed to take
    // them; what it may not do is pass sixty megabits, and neither may anything
    // else.
    const args = hevcArguments('in.mov', 'out.mp4', 'hevc_nvenc', 10, rate, CONVENTIONAL_TAGS);
    const after = (flag: string): string | undefined => args[args.indexOf(flag) + 1];
    expect(after('-pix_fmt')).toBe(PIXEL_FORMAT[10]);
    expect(after('-cq')).toBe(String(CONSTANT_QUALITY));
    expect(Number(after('-maxrate'))).toBe(MOST_BITS_PER_SECOND);
  });

  it('do not tell Media Foundation about a scene-cut detector it has not got', () => {
    const args = hevcArguments('in.mov', 'out.mp4', 'hevc_mf', 8, rate, CONVENTIONAL_TAGS);
    expect(args).not.toContain('-no-scenecut');
    expect(args).not.toContain('-preset');
    // But the keyframe clause is still written, because it is the clause.
    expect(args[args.indexOf('-g') + 1]).toBe('50');
  });

  it('ask Media Foundation for a quality and never for a bitrate', () => {
    // The same fault on the other backend, guarded the same way. Media
    // Foundation cannot cap a quality-mode encode, so there is no maxrate to
    // assert either: what catches a file over the ceiling is the probe
    // `ingest.ts` runs on every prepared file before it is sent.
    const args = hevcArguments('in.mov', 'out.mp4', 'hevc_mf', 8, rate, CONVENTIONAL_TAGS);
    const after = (flag: string): string | undefined => args[args.indexOf(flag) + 1];

    expect(after('-rate_control')).toBe('quality');
    expect(after('-quality')).toBe(String(MEDIA_FOUNDATION_QUALITY));
    expect(args).not.toContain('-b:v');
  });

  it('ask HAP for Snappy and for chunks a decoder can share out', () => {
    const args = hapArguments('in.mov', 'out.mov', rate);
    const after = (flag: string): string | undefined => args[args.indexOf(flag) + 1];
    expect(after('-c:v')).toBe('hap');
    expect(after('-format')).toBe('hap_alpha');
    expect(after('-compressor')).toBe('snappy');
    expect(after('-chunks')).toBe('4');
  });

  it('carry a rate as a fraction, because 29.97 is not a rate', () => {
    expect(rateAsArgument({ numerator: 30000, denominator: 1001 })).toBe('30000/1001');
    expect(rateAsArgument({ numerator: 25, denominator: 1 })).toBe('25');
    expect(rateAsArgument({ numerator: 25, denominator: 0 })).toBe('25');

    const args = hevcArguments(
      'in.mov',
      'out.mp4',
      'hevc_nvenc',
      8,
      { numerator: 30000, denominator: 1001 },
      CONVENTIONAL_TAGS,
    );
    // A keyframe a second at 29.97 is thirty frames, not 30000.
    expect(args[args.indexOf('-g') + 1]).toBe('30');
  });

  it('keep the source’s own colour tags and invent only what is missing', () => {
    expect(tagsFor({ primaries: 'bt2020', transfer: 'smpte2084', matrix: 'bt2020nc', range: 'tv' }))
      .toEqual({ primaries: 'bt2020', transfer: 'smpte2084', matrix: 'bt2020nc', range: 'tv' });
    expect(tagsFor({})).toEqual(CONVENTIONAL_TAGS);
    expect(tagsFor({ primaries: 'unknown', range: 'pc' })).toEqual({
      ...CONVENTIONAL_TAGS,
      range: 'pc',
    });
  });

  it('name a prepared file after what it became', () => {
    expect(preparedName('clip.mov', 'hevc_8')).toBe('clip.hevc.mp4');
    expect(preparedName('clip.mov', 'hevc_10')).toBe('clip.hevc10.mp4');
    expect(preparedName('clip.mov', 'hap')).toBe('clip.hap.mov');
    expect(preparedName('no extension', 'hap')).toBe('no extension.hap.mov');
  });
});

describe('how far an encode has got', () => {
  it('reads the microseconds out of a progress line', () => {
    expect(progressFrom('out_time_us=4500000')).toBeCloseTo(4.5);
  });

  it('reads `out_time_ms` as microseconds too, because FFmpeg misnamed it', () => {
    // Treating it as milliseconds would report a preparation as a thousand
    // times further along than it is.
    expect(progressFrom('out_time_ms=4500000')).toBeCloseTo(4.5);
  });

  it('says nothing about a line that says nothing about time', () => {
    for (const line of ['progress=continue', 'frame=42', 'bitrate=N/A', '', 'nonsense']) {
      expect(progressFrom(line)).toBeNull();
    }
  });

  it('refuses a time that is not one', () => {
    expect(progressFrom('out_time_us=N/A')).toBeNull();
    expect(progressFrom('out_time_us=-1')).toBeNull();
  });
});

describe('the two copies of the contract agree', () => {
  /** A `pub const NAME: TYPE = VALUE;` from the Rust. */
  function constant(name: string): number {
    const match = new RegExp(`pub const ${name}: \\w+ = ([^;]+);`).exec(profileRs);
    if (!match?.[1]) {
      throw new Error(`${name} is not in the Rust; the client's copy has nothing to check against`);
    }
    return Number(match[1].replace(/_/g, '').trim());
  }

  it('on how long a clip may go without a keyframe', () => {
    expect(MOST_SECONDS_BETWEEN_KEYFRAMES).toBe(constant('MOST_SECONDS_BETWEEN_KEYFRAMES'));
  });

  it('on the pixel format each depth must be in', () => {
    expect(profileRs).toContain(`Self::Eight => "${PIXEL_FORMAT[8]}"`);
    expect(profileRs).toContain(`Self::Ten => "${PIXEL_FORMAT[10]}"`);
  });

  it('on what a clip may carry', () => {
    // Underscored, as the Rust writes it: `60_000_000`.
    const ceiling = /pub const MOST_BITS_PER_SECOND: u64 = ([\d_]+);/.exec(profileRs);
    expect(ceiling, 'the Rust no longer declares one ceiling').not.toBeNull();
    expect(Number((ceiling?.[1] ?? '').replace(/_/g, ''))).toBe(MOST_BITS_PER_SECOND);
  });
});
