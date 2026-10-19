// Looking through the library one folder at a time, and searching all of it.

import { describe, expect, it } from 'vitest';

import { browse, crumbsOf, whereLabel, type Browsable } from '../src/shared/browse.js';

const item = (name: string, folder = '', filename = `${name}.mov`): Browsable & { id: string } => ({
  id: `${folder}/${name}`,
  display_name: name,
  original_filename: filename,
  folder,
});

/** The owner's case: a lot of folders, most of them still empty. */
const FOLDERS = ['Intros', 'Intros/2026', 'Intros/2025', 'Loops', 'Loops/Smiley', 'Logos'];

const LIBRARY = [
  item('Opener'),
  item('Visual_Smiley_Tunnel', 'Loops/Smiley'),
  item('Visual_Smiley_Tunnel', 'Intros/2026', 'Visual_Smiley_Tunnel_v2.mov'),
  item('Countdown', 'Intros'),
];

const same = <T>(one: T) => one;

describe('one folder at a time', () => {
  it('shows the root’s own folders, as folders, and only what is filed at the root', () => {
    const view = browse(LIBRARY, same, FOLDERS, '', '');

    expect(view.folders.map((folder) => folder.name)).toEqual(['Intros', 'Logos', 'Loops']);
    expect(view.items.map((entry) => entry.display_name)).toEqual(['Opener']);
    expect(view.up).toBeNull();
    expect(view.searching).toBe(false);
  });

  it('shows an empty folder as a folder, with nothing in it', () => {
    // The owner had made many folders and filed nothing yet. They have to be
    // there to be filed into.
    const view = browse(LIBRARY, same, FOLDERS, '', '');
    const logos = view.folders.find((folder) => folder.name === 'Logos');
    expect(logos).toMatchObject({ path: 'Logos', count: 0 });

    const inside = browse(LIBRARY, same, FOLDERS, 'Logos', '');
    expect(inside.folders).toEqual([]);
    expect(inside.items).toEqual([]);
    expect(inside.up).toBe('');
  });

  it('counts what is below a folder, not only in it', () => {
    const intros = browse(LIBRARY, same, FOLDERS, '', '').folders.find(
      (folder) => folder.name === 'Intros',
    );
    expect(intros?.count).toBe(2);
  });

  it('goes into a folder and says how to get back', () => {
    const view = browse(LIBRARY, same, FOLDERS, 'Intros/2026', '');

    expect(view.crumbs.map((crumb) => crumb.name)).toEqual(['All media', 'Intros', '2026']);
    expect(view.crumbs.map((crumb) => crumb.path)).toEqual(['', 'Intros', 'Intros/2026']);
    expect(view.up).toBe('Intros');
    expect(view.items.map((entry) => entry.original_filename)).toEqual([
      'Visual_Smiley_Tunnel_v2.mov',
    ]);
  });

  it('puts folders in the order somebody reads them', () => {
    const view = browse([], same, ['Take 10', 'Take 2', 'take 1'], '', '');
    expect(view.folders.map((folder) => folder.name)).toEqual(['take 1', 'Take 2', 'Take 10']);
  });

  it('falls back to the nearest folder that still exists', () => {
    // Deleted or renamed on another screen while this one was looking at it.
    const view = browse(LIBRARY, same, FOLDERS, 'Intros/1999/Night', '');
    expect(view.at).toBe('Intros');
  });

  it('opens a folder an item is filed in even when the list forgot it', () => {
    const view = browse([item('Lost', 'Archive/Old')], same, [], '', '');
    expect(view.folders.map((folder) => folder.path)).toEqual(['Archive']);
  });
});

describe('a search', () => {
  it('reaches every folder, and says where each find is', () => {
    const view = browse(LIBRARY, same, FOLDERS, 'Logos', 'smiley');

    expect(view.searching).toBe(true);
    // Both clips of the same name, from two folders, which is the case the
    // owner could not tell apart.
    expect(view.items.map((entry) => entry.folder)).toEqual(['Loops/Smiley', 'Intros/2026']);
    // And the folder called Smiley, which opens on its contents.
    expect(view.folders).toEqual([
      { path: 'Loops/Smiley', name: 'Smiley', count: 1, where: 'Loops' },
    ]);
  });

  it('finds a folder by its own name, not by a parent’s', () => {
    // Otherwise "intros" would list every folder under Intros as well.
    const view = browse(LIBRARY, same, FOLDERS, '', 'intros');
    expect(view.folders.map((folder) => folder.path)).toEqual(['Intros']);
  });

  it('finds an item by the file it came in as', () => {
    const view = browse(LIBRARY, same, FOLDERS, '', '_v2');
    expect(view.items.map((entry) => entry.folder)).toEqual(['Intros/2026']);
  });

  it('keeps the folder it was started from, to go back to', () => {
    const view = browse(LIBRARY, same, FOLDERS, 'Intros', 'tunnel');
    expect(view.at).toBe('Intros');
    expect(view.crumbs.map((crumb) => crumb.name)).toEqual(['All media', 'Intros']);
  });

  it('reads an item out of whatever the caller draws', () => {
    const lines = LIBRARY.map((each) => ({ item: each, onAir: false }));
    const view = browse(lines, (line) => line.item, FOLDERS, 'Loops/Smiley', '');
    expect(view.items).toHaveLength(1);
    expect(view.items[0]?.item.display_name).toBe('Visual_Smiley_Tunnel');
  });
});

describe('saying where something is', () => {
  it('names the root and joins the rest', () => {
    expect(whereLabel('')).toBe('All media');
    expect(whereLabel('Intros/2026')).toBe('Intros › 2026');
    expect(crumbsOf('')).toEqual([{ path: '', name: 'All media' }]);
  });
});
