// Finds text drawn outside its box, panels that scroll, stages drawn at the
// wrong shape, element ids that are in the page twice, how much of the layer
// band the layers use, and anything on a layer row that is off its line.
//
// Loads the client's real page with a stubbed bridge that answers with a full
// show under deliberately long names, shows each screen in turn, and reports
// every element whose content spills out of it.
//
//   npm run overflow --workspace ovs-desktop-client
//
// It needs `npm run build` to have run: it loads what is in `dist`.
//
// **Only boxes that do not clip are findings.** A box with overflow hidden cuts
// its content off, which is what it is for; one that scrolls is meant to hold
// more than it shows. What is being looked for is text drawn over its
// neighbours, and that is overflow visible and nothing else.
//
// **Three more things are measured.** The first two because the owner had to
// report them by eye (2026-09-23), the third because nothing would have:
//
//   - **What scrolls.** A panel holding two shows and four numbers that has
//     to be scrolled is a layout fault, and it is invisible in the source: it
//     depends on what the box was given and what went in it.
//   - **What shape a stage came out.** A box with an aspect ratio takes its
//     height from its width, so a `max-height` on one does not narrow it, it
//     squashes it. A 1920x1080 wall drawn at 5:2 looks deliberate in the CSS
//     and wrong on the screen.
//   - **An element id that is in the page twice.** `getElementById` answers
//     with the first and says nothing about the second, so whatever is wired
//     to the second is wired to nothing — it draws, it takes clicks, and none
//     of them arrive. A patch that ran twice leaves exactly this, and every
//     other check here passes it: both copies are inside their boxes, the
//     right shape, and not scrolling.
//
// It measures rather than reasons, which is the point: a fixed width that fits
// one word and not the next is invisible in the source and obvious here.

const { app, BrowserWindow } = require('electron');
const { join } = require('node:path');

const CLIENT = join(__dirname, '..', '..');

/** `16 / 9` as a number, for comparing what was wanted with what was drawn. */
function ratioOf(declared) {
  const parts = String(declared).split('/');
  const width = Number(parts[0]);
  const height = Number(parts[1]);
  return Number.isFinite(width) && Number.isFinite(height) && height !== 0
    ? width / height
    : Number.NaN;
}

const PROBE = [
  '(() => {',
  '  const out = [];',
  '  const seen = new Set();',
  '  for (const node of document.querySelectorAll("*")) {',
  '    const style = getComputedStyle(node);',
  '    if (style.display === "none" || style.visibility === "hidden") continue;',
  '    const box = node.getBoundingClientRect();',
  '    if (box.width === 0 || box.height === 0) continue;',
  '    if (style.overflowX !== "visible" || style.overflowY !== "visible") continue;',
  '    const wide = node.scrollWidth - node.clientWidth;',
  '    const tall = node.scrollHeight - node.clientHeight;',
  '    if (wide <= 1 && tall <= 1) continue;',
  '    const where = node.id !== ""',
  '      ? "#" + node.id',
  '      : node.className',
  '        ? "." + String(node.className).split(" ").join(".")',
  '        : node.tagName.toLowerCase();',
  '    const key = where + "|" + wide + "|" + tall;',
  '    if (seen.has(key)) continue;',
  '    seen.add(key);',
  '    out.push({',
  '      where: where,',
  '      text: (node.textContent || "").trim().slice(0, 40),',
  '      over: { x: wide, y: tall },',
  '      box: { w: Math.round(box.width), h: Math.round(box.height) },',
  '    });',
  '  }',
  '  return out;',
  '})()',
].join('\n');

// Panels that have more in them than they are showing. `auto` and `scroll`
// only: a box with `hidden` is clipping on purpose and `visible` is the
// finding above.
const SCROLLERS = [
  '(() => {',
  '  const out = [];',
  '  for (const node of document.querySelectorAll("*")) {',
  '    const style = getComputedStyle(node);',
  '    if (style.display === "none" || style.visibility === "hidden") continue;',
  '    const scrolls = style.overflowY === "auto" || style.overflowY === "scroll";',
  '    if (!scrolls) continue;',
  '    const over = node.scrollHeight - node.clientHeight;',
  '    if (over <= 2) continue;',
  '    const where = node.id !== ""',
  '      ? "#" + node.id',
  '      : node.className',
  '        ? "." + String(node.className).split(" ").join(".")',
  '        : node.tagName.toLowerCase();',
  '    out.push({ where: where, over: over, shows: node.clientHeight });',
  '  }',
  '  return out;',
  '})()',
].join('\n');

