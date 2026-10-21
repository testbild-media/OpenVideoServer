// Presses the live screen's controls and says which ones do nothing.
//
// The audit next door measures where things are *drawn*. This measures whether
// they *act*, which is a different question and one nothing was asking.
//
// # Why this exists
//
// The FREEZE button had no click handler. Its listener sat in a `#wireMaster`
// method that nothing ever called, so the button drew correctly, lit correctly,
// enabled and disabled itself correctly — and answered a press with nothing. It
// survived two rounds of being reported and "fixed", because every check this
// client had looked at the drawing:
//
// * the unit suites test the command *builders*, and `masterCommand` was right;
// * the overflow audit clicks controls, but only to reach a screen it wants to
//   measure, and never looks at what the click did;
// * the one test that pressed FREEZE for real is in the integration suite,
//   which is skipped unless a live server address is given.
//
// So a control could be wired to nothing and every green tick would stay green.
// This closes that: it runs the real interface offscreen against the audit's
// stub, presses things, and fails when a press produces no call.
//
// A control that is deliberately disabled is not dead — it says so, and is
// reported as skipped rather than failed.

const { app, BrowserWindow } = require('electron');
const { join } = require('node:path');

const NEWLINE = String.fromCharCode(10);

const CLIENT = join(__dirname, '..', '..');

/** Anything the page threw while this ran. */
const thrown = [];

/** Findings that are not about a control being pressed. */
let bad = 0;

/**
 * What to press, and what a press has to reach the bridge with.
 *
 * `run` covers every live command; anything else names the bridge call. `wants`
 * is matched against the command's `do` for `run`, and against the call's own
 * name otherwise.
 *
 * `changes` is for a control that reaches nothing: it names an element whose
 * measured shape has to be different afterwards. Zoom is the case -- how
 * closely the canvas is being looked at is nobody's business but this
 * window's -- and a control like that still has to be proved to do something.
 *
 * `turn` is for a control that is not a button. A press is a click, and a click
 * is nothing at all to a field: a fader listens for `input`, a number field and
 * a checkbox for `change`. So every entry here was a button until 2026-09-27,
 * which is why no parameter control in this client had ever been pressed -- and
 * why the owner found the generator's parameters broken rather than a run did.
 * It says what to put in and which event says it.
 */
