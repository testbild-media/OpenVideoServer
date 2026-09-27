// The window.
//
// It draws what the main process sends and sends back what the operator does.
// It holds no show state of its own: a pad lights up because the server said
// the slot is active, never because it was clicked. During a show the
// difference is whether the screen agrees with the wall.
//
// This file is the frame: the connection, the four modes, and the line at the
// foot where anything that went wrong is said. Each mode is its own panel —
// `live.ts`, `editor.ts`, `patch.ts`, `media.ts` — and none of them knows about
// the others.

import type { FoundServer } from '../main/discovery.js';
import type { ConnectionStatus } from '../shared/connection.js';
import type { ShowView } from '../shared/view.js';
import type { OvsBridge } from '../preload.js';
import { bytesLabel } from '../shared/ingest.js';
import { element } from './dom.js';
import { EditorPanel } from './editor.js';
import { LivePanel } from './live.js';
import { MediaPanel } from './media.js';

declare global {
  interface Window {
    ovs: OvsBridge;
  }
}

const ovs = window.ovs;

const ui = {
  modeLive: element<HTMLButtonElement>('mode-live'),
  modeMedia: element<HTMLButtonElement>('mode-media'),
  modeSettings: element<HTMLButtonElement>('mode-settings'),
  settings: element<HTMLElement>('settings'),
  tabMapping: element<HTMLButtonElement>('tab-mapping'),
  tabDmx: element<HTMLButtonElement>('tab-dmx'),
  tabLayers: element<HTMLButtonElement>('tab-layers'),
  tabShow: element<HTMLButtonElement>('tab-show'),
  paneMapping: element<HTMLElement>('settings-mapping'),
  paneDmx: element<HTMLElement>('settings-dmx'),
  paneLayers: element<HTMLElement>('settings-layers'),
  paneShow: element<HTMLElement>('settings-show'),
  // `edit` and `patch` were here, and the page has had neither since the top
  // bar became LIVE, MEDIA and SETTINGS. `element` throws for an id the page
  // does not have — which is what it is for — and because this map is built
  // at module load, that one throw took the whole window with it: no panels,
  // no connect form, no way to sign in. `test/page.test.ts` now checks every
  // id this client asks the page for.
  media: element('media'),
  show: element('show'),
  server: element('server'),
  renderer: element('renderer'),
  storage: element('storage'),
  link: element('link'),
  disconnect: element<HTMLButtonElement>('disconnect'),
  live: element('live'),
  connect: element('connect'),
  found: element('found'),
  foundList: element('found-list'),
  foundNote: element('found-note'),
  connectForm: element<HTMLFormElement>('connect-form'),
  connectButton: element<HTMLButtonElement>('connect-button'),
  connectNote: element('connect-note'),
  address: element<HTMLInputElement>('address'),
  username: element<HTMLInputElement>('username'),
  password: element<HTMLInputElement>('password'),
  faults: element('faults'),
};

let view: ShowView | null = null;

/**
 * What the server last said about the disc media is kept on.
 *
 * Held rather than drawn as it arrives, because the pill is only on screen in
 * MEDIA and switching to MEDIA must not wait a second for the next sample.
 */
let storage: { used: number; total: number } | null = null;

/**
 * Which part of the client is on screen.
 *
 * Live is the show; Edit is the document it was built from; Patch is the
 * DMX-over-IP half of that same document; Media is what the server holds and
 * what is on its way there. Switching between them changes nothing on the wall,
 * and neither does anything in Edit or Patch until a show is saved.
 */
/** Which of the three parts of the client is on screen. */
type Mode = 'live' | 'media' | 'settings';

/** Which tab of Settings, when Settings is the one on screen. */
type SettingsTab = 'show' | 'mapping' | 'layers' | 'dmx';

let mode: Mode = 'live';

/**
 * The show, which is the first of the four and where the list of them is.
 *
 * Remembered across a visit rather than reset: an operator who was in Mapping,
 * looked at the show and came back was in the middle of something.
 */
let settingsTab: SettingsTab = 'show';
/** Whether there is anything to show at all, as opposed to the sign-in panel. */
let showing = false;
/** So the editor asks the server for its lists once per connection, not per redraw. */
let wasConnected = false;

/** The server signed in to, which is the preview socket's address. */
let origin: string | null = null;
/** Whether the realtime connection is live, as opposed to on its way back. */
let connected = false;

