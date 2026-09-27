// The library's own structure, and the rules it keeps.
//
// Two things are checked here that nobody can check by looking at a running
// client: that the naming rules this client applies are the ones the server
// enforces — it reads the Rust and fails when they drift, the same arrangement
// `limits.test.ts` uses — and that a folder tree drawn from a flat list of
// paths is the tree the operator made, including the empty ones.
//
// The rule the whole feature exists under is the owner's (2026-09-23):
// **organising media must not move the real files.** Nothing here is a path on
// any filesystem.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import type { MediaItem } from '@ovs/schema';

import {
  MAX_FOLDER_PATH,
  MAX_FOLDER_SEGMENT,
  ROOT_FOLDER,
  countsByFolder,
  folderNameProblem,
  folderTree,
  inDrawOrder,
  isInside,
  itemsIn,
  joinFolder,
  moveFolder,
  nameOf,
  parentOf,
} from '../src/shared/folders.js';

const repository = (path: string) => fileURLToPath(new URL(`../../../${path}`, import.meta.url));
const mediaRs = readFileSync(repository('crates/ovs-types/src/media/mod.rs'), 'utf8');

function item(overrides: Partial<MediaItem> = {}): MediaItem {
  return {
    id: 'MEDIA1',
    display_name: 'Kaleidoskop',
    original_filename: 'kaleidoskop.mov',
    kind: 'video',
    state: 'ready',
    has_alpha: false,
    created_at: 10,
    ...overrides,
  };
}

// --- the rules are the server's -----------------------------------------------

describe('the folder rules', () => {
  it('carry the same numbers the model does', () => {
    expect(mediaRs).toContain(`pub const MAX_FOLDER_SEGMENT: usize = ${MAX_FOLDER_SEGMENT};`);
    expect(mediaRs).toContain(`pub const MAX_FOLDER_PATH: usize = ${MAX_FOLDER_PATH};`);
  });

  it('put no ceiling on how deep folders nest, because the model puts none', () => {
    // The owner's instruction (2026-09-23): folders in folders in folders, as
    // far as they care to go. A depth constant would be the thing to delete if
    // one were ever added, so this is the test that says why there is not one.
    expect(mediaRs).not.toContain('MAX_FOLDER_DEPTH');
    const deep = Array.from({ length: 40 }, (_, level) => `Level${level}`).join('/');
    const tree = folderTree([deep, ...deep.split('/').map((_, at, all) => all.slice(0, at + 1).join('/'))]);
    expect(inDrawOrder(tree)).toHaveLength(40);
  });

  it('refuse a name nobody could tell from another one', () => {
    expect(folderNameProblem('Intros')).toBeNull();
    expect(folderNameProblem('')).not.toBeNull();
    expect(folderNameProblem(' Intros')).not.toBeNull();
    expect(folderNameProblem('Intros ')).not.toBeNull();
    expect(folderNameProblem('a'.repeat(MAX_FOLDER_SEGMENT + 1))).not.toBeNull();
  });

  it('refuse a slash, rather than quietly making two folders out of one name', () => {
    // Somebody typing `Intros/2026` into a field labelled "name" has not been
    // told that is what would happen.
    expect(folderNameProblem('Intros/2026')).not.toBeNull();
    expect(folderNameProblem('Intros\\2026')).not.toBeNull();
  });
});

// --- the tree -----------------------------------------------------------------

describe('the folder tree', () => {
  it('nests what the server sent, deepest last, sorted by name at every level', () => {
    const tree = folderTree([
      'Tour',
      'Tour/September',
      'Tour/August',
      'Tour/August/Berlin',
      'Archive',
    ]);
    expect(inDrawOrder(tree).map((node) => node.path)).toEqual([
      'Archive',
      'Tour',
      'Tour/August',
      'Tour/August/Berlin',
      'Tour/September',
    ]);
  });

  it('keeps a folder with nothing in it, which is the point of storing them', () => {
    // An operator makes a folder in order to put things in it. One worked out
    // from the items could not exist between being made and being filled.
    const tree = folderTree(['Intros']);
    expect(tree).toHaveLength(1);
    expect(tree[0]?.path).toBe('Intros');
    expect(countsByFolder([])).toEqual(new Map());
  });

  it('fills in a parent the server did not send rather than losing the child', () => {
    // The server makes every folder on the way, so this should not happen. If
    // it ever does, the operator's folder is still on screen.
    const tree = folderTree(['Intros/2026/Openers']);
    expect(inDrawOrder(tree).map((node) => node.path)).toEqual([
      'Intros',
      'Intros/2026',
      'Intros/2026/Openers',
    ]);
  });

  it('knows a path from its parts', () => {
    expect(joinFolder(ROOT_FOLDER, 'Intros')).toBe('Intros');
    expect(joinFolder('Intros', '2026')).toBe('Intros/2026');
    expect(parentOf('Intros/2026')).toBe('Intros');
    expect(parentOf('Intros')).toBe(ROOT_FOLDER);
    expect(nameOf('Intros/2026')).toBe('2026');
  });
});

// --- what is in a folder ------------------------------------------------------

describe('what a folder holds', () => {
  const library = [
    item({ id: 'A', folder: 'Intros' }),
    item({ id: 'B', folder: 'Intros/2026' }),
    item({ id: 'C' }),
  ];

  it('shows what is filed there and not what is under it', () => {
    // A folder that showed its descendants' contents would put the same clip in
    // several places, and renaming it in one of them would rename it in all of
    // them without anybody having been told.
    expect(itemsIn(library, 'Intros').map((each) => each.id)).toEqual(['A']);
    expect(itemsIn(library, 'Intros/2026').map((each) => each.id)).toEqual(['B']);
  });

  it('puts an item nothing has filed in the root', () => {
    expect(itemsIn(library, ROOT_FOLDER).map((each) => each.id)).toEqual(['C']);
  });

  it('counts downwards, so a closed folder still says there is something in it', () => {
    const counts = countsByFolder(library);
    expect(counts.get('Intros')).toBe(2);
    expect(counts.get('Intros/2026')).toBe(1);
    expect(counts.has(ROOT_FOLDER)).toBe(false);
  });
});

// --- moving a folder ----------------------------------------------------------

describe('moving a folder', () => {
  it('refuses to put one inside itself', () => {
    // Every path under it would be left pointing at a parent that no longer
    // exists.
    expect(moveFolder('Intros', 'Intros/2026', []).ok).toBe(false);
    expect(moveFolder('Intros', 'Intros', []).ok).toBe(false);
    expect(isInside('Intros/2026', 'Intros')).toBe(true);
    expect(isInside('Intros', 'Intros/2026')).toBe(false);
  });

  it('refuses to land on a folder that is already there', () => {
    expect(moveFolder('Intros', 'Archive', ['Archive/Intros']).ok).toBe(false);
  });

  it('gives the path the server will store', () => {
    expect(moveFolder('Intros', 'Archive', [])).toEqual({ ok: true, to: 'Archive/Intros' });
    expect(moveFolder('Archive/Intros', ROOT_FOLDER, [])).toEqual({ ok: true, to: 'Intros' });
  });
});