const CONTROLS = [
  // --- the global bar ---
  { what: 'FREEZE', id: 'freeze', sends: 'set_master' },
  { what: 'TAP', id: 'tap', sends: 'tap' },
  { what: 'phase reset', id: 'phase-reset', sends: 'reset_phase' },
  { what: 'DMX on/off', id: 'dmx-enabled', sends: 'set_master' },
  { what: 'BPM down', id: 'bpm-down', sends: 'set_master' },
  { what: 'BPM up', id: 'bpm-up', sends: 'set_master' },
  { what: 'save the live state', id: 'save-live', call: 'captureLiveState' },
  // --- a layer, and a slot on it ---
  { what: 'MUTE', selector: '.layer-mute', sends: 'set_layer' },
  // Not the bus it is already on: a command that would move nothing is not
  // sent, on purpose, so pressing the lit pad proves nothing either way.
  { what: 'a bus pad', selector: '.routes .route:not(.on)', sends: 'set_layer' },
  // On a layer whose slots are the client's: the stub's sixth is a desk's.
  { what: 'a slot pad', selector: '.slots:not(.desk) .pad:not(.empty)', sends: 'select_slot' },
  // --- what a lighting console holds (owner, 2026-10-19) ---
  //
  // The other half of this file's question: a control a desk holds must *not*
  // act. Each of these fails when the control is offered after all, and when
  // the screen stops marking it — a selector that matches nothing is MISSING.
  {
    what: "a held layer's pad",
    selector: '.slots.desk .pad:not(.empty)',
    sends: 'select_slot',
    expect: 'nothing',
  },
  { what: "a held layer's opacity", selector: '.layer-opacity.desk input', expect: 'held' },
  { what: 'a held crossfader', selector: '.desk > #crossfader', expect: 'held' },
  // --- a slot's own settings, which is where the newest controls are ---
  {
    what: "a slot's speed pad",
    selector: '.option-control .speed-steps .speed-step:not(.on)',
    call: 'setSlot',
    open: '.pad:not(.empty) .pad-label',
  },
  // --- a generator slot, whose controls live in OPTIONS beside the slot's own
  //
  // Nothing pressed these until 2026-09-27: every pad in the stub played a clip,
  // so the panel a generator draws was never built, let alone pressed. The owner
  // found it broken instead, which is what this file exists to prevent.
  //
  // Slot 2 is the generator, and a pad is opened by its label rather than by
  // playing it: pressing the pad itself would put it on air.
  {
    what: "a generator's colour",
    // Inside the `.work-row`: the slot's own settings are a `.live-facts` as
    // well, and a selector without this presses one of those instead.
    selector: '.options-column .work-row .live-facts input[type="color"]',
    sends: 'set_generator',
    open: '.pad-label[title="Set up slot 2 in OPTIONS without playing it"]',
    // sRGB hex, which is the only thing the element takes. Anything else is
    // read as black, and a probe that sent black would pass while proving
    // nothing — which is what `0.75` did here when this was a red channel.
    turn: { value: '#3366ff', event: 'change' },
  },
  {
    what: "a generator's alpha",
    // Its own fader beside the swatch, because `input[type=color]` has no
    // alpha channel — and its own listener, which is why it is pressed. It was
    // a number field until 2026-09-27 and this said `[type="number"]`, so the
    // change to a fader reported MISSING here rather than passing quietly.
    selector: '.options-column .work-row .option-colour input[type="range"]',
    sends: 'set_generator',
    open: '.pad-label[title="Set up slot 2 in OPTIONS without playing it"]',
    // `input`, not `change`: a fader sends as it moves, like every other one.
    turn: { value: 0.5, event: 'input' },
  },
  {
    what: "a generator's menu",
    // A `select`, which is what a declared `choice` is drawn with. It was four
    // colour number fields until 2026-09-27, so this selector matched nothing
    // — a MISSING here is the regression coming back.
    selector: '.options-column .work-row .live-facts select',
    sends: 'set_generator',
    open: '.pad-label[title="Set up slot 2 in OPTIONS without playing it"]',
    turn: { value: 'triangle', event: 'change' },
  },
  {
    what: "a generator's text",
    // A `textarea`, because the text generator sets a line break as one.
    selector: '.options-column .work-row .live-facts textarea',
    sends: 'set_generator',
    open: '.pad-label[title="Set up slot 2 in OPTIONS without playing it"]',
    turn: { value: 'PRESSED', event: 'change' },
  },
  {
    what: "a generator's Put back",
    selector: '.options-column .work-row-head button',
    sends: 'set_generator',
    open: '.pad-label[title="Set up slot 2 in OPTIONS without playing it"]',
  },
  // --- an effect on a chain, which is the other column of OPTIONS
  //
  // The three faults the owner reported on 2026-09-26 were all in this column,
  // and all three reached him for one reason: the stub's chains were empty, so
  // nothing had ever drawn it. Every probe here returns to the layer first,
  // because the generator probes above leave the panel on a slot and a slot
  // chain has one effect on it -- which cannot be reordered.
  {
    // Folded until opened, and still open after OPTIONS has gone to a slot
    // and come back to the layer, which draws the chain again (owner,
    // 2026-10-21). First in this group, because it is the one that needs the
    // chain untouched by anybody.
    what: 'an effect stays as it was left',
    selector: '.options-column details.work-row',
    open: '.options-subject button',
    folds: ['.pad:not(.empty) .pad-label', '.options-subject button'],
  },
  {
    what: "an effect's parameter",
    // `details.work-row`: an effect's row is a fold, and the generator's row in
    // the other column is a plain div. A closed fold still has its fields.
    selector: '.options-column details.work-row .live-facts input',
    sends: 'set_effect',
    open: '.options-subject button',
    // Brightness is declared from -1 to 1, and a fader listens for `input`.
    turn: { value: 0.5, event: 'input' },
  },
  {
    what: "an effect's switch",
    // The only checkbox in the panel, and the only thing that draws a `toggle`
    // at all. A click is what a checkbox takes, so this one needs no `turn`.
    selector: '.options-column details.work-row .live-facts input[type="checkbox"]',
    sends: 'set_effect',
    open: '.options-subject button',
  },
  {
    what: 'move an effect down the chain',
    selector: '.options-column button[title="one place later in the chain"]',
    call: 'orderEffects',
    open: '.options-subject button',
  },
  {
    what: 'take an effect off the chain',
    // The only red thing on the row, which is how it says it edits the show.
    selector: '.options-column .work-row-head button.danger',
    call: 'removeEffect',
    open: '.options-subject button',
  },
  // A locked effect: its ✕ is not offered, and neither is the fader a desk
  // holds on it. The lock itself is, to an administrator.
  {
    what: 'remove a locked effect',
    selector: '.options-column details.work-row:has(.lock-mark) .work-row-head button.danger',
    open: '.options-subject button',
    expect: 'held',
  },
  {
    what: 'a fader the desk holds',
    selector: '.options-column details.work-row:has(.lock-mark) .live-fact.desk input',
    open: '.options-subject button',
    expect: 'held',
  },
  {
    what: 'unlock an effect',
    selector: '.options-column details.work-row:has(.lock-mark) button[title^="Let "]',
    call: 'setLock',
    open: '.options-subject button',
  },
  {
    what: 'lock an effect',
    selector: '.options-column details.work-row:not(:has(.lock-mark)) button[title^="Lock "]',
    call: 'setLock',
    open: '.options-subject button',
  },
  // --- IO mapping, rebuilt on 2026-09-25 and so the part least proved ---
  //
  // Each one picks a piece first: the controls act on the selection, so a
  // press with nothing selected proves only that the button is disabled.
  ...[
    { what: 'move a piece up', id: 'map-move-up' },
    { what: 'move a piece down', id: 'map-move-down' },
    { what: 'duplicate a piece', id: 'map-duplicate' },
    { what: 'take the other side', id: 'mapping-face-take' },
    { what: 'add a piece', id: 'map-add-piece' },
    // Last, because it takes away the piece the others need.
    { what: 'remove a piece', id: 'map-remove' },
  ].map((control) => ({
    ...control,
    call: 'setMapping',
    open: ['#mode-settings', '#tab-mapping', '.row-piece .row-name'],
  })),
  // Puts the pieces on the walls. Its own entry rather than one of the six
  // above: it needs no selection, and it answers a different call.
  // Zoom does not reach the server — it is the canvas, not the show — so it
  // is proved by what it does to the page rather than by a call.
  {
    what: 'zoom in',
    id: 'zoom-in',
    open: ['#mode-settings', '#tab-mapping'],
    changes: '.stage',
  },
  {
    what: 'Test Pieces',
    id: 'mapping-test-pieces',
    call: 'setTestPattern',
    open: ['#mode-settings', '#tab-mapping'],
  },
  // --- the patch as a table (owner, 2026-10-21) ---
  //
  // (html, how, suggested): a whole sheet with a row in it, asked for the way
  // the button says. A sheet with no table would mean the show in hand never
  // reached it, which is the failure a press alone would not see.
  {
    what: 'print the patch',
    id: 'patch-print',
    call: 'printSheet',
    open: ['#mode-settings', '#tab-dmx'],
    args: (args) => args[1] === 'print' && /<table>[\s\S]*<td/.test(args[0]),
  },
  {
    what: 'save the patch as a PDF',
    id: 'patch-pdf',
    call: 'printSheet',
    args: (args) => args[1] === 'pdf' && /<table>[\s\S]*<td/.test(args[0]),
  },
  // Setting an output's mode. The one control in this panel that reaches a
  // cable: the fields used to send on every change, so getting from one
  // resolution to another took the panel through the sizes in between and made
  // it resynchronise for each (owner, 2026-09-25).
  {
    what: 'set an output mode',
    id: 'map-set-mode',
    call: 'setMapping',
    // The mode button on the output's own row selects it. Not the name field:
    // that selects on focus, and `click()` dispatches a click without moving
    // focus, so the panel on the right would never have been about this output.
    open: ['#mode-settings', '#tab-mapping', '.row-output .row-says'],
    prepare:
      // A mode is chosen now, never typed, so this picks a different one
      // from the list. Set on the mode already running deliberately sends
      // nothing, which is why it has to be a different one.
      "const mode = document.getElementById('map-output-mode');" +
      " const other = [...mode.options].find((each) => each.value !== mode.value);" +
      " mode.value = other.value;" +
      " mode.dispatchEvent(new Event('change'));",
  },
  // A network output. The confirm button is disabled until the stream has a
  // name — which is the point of it — so this one has to type before it presses,
  // and a press without the typing would prove only that the guard works.
  {
    what: 'add a network output',
    id: 'add-output-confirm',
    call: 'setMapping',
    open: ['#mode-settings', '#tab-mapping', '#map-add-output'],
    prepare:
      // 'ndi' is the connector that means the network. It used to be a chooser
      // of its own with 'network' in it; NDI is one of the sockets now, which
      // is what lets an output move between a cable and the network.
      "const where = document.querySelector('#add-output-fields select');" +
      " where.value = 'ndi';" +
      " where.dispatchEvent(new Event('change'));" +
      " const named = document.querySelector('#add-output-fields input[type=text]');" +
      " named.value = 'Press probe';" +
      " named.dispatchEvent(new Event('input'));",
  },
  // --- folders, as folders (owner, 2026-10-20) ---
  //
  // In MEDIA and in the Live browser alike: a folder is a tile that opens, a
  // path goes back up, a search finds folders by name, and a clip or a folder
  // dragged onto a folder goes into it.
  {
    // The owner, 2026-10-20: during an upload the box for a new folder closed
    // as soon as it opened, as if somebody had clicked beside it.
    what: 'name a folder mid-upload',
    id: 'library-new-folder',
    open: ['#mode-media'],
    keeps: '#library-folders .folder-input',
  },
  {
    // The same page, the other box on it: a clip's name, which is filed the
    // moment the box loses focus — so a box taken away mid-word would rename
    // the clip to half of what was being typed.
    what: 'rename a clip mid-upload',
    selector: '#media-fields .media-rename-box',
    open: ['#mode-media', '#library-grid .media-card'],
    prepare: "document.querySelector('#media-fields .media-rename-box').focus();",
    keeps: '#media-fields .media-rename-box',
  },
  {
    what: 'open a folder tile',
    selector: '#library-grid .folder-tile:not(.up)',
    open: ['#mode-media'],
    reveals: { selector: '#library-path .browse-path-here', text: 'Nachtfahrt' },
  },
  {
    what: 'go back up the path',
    selector: '#library-path .browse-path-step',
    reveals: { selector: '#library-grid .folder-tile-name', text: 'Zweiter Ordner' },
  },
  {
    what: 'file a clip on a folder',
    selector: '#library-grid .media-card',
    drag: '#library-grid .folder-tile:not(.up)',
    call: 'fileMedia',
  },
  {
    what: 'drag a folder into one',
    selector: '#library-grid .folder-tile[title="Open Zweiter Ordner"]',
    drag: '#library-grid .folder-tile[title^="Open Nachtfahrt"]',
    call: 'renameFolder',
  },
  {
    what: 'drag a tree folder in',
    selector: '#library-folders .folder-row:has(.folder-name[title="Zweiter Ordner"])',
    drag: '#library-folders .folder-row:has(.folder-name[title^="Nachtfahrt"])',
    call: 'renameFolder',
  },
  {
    what: 'search for a folder',
    selector: '#library-search',
    turn: { value: 'Unterordner', event: 'input' },
    reveals: { selector: '#library-grid .folder-tile-name', text: 'Unterordner' },
  },
  {
    what: 'open a folder it found',
    selector: '#library-grid .folder-tile:not(.up)',
    reveals: { selector: '#library-path .browse-path-here', text: 'Unterordner' },
  },
  {
    what: 'a Live browser folder',
    selector: '.browser-folder:not(.up)',
    open: ['#mode-live', '[data-tab="media"]'],
    reveals: { selector: '.browser-path .browse-path-here', text: 'Nachtfahrt' },
  },
  {
    what: 'Live browser back up',
    selector: '.browser-folder.up',
    reveals: { selector: '.browser-folder .folder-tile-name', text: 'Zweiter Ordner' },
  },
  {
    what: 'Live search a folder',
    selector: '.browser-search',
    // One a level down, which the top of the library does not show: a search
    // that found only what was already on screen would prove nothing.
    turn: { value: 'Unterordner', event: 'input' },
    reveals: { selector: '.browser-folder .folder-tile-name', text: 'Unterordner' },
  },
  // --- the matrix's own drag, and its banks (owner, 2026-10-21) ---
  {
    what: 'move a pad to an empty pad',
    selector: '#matrix > .layer:nth-child(2) .pad:not(.empty)',
    drag: '#matrix > .layer:nth-child(2) .pad.empty',
    call: 'moveSlot',
  },
  {
    // And to another layer's pad (owner, 2026-10-21: "von einem Layer auf
    // einen anderen Layer packen"), which was refused at first.
    what: "a pad onto another layer's",
    selector: '#matrix > .layer:nth-child(2) .pad:not(.empty)',
    drag: '#matrix > .layer:nth-child(3) .pad.empty',
    call: 'moveSlot',
    // (showId, layerId, from, to, toLayer): the layer it goes to is not the
    // one it left, which a move within the row would also have reached.
    args: (args) => typeof args[4] === 'string' && args[4] !== args[1],
  },
  {
    // A bank not opened yet is on the bar, and pressing it opens it.
    what: 'open a closed bank',
    selector: '#banks .bank.closed',
    call: 'setBanks',
  },
  {
    // And back, which the probes after this need: they press pads with
    // something in them, and the bank just opened has none.
    what: 'page back to the first bank',
    selector: '#banks .bank:first-child',
    reveals: { selector: '#matrix .pad:not(.empty) .pad-label', text: '1 ·' },
  },
  // Emptying a pad. Last in this list on purpose: every probe above it works
  // on a pad with something in it, and this one takes that away. A probe that
  // quietly broke the ones after it would read as a bug in whatever ran next.
  {
    what: 'clear a slot',
    id: 'live-clear-slot',
    call: 'clearSlot',
    open: '.pad:not(.empty) .pad-label',
  },
  // Handing the wall to another show. Before 'delete a show' because that one
  // takes a row away, and this one needs a show that is not already on air.
  {
    what: 'load a show',
    selector: '.show-load',
    call: 'runShow',
    open: ['#mode-settings', '#tab-show'],
    // The first press arms it and redraws the row, so this finds the button
    // again rather than keeping the one it pressed.
    prepare: "document.querySelector('.show-load').click();",
  },
  // Deleting a show. Two presses, like LOAD on the same row: the first arms it
  // and sends nothing, which is the whole point of it. So the preparation makes
  // the first and this makes the second — the gesture an operator makes.
  {
    what: 'delete a show',
    selector: '.show-delete',
    call: 'deleteShow',
    open: ['#mode-settings', '#tab-show'],
    prepare: "document.querySelector('.show-delete').click();",
  },
];