/**
 * Draws the servers this client has found.
 *
 * Clicking one fills the address in rather than connecting outright: the
 * password is still the operator's to type, and a list entry is a claim by
 * something on the network rather than a server this client has verified.
 *
 * A server that has never been set up says so. That is the difference between
 * offering a password box nothing will answer and offering to commission the
 * machine — and on a fresh install it is the only thing that tells an operator
 * they are looking at a server at all.
 */
function drawServers(servers: FoundServer[]): void {
  // **Never hidden.** It used to disappear when the list was empty, on the
  // reasoning that an empty list is an apology. The owner, 2026-09-24: "ich sehe
  // sowieso kein fenster für server liste also kann ich da auch nie was sehen" —
  // which is the whole objection. A panel that is not there cannot say whether
  // it is searching, whether it found nothing, or whether this build even has
  // the feature, and the operator is left guessing about all three.
  ui.foundNote.textContent =
    servers.length === 0
      ? 'None found yet. Type the address instead, or check the server is on this network.'
      : '';
  ui.foundNote.hidden = servers.length > 0;
  ui.foundList.replaceChildren();

  for (const server of servers) {
    const node = document.createElement('button');
    node.type = 'button';
    node.className = 'found-server';

    const name = document.createElement('span');
    name.className = 'found-server-name';
    name.textContent = server.name;

    const address = document.createElement('span');
    address.className = 'found-server-address';
    address.textContent = `${server.address}:${server.port}`;

    node.append(name, address);

    if (server.needsSetup) {
      const setup = document.createElement('span');
      setup.className = 'found-server-setup';
      setup.textContent = 'needs setting up';
      node.append(setup);
    }

    node.title = server.version === null
      ? `Use ${server.address}:${server.port}`
      : `Use ${server.address}:${server.port}, running ${server.version}`;

    node.addEventListener('click', () => {
      ui.address.value = `${server.address}:${server.port}`;
      ui.username.focus();
    });

    ui.foundList.append(node);
  }
}

// --- drawing -----------------------------------------------------------------

function drawStatus(next: ConnectionStatus): void {
  const text = {
    offline: 'Offline',
    connecting: 'Connecting…',
    live: 'Live',
    waiting: 'Reconnecting…',
    refused: 'Refused',
  }[next.state];

  ui.link.textContent = text;
  ui.link.className = `pill ${next.state}`;
  ui.link.title = 'reason' in next ? next.reason : '';

  connected = next.state === 'live';
  showing = connected || next.state === 'waiting';
  applyMode();
  // While reconnecting the matrix is the last thing the server said, not what
  // it is saying. The show carries on without this client, so the layout stays
  // — dimmed, because an operator must not read it as current.
  document.body.classList.toggle('stale', next.state === 'waiting');
  ui.connect.hidden = connected || next.state === 'waiting' || next.state === 'connecting';
  ui.disconnect.hidden = next.state === 'offline';

  if (next.state === 'refused') {
    ui.connectNote.textContent = next.reason;
    ui.connectButton.disabled = false;
  }
  if (next.state === 'waiting') {
    note(`${next.reason}. Trying again in ${Math.round(next.retryInMs / 100) / 10}s…`);
  }

  live.setConnection(next, origin);

  // The show list, the catalogues and the media library, once per
  // connection. The editor keeps whatever document is in hand across a
  // reconnect: unsaved work is the operator's, and a dropped socket is not a
  // reason to throw it away.
  if (connected && !wasConnected) {
    void editor.load();
  }
  media.setConnected(connected);
  wasConnected = connected;
}

/** Puts the chosen part of the client on screen. */
function applyMode(): void {
  ui.modeLive.classList.toggle('on', mode === 'live');
  ui.modeMedia.classList.toggle('on', mode === 'media');
  ui.modeSettings.classList.toggle('on', mode === 'settings');
  ui.live.hidden = !showing || mode !== 'live';
  ui.media.hidden = !showing || mode !== 'media';
  ui.settings.hidden = !showing || mode !== 'settings';

  for (const [name, tab, pane] of [
    ['show', ui.tabShow, ui.paneShow],
    ['mapping', ui.tabMapping, ui.paneMapping],
    ['layers', ui.tabLayers, ui.paneLayers],
    ['dmx', ui.tabDmx, ui.paneDmx],
  ] as const) {
    const chosen = settingsTab === name;
    tab.classList.toggle('on', chosen);
    tab.setAttribute('aria-selected', String(chosen));
    pane.hidden = !chosen;
  }

  live.setVisible(!ui.live.hidden);
  drawStorage();
}

