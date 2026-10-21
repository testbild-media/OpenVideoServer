// A bridge that answers with a full, deliberately awkward show.
//
// The page is drawn without a server, with long names everywhere, so that every
// box that holds text is holding as much text as it ever will. That is the
// state the overflow probe cares about: a fixed width that fits one word and
// not the next is invisible in the source and obvious here.
//
// Written out as real properties rather than a Proxy: `contextBridge` copies
// the own keys of what it is given, and a Proxy that fabricates members on
// access has none.

const { contextBridge } = require('electron');

const refusal = { ok: false, code: 'measuring', message: 'no server' };
const ok = (value) => ({ ok: true, value });

/** A name long enough to find any box that cannot cut one off. */
const LONG = 'Nachtfahrt durch die Innenstadt, sehr langer Name 2026';

const TRANSFORM = { x: 0, y: 0, scale_x: 1, scale_y: 1, rotation_deg: 0 };

// **One of them plays a generator.** Every slot used to play a clip, so the
// controls a generator slot draws in OPTIONS were never built by the audit or
// the press harness — which is why the owner found them broken rather than a
// run did. Slot 2, because the first pad is the one everything else reaches for.
const slot = (index) => ({
  index,
  source:
    index === 2
      ? {
          kind: 'generator',
          instanceId: 'GENERATOR2',
          typeId: 'ovs.solid_colour',
          version: 1,
          // **A value the show set, and one a control source has moved.** An
          // empty pair never exercises `moved`, the Put back button, or the
          // line under a control that says what the show opens it on.
          parameters: {
            colour: { type: 'color', value: [1, 0.5, 0, 1] },
            shape: { type: 'choice', value: 'rectangle' },
            caption: { type: 'text', value: 'WERKHALLE' },
          },
          live: { colour: { type: 'color', value: [0, 0.25, 1, 1] } },
          // Nothing patched to it and nobody has locked it, so every probe
          // below can press its controls.
          held: [],
          locked: false,
        }
      : { kind: 'media', mediaItemId: 'MEDIA' + index },
  // A beat-locked slot on a four-minute clip, which is what the rate note is
  // for: it says the number, and at this setting it says it in red.
  sync: { kind: 'BEAT_SYNC', beats: 7 },
  speed: 1,
  playbackMode: 'loop',
  direction: 'forward',
  transform: TRANSFORM,
  // A slot's own chain, which the server orders by its own route. One is
  // enough here: what a slot chain has to prove is that the panel reaches it
  // at all, and the reordering is proved on the layer's two below.
  effects: [
    {
      id: 'SLOTFX' + index,
      typeId: 'ovs.colour_correct',
      version: 1,
      enabled: true,
      parameters: { brightness: { type: 'float', value: 0.25 } },
      live: {},
      held: [],
      locked: false,
    },
  ],
});

const layer = (id, order) => ({
  id,
  name: `${LONG} ${order}`,
  enabled: true,
  transport: 'playing',
  route: 'A',
  blendMode: 'normal',
  opacity: 1,
  // The layer's live placement. Added when OPTIONS grew faders for it: a stub
  // that leaves out a field the view declares is a stub that tests a window
  // nobody will ever run.
  transform: TRANSFORM,
  activeSlot: 1,
  trigger: 1,
  banks: 2,
  // **The sixth is a desk's** (owner, 2026-10-19): patched, with DMX on, so
  // its opacity, slots and placement are shown and not offered. The others are
  // nobody's but the client's, so every probe that presses a layer still can.
  mayChooseSlot: order !== 6,
  held: { opacity: order === 6, slot: order === 6, transform: order === 6 },
  defaults: { enabled: true, route: 'A', opacity: 1 },
  // **Two, and the second one switched off.** Two because a chain of one
  // cannot be reordered -- both arrows are disabled, rightly, and pressing one
  // would prove the guard rather than the control. Switched off because "off --
  // the chain skips it" is a line the panel draws and nothing had ever drawn.
  // The second also carries a moved value, which is what the offer to put it
  // back is for.
  effects: [
    {
      id: 'FX' + order + 'A',
      typeId: 'ovs.colour_correct',
      version: 1,
      enabled: true,
      parameters: { brightness: { type: 'float', value: -0.5 } },
      live: {},
      held: [],
      locked: false,
    },
    {
      id: 'FX' + order + 'B',
      typeId: 'ovs.colour_correct',
      version: 1,
      enabled: false,
      parameters: {},
      live: { brightness: { type: 'float', value: 0.75 } },
      held: [],
      locked: false,
    },
    // **Locked, and its brightness a desk's** (owner, 2026-10-19). Last, so
    // that moving the first one down still has somewhere to go; and so that
    // the probes can ask that its ✕ and its held fader are not offered.
    {
      id: 'FX' + order + 'C',
      typeId: 'ovs.colour_correct',
      version: 1,
      enabled: true,
      parameters: {},
      live: { brightness: { type: 'float', value: 0.1 } },
      held: ['brightness'],
      locked: true,
    },
  ],
  slots: [slot(1), slot(2), slot(13)],
});

