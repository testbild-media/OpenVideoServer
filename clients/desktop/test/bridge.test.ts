// The boundary between the main process and the window.
//
// Two things meet here. The window has to open the preview socket itself,
// because `VideoDecoder` exists nowhere else in this client; only the main
// process can ask the server for permission to watch, and the server wants that
// permission in the socket's first message. So this is about what crosses the
// bridge — one minted ticket per socket, and the telemetry the panel lists its
// outputs from — and about what does not, which is everything else, and which
// since tickets no longer includes the session token.
//
// Electron is mocked rather than run: the bridge is a list of named channels,
// and a list is worth checking exactly.

import type { ServerTelemetry } from '@ovs/schema';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { Session } from '../src/main/session.js';
import type { IngestFiles } from '../src/shared/ingest.js';
import {
  COMPOSITION_SURFACE,
  openStream,
  streamUrl,
  type StreamChunk,
  type StreamSocket,
  type StreamSocketHandlers,
} from '../src/shared/stream.js';

/** Electron, as far as a preload script can tell. */
const electron = vi.hoisted(() => {
  const exposed = new Map<string, Record<string, unknown>>();
  const invocations: Array<{ channel: string; args: unknown[] }> = [];
  const answers = new Map<string, () => unknown>();
  const listeners = new Map<string, (event: unknown, payload: unknown) => void>();

  return {
    exposed,
    invocations,
    answers,
    listeners,
    contextBridge: {
      exposeInMainWorld(name: string, api: Record<string, unknown>): void {
        exposed.set(name, api);
      },
    },
    ipcRenderer: {
      invoke(channel: string, ...args: unknown[]): Promise<unknown> {
        invocations.push({ channel, args });
        return Promise.resolve(answers.get(channel)?.() ?? null);
      },
      on(channel: string, listener: (event: unknown, payload: unknown) => void): void {
        listeners.set(channel, listener);
      },
    },
  };
});

vi.mock('electron', () => ({
  contextBridge: electron.contextBridge,
  ipcRenderer: electron.ipcRenderer,
}));

// Imported for its effect: a preload script exposes its bridge when it is run.
import '../src/preload.js';
import type { OvsBridge } from '../src/preload.js';

const bridge = electron.exposed.get('ovs') as unknown as OvsBridge;

/** What the main process answers `ovs:stream-credential` with, for these tests. */
function mainAnswers(credential: () => string | null | Promise<string | null>): void {
  electron.answers.set('ovs:stream-credential', credential);
}

/** A JSON answer, as the server sends one. */
const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  });

/**
 * A filesystem nothing in this file uses.
 *
 * The ingest queue needs one to exist; these tests are about the bridge, so any
 * call would be a test reaching somewhere it did not mean to and says so.
 */
const noFiles: IngestFiles = {
  probe: () => Promise.reject(new Error('no file was meant to be read here')),
  size: () => Promise.reject(new Error('no file was meant to be read here')),
  hash: () => Promise.reject(new Error('no file was meant to be read here')),
  read: () => Promise.reject(new Error('no file was meant to be read here')),
  prepare: () => Promise.reject(new Error('no file was meant to be prepared here')),
  discard: () => Promise.reject(new Error('no file was meant to be removed here')),
  encoders: () => Promise.reject(new Error('no encoder was meant to be asked for here')),
  keyframeInterval: () => Promise.reject(new Error('no file was meant to be read here')),
};

/** A socket that records what was sent and can be driven from the test. */
class FakeSocket implements StreamSocket {
  readonly sent: string[] = [];
  closed = false;

  constructor(
    readonly url: string,
    readonly handlers: StreamSocketHandlers,
  ) {}

  send(text: string): void {
    this.sent.push(text);
  }

  close(): void {
    this.closed = true;
  }
}

/** One message of the shape `stream_routes.rs` sends. */
function picture(): ArrayBuffer {
  const data = [0, 0, 0, 1, 0x65];
  const buffer = new ArrayBuffer(16 + data.length);
  const header = new DataView(buffer);
  header.setUint8(0, 1);
  header.setUint8(1, 1);
  header.setUint16(4, 1920, true);
  header.setUint16(6, 1080, true);
  header.setBigUint64(8, 16_683n, true);
  new Uint8Array(buffer, 16).set(data);
  return buffer;
}