/**
 * How full the disc media is kept on, in the top bar, while MEDIA is showing.
 *
 * "It is the user's business when the server is full and nothing more can be
 * uploaded" (owner, 2026-09-23). The figures are the ones the Service Console
 * shows, measured on the server; nothing here estimates anything.
 */
function drawStorage(): void {
  const showingMedia = showing && mode === 'media';
  if (!showingMedia || storage === null || storage.total === 0) {
    ui.storage.hidden = true;
    return;
  }

  const { used, total } = storage;
  const percent = Math.round((used / total) * 100);
  ui.storage.hidden = false;
  ui.storage.textContent = `Media: ${bytesLabel(used)} of ${bytesLabel(total)} (${percent}%)`;
  ui.storage.title = `${bytesLabel(Math.max(total - used, 0))} free where this server keeps media.`;
  // Loud only when it matters. A disc at nine tenths is a show with a few
  // clips left in it, and that is worth seeing before the import is refused.
  ui.storage.classList.toggle('warn', percent >= 90);
}

function drawView(next: ShowView): void {
  const was = view?.showId ?? null;
  view = next;
  ui.show.textContent = next.showName ?? 'No show';
  ui.show.title = next.showName ?? 'No show is open on this server';

  // **The server owns the show** (owner, 2026-09-23). When it says which one
  // it is running, the editor opens that document — so signing in puts an
  // operator in front of tonight's show rather than a list of shows. It only
  // fills an empty editor; a draft in hand is somebody's work.
  if (next.showId !== null && next.showId !== was) {
    void editor.followTheOpenShow();
  }

  const renderer = next.renderer;
  ui.renderer.textContent = `Renderer: ${renderer.state}${renderer.reason ? ` — ${renderer.reason}` : ''}`;
  ui.renderer.className = `pill ${renderer.state}`;

  live.draw(next);
}

// --- doing -------------------------------------------------------------------

/**
 * Says what went wrong, in the bar.
 *
 * **Nothing below it moves** (owner, 2026-09-23): "Ich möchte diese Rote
 * Status/Hinweiß Leiste unten nie wieder sehen… So verschiebt sich niemals die
 * komplette GUI während des bedienen." It was a band at the foot of the window
 * that grew when something failed and shrank six seconds later, and each of
 * those moved every pad on the screen under whoever was reaching for one.
 *
 * In the bar it takes room from the show's name, which is text that can be
 * cut, and the bar's height does not depend on it.
 *
 * One timer, restarted: two faults in quick succession used to leave the first
 * one's timer running, so the second disappeared early.
 */
let clearing: number | null = null;

function note(message: string): void {
  ui.faults.textContent = message;
  ui.faults.hidden = message === '';
  ui.faults.title = message;
  if (clearing !== null) {
    window.clearTimeout(clearing);
  }
  clearing = window.setTimeout(() => {
    ui.faults.textContent = '';
    ui.faults.hidden = true;
    clearing = null;
  }, 6000);
}

/**
 * Live Mode.
 *
 * The show: the layer × slot matrix, the per-layer live state, the master and
 * the preview. Nothing in it writes to the show document — the commands it
 * can send are `LiveCommand`, and that union has no way to reach one.
 */
const live = new LivePanel({
  ovs,
  note,
  /**
   * Live Mode's work area asking for Settings at one thing.
   *
   * The one place the two meet, and it goes one way: the live screen can ask
   * to *look* at a piece of configuration, and the editor decides what to do
   * with that — including refusing while it holds work nobody has saved.
   * Nothing on stage moves either way; only a save does.
   *
   * It lands on Layers, because that is where what it can be asked about lives.
   * A layer, today: a slot is put together in OPTIONS and Settings has no panel
   * for one, so nothing asks to open a slot here any more.
   */
  openEditor: (target) => {
    void editor.openAt(target).then((opened) => {
      if (opened) {
        mode = 'settings';
        settingsTab = 'layers';
        applyMode();
      }
    });
  },
});

/**
 * Edit/Setup.
 *
 * It holds the document being edited and nothing about the show. It is told
 * which show the server has open so it can warn that saving that one
 * reloads it, which is the single place where editing and the wall meet.
 */
const editor = new EditorPanel({
  ovs,
  note,
  liveShowId: () => view?.showId ?? null,
});