// The one invariant the owner has had to report twice by eye: a slot pad is
// never wider than the layer's own picture beside it. Measured rather than
// reasoned about, because it depends on the window's width and nothing in the
// source says so.
const PADS = [
  '(() => {',
  '  const out = [];',
  '  for (const row of document.querySelectorAll(".layer")) {',
  '    const picture = row.querySelector(".showing-picture");',
  '    const pad = row.querySelector(".pad");',
  '    if (!picture || !pad) continue;',
  '    out.push({',
  '      picture: Math.round(picture.getBoundingClientRect().width),',
  '      pad: Math.round(pad.getBoundingClientRect().width),',
  '    });',
  '  }',
  '  return out;',
  '})()',
].join('\n');

// Everything on a layer row starts and ends on the same two lines.
//
// The owner's rule for the matrix (2026-09-23): "Es sollte alles in einer
// Flucht sein… damit alles auf einer Ebene/Linie ist." It is a rule about
// arithmetic that only comes out at a particular window size, which is how it
// was broken the first time — the three controls held their height while the
// pictures grew, so a window dragged larger came apart and this one did not.
//
// A pixel of slack is allowed for a border that rounds the other way.
const FLUSH = [
  '(() => {',
  '  const rows = document.querySelectorAll(".layer");',
  '  const out = [];',
  '  const whole = [".layer-name", ".showing-picture", ".layer-controls",',
  '    ".layer-mute", ".pad"];',
  '  const off = (part, box, top, bottom, both) => {',
  '    const high = both || bottom === null ? Math.round(box.top - top) : 0;',
  '    const low = both || top === null ? Math.round(box.bottom - bottom) : 0;',
  '    if (Math.abs(high) > 1 || Math.abs(low) > 1) {',
  '      out.push({ part: part, top: high, bottom: low });',
  '    }',
  '  };',
  '  for (const row of rows) {',
  '    const line = row.querySelector(".pad");',
  '    if (!line) continue;',
  '    const edge = line.getBoundingClientRect();',
  '    for (const part of whole) {',
  '      const node = row.querySelector(part);',
  '      if (!node) continue;',
  '      off(part, node.getBoundingClientRect(), edge.top, edge.bottom, true);',
  '    }',
  '    // And inside the controls column, which is on the line whatever its',
  '    // contents do: the first control must start on it and the last must',
  '    // end on it, or the three have stopped sharing the height.',
  '    const controls = row.querySelector(".layer-controls");',
  '    if (!controls || controls.children.length === 0) continue;',
  '    const kids = controls.children;',
  '    off("first control", kids[0].getBoundingClientRect(), edge.top, null, false);',
  '    off("last control", kids[kids.length - 1].getBoundingClientRect(),',
  '      null, edge.bottom, false);',
  '  }',
  '  return { rows: rows.length, off: out };',
  '})()',
].join('\n');

// How much of the band the layer rows actually use.
//
// The owner, 2026-09-23: "Möglichst wenig Platz verschenken und immer noch
// alles im Blick haben." What they were looking at was three layers in the
// top third of a window with grey under them, which is a number nobody was
// measuring. Reported rather than flagged: one layer on a tall window fills
// less of it than eight do and is not wrong for that, so what this is for is
// noticing a change, not passing a threshold.
const FILL = [
  '(() => {',
  '  const matrix = document.getElementById("matrix");',
  '  if (!matrix || matrix.clientHeight === 0) return null;',
  '  const rows = matrix.querySelectorAll(".layer");',
  '  if (rows.length === 0) return null;',
  '  let used = 0;',
  '  for (const row of rows) used += row.getBoundingClientRect().height;',
  '  return { rows: rows.length, used: Math.round(used), has: matrix.clientHeight };',
  '})()',
].join('\n');