/** The surfaces a ticket was asked for, in order. */
let ticketsAsked: string[] = [];
/** What the server answers a ticket request with. Replaced per test. */
let ticketAnswer: (surface: string) => Response = () => json({ ticket: 'TICKET', expires_at: 1 });
/** Anything the session said went wrong, which is how a refused mint is reported. */
let faults: string[] = [];

/** A session signed in to a server that is not this machine. */
async function signedIn(token = 'SESSION'): Promise<Session> {
  const session = new Session(
    {
      fetch: async (url: string) => {
        if (url.endsWith('/auth/login')) {
          return json({ token, account_id: 'A', expires_at: 1 });
        }
        const asking = /\/preview\/stream\/([^/]+)\/ticket$/.exec(url);
        if (asking?.[1] !== undefined) {
          ticketsAsked.push(asking[1]);
          return ticketAnswer(asking[1]);
        }
        return new Response(null, { status: 204 });
      },
      open: (_url, handlers) => ({
        send: () => undefined,
        close: () => handlers.closed('stopped'),
      }),
      clientVersion: '0.0.1',
      files: noFiles,
    },
    {
      status: () => undefined,
      view: () => undefined,
      telemetry: () => undefined,
      ndiSources: () => undefined,
      connectors: () => undefined,
      fault: (code, message) => faults.push(`${code}: ${message}`),
      ingest: () => undefined,
    },
  );

  await session.connect('10.0.0.12', 'operator', 'correct horse battery staple');
  return session;
}

beforeEach(() => {
  electron.invocations.length = 0;
  electron.answers.clear();
  electron.listeners.clear();
  ticketsAsked = [];
  faults = [];
  let minted = 0;
  ticketAnswer = () => {
    minted += 1;
    return json({ ticket: `TICKET-${minted}`, expires_at: 1 });
  };
  vi.useFakeTimers();
});

