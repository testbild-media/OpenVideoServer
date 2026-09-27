// Every id this client asks the page for is an id the page has.
//
// `element()` throws for an id that is not there, which is what it is for: a
// panel wired to nothing is worse than one that says so. But the screens build
// their node maps at module load, so **one missing id takes the whole window
// with it** — no panels, no connect form, no way to sign in. That is exactly
// what happened when the top bar became LIVE, MEDIA and SETTINGS and two
// lookups for the screens that went were left behind (owner, 2026-09-23).
//
// Nothing here needs a DOM. It reads the page and the source, which is why it
// runs in an ordinary `npm test` rather than only when somebody starts the app.

import { readdirSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

const uiDirectory = fileURLToPath(new URL('../src/ui/', import.meta.url));
const page = readFileSync(`${uiDirectory}index.html`, 'utf8');

/** Every `id="…"` the page defines. */
function idsOnThePage(): Set<string> {
  return new Set([...page.matchAll(/id="([^"]+)"/g)].map((match) => match[1] as string));
}

/** Every `element('…')` one source file asks for, with where it asked. */
function idsAskedFor(file: string): { id: string; file: string }[] {
  const source = readFileSync(`${uiDirectory}${file}`, 'utf8');
  return [...source.matchAll(/\belement(?:<[^>]*>)?\('([^']+)'\)/g)].map((match) => ({
    id: match[1] as string,
    file,
  }));
}

const sources = readdirSync(uiDirectory).filter((name) => name.endsWith('.ts'));

describe('the page and the code that draws it', () => {
  it('agree on every id, so one stale lookup cannot take the window down', () => {
    const present = idsOnThePage();
    const missing = sources
      .flatMap(idsAskedFor)
      .filter((asked) => !present.has(asked.id))
      .map((asked) => `${asked.file} asks for #${asked.id}`);

    expect(missing).toEqual([]);
  });

  it('is actually looking at something, rather than passing because it found nothing', () => {
    // A regex that stopped matching would make the test above vacuous, and a
    // vacuous test is worse than no test: it says the thing is checked.
    expect(idsOnThePage().size).toBeGreaterThan(40);
    expect(sources.flatMap(idsAskedFor).length).toBeGreaterThan(40);
    expect(sources).toContain('app.ts');
  });
});