/**
 * Media.
 *
 * The library the server holds and the files on their way into it. It touches
 * nothing on stage: an import is a file arriving in a database, and a show
 * has to be edited and saved before any of it reaches a wall.
 */
const media = new MediaPanel({
  ovs,
  note,
  // The two screens that hold their own copy of the library. Both read it once
  // when they sign in, which is right for a list that only grows while nobody
  // is looking and wrong the moment something is deleted.
  changed: () => {
    live.reloadLibrary();
    void editor.load();
  },
});

// --- wiring ------------------------------------------------------------------

ui.connectForm.addEventListener('submit', (event) => {
  event.preventDefault();
  void signIn();
});

async function signIn(): Promise<void> {
  ui.connectButton.disabled = true;
  ui.connectNote.textContent = 'Connecting…';

  const address = ui.address.value;
  const username = ui.username.value;
  const password = ui.password.value;

  const inspected = await ovs.inspect(address);
  if (!inspected.ok) {
    ui.connectNote.textContent = inspected.message;
    ui.connectButton.disabled = false;
    return;
  }

  ui.server.textContent = `${inspected.origin} · ${inspected.version}`;
  // Where the preview socket goes. The main process holds the session; this is
  // the only thing about the server the window is told.
  origin = inspected.origin;

  // A server with no operator account is in first-run setup, and the first
  // credentials typed are the ones it will be set up with.
  const result = inspected.configured
    ? await ovs.connect(address, username, password)
    : await ovs.createAccount(address, username, password);

  ui.connectButton.disabled = false;
  if (result.ok) {
    ui.connectNote.textContent = '';
    ui.password.value = '';
  } else {
    ui.connectNote.textContent = result.message;
  }
}

ui.disconnect.addEventListener('click', () => {
  void ovs.disconnect();
  view = null;
  ui.server.textContent = '';
});

for (const [choice, node] of [
  ['live', ui.modeLive],
  ['media', ui.modeMedia],
  ['settings', ui.modeSettings],
] as const) {
  node.addEventListener('click', () => {
    mode = choice;
    applyMode();
  });
}

for (const [choice, node] of [
  ['show', ui.tabShow],
  ['mapping', ui.tabMapping],
  ['layers', ui.tabLayers],
  ['dmx', ui.tabDmx],
] as const) {
  node.addEventListener('click', () => {
    settingsTab = choice;
    applyMode();
  });
}

ovs.onStatus(drawStatus);
ovs.onServers(drawServers);
ovs.onView(drawView);
ovs.onFault((fault) => note(fault.message));
ovs.onIngest((items) => media.setQueue(items));
// The NDI senders on the server's network. Not part of the telemetry sample:
// this arrives when the network changes, which is when the operator's list of
// what they can put on a layer has just become wrong.
ovs.onNdiSources((sources) => live.setNdiSources(sources));
// And what the machine is wired to. Sent when it moves rather than asked for,
// because a chooser filled in at sign-in describes what was plugged in then.
ovs.onConnectors((connectors) => editor.setConnectors(connectors));
ovs.onTelemetry((telemetry) => {
  // The outputs the server says it is driving, and only those: a chooser that
  // listed a wall nobody is driving would be an invitation to cue to it.
  live.setSurfaces(
    telemetry.outputs.map((output) => ({ id: output.output_id, name: output.name })),
  );
  // Where the master clock is. It rides with the rest of the telemetry rather
  // than in the state stream because the beat moves sixty times a second and
  // nobody edits it, and it is absent while no show is open — which is a
  // different thing from a beat at zero, and is drawn differently.
  live.setClock(telemetry.clock ?? null);
  // And where each playing layer's clip has got to, for the same reason. One
  // entry per layer actually playing one: a layer that is not gets no entry,
  // which is not the same as a playhead at zero and is drawn as no bar at all.
  live.setPlayheads(telemetry.playheads ?? []);
  // How full the disc is. Kept whatever is on screen and drawn only in MEDIA,
  // so switching there shows it at once rather than after the next sample.
  storage = { used: telemetry.disk_used_bytes, total: telemetry.disk_total_bytes };
  drawStorage();
});

editor.draw();
media.draw();

void (async () => {
  const snapshot = await ovs.snapshot();
  drawStatus(snapshot.status);
  drawView(snapshot.view);
  // An import that was running when this window was reloaded is still running:
  // the queue lives in the main process, which is the half that has the file
  // and the session.
  media.setQueue(snapshot.ingest);
})();
