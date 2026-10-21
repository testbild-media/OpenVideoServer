// The DMX patch as a table to print or keep as a PDF.
//
// The owner, 2026-10-21: "Print DMX Table.. Es baut eine Tabelle mit den ganzen
// Fixture Patches von allen Sachen, die gepatcht sind. Gerne direkt als PDF oder
// HTML to Print" — and then, of a first draft that listed every channel too:
// "Nicht Kanal Patch sondern nur Fixture": universe, DMX start address, channel
// count, fixture name, and which layer an effect belongs to. One row per
// patched fixture, which is what a desk is patched from.
//
// A whole document rather than a fragment, so the same string is what is
// printed, what becomes the PDF and what a test reads. Nothing in it runs:
// there is no script, and every word that came from a show is escaped, because
// a layer can be called anything.

import type { FixtureInstance, FixtureTarget, Show } from '@ovs/schema';

import {
  collisions,
  effectPlaces,
  findPersonality,
  generatorPlaces,
  readPatch,
  type FixtureCatalogue,
} from './patch.js';

/** What the sheet says about itself beyond the patch. */
export interface SheetContext {
  /** When it was made, which a printed page otherwise never says. */
  now: Date;
  /** Whether the document in hand has edits the server has not got. */
  unsaved: boolean;
}

/** One patched fixture, as the sheet lists it. */
export interface SheetRow {
  universe: number;
  start: number;
  channels: number;
  /** The personality's name, or its type where this server does not publish it. */
  fixture: string;
  /** Where it belongs: the master, a layer, or the layer and slot of a module. */
  belongsTo: string;
  /** Parked or overlapping, said beside the name; empty when neither. */
  remark: string;
}

/**
 * The patch in the order a desk is patched: by universe, then by address.
 *
 * Parked entries are listed with the rest and say so. A spare patch an
 * operator parked is still a thing they will look for on paper.
 */
export function sheetRows(show: Show, catalogue: FixtureCatalogue): SheetRow[] {
  const clashes = collisions(show);
  const nameOf = (fixture: FixtureInstance) =>
    findPersonality(catalogue, fixture.type_id)?.name ?? fixture.type_id;
  return [...readPatch(show)]
    .sort((a, b) => a.universe - b.universe || a.start_address - b.start_address)
    .map((fixture) => {
      const overlaps = clashes
        .filter((clash) => clash.a.id === fixture.id || clash.b.id === fixture.id)
        .map((clash) => nameOf(clash.a.id === fixture.id ? clash.b : clash.a));
      return {
        universe: fixture.universe,
        start: fixture.start_address,
        channels: fixture.footprint,
        fixture: nameOf(fixture),
        belongsTo: belongsTo(show, fixture.target),
        remark: !fixture.enabled
          ? 'parked'
          : overlaps.length > 0
            ? `overlaps ${overlaps.join(', ')}`
            : '',
      };
    });
}

/**
 * Where a patched fixture belongs, in the words the matrix uses.
 *
 * For an effect or a generator, the layer it is on and the slot when it is a
 * slot's — which is the question the owner asked of the sheet: which effect
 * belongs to which layer. Something the show no longer has is said to be
 * missing rather than named by an identifier nobody can read.
 */
export function belongsTo(show: Show, target: FixtureTarget): string {
  switch (target.kind) {
    case 'master':
      return 'Master';
    case 'layer':
      return show.layers.find((layer) => layer.id === target.layer_id)?.name ?? 'a layer this show no longer has';
    case 'effect': {
      const place = effectPlaces(show).find((each) => each.instance.id === target.effect_instance_id);
      if (place === undefined) {
        return 'an effect this show no longer has';
      }
      return place.slotIndex === undefined
        ? `${place.layer.name} (the layer)`
        : `${place.layer.name}, slot ${place.slotIndex}`;
    }
    case 'generator': {
      const place = generatorPlaces(show).find(
        (each) => each.instanceId === target.generator_instance_id,
      );
      return place === undefined
        ? 'a generator this show no longer has'
        : `${place.layer.name}, slot ${place.slotIndex}`;
    }
  }
}

