// Which folds an operator has opened, remembered across rebuilds and restarts.
//
// A panel that is rebuilt from what the server sent — which is every panel on
// the live screen — builds its folds afresh each time, and a fold built afresh
// is in whatever state the code says rather than the one the operator left it
// in. The owner's case (2026-10-21): an effect chain folded up by hand came
// open again on every lock, unlock and slot change. What is open is kept here
// instead, by the identity of the thing folded, so a rebuild asks.
//
// Folded is the default, so only what is open is stored: a chain nobody has
// touched costs nothing, and an effect that is taken off leaves at most an id.

/** The part of `localStorage` this needs, so a test can hand it a map. */
export interface FoldStore {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/** Where the open effects of Live's OPTIONS are kept. */
export const OPEN_EFFECTS_KEY = 'ovs.live.open-effects';

/**
 * How many open folds are remembered at most.
 *
 * Past this the one opened longest ago is forgotten first, which folds it —
 * the default, and the cheap way to be wrong. Ids of effects taken off a show
 * are never cleaned up otherwise, so without a bound this would only grow.
 */
export const MOST_REMEMBERED = 500;

export class OpenFolds {
  readonly #key: string;
  readonly #store: FoldStore | null;
  /** Open ones, the most recently opened last. */
  #open: string[];

  constructor(key: string, store: FoldStore | null = browserStore()) {
    this.#key = key;
    this.#store = store;
    this.#open = read(store, key);
  }

  /** Whether this fold was left open. */
  has(id: string): boolean {
    return this.#open.includes(id);
  }

  /** Remembers how the operator left this fold, and keeps it for next time. */
  remember(id: string, open: boolean): void {
    const without = this.#open.filter((each) => each !== id);
    if (!open && without.length === this.#open.length) {
      // Folded, and was never open: nothing to keep, nothing to write.
      return;
    }
    this.#open = open ? [...without, id].slice(-MOST_REMEMBERED) : without;
    try {
      this.#store?.setItem(this.#key, JSON.stringify(this.#open));
    } catch {
      // A store that refuses — full, or switched off — leaves this window's
      // own memory, which still keeps a fold through a rebuild.
    }
  }
}

/** The window's own store, where there is one that answers. */
function browserStore(): FoldStore | null {
  try {
    return (globalThis as { localStorage?: FoldStore }).localStorage ?? null;
  } catch {
    return null;
  }
}

/** What was kept last time, or nothing when it cannot be read as a list of ids. */
function read(store: FoldStore | null, key: string): string[] {
  try {
    const kept: unknown = JSON.parse(store?.getItem(key) ?? '[]');
    return Array.isArray(kept)
      ? kept.filter((each): each is string => typeof each === 'string').slice(-MOST_REMEMBERED)
      : [];
  } catch {
    return [];
  }
}
