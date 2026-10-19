// Looking through the library the way a file browser does.
//
// One folder at a time: its own folders first, as folders, then what is filed
// in it. A path across the top says where that is and takes you back up. A
// search reaches every folder, and finds folders by their name as well as
// media by theirs (owner, 2026-10-20: "dass man da richtige Ordner sieht und
// nicht nur Links in dem Baum … ich konnte z.B. auch nicht nach Ordnername
// suchen, so dass mir dann der Inhalt des Ordners angezeigt wird").
//
// The same reading for the MEDIA screen and for the Live BROWSER tab, which is
// why it is here and not in either: one of them drew every clip in the library
// in one flat grid, and two answers to "what is in this folder" would be the
// next thing to disagree.

import {
  ROOT_FOLDER,
  countsByFolder,
  nameOf,
  parentOf,
  type Filed,
} from './folders.js';

/** What the root is called wherever a person reads it. */
export const ROOT_NAME = 'All media';

/** One step of the path across the top. */
export interface Crumb {
  path: string;
  name: string;
}

/** One folder, drawn as a folder. */
export interface FolderTile {
  path: string;
  /** Its own name, which is what the tile says. */
  name: string;
  /** How many items are filed in it or below it. */
  count: number;
  /** The folder it is in, as somebody reads it, for a search result. */
  where: string;
}

/** What browsing needs to know about an item, and nothing else. */
export interface Browsable extends Filed {
  display_name: string;
  original_filename: string;
}

/** What one view of the library shows. */
export interface Browsing<T> {
  /**
   * The folder being looked at; the root is the empty string.
   *
   * Not always the one asked for: a folder that has since been deleted or
   * renamed is answered with the nearest one above it that still exists,
   * rather than with an empty page that looks like a folder with nothing in it.
   */
  at: string;
  /** From the root down to `at`, each one a place to go back to. */
  crumbs: Crumb[];
  /** The folder above `at`, or null at the root. */
  up: string | null;
  /** Folders: the ones in `at`, or every one a search found. */
  folders: FolderTile[];
  /** Items: the ones filed in `at`, or every one a search found. */
  items: T[];
  /** Whether this is a search across the whole library rather than one folder. */
  searching: boolean;
}

/** A folder's path the way somebody reads it: `Intros › 2026`. */
export function whereLabel(folder: string): string {
  return folder === ROOT_FOLDER ? ROOT_NAME : folder.split('/').join(' › ');
}

/** The path across the top, from the root down to `path`. */
export function crumbsOf(path: string): Crumb[] {
  const crumbs: Crumb[] = [{ path: ROOT_FOLDER, name: ROOT_NAME }];
  if (path === ROOT_FOLDER) {
    return crumbs;
  }
  const parts = path.split('/');
  for (let at = 0; at < parts.length; at += 1) {
    crumbs.push({ path: parts.slice(0, at + 1).join('/'), name: parts[at] ?? '' });
  }
  return crumbs;
}

/**
 * Every folder there is: the ones the server listed, and every folder an item
 * is filed in, with everything above each.
 *
 * The second half is not expected to add anything — the server makes every
 * folder a path passes through — but an item filed somewhere the list does
 * not mention would otherwise be in no folder anybody can open.
 */
function knownFolders(listed: readonly string[], items: readonly Filed[]): Set<string> {
  const known = new Set<string>();
  const add = (path: string) => {
    let at = path;
    while (at !== ROOT_FOLDER && !known.has(at)) {
      known.add(at);
      at = parentOf(at);
    }
  };
  for (const path of listed) {
    add(path);
  }
  for (const item of items) {
    add(item.folder ?? ROOT_FOLDER);
  }
  return known;
}

/** Names in the order somebody reads them, with `Clip 2` before `Clip 10`. */
const byName = (one: string, other: string) =>
  one.localeCompare(other, undefined, { numeric: true, sensitivity: 'base' });

/**
 * What to show for one folder, or for a search.
 *
 * `entries` are whatever the caller draws — a library item, or a line that
 * carries one — and `itemOf` reads the item out of one. Their order is kept:
 * the caller already chose it.
 *
 * **A search reaches every folder.** Somebody looking for a clip does not know
 * which folder it is in, which is why they are searching; a search that only
 * looked in the open folder would answer "nothing" for a clip one folder over.
 * It finds a folder by its own name — not by a parent's, or a search for
 * "Intros" would list every folder under it — and an item by the name the
 * library shows or the name of the file it came in as.
 */
export function browse<T>(
  entries: readonly T[],
  itemOf: (entry: T) => Browsable,
  folders: readonly string[],
  at: string,
  query: string,
): Browsing<T> {
  const items = entries.map(itemOf);
  const known = knownFolders(folders, items);
  const counts = countsByFolder(items);

  let here = at;
  while (here !== ROOT_FOLDER && !known.has(here)) {
    here = parentOf(here);
  }

  const tile = (path: string): FolderTile => ({
    path,
    name: nameOf(path),
    count: counts.get(path) ?? 0,
    where: whereLabel(parentOf(path)),
  });

  const needle = query.trim().toLowerCase();
  const base = {
    at: here,
    crumbs: crumbsOf(here),
    up: here === ROOT_FOLDER ? null : parentOf(here),
  };

  if (needle === '') {
    return {
      ...base,
      folders: [...known]
        .filter((path) => parentOf(path) === here)
        .sort((one, other) => byName(nameOf(one), nameOf(other)))
        .map(tile),
      items: entries.filter((_, index) => (items[index]?.folder ?? ROOT_FOLDER) === here),
      searching: false,
    };
  }

  return {
    ...base,
    folders: [...known]
      .filter((path) => nameOf(path).toLowerCase().includes(needle))
      .sort(byName)
      .map(tile),
    items: entries.filter((_, index) => {
      const item = items[index];
      return (
        item !== undefined &&
        (item.display_name.toLowerCase().includes(needle) ||
          item.original_filename.toLowerCase().includes(needle))
      );
    }),
    searching: true,
  };
}