const view = {
  sequence: 1,
  showId: 'SHOW',
  showName: LONG,
  composition: { width: 1920, height: 1080 },
  master: {
    dimmer: 1,
    crossfader: 0.5,
    bpm: 128.25,
    speedScale: 1,
    freeze: false,
    dmxEnabled: true,
    // A master fixture holding the crossfader and nothing the probes press.
    held: { dimmer: false, crossfader: true, speedScale: false, freeze: false },
  },
  masterDefaults: {
    bpm: 120,
    dimmer: 1,
    crossfader: 0,
    speedScale: 1,
    beatsPerBar: 4,
    crossfaderCurve: 'equal_power',
  },
  // **Six, because the trouble starts above four.** It was three, so nothing
  // had ever drawn a matrix that has to scroll -- and the owner found on
  // 2026-09-27 that the fifth layer resized every pad on the desk rather than
  // scrolling. Six also puts a row below the fold at the audit's own window
  // size, which is what makes that measurable here.
  layers: [
    layer('L1', 1),
    layer('L2', 2),
    layer('L3', 3),
    layer('L4', 4),
    layer('L5', 5),
    layer('L6', 6),
  ],
  mapping: {
    outputs: [
      {
        id: 'OUT1',
        name: `Ausgang ${LONG}`,
        connector: { gpu: 0, connector: 'HDMI-A-1' },
        size: { width: 1920, height: 1080 },
        enabled: true,
      },
    ],
    pieces: [
      {
        id: 'PIECE1',
        name: `Fläche ${LONG}`,
        outputId: 'OUT1',
        source: { x: 0, y: 0, width: 1920, height: 1080 },
        destination: { x: 0, y: 0, width: 1920, height: 1080 },
        enabled: true,
        order: 0,
      },
    ],
  },
  renderer: { state: 'running', reason: null },
  // An administrator, so the lock is offered and can be pressed.
  mayAdminister: true,
};

const item = (n) => ({
  id: 'MEDIA' + n,
  display_name: `${LONG} ${n}`,
  original_filename: `nachtfahrt-durch-die-innenstadt-sehr-langer-dateiname-${n}.mov`,
  kind: 'video',
  state: 'ready',
  has_alpha: false,
  created_at: 1000 - n,
  duration_ms: 240000,
  dimensions: { width: 1920, height: 1080 },
  frame_rate: { numerator: 25, denominator: 1 },
  folder: '',
});

// **Pictures, when asked for** (`OVS_STUB_STRIPS=1`): a still and a ten-frame
// strip for each clip, every frame numbered and in its own colour, so a
// screenshot says which frame a box is showing. Three shapes — 16:9, 4:3 and
// square — because a strip stretched to a 16:9 box looked right for the first
// and wrong for the other two, beside a still of the same clip that was not
// (2026-10-20). Off by default: the overflow audit measures text, and the
// press tool presses controls, and neither needs them.
const PICTURES = process.env.OVS_STUB_STRIPS === '1';
const SHAPES = { MEDIA1: [160, 90], MEDIA2: [120, 90], MEDIA13: [90, 90] };
const COLOURS = ['#b33', '#b73', '#bb3', '#7b3', '#3b3', '#3b7', '#3bb', '#37b', '#33b', '#73b'];