// An element id that is in the page twice.
//
// `getElementById` answers with the first one and says nothing about the
// second, so a panel wired to the second is wired to nothing: it draws, it
// takes clicks, and none of them arrive. It is what a patch that ran twice
// leaves behind, and nothing else here would have found it — every copy is
// inside its box, the right shape and not scrolling.
const DUPLICATES = [
  '(() => {',
  '  const seen = new Map();',
  '  for (const node of document.querySelectorAll("[id]")) {',
  '    seen.set(node.id, (seen.get(node.id) || 0) + 1);',
  '  }',
  '  const out = [];',
  '  for (const pair of seen) {',
  '    if (pair[1] > 1) out.push({ id: pair[0], times: pair[1] });',
  '  }',
  '  return out;',
  '})()',
].join('\n');

// Every surface that claims a shape, and the shape it was actually drawn at.
const SHAPES = [
  '(() => {',
  '  const out = [];',
  '  for (const node of document.querySelectorAll(".stage")) {',
  '    const box = node.getBoundingClientRect();',
  '    if (box.width === 0 || box.height === 0) continue;',
  '    const wanted = getComputedStyle(node).aspectRatio;',
  '    out.push({',
  '      where: node.className,',
  '      w: Math.round(box.width),',
  '      h: Math.round(box.height),',
  '      drawn: box.width / box.height,',
  '      wanted: wanted,',
  '    });',
  '  }',
  '  return out;',
  '})()',
].join('\n');

/** Reports any part of a layer row that is off the row's own two lines. */
async function flush(win, prefix) {
  const found = await win.webContents.executeJavaScript(FLUSH);
  if (!found || found.rows === 0) {
    return;
  }
  if (found.off.length === 0) {
    console.log(prefix + '  flush: ' + found.rows + ' rows, every block on the line');
    return;
  }
  for (const entry of found.off) {
    console.log(
      prefix + '  OFF THE LINE: ' + entry.part + ' top ' + entry.top +
        'px, bottom ' + entry.bottom + 'px',
    );
  }
}