describe('the bridge the window is given', () => {
  it('offers the named calls and events, and nothing else', () => {
    // The whole of what a page can reach from here. Anything not on this list
    // is a route to a server that nobody meant to open.
    expect(Object.keys(bridge).sort()).toEqual([
      'addEffect',
      'assignSlot',
      'captureLiveState',
      'catalogues',
      'clearIngest',
      'clearSlot',
      'connect',
      'connectors',
      'createAccount',
      'createFolder',
      'createShow',
      'createShowFolder',
      'declareIngest',
      'deleteFolder',
      'deleteMedia',
      'deleteShow',
      'deleteShowFolder',
      'disconnect',
      'dmxProtocols',
      'exportShow',
      'exportShowWithMedia',
      'fileMedia',
      'fileShow',
      'fixtures',
      'importShow',
      'ingest',
      'inspect',
      'loadShow',
      'mediaLibrary',
      'mediaStrip',
      'mediaThumbnail',
      'mediaUsage',
      'networkModes',
      'onConnectors',
      'onFault',
      'onIngest',
      'onNdiSources',
      'onServers',
      'onStatus',
      'onTelemetry',
      'onView',
      'orderEffects',
      'pathForFile',
      'pickImage',
      'pickMedia',
      'preview',
      'removeEffect',
      'removeIngest',
      'renameFolder',
      'renameShowFolder',
      'resumeIngest',
      'run',
      'runShow',
      'saveShow',
      'setBanks',
      'setDmxProtocols',
      'setLock',
      'setMapping',
      'setSlot',
      'setTestPattern',
      'showFolders',
      'shows',
      'snapshot',
      'streamCredential',
      'suggestAddress',
]);
  });

  it('bakes nothing into the bridge, and least of all a session', () => {
    // Every member is a call. A value sitting on the bridge would be a copy of
    // something the main process knew when the window loaded, and a token
    // copied once is a token that outlives the session it came from.
    for (const value of Object.values(bridge as unknown as Record<string, unknown>)) {
      expect(typeof value).toBe('function');
    }
  });

  it('asks the main process for a ticket for one surface and hands back what it minted', async () => {
    mainAnswers(() => '{"ticket":"TICKET-1"}');

    await expect(bridge.streamCredential(COMPOSITION_SURFACE)).resolves.toBe('{"ticket":"TICKET-1"}');
    expect(electron.invocations).toEqual([
      { channel: 'ovs:stream-credential', args: [COMPOSITION_SURFACE] },
    ]);
  });

  it('asks the main process for the connectors rather than naming any itself', async () => {
    // The only way an output can be built. A page that could name a socket
    // would be a page that could map a wall onto hardware nobody has.
    electron.answers.set('ovs:connectors', () => ({
      ok: true,
      value: [
        {
          gpu_pci_address: '0000:01:00.0',
          connector: 'DP-1',
          connected: true,
          role: 'MEDIA_OUTPUT',
          modes: [{ size: { width: 1920, height: 1080 }, refresh_millihertz: 60_000 }],
        },
      ],
    }));

    const answer = await bridge.connectors();
    expect(answer.ok && answer.value[0]?.connector).toBe('DP-1');
    expect(electron.invocations).toContainEqual({ channel: 'ovs:connectors', args: [] });
  });

  it('listens for telemetry on the channel the main process sends it on', () => {
    const heard: ServerTelemetry[] = [];
    bridge.onTelemetry((telemetry) => heard.push(telemetry));

    const telemetry = {
      active_decoders: 1,
      cpu_percent: 12,
      decode_errors: 0,
      disk_total_bytes: 0,
      disk_used_bytes: 0,
      ip_address: '10.0.0.2',
      outputs: [
        {
          active: true,
          actual_fps: 60,
          dropped_frames: { since_reset: 0, since_start: 0 },
          frame_render_time_us: 2100,
          name: 'OUTPUT 1',
          output_id: '0192f4c2-7f3a-7c5e-9b1d-2a4c6e8f0a11',
          refresh: { denominator: 1, numerator: 60 },
          size: { height: 1080, width: 1920 },
        },
      ],
      prepared_decoders: 0,
      ram_percent: 30,
      renderer: { state: 'running' },
      uptime_s: 60,
      version: '0.0.1',
    } as unknown as ServerTelemetry;

    electron.listeners.get('ovs:telemetry')?.({}, telemetry);

    // Handed over as it arrived: the chooser lists the outputs the server
    // reports, named the way it names them, and nothing this client made up.
    expect(heard).toEqual([telemetry]);
    expect(heard[0]!.outputs[0]!.name).toBe('OUTPUT 1');
  });

  it('subscribes to the channels the main process sends on, and no others', () => {
    bridge.onStatus(() => undefined);
    bridge.onServers(() => undefined);
    bridge.onView(() => undefined);
    bridge.onTelemetry(() => undefined);
    bridge.onFault(() => undefined);
    bridge.onIngest(() => undefined);

    expect([...electron.listeners.keys()].sort()).toEqual([
      'ovs:fault',
      'ovs:ingest',
      'ovs:servers',
      'ovs:status',
      'ovs:telemetry',
      'ovs:view',
    ]);
  });
});