const stubPicture = (id, frames) => {
  const [width, height] = SHAPES[id] ?? [160, 90];
  const cells = Array.from(
    { length: frames },
    (_, n) =>
      `<rect x="${n * width}" y="0" width="${width}" height="${height}" fill="${COLOURS[n]}"/>` +
      `<text x="${n * width + width / 2}" y="${height * 0.7}" font-size="${height * 0.6}" ` +
      `text-anchor="middle" fill="#fff" font-family="sans-serif">${n + 1}</text>`,
  ).join('');
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="${width * frames}" height="${height}">` +
    `${cells}</svg>`;
  return `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`;
};

const effect = {
  type_id: 'ovs.colour_correct',
  version: 1,
  name: 'Colour correct, with a long name',
  description: 'Brightness, contrast, saturation and hue, in that order.',
  parameters: [
    {
      key: 'brightness',
      name: 'Brightness, a long parameter name',
      parameter_type: { kind: 'number', min: -1, max: 1, default: 0 },
    },
    // The only toggle anywhere in the stub, and so the only thing that draws a
    // live checkbox at all.
    {
      key: 'invert',
      name: 'Invert',
      parameter_type: { kind: 'toggle', default: false },
    },
  ],
};

const generator = {
  type_id: 'ovs.solid_colour',
  version: 1,
  name: 'Solid colour, with a long name',
  description: 'Fills the layer with one colour.',
  // **All five kinds are declared across these two.** It was a colour here and
  // a number on the effect, so `text` and `choice` had never been drawn — and
  // the live panel drew both of them as four colour channels for as long as
  // that was true. The real catalogue ships both: `ovs.shape` declares a
  // choice, `ovs.text` declares a text and a choice.
  parameters: [
    { key: 'colour', name: 'Colour', parameter_type: { kind: 'colour', default: [1, 1, 1, 1] } },
    {
      key: 'shape',
      name: 'Shape',
      parameter_type: {
        kind: 'choice',
        choices: [
          { id: 'ellipse', name: 'Ellipse' },
          { id: 'rectangle', name: 'Rectangle' },
          { id: 'triangle', name: 'Triangle' },
        ],
        default: 'ellipse',
      },
    },
    {
      key: 'caption',
      name: 'Caption, a long parameter name',
      parameter_type: { kind: 'text', default: 'OPENVIDEOSERVER', max_chars: 240 },
    },
  ],
};

/** A stored show, as `GET /shows/{id}` answers with one.
 *
 *  Enough of one to draw the Show tab's look inside: an output with a shape
 *  of its own and four pieces on it, which is what the preview measures.
 */
const OUTPUT = 'OUTPUT';
const piece = (n, x, y) => ({
  id: 'PIECE' + n,
  name: `${LONG} ${n}`,
  enabled: true,
  order: n,
  output_id: OUTPUT,
  source_rect: { x: 0, y: 0, width: 960, height: 540 },
  dest_rect: { x: x, y: y, width: 900, height: 480 },
  rotation_deg: 0,
  anchor: { x: 0.5, y: 0.5 },
  source_rotation_deg: 0,
  source_anchor: { x: 0.5, y: 0.5 },
});

const document_ = {
  id: 'SHOW',
  schema_version: 1,
  name: LONG,
  created_at: 1,
  updated_at: 2,
  composition: { size: { width: 1920, height: 1080 }, render_rate: { numerator: 60, denominator: 1 } },
  master: {
    default_bpm: 128,
    beats_per_bar: 4,
    default_speed_scale: 1,
    crossfader_curve: 'equal_power',
    default_dimmer: 1,
    default_crossfader: 0.5,
  },
  layers: [],
  pieces: [piece(1, 0, 0), piece(2, 960, 0), piece(3, 0, 540), piece(4, 960, 540)],
  outputs: [
    {
      id: OUTPUT,
      name: LONG,
      enabled: true,
      identity: { gpu_pci_address: '0000:01:00.0', connector: 'DP-1', edid_hash: null },
      mode: { size: { width: 1920, height: 1080 }, refresh: { numerator: 60, denominator: 1 } },
    },
  ],
  // One patched fixture, so the patch page has a block to draw and its printed
  // table a row to print (2026-10-21): a sheet of "nothing is patched" proves
  // only that the button reached the bridge.
  fixture_patch: [
    {
      id: 'FIXTURE1',
      type_id: 'ovs.master',
      version: 1,
      target: { kind: 'master' },
      universe: 1,
      start_address: 1,
      footprint: 9,
      enabled: true,
    },
  ],
  settings: { auto_save: true, notes: '' },
};

const answers = {
  snapshot: async () => ({ status: { state: 'live' }, view, ingest: [] }),
  mediaLibrary: async () =>
    ok({
      items: [item(1), item(2), item(13)],
      // Two at the top, so one can be dragged into the other (owner,
      // 2026-10-20: folders into folders, the way clips go into them).
      folders: [LONG, `${LONG}/Unterordner mit langem Namen`, 'Zweiter Ordner'],
    }),
  catalogues: async () => ok({ effects: [effect], generators: [generator] }),
  shows: async () =>
    ok([
      // The running one: `view.showId` is this, so its row draws ON AIR.
      {
        id: 'SHOW',
        name: LONG,
        loadable: true,
        schema_version: 1,
        created_at: 1,
        updated_at: 2,
      },
      // **One that is not running**, which is the only kind with a LOAD button
      // on it. There was no such row until 2026-09-27, so the press that hands
      // the wall to another show had never been drawn by anything.
      {
        id: 'OTHER',
        name: 'Zweite Show',
        loadable: true,
        schema_version: 1,
        created_at: 3,
        updated_at: 4,
      },
      // And one written by a newer build. Its row says the schema version
      // where the others say a date, and offers Delete without LOAD — a
      // document somebody may well want rid of.
      {
        id: 'FUTURE',
        name: 'Aus einem neueren Build',
        loadable: false,
        schema_version: 9,
        created_at: 5,
        updated_at: 6,
      },
    ]),
  showFolders: async () => ok([LONG, `${LONG}/Unterordner`]),
  loadShow: async () => ok(document_),
  // Saving the mapping answers with the document as the server now has it,
  // and the editor stamps its draft from that. Answering ok with nothing threw
  // on every press of every control on the mapping screen — which is the way
  // this stub rots: the call exists, so nothing says it is lying about what it
  // gives back.
  setMapping: async () => ok({ ...document_, updated_at: 3 }),
  // One socket with two modes, matching the output the stub show puts on it.
  // Empty is honest for a machine with no renderer and useless here: a mode is
  // chosen from this list now, so with nothing in it there is nothing to press
  // Set about.
  connectors: async () =>
    ok([
      {
        gpu_pci_address: '0000:01:00.0',
        connector: 'DP-1',
        connected: true,
        role: 'MEDIA_OUTPUT',
        model: 'Stub panel',
        modes: [
          { size: { width: 1920, height: 1080 }, refresh_millihertz: 60000 },
          { size: { width: 1280, height: 720 }, refresh_millihertz: 50000 },
        ],
      },
    ]),
  // The three rasters at eight rates the server publishes, as far as the
  // audits need: enough that a stream's chooser has something in it and the
  // Add button is not disabled for want of a mode.
  networkModes: async () =>
    ok([
      { size: { width: 1280, height: 720 }, refresh: { numerator: 60, denominator: 1 } },
      { size: { width: 1920, height: 1080 }, refresh: { numerator: 60000, denominator: 1001 } },
      { size: { width: 1920, height: 1080 }, refresh: { numerator: 60, denominator: 1 } },
      { size: { width: 3840, height: 2160 }, refresh: { numerator: 25, denominator: 1 } },
    ]),
  fixtures: async () => ok([]),
  mediaThumbnail: async (id) => (PICTURES ? stubPicture(id, 1) : null),
  mediaStrip: async (id) => (PICTURES ? stubPicture(id, 10) : null),
  exportShow: async () => ({ ok: true, value: null }),
  exportShowWithMedia: async () => ({ ok: true, value: null }),
  importShow: async () => ({ ok: true, value: null }),
  mediaUsage: async () => ok([LONG]),
  pathForFile: () => '',
  preview: async () => null,
  streamCredential: async () => refusal,
};

const CALLS = [
  'addEffect',
  'assignSlot',
  'captureLiveState',
  'clearIngest',
  'clearSlot',
  'deleteShow',
  'connect',
  'createAccount',
  'createFolder',
  'createShow',
  'declareIngest',
  'deleteFolder',
  'deleteMedia',
  'disconnect',
  'dmxProtocols',
  'createShowFolder',
  'deleteShowFolder',
  'fileMedia',
  'fileShow',
  'ingest',
  'inspect',
  'loadShow',
  'orderEffects',
  'pickImage',
  'pickMedia',
  'removeEffect',
  'removeIngest',
  'renameFolder',
  'renameShowFolder',
  'resumeIngest',
  'run',
  'runShow',
  'saveShow',
  'setBanks',
  'moveSlot',
  'printSheet',
  'setDmxProtocols',
  'setLock',
  'setMapping',
  'setSlot',
  'setTestPattern',
  'suggestAddress',
];

// Every call the interface makes, written down. The overflow audit does not
// read this; the press tool next door does, and what it is for is finding a
// control that reaches nothing at all.
//
// Each call answers **ok** rather than the refusal this used to give, because a
// refusal is indistinguishable from a control that never sent: both leave the
// screen unchanged. The readers keep their canned answers, since a screen with
// no show in it draws nothing to press.
const sent = [];

const bridge = {};
for (const name of CALLS) {
  bridge[name] = async (...args) => {
    sent.push({ call: name, args });
    return { ok: true, value: null };
  };
}
for (const [name, answer] of Object.entries(answers)) {
  bridge[name] = async (...args) => {
    sent.push({ call: name, args });
    return answer(...args);
  };
}
contextBridge.exposeInMainWorld('sent', { all: () => sent });

// The subscriptions. Each is taken and then fired once, so the screens draw
// the show above rather than the empty one they start at.
// Two servers, so the connect screen's list is drawn and measured: one ready to
// sign in to and one that has never been set up.
bridge.onServers = (listener) => {
  if (process.env.OVS_STUB_NO_SERVERS === '1') {
    setTimeout(() => listener([]), 0);
    return () => {};
  }
  setTimeout(
    () =>
      listener([
        { name: LONG, address: '192.0.2.10', port: 7420, version: '0.2.0', needsSetup: false, seenAt: Date.now() },
        { name: 'Rack 2', address: '192.0.2.11', port: 7420, version: null, needsSetup: true, seenAt: Date.now() },
      ]),
    0,
  );
  return () => {};
};
bridge.onStatus = (listener) => {
  setTimeout(() => listener({ state: 'live' }, 'http://192.0.2.10:7420'), 0);
  return () => {};
};
bridge.onView = (listener) => {
  setTimeout(() => listener(view), 0);
  return () => {};
};
bridge.onTelemetry = () => () => {};
bridge.onNdiSources = () => () => {};
bridge.onConnectors = () => () => {};
// **An upload in progress**, for the press tool: a row whose progress moves ten
// times a second, which is what the screen has to stay usable under. A folder
// could not be named during an upload because every progress report redrew the
// page and took the box away (owner, 2026-10-20). The overflow audit measures
// a still page and does not ask for it.
bridge.onIngest = (listener) => {
  if (process.env.OVS_STUB_UPLOADING !== '1') {
    return () => {};
  }
  let sent = 0;
  const timer = setInterval(() => {
    sent = (sent + 1_000_000) % 100_000_000;
    listener([
      {
        id: 'UPLOAD1',
        path: 'C:/media/clip.mov',
        name: 'clip.mov',
        folder: '',
        byteSize: 100_000_000,
        stage: { at: 'uploading', sent, byteSize: 100_000_000 },
        upload: null,
        prepared: null,
        hand: null,
        declaration: null,
        stop: null,
      },
    ]);
  }, 100);
  return () => clearInterval(timer);
};
bridge.onFault = () => () => {};

contextBridge.exposeInMainWorld('ovs', bridge);
