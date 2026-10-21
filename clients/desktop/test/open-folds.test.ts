// Which effects in Live's OPTIONS are unfolded, kept across rebuilds and
// restarts (owner, 2026-10-21: folded by default, and "der letzte Stand sollte
// zumindest immer wiederkehrbar sein, auch bei Slotwechsel").

import { describe, expect, it } from 'vitest';

import { MOST_REMEMBERED, OpenFolds, type FoldStore } from '../src/shared/open-folds.js';

/** A store that is a map, and counts its writes. */
function store(initial: Record<string, string> = {}): FoldStore & { writes: number } {
  const held = new Map(Object.entries(initial));
  return {
    writes: 0,
    getItem: (key) => held.get(key) ?? null,
    setItem(key, value) {
      this.writes += 1;
      held.set(key, value);
    },
  };
}

describe('the folds an operator opened', () => {
  it('starts with everything folded', () => {
    const folds = new OpenFolds('k', store());
    expect(folds.has('EFFECT1')).toBe(false);
  });

  it('keeps an opened fold open and a closed one closed', () => {
    const folds = new OpenFolds('k', store());
    folds.remember('EFFECT1', true);
    folds.remember('EFFECT2', true);
    folds.remember('EFFECT1', false);
    expect(folds.has('EFFECT1')).toBe(false);
    expect(folds.has('EFFECT2')).toBe(true);
  });

  it('is still that way in the next window, which is the client started again', () => {
    const shared = store();
    new OpenFolds('k', shared).remember('EFFECT1', true);
    expect(new OpenFolds('k', shared).has('EFFECT1')).toBe(true);
  });

  it('writes nothing for a fold that was folded and is folded again', () => {
    // Every rebuild sets each fold and the browser reports it as a toggle; a
    // write per rebuild per effect would be a store hammered for nothing.
    const kept = store();
    const folds = new OpenFolds('k', kept);
    folds.remember('EFFECT1', false);
    expect(kept.writes).toBe(0);
  });

  it('forgets the one opened longest ago past its bound, rather than growing forever', () => {
    const folds = new OpenFolds('k', store());
    for (let n = 0; n <= MOST_REMEMBERED; n += 1) {
      folds.remember(`EFFECT${n}`, true);
    }
    expect(folds.has('EFFECT0')).toBe(false);
    expect(folds.has(`EFFECT${MOST_REMEMBERED}`)).toBe(true);
  });

  it('reads anything it cannot understand as everything folded', () => {
    expect(new OpenFolds('k', store({ k: 'not json' })).has('EFFECT1')).toBe(false);
    expect(new OpenFolds('k', store({ k: '{"EFFECT1":true}' })).has('EFFECT1')).toBe(false);
  });

  it('still works through a rebuild with no store at all', () => {
    const folds = new OpenFolds('k', null);
    folds.remember('EFFECT1', true);
    expect(folds.has('EFFECT1')).toBe(true);
  });

  it('keeps working when the store refuses to write', () => {
    const refusing: FoldStore = {
      getItem: () => null,
      setItem: () => {
        throw new Error('quota');
      },
    };
    const folds = new OpenFolds('k', refusing);
    folds.remember('EFFECT1', true);
    expect(folds.has('EFFECT1')).toBe(true);
  });
});
