// What the library shows, and — just as much — what it does not.
//
// The rule these are about is that nothing appears in front of an operator that
// the server did not send. A duration nobody measured, a size that is not in the
// response and a picture that is not there are three different absences, and
// each of them has to read as itself.

import type { MediaItem } from '@ovs/schema';
import { describe, expect, it } from 'vitest';

import {
  durationLabel,
  facts,
  listOf,
  newestFirst,
  picture,
  rateLabel,
  refusedDeletion,
  sizeLabel,
  stateLabel,
  usageLine,
} from '../src/shared/media.js';

/** A ready clip, as `/media` sends one. */
const clip = (over: Partial<MediaItem> = {}): MediaItem => ({
  id: '01J8ZK3S0000000000000001',
  display_name: 'Conference opener',
  original_filename: 'conference-opener.mp4',
  kind: 'video',
  state: 'ready',
  has_alpha: false,
  created_at: 1_700_000_000_000,
  duration_ms: 62_500,
  dimensions: { width: 1920, height: 1080 },
  frame_rate: { numerator: 60000, denominator: 1001 },
  ...over,
});

describe('an item in the library', () => {
  it('shows only the fields the server sent', () => {
    // An item still being verified has no size and no duration yet. A dash in a
    // column an operator scans reads as "none", and a clip nobody has measured
    // is not a clip of no length.
    const importing = clip({
      state: 'verifying',
      duration_ms: null,
      dimensions: null,
      frame_rate: null,
    });

    const labels = facts(importing).map((fact) => fact.label);
    expect(labels).not.toContain('Size');
    expect(labels).not.toContain('Duration');
    expect(labels).not.toContain('Rate');
    expect(labels).toContain('Type');
  });

  it('never claims a size on disk or a playback format, because neither is sent', () => {
    // Both live on `MediaAsset`, which is server-internal and never serialised.
    // Drawing either would mean this client inventing a number.
    const values = facts(clip()).map((fact) => `${fact.label} ${fact.value}`.toLowerCase());
    expect(values.some((value) => value.includes('byte'))).toBe(false);
    expect(values.some((value) => value.includes('prores'))).toBe(false);
    expect(values.some((value) => value.includes('hap'))).toBe(false);
  });

  it('keeps the exact number beside the rounded one', () => {
    // 60000/1001 is not 59.94, and the scheduler uses the ratio. The decimal is
    // for reading; the tooltip is the truth.
    const rate = facts(clip()).find((fact) => fact.label === 'Rate');
    expect(rate?.value).toBe('59.94 fps');
    expect(rate?.title).toBe('60000/1001');

    const duration = facts(clip()).find((fact) => fact.label === 'Duration');
    expect(duration?.value).toBe('1:02.5');
    expect(duration?.title).toBe('62500 ms');
  });

  it('says alpha only where there is some', () => {
    expect(facts(clip()).some((fact) => fact.label === 'Alpha')).toBe(false);
    expect(facts(clip({ has_alpha: true })).some((fact) => fact.label === 'Alpha')).toBe(true);
  });

  it('reads a length the way a cue sheet does', () => {
    expect(durationLabel(0)).toBe('0:00.0');
    expect(durationLabel(950)).toBe('0:01.0');
    expect(durationLabel(62_500)).toBe('1:02.5');
    expect(durationLabel(3_600_000)).toBe('60:00.0');
    expect(durationLabel(null)).toBeNull();
  });

  it('reads a rate as a decimal and refuses to divide by nothing', () => {
    expect(rateLabel({ numerator: 60, denominator: 1 })).toBe('60 fps');
    expect(rateLabel({ numerator: 30000, denominator: 1001 })).toBe('29.97 fps');
    expect(rateLabel({ numerator: 60, denominator: 0 })).toBeNull();
    expect(rateLabel(null)).toBeNull();
  });

  it('reads a size as pixels', () => {
    expect(sizeLabel({ width: 1920, height: 1080 })).toBe('1920 × 1080');
    expect(sizeLabel(null)).toBeNull();
  });

  it('calls each pipeline state what the operator is shown', () => {
    expect(stateLabel('importing')).toBe('Importing');
    expect(stateLabel('processing')).toBe('Processing');
    expect(stateLabel('uploading')).toBe('Uploading');
    expect(stateLabel('verifying')).toBe('Verifying');
    expect(stateLabel('ready')).toBe('Ready');
    expect(stateLabel('failed')).toBe('Failed');
  });

  it('shows the newest import first', () => {
    const older = clip({ id: 'OLD', created_at: 1 });
    const newer = clip({ id: 'NEW', created_at: 2 });
    expect(newestFirst([older, newer]).map((item) => item.id)).toEqual(['NEW', 'OLD']);
  });
});

describe('the picture beside an item', () => {
  it('shows the picture when the server has one', () => {
    expect(picture(clip(), 'data:image/png;base64,AAA')).toEqual({
      kind: 'picture',
      src: 'data:image/png;base64,AAA',
      alt: 'Conference opener',
    });
  });

  it('falls back to its name when the item has no picture', () => {
    // `GET /media/{id}/thumbnail` answers 404 for an item registered before
    // thumbnails existed and for one nothing could be read from. That is not a
    // fault and must not draw as one: a library entry with no picture is a
    // library entry with a name on it, which is what every one of them was.
    expect(picture(clip(), null)).toEqual({ kind: 'name', text: 'Conference opener' });
    expect(picture(clip(), '')).toEqual({ kind: 'name', text: 'Conference opener' });
  });
});

describe('deleting an item', () => {
  it('names the shows when a delete is refused because a show uses it', () => {
    // The server refuses with its own sentence, which lists the shows inside
    // prose. The usage endpoint gives the same answer as a list, and an operator
    // has to be told which shows they would have broken.
    const said = refusedDeletion(
      { code: 'media_in_use', message: 'this media is used by: Spring Tour, Gala' },
      ['Spring Tour', 'Gala'],
    );

    expect(said).toContain('Spring Tour');
    expect(said).toContain('Gala');
    expect(said).toContain('Not deleted');
  });

  it('names one show without turning it into a list', () => {
    const said = refusedDeletion({ code: 'media_in_use', message: 'in use' }, ['Spring Tour']);
    expect(said).toBe('Not deleted: Spring Tour still uses it. Take it out of that show first.');
  });

  it('keeps the server’s own words when it refused for another reason', () => {
    expect(refusedDeletion({ code: 'not_found', message: 'there is no media item here' }, [])).toBe(
      'there is no media item here',
    );
  });

  it('keeps the server’s own words when usage and the refusal disagree', () => {
    // A show deleted between the two requests. Reconciling two answers a
    // second apart would be this client deciding which of them it believes.
    expect(refusedDeletion({ code: 'media_in_use', message: 'this media is used by: Gala' }, [])).toBe(
      'this media is used by: Gala',
    );
  });

  it('says plainly that nothing uses an item, which is not the same as not having asked', () => {
    expect(usageLine([])).toBe('No show uses this.');
    expect(usageLine(['Gala'])).toBe('Used by Gala.');
    expect(usageLine(['Gala', 'Spring Tour'])).toBe('Used by Gala and Spring Tour.');
  });

  it('writes a list of names the way a sentence does', () => {
    expect(listOf([])).toBe('');
    expect(listOf(['one'])).toBe('one');
    expect(listOf(['one', 'two'])).toBe('one and two');
    expect(listOf(['one', 'two', 'three'])).toBe('one, two and three');
  });
});