describe('the credential the main process mints', () => {
  it('mints nothing before there is a session', async () => {
    const session = new Session(
      {
        fetch: async () => new Response(null, { status: 204 }),
        open: () => ({ send: () => undefined, close: () => undefined }),
        clientVersion: '0.0.1',
        files: noFiles,
      },
      {
        status: () => undefined,
        view: () => undefined,
        telemetry: () => undefined,
        ndiSources: () => undefined,
        connectors: () => undefined,
        fault: () => undefined,
        ingest: () => undefined,
      },
    );

    await expect(session.streamCredential(COMPOSITION_SURFACE)).resolves.toBeNull();
  });

  it('mints a ticket for the surface being watched, and never the session token', async () => {
    const session = await signedIn();

    const minted = await session.streamCredential(COMPOSITION_SURFACE);

    expect(JSON.parse(minted ?? '')).toEqual({ ticket: 'TICKET-1' });
    expect(minted).not.toContain('SESSION');
    expect(ticketsAsked).toEqual([COMPOSITION_SURFACE]);
  });

  it('mints a fresh ticket every time it is asked, because one is spent by being used', async () => {
    const session = await signedIn();

    const first = await session.streamCredential(COMPOSITION_SURFACE);
    const second = await session.streamCredential(COMPOSITION_SURFACE);

    expect(first).not.toBe(second);
    expect(JSON.parse(second ?? '')).toEqual({ ticket: 'TICKET-2' });
    expect(ticketsAsked).toHaveLength(2);
  });

  it('hands over nothing to a server that does not know the ticket route', async () => {
    // Axum answers an unmatched path with an empty 404, so there is no error
    // shape and the code is the status. That is a server built before tickets
    // — and since 2026-09-22 its socket takes nothing else either, so sending
    // the old `ClientHello` would put the session token in a window and still
    // not get a picture (owner decision Q10).
    ticketAnswer = () => new Response(null, { status: 404 });
    const session = await signedIn();

    await expect(session.streamCredential(COMPOSITION_SURFACE)).resolves.toBeNull();
    expect(faults).toHaveLength(1);
    expect(faults[0]).toContain('could not get permission to watch');
  });

  it('hands over nothing for a surface the server says it does not have', async () => {
    // The same status, a different answer: this one carries the server's error
    // shape. A surface that is not there is not a reason to put a session token
    // in a window.
    ticketAnswer = () => json({ code: 'not_found', message: 'this server has no such surface' }, 404);
    const session = await signedIn();

    await expect(session.streamCredential('OUTPUT 9')).resolves.toBeNull();
    expect(faults).toEqual([
      'not_found: this client could not get permission to watch OUTPUT 9: this server has no such surface',
    ]);
  });

  it('mints nothing once the session has ended', async () => {
    const session = await signedIn();
    session.disconnect();

    await expect(session.streamCredential(COMPOSITION_SURFACE)).resolves.toBeNull();
  });
});

describe('a window watching a server that is not this machine', () => {
  it('proves it may watch across the bridge and then gets pictures', async () => {
    // The gap this closes: with no way to prove anything, the window's preview
    // socket only worked against a server on this machine. What crosses now is
    // a ticket, so the page holds a permission to watch one surface rather than
    // a credential that could also stop the show.
    const session = await signedIn('REMOTE');
    mainAnswers(() => session.streamCredential(COMPOSITION_SURFACE));

    const sockets: FakeSocket[] = [];
    const chunks: StreamChunk[] = [];
    openStream({
      url: streamUrl('http://10.0.0.12:7420', COMPOSITION_SURFACE),
      credential: () => bridge.streamCredential(COMPOSITION_SURFACE),
      quietMs: 0,
      onChunk: (chunk) => chunks.push(chunk),
      open: (url, handlers) => {
        const socket = new FakeSocket(url, handlers);
        sockets.push(socket);
        return socket;
      },
    });

    sockets[0]!.handlers.opened();
    await vi.advanceTimersByTimeAsync(0);

    expect(sockets[0]!.url).toBe('ws://10.0.0.12:7420/api/v1/preview/stream/composition');
    expect(JSON.parse(sockets[0]!.sent[0] ?? '')).toEqual({ ticket: 'TICKET-1' });
    expect(sockets[0]!.sent[0]).not.toContain('REMOTE');

    sockets[0]!.handlers.received(picture());
    expect(chunks).toHaveLength(1);
    expect(chunks[0]).toMatchObject({ keyframe: true, width: 1920, height: 1080 });
  });

  it('is refused in so many words when the session has gone', async () => {
    // `authenticated()` closes a socket whose first message proves nothing, and
    // the server says why. Waiting does not produce a session, so this stream
    // stops rather than reopening a socket every ten seconds to be refused
    // again.
    mainAnswers(() => null);

    const sockets: FakeSocket[] = [];
    const errors: string[] = [];
    openStream({
      url: streamUrl('http://10.0.0.12:7420', COMPOSITION_SURFACE),
      credential: () => bridge.streamCredential(COMPOSITION_SURFACE),
      quietMs: 0,
      onChunk: () => undefined,
      onError: (reason) => errors.push(reason),
      open: (url, handlers) => {
        const socket = new FakeSocket(url, handlers);
        sockets.push(socket);
        return socket;
      },
    });

    sockets[0]!.handlers.opened();
    await vi.advanceTimersByTimeAsync(0);
    expect(sockets[0]!.sent).toEqual([]);

    sockets[0]!.handlers.received('{"error":"unauthenticated"}');
    await vi.advanceTimersByTimeAsync(60_000);

    expect(errors).toEqual(['unauthenticated']);
    expect(sockets).toHaveLength(1);
  });
});