app.whenReady().then(async () => {
  const win = new BrowserWindow({
    width: 1586,
    height: 893,
    show: false,
    webPreferences: {
      offscreen: true,
      preload: join(__dirname, 'stub-preload.js'),
      sandbox: false,
    },
  });

  win.webContents.on('console-message', (_event, _level, message) => {
    if (message.includes('Uncaught')) {
      console.log('RENDERER ERROR: ' + message);
    }
  });

  await win.loadFile(join(CLIENT, 'dist/ui/index.html'));
  await new Promise((resolve) => setTimeout(resolve, 600));

  // Every screen, and every tab of the one that has tabs: a box that only
  // exists on the DMX pane is a box nobody measures if only the first pane is
  // ever opened.
  const screens = [
    // The connect screen, which every other scene hides. It is the first thing
    // an operator sees and it now carries the list of servers this client
    // found — the longest name in the stub is in it on purpose.
    [
      'connect',
      'for (const id of ["live", "media", "settings"]) document.getElementById(id).hidden = true;' +
        ' document.getElementById("connect").hidden = false',
    ],
    ['live', 'document.getElementById("connect").hidden = true; document.getElementById("mode-live").click()'],
    // A slot's own OPTIONS, which the layer's does not cover: it carries the
    // beat count, the rate, and the sentence that says what the two of them are
    // asking of the decoder — the longest line in the panel, and the one added
    // because the owner's wall stopped without it (2026-09-23).
    [
      'live, a slot chosen',
      'document.querySelector(".pad:not(.empty) .pad-label").click()',
    ],
    ['media', 'document.getElementById("mode-media").click()'],
    ['settings/mapping', 'document.getElementById("mode-settings").click(); document.getElementById("tab-mapping").click()'],
    ['settings/mapping output', 'document.getElementById("mapping-face-output").click()'],
    ['settings/dmx', 'document.getElementById("tab-dmx").click()'],
    ['settings/layers', 'document.getElementById("tab-layers").click()'],
    ['settings/show', 'document.getElementById("tab-show").click()'],
    // With a show chosen, which is the state the look-inside panel is for.
    // Left unclicked, the middle column says "click a show" and the whole
    // preview goes unmeasured.
    ['settings/show chosen', 'document.querySelector("#show-list .show").click()'],
  ];

  let total = 0;
  // Reported once each rather than once per screen: a duplicate in the
  // shared markup is on every screen, and eight copies of one finding is a
  // way of burying the other seven.
  const doubled = new Set();
  for (const pair of screens) {
    const name = pair[0];
    await win.webContents.executeJavaScript(pair[1]).catch(() => {});
    await new Promise((resolve) => setTimeout(resolve, 250));
    const found = await win.webContents.executeJavaScript(PROBE);
    total += found.length;
    console.log('=== ' + name + ': ' + found.length + ' drawn outside their box');
    for (const entry of found) {
      console.log(
        '  ' + entry.where + '  box ' + entry.box.w + 'x' + entry.box.h +
          '  over x' + entry.over.x + ' y' + entry.over.y +
          '  "' + entry.text + '"',
      );
    }

    const scrolling = await win.webContents.executeJavaScript(SCROLLERS);
    for (const entry of scrolling) {
      console.log(
        '  scrolls: ' + entry.where + '  shows ' + entry.shows +
          'px of ' + (entry.shows + entry.over) + 'px',
      );
    }

    const pads = await win.webContents.executeJavaScript(PADS);
    for (const entry of pads) {
      const wrong = entry.pad > entry.picture + 1;
      console.log(
        '  ' + (wrong ? 'PAD TOO WIDE' : 'pad') + ': ' + entry.pad +
          'px against a picture of ' + entry.picture + 'px',
      );
    }

    await flush(win, '');

    const fill = await win.webContents.executeJavaScript(FILL);
    if (fill) {
      console.log(
        '  layers: ' + fill.rows + ' rows use ' + fill.used + 'px of ' +
          fill.has + 'px (' + Math.round((fill.used / fill.has) * 100) + '%)',
      );
    }

    const twice = await win.webContents.executeJavaScript(DUPLICATES);
    for (const entry of twice) {
      if (doubled.has(entry.id)) continue;
      doubled.add(entry.id);
      console.log(
        '  DUPLICATE ID: #' + entry.id + ' is in the page ' + entry.times +
          ' times',
      );
    }

    const shapes = await win.webContents.executeJavaScript(SHAPES);
    for (const entry of shapes) {
      const off = entry.wanted && entry.wanted !== 'auto'
        ? Math.abs(entry.drawn - ratioOf(entry.wanted)) > 0.02
        : false;
      console.log(
        '  ' + (off ? 'WRONG SHAPE' : 'shape') + ': .' +
          String(entry.where).split(' ').join('.') + '  ' +
          entry.w + 'x' + entry.h + '  drawn ' + entry.drawn.toFixed(2) +
          '  wanted ' + entry.wanted,
      );
    }
  }
  // And again on a window somebody has dragged larger, which is the size the
  // arithmetic came apart at. The rows are the only thing measured twice: they
  // are the only part of the window whose sizes are solved rather than written.
  await win.webContents.executeJavaScript('document.getElementById("mode-live").click()');
  win.setSize(2560, 1440);
  await new Promise((resolve) => setTimeout(resolve, 400));
  console.log('=== live, on a window dragged to 2560x1440');
  await flush(win, '');
  const wide = await win.webContents.executeJavaScript(PADS);
  for (const entry of wide) {
    const wrong = entry.pad > entry.picture + 1;
    console.log(
      '  ' + (wrong ? 'PAD TOO WIDE' : 'pad') + ': ' + entry.pad +
        'px against a picture of ' + entry.picture + 'px',
    );
  }

  // And the same window dragged short. **Nothing may move.** Less height per
  // row is the same arithmetic as another layer, and until 2026-09-27 both
  // resized every pad on the desk: the tile was the lower of what the width
  // allowed and what the row's share of the height allowed, so past four layers
  // the height won and the whole matrix changed size under the operator. The
  // width is unchanged across these two passes, so this compares exactly.
  win.setSize(2560, 620);
  await new Promise((resolve) => setTimeout(resolve, 400));
  console.log('=== live, on the same window dragged short');
  const short = await win.webContents.executeJavaScript(PADS);
  const was = wide[0] ? wide[0].pad : null;
  const now = short[0] ? short[0].pad : null;
  if (was === null || now === null) {
    console.log('  NO PADS to measure, which is a finding in itself');
    total += 1;
  } else if (was === now) {
    console.log(
      '  pad size: ' + now + 'px at both 1440px and 620px of window, ' +
        'and ' + short.length + ' rows drawn',
    );
  } else {
    console.log(
      '  PAD SIZE MOVED: ' + was + 'px on the tall window and ' + now +
        'px on the short one; height must not reach the pads',
    );
    total += 1;
  }

  console.log('total: ' + total + ', duplicate ids: ' + doubled.size);

  app.exit(0);
});