/** The whole sheet, as a standalone HTML document. */
export function patchSheet(show: Show, catalogue: FixtureCatalogue, context: SheetContext): string {
  const rows = sheetRows(show, catalogue);
  const universes = new Set(rows.map((row) => row.universe)).size;
  const notes: string[] = [];
  if (context.unsaved) {
    notes.push('Includes changes that have not been saved to the server yet.');
  }
  if (!catalogue.loaded) {
    notes.push('The fixture list had not arrived from the server, so fixtures are named by type.');
  }

  const table =
    rows.length === 0
      ? '<p class="empty">Nothing in this show is patched.</p>'
      : `<table>
<thead><tr><th class="num">Universe</th><th class="num">Start address</th><th class="num">Channels</th><th>Fixture</th><th>Belongs to</th></tr></thead>
<tbody>
${rows.map(row).join('\n')}
</tbody>
</table>`;

  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>${escape(`DMX patch — ${show.name}`)}</title>
<style>${STYLE}</style>
</head>
<body>
<header>
<h1>DMX patch</h1>
<p class="show">${escape(show.name)}</p>
<p class="meta">${escape(stamp(context.now))} · ${rows.length} ${rows.length === 1 ? 'fixture' : 'fixtures'} in ${universes} ${universes === 1 ? 'universe' : 'universes'} · universe numbers as patched</p>
${notes.map((note) => `<p class="note">${escape(note)}</p>`).join('\n')}
</header>
${table}
</body>
</html>
`;
}

/** What the file is called when it is kept, from the show's own name. */
export function sheetFileName(show: Show): string {
  const name = show.name.replace(/[\\/:*?"<>|]+/g, ' ').replace(/\s+/g, ' ').trim();
  return `${name === '' ? 'Show' : name} - DMX patch`;
}

function row(entry: SheetRow): string {
  const kind = entry.remark === 'parked' ? ' class="parked"' : entry.remark !== '' ? ' class="clash"' : '';
  const remark = entry.remark === '' ? '' : ` <span class="tag">${escape(entry.remark)}</span>`;
  return `<tr${kind}><td class="num">${entry.universe}</td><td class="num">${entry.start}</td><td class="num">${entry.channels}</td><td>${escape(entry.fixture)}${remark}</td><td>${escape(entry.belongsTo)}</td></tr>`;
}

function stamp(now: Date): string {
  const two = (value: number) => String(value).padStart(2, '0');
  return `${now.getFullYear()}-${two(now.getMonth() + 1)}-${two(now.getDate())} ${two(now.getHours())}:${two(now.getMinutes())}`;
}

function escape(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/**
 * The sheet's look: black on white, A4, and the column heads repeated on every
 * page a long patch runs onto.
 */
const STYLE = `
@page { size: A4; margin: 12mm; }
* { box-sizing: border-box; }
html { color: #000; background: #fff; }
body { margin: 0; font: 10pt/1.35 "Segoe UI", Arial, sans-serif; }
header { border-bottom: 1.5pt solid #000; margin-bottom: 10pt; padding-bottom: 6pt; }
h1 { font-size: 16pt; margin: 0; }
.show { font-size: 12pt; font-weight: 600; margin: 2pt 0 0; }
.meta { color: #444; margin: 2pt 0 0; }
.note { margin: 4pt 0 0; font-weight: 600; }
.empty { color: #444; font-style: italic; }
table { border-collapse: collapse; width: 100%; }
thead { display: table-header-group; }
th { text-align: left; font-weight: 600; border-bottom: 1pt solid #000; padding: 3pt 5pt; }
td { border-bottom: 0.5pt solid #bbb; padding: 3pt 5pt; vertical-align: top; }
tr { break-inside: avoid; }
.num { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; width: 1%; }
.parked td { color: #666; }
.clash td { font-weight: 600; }
.tag { border: 0.5pt solid #666; border-radius: 2pt; padding: 0 3pt; font-size: 8.5pt; font-weight: 400; }
`;