/** Runs one press and says what it produced. */
async function press(win, control) {
  const find = control.id
    ? `document.getElementById(${JSON.stringify(control.id)})`
    : `document.querySelector(${JSON.stringify(control.selector)})`;

  // A list, because reaching a control can take more than one click: the
  // screen, then the tab on it, then the thing the control acts on.
  const opens = control.open === undefined ? [] : [control.open].flat();
  for (const step of opens) {
    await win.webContents.executeJavaScript(
      `(() => { const n = document.querySelector(${JSON.stringify(step)});` +
        ' if (n) n.click(); })()',
    );
    await new Promise((resolve) => setTimeout(resolve, 250));
  }

  // Some controls are deliberately unusable until something has been said to
  // them. Typing into a field is not something a click can do, so a probe may
  // bring its own preparation — and one that presses without it would be
  // proving the guard rather than the control.
  if (control.prepare) {
    // Caught, because a preparation that throws used to leave the run waiting
    // on a promise nobody would settle: a broken probe looked like a slow one
    // and the audit timed out with no summary at all. A tool for noticing
    // things that reach nothing has to notice itself reaching nothing.
    try {
      await win.webContents.executeJavaScript(`(() => { ${control.prepare} })()`);
    } catch (error) {
      return { verdict: 'BROKEN', detail: `its preparation failed: ${error.message}` };
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }

  const state = await win.webContents.executeJavaScript(
    `(() => { const n = ${find};` +
      ' if (!n) return { missing: true };' +
      ' return { disabled: n.disabled === true, hidden: n.hidden === true }; })()',
  );

  if (state.missing) {
    return { verdict: 'MISSING', detail: 'no such control on the screen' };
  }
  // A control a desk holds has to be shown and not offered. Offered is the
  // failure here, the opposite of every other probe.
  if (control.expect === 'held') {
    return state.disabled
      ? { verdict: 'held', detail: 'shown and not offered' }
      : { verdict: 'OFFERED', detail: 'a control the desk holds can be pressed' };
  }
  if (state.disabled) {
    return { verdict: 'skipped', detail: 'disabled, and says so' };
  }

  // **A fold that has to come back the way it was left** (owner, 2026-10-21):
  // an effect folded up by hand came open again on every rebuild of OPTIONS.
  // Folded at first, opened by the probe, then the panel drawn again by the
  // steps in `folds` — and it has to be open still. The old element is marked
  // first, and one that was never replaced is BROKEN rather than kept: a fold
  // nobody drew again proves nothing about drawing.
  if (control.folds) {
    const read =
      `(() => { const n = ${find}; if (!n) return null;` +
      ' return { open: n.open === true, old: n.dataset.probe === "before" }; })()';
    const first = await win.webContents.executeJavaScript(read);
    if (first.open) {
      return { verdict: 'OPENED', detail: 'unfolded before anybody opened it' };
    }
    await win.webContents.executeJavaScript(
      `(() => { const n = ${find}; n.dataset.probe = "before"; n.open = true; })()`,
    );
    await new Promise((resolve) => setTimeout(resolve, 250));
    for (const step of control.folds) {
      await win.webContents.executeJavaScript(
        `(() => { const n = document.querySelector(${JSON.stringify(step)}); if (n) n.click(); })()`,
      );
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    const after = await win.webContents.executeJavaScript(read);
    if (after === null) {
      return { verdict: 'MISSING', detail: 'gone after it was drawn again' };
    }
    if (after.old) {
      return { verdict: 'BROKEN', detail: 'never drawn again, so this proves nothing' };
    }
    return after.open
      ? { verdict: 'keeps', detail: 'still unfolded after it was drawn again' }
      : { verdict: 'LOST', detail: 'folded again when it was drawn again' };
  }

  const measure = control.changes
    ? `(() => { const n = document.querySelector(${JSON.stringify(control.changes)});` +
      ' if (!n) return null; const r = n.getBoundingClientRect();' +
      ' return Math.round(r.width) + "x" + Math.round(r.height); })()'
    : null;
  const shapeBefore = measure === null ? null : await win.webContents.executeJavaScript(measure);

  const before = await win.webContents.executeJavaScript('window.sent.all().length');
  // A button is clicked and a field is turned. The value is assigned before the
  // event because the listener reads it off the element rather than off the
  // event -- which is also the reason a click never reached one.
  //
  // And a thing that is dragged is dragged: picked up, carried over the
  // target and let go, with one DataTransfer through all four events, which
  // is what a real drag hands every listener. Filing a clip and moving a
  // folder are nothing but this, so a click proves nothing about either.
  const act = control.drag
    ? `const t = document.querySelector(${JSON.stringify(control.drag)});` +
      ' if (!t) throw new Error("nothing to drop on");' +
      ' const d = new DataTransfer();' +
      ' const ev = (type) => new DragEvent(type, { bubbles: true, cancelable: true, dataTransfer: d });' +
      " n.dispatchEvent(ev('dragstart')); t.dispatchEvent(ev('dragover'));" +
      " t.dispatchEvent(ev('drop')); n.dispatchEvent(ev('dragend'))"
    : control.turn
      ? `n.value = ${JSON.stringify(String(control.turn.value))};` +
        ` n.dispatchEvent(new Event(${JSON.stringify(control.turn.event)}))`
      : 'n.click()';
  try {
    await win.webContents.executeJavaScript(`(() => { const n = ${find}; ${act}; })()`);
  } catch (error) {
    return { verdict: 'BROKEN', detail: `the press itself failed: ${error.message}` };
  }
  await new Promise((resolve) => setTimeout(resolve, 250));

  // A control that opens a box to type into, which has to *stay* open while the
  // page keeps receiving things — an upload's progress, ten times a second.
  // Proved by the box still being there, and still being where the keys go,
  // a second and a half later.
  //
  // **What was typed, not only where the focus is.** An offscreen window
  // fires no blur when a focused box is taken out of the page, so the first
  // version of this passed against the very code that lost the owner's box:
  // the box was replaced by a fresh one every update, and the fresh one took
  // the focus quietly. What the fresh one did not have was the text.
  if (control.keeps) {
    await win.webContents.executeJavaScript(
      `(() => { const b = document.querySelector(${JSON.stringify(control.keeps)});` +
        " if (b) { b.value = 'Typed by the probe'; b.dispatchEvent(new Event('input')); } })()",
    );
    await new Promise((resolve) => setTimeout(resolve, 1500));
    const kept = await win.webContents.executeJavaScript(
      `(() => { const a = document.activeElement;` +
        ` return a !== null && a.matches(${JSON.stringify(control.keeps)}) && a.value === 'Typed by the probe'; })()`,
    );
    return kept
      ? { verdict: 'keeps', detail: `${control.keeps} still holds what was typed` }
      : { verdict: 'LOST', detail: `${control.keeps} lost what was typed while the page updated` };
  }

  // A control that changes what the screen says rather than what it sends:
  // opening a folder, searching for one. Proved by the words that appear.
  if (control.reveals) {
    const shown = await win.webContents.executeJavaScript(
      `(() => { const r = [...document.querySelectorAll(${JSON.stringify(control.reveals.selector)})];` +
        ` return r.some((x) => x.offsetParent !== null && x.textContent.includes(${JSON.stringify(control.reveals.text)})); })()`,
    );
    return shown
      ? { verdict: 'shows', detail: `${control.reveals.selector} says “${control.reveals.text}”` }
      : { verdict: 'DEAD', detail: `${control.reveals.selector} never said “${control.reveals.text}”` };
  }

  if (measure !== null) {
    const shapeAfter = await win.webContents.executeJavaScript(measure);
    if (shapeBefore === null || shapeAfter === null) {
      return { verdict: 'DEAD', detail: `no ${control.changes} to measure` };
    }
    return shapeAfter === shapeBefore
      ? { verdict: 'DEAD', detail: `${control.changes} is still ${shapeAfter}` }
      : { verdict: 'changes', detail: `${control.changes} ${shapeBefore} -> ${shapeAfter}` };
  }
  const after = JSON.parse(
    await win.webContents.executeJavaScript(
      `JSON.stringify(window.sent.all().slice(${before}))`,
    ),
  );

  // `args`, where a probe has one, is what the call has to have been asked:
  // reaching the bridge with the wrong question is not reaching it.
  const reached = after.some(
    (entry) =>
      (control.call !== undefined
        ? entry.call === control.call
        : entry.call === 'run' && entry.args[0]?.do === control.sends) &&
      (control.args === undefined || control.args(entry.args)),
  );
  const names = after
    .map((entry) => (entry.call === 'run' ? `run:${entry.args[0]?.do}` : entry.call))
    .join(', ');

  // A pad a desk holds is still a button — it says why it will not act — so
  // it is pressed, and what it must produce is nothing.
  if (control.expect === 'nothing') {
    return reached
      ? { verdict: 'SENT', detail: `a press the desk holds sent ${names}` }
      : { verdict: 'refuses', detail: names === '' ? 'nothing sent' : names };
  }

  return reached
    ? { verdict: 'sends', detail: names }
    : { verdict: 'DEAD', detail: names === '' ? 'nothing at all' : names };
}

// The stub keeps an upload moving while this runs, so that every control is
// pressed on a page that is being updated under it, as it is during a get-in.
process.env.OVS_STUB_UPLOADING = '1';

app.whenReady().then(async () => {
  const win = new BrowserWindow({
    width: 1586,
    height: 893,
    show: false,
    webPreferences: {
      offscreen: true,
      preload: join(__dirname, '..', 'overflow', 'stub-preload.js'),
      sandbox: false,
    },
  });

  // **A throw is a failure here, not a line in the log.** The way the stub
  // rots is that the bridge grows a call and this does not, and then every
  // control that uses it dies with `is not a function` — which is exactly how
  // `setSlot` came to be missing while both of these tools reported green.
  win.webContents.on('console-message', (_event, _level, message) => {
    if (message.includes('Uncaught')) {
      thrown.push(message.split(NEWLINE)[0]);
    }
  });

  // **What the last run left behind is not this run's.** The window keeps what
  // an operator unfolded (OPTIONS' effects, 2026-10-21) in its own storage,
  // and that storage outlives the run: the second run found an effect open
  // that nobody in it had opened, and a probe that says "folded until opened"
  // has to start from a window nobody has used.
  await win.webContents.session.clearStorageData({ storages: ['localstorage'] });
  await win.loadFile(join(CLIENT, 'dist/ui/index.html'));
  await new Promise((resolve) => setTimeout(resolve, 600));
  await win.webContents.executeJavaScript(
    'document.getElementById("connect").hidden = true; document.getElementById("mode-live").click()',
  );
  await new Promise((resolve) => setTimeout(resolve, 400));

  // What the connect screen says about servers, in both states. The owner could
  // not tell "searching" from "found nothing" from "this build has no such
  // feature", because the panel was hidden when the list was empty.
  const note = async () =>
    win.webContents.executeJavaScript(
      '(() => { const f = document.getElementById("found"); const n = document.getElementById("found-note");' +
        ' const l = document.getElementById("found-list");' +
        ' return { shown: f ? !f.hidden : false, note: n ? n.textContent.trim() : null, rows: l ? l.children.length : 0 }; })()',
    );
  await win.webContents.executeJavaScript(
    'for (const id of ["live", "media", "settings"]) document.getElementById(id).hidden = true;' +
      ' document.getElementById("connect").hidden = false',
  );
  const found = await note();
  console.log('  connect, servers found:  ' + JSON.stringify(found));

  // What 100% means on the mapping canvas. The stub's composition is 1920
  // wide, so at one output pixel per screen pixel the canvas is 1920 across
  // and fitted is well under that -- which is the whole distinction, and the
  // one this got wrong first time round.
  const zoom = await win.webContents.executeJavaScript(
    '(async () => {' +
      ' document.getElementById("connect").hidden = true;' +
      ' document.getElementById("mode-settings").click();' +
      ' document.getElementById("tab-mapping").click();' +
      ' await new Promise((r) => setTimeout(r, 300));' +
      ' const wide = () => Math.round(document.querySelector(".stage").getBoundingClientRect().width);' +
      ' const fitted = wide();' +
      ' const said = document.getElementById("zoom-fit").textContent;' +
      ' for (let i = 0; i < 8; i += 1) {' +
      '   document.getElementById("zoom-in").click();' +
      '   await new Promise((r) => setTimeout(r, 40));' +
      '   if (document.getElementById("zoom-fit").textContent === "100%") break;' +
      ' }' +
      ' const out = { fitted, said, atOneToOne: wide(), reads: document.getElementById("zoom-fit").textContent };' +
      // Two steps past 100%, because at 100% this cannot fail: 1920 is wider
      // than the box either way, but 1080 still fits its height, so the column
      // has nothing to scroll whether it is allowed to or not. Four times that
      // leaves no ambiguity.
      ' for (let i = 0; i < 2; i += 1) {' +
      '   document.getElementById("zoom-in").click();' +
      '   await new Promise((r) => setTimeout(r, 40));' +
      ' }' +
      ' out.zoomedTo = document.getElementById("zoom-fit").textContent;' +
      // Which element is the scroller, while the canvas is bigger than its box
      // in both directions. The box must be the one: when the column was,
      // reaching the canvas's own horizontal bar scrolled the tabs and the zoom
      // buttons off the top of the screen.
      ' const box = document.getElementById("composition-stage");' +
      ' const column = document.getElementById("mapping-main");' +
      ' out.boxScrolls = box.scrollWidth > box.clientWidth + 1 ||' +
      '   box.scrollHeight > box.clientHeight + 1;' +
      ' out.columnScrolls = column.scrollHeight > column.clientHeight + 1;' +
      ' const buttons = document.getElementById("stage-zoom").getBoundingClientRect();' +
      ' out.controlsOnScreen = buttons.top >= 0 && buttons.bottom <= window.innerHeight;' +
      ' document.getElementById("zoom-fit").click();' +
      ' await new Promise((r) => setTimeout(r, 120));' +
      ' out.said = document.getElementById("zoom-fit").textContent;' +
      ' out.backTo = Math.round(document.querySelector(".stage").getBoundingClientRect().width);' +
      ' return out;' +
      '})()',
  );
  console.log(
    `  zoom: fitted ${zoom.fitted}px, reads "${zoom.said}"; at ${zoom.reads} the canvas is ${zoom.atOneToOne}px`,
  );
  if (zoom.reads === '100%' && zoom.atOneToOne !== 1920) {
    console.log(`  BAD  100% is not one pixel for one: 1920 wide drew ${zoom.atOneToOne}px`);
    bad += 1;
  }
  if (zoom.fitted >= 1920) {
    console.log('  BAD  fitted is not under 100%, so the two states say the same thing');
    bad += 1;
  }
  console.log(
    `  zoom: at ${zoom.zoomedTo} the box scrolls ${zoom.boxScrolls}, the column scrolls ` +
      `${zoom.columnScrolls}, the zoom buttons are on screen ${zoom.controlsOnScreen}`,
  );
  if (!zoom.boxScrolls) {
    console.log('  BAD  the canvas box does not scroll, so a zoomed canvas cannot be reached');
    bad += 1;
  }
  if (zoom.columnScrolls) {
    console.log('  BAD  the column scrolls, which takes the controls off the top of the screen');
    bad += 1;
  }
  if (!zoom.controlsOnScreen) {
    console.log('  BAD  the zoom buttons are off screen while zoomed');
    bad += 1;
  }
  // **The panel is never hidden**, in either state. That is the whole point of
  // it: hidden, it could not say whether it was searching, whether it had found
  // nothing, or whether this build had the feature at all.
  const empty = process.env.OVS_STUB_NO_SERVERS === '1';
  const wrong = empty
    ? !found.shown || found.note === '' || found.rows !== 0
    : !found.shown || found.rows === 0 || found.note !== '';
  if (wrong) {
    bad += 1;
    console.log(
      '  DEAD     the connect screen says the wrong thing about ' +
        (empty ? 'an empty network' : 'the servers it found'),
    );
  }

  let dead = bad;
  for (const control of CONTROLS) {
    const result = await press(win, control);
    if (['DEAD', 'MISSING', 'BROKEN', 'OFFERED', 'SENT', 'LOST', 'OPENED'].includes(result.verdict)) {
      dead += 1;
    }
    const pad = control.what.padEnd(22, ' ');
    console.log(`  ${result.verdict.padEnd(8, ' ')} ${pad} ${result.detail}`);
  }

  for (const message of thrown) {
    console.log('  THREW    ' + message);
  }
  console.log(
    `controls that do nothing: ${dead}, uncaught errors: ${thrown.length}`,
  );
  app.exit(dead === 0 && thrown.length === 0 ? 0 : 1);
});
