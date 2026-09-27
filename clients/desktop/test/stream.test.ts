// What the preview stream does with the wire, and with a wire that goes away.
//
// The transport half only: no canvas, no WebCodecs, no browser. The socket and
// the clock are handed in, so a stream that drops during a show, a server that
// refuses one and a stream that quietly stops are tested rather than hoped for.

import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { StreamEncoding } from '@ovs/schema';

import {
  COMPOSITION_SURFACE,
  DECLINED_MS,
  openStream,
  parseChunk,
  statusLine,
  streamUrl,
  type StreamChunk,
  type StreamOptions,
  type StreamSocket,
  type StreamSocketHandlers,
} from '../src/shared/stream.js';
import { streamTicket } from '../src/shared/versions.js';

/** An output identifier of the shape the telemetry reports. */
const OUTPUT_ID = '0192f4c2-7f3a-7c5e-9b1d-2a4c6e8f0a11';

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

  /** What the client sent, parsed, in order. */
  messages(): Array<Record<string, unknown>> {
    return this.sent.map((text) => JSON.parse(text) as Record<string, unknown>);
  }
}

/** One message of the shape `stream_routes.rs` sends. */
function picture({
  version = 1,
  keyframe = true,
  width = 1280,
  height = 720,
  timestampUs = 33_366,
  data = [0, 0, 0, 1, 0x65],
}: {
  version?: number;
  keyframe?: boolean;
  width?: number;
  height?: number;
  timestampUs?: number;
  data?: number[];
} = {}): ArrayBuffer {
  const buffer = new ArrayBuffer(16 + data.length);
  const header = new DataView(buffer);
  header.setUint8(0, version);
  header.setUint8(1, keyframe ? 1 : 0);
  header.setUint16(4, width, true);
  header.setUint16(6, height, true);
  header.setBigUint64(8, BigInt(timestampUs), true);
  new Uint8Array(buffer, 16).set(data);
  return buffer;
}

/**
 * A stream wired to fake sockets and fake timers.
 *
 * The quiet check is off unless a test is about it: every other test advances
 * the clock past three seconds while proving something else.
 */
function harness(options: Partial<StreamOptions> = {}) {
  const sockets: FakeSocket[] = [];
  const chunks: StreamChunk[] = [];
  const errors: string[] = [];
  const closes: string[] = [];
  const encodings: StreamEncoding[] = [];

  const stream = openStream({
    url: streamUrl('http://server:7420', COMPOSITION_SURFACE),
    quietMs: 0,
    onChunk: (chunk) => chunks.push(chunk),
    onEncoding: (encoding) => encodings.push(encoding),
    onError: (reason) => errors.push(reason),
    onClose: (reason) => closes.push(reason),
    open: (url, handlers) => {
      const socket = new FakeSocket(url, handlers);
      sockets.push(socket);
      return socket;
    },
    schedule: (callback, ms) => setTimeout(callback, ms),
    cancel: (timer) => clearTimeout(timer),
    ...options,
  });

  /** The socket the stream is currently using. */
  const current = () => sockets[sockets.length - 1]!;

  return { stream, sockets, chunks, errors, closes, encodings, current };
}

/**
 * What the main process mints for one socket.
 *
 * A ticket: good for this surface, for a minute, and spent the first time it is
 * used. Each socket gets its own, which is why they are numbered here.
 */
const ticket = (nth: number) => streamTicket(`ticket-${nth}`);

/**
 * Lets the credential finish crossing whatever it had to cross.
 *
 * Minting one is asynchronous even when the answer is already known, because in
 * the window it is a trip to another process and then to the server.
 */
const settle = () => vi.advanceTimersByTimeAsync(0);

beforeEach(() => {
  vi.useFakeTimers();
});

describe('a picture off the wire', () => {
  it('reads the header the server puts in front of every picture', () => {
    const chunk = parseChunk(picture());

    expect(chunk).toEqual({
      keyframe: true,
      width: 1280,
      height: 720,
      timestampUs: 33_366,
      data: new Uint8Array([0, 0, 0, 1, 0x65]),
    });
  });

  it('reads a picture a watcher cannot start from as one', () => {
    expect(parseChunk(picture({ keyframe: false }))?.keyframe).toBe(false);
  });

  it('ignores a message too short to hold a picture', () => {
    // Nothing thrown: during a show a preview is worth exactly as much as the
    // rest of the client still running.
    expect(parseChunk(new ArrayBuffer(0))).toBeNull();
    expect(parseChunk(new ArrayBuffer(15))).toBeNull();
    expect(parseChunk(new ArrayBuffer(16))).toBeNull();
  });

  it('ignores a version this build was not built for', () => {
    expect(parseChunk(picture({ version: 2 }))).toBeNull();
  });
});

describe('the address of a surface', () => {
  it('is the socket scheme that matches how the server is reached', () => {
    expect(streamUrl('http://server:7420', COMPOSITION_SURFACE)).toBe(
      'ws://server:7420/api/v1/preview/stream/composition',
    );
    expect(streamUrl('https://server:8443', COMPOSITION_SURFACE)).toBe(
      'wss://server:8443/api/v1/preview/stream/composition',
    );
  });

  it('names an output the way the telemetry names it', () => {
    expect(streamUrl('http://server:7420', OUTPUT_ID)).toBe(
      `ws://server:7420/api/v1/preview/stream/${OUTPUT_ID}`,
    );
  });

  it('never carries what proves the watcher', () => {
    // A query string is written into every access log and proxy cache between
    // here and the server.
    const { sockets } = harness({ credential: () => ticket(1) });

    expect(sockets[0]!.url).toBe('ws://server:7420/api/v1/preview/stream/composition');
    expect(sockets[0]!.url).not.toContain('ticket');
    expect(sockets[0]!.url).not.toContain('?');
  });
});

describe('proving a watcher may watch', () => {
  it('sends the ticket it was given as the first message, before anything else', async () => {
    const { sockets } = harness({ credential: () => ticket(1) });
    sockets[0]!.handlers.opened();
    await settle();

    expect(sockets[0]!.messages()[0]).toEqual({ ticket: 'ticket-1' });
  });

  it('mints a ticket for every socket and never sends one twice', async () => {
    // A ticket is spent the first time it is used (`spend_stream_ticket`), so a
    // second socket offering the first socket's ticket is refused and the
    // operator watches a panel reconnect for ever. One socket, one mint.
    let minted = 0;
    const { sockets, current } = harness({
      credential: () => {
        minted += 1;
        return ticket(minted);
      },
    });
    current().handlers.opened();
    await settle();
    current().handlers.closed('the preview stream closed');

    await vi.advanceTimersByTimeAsync(500);
    current().handlers.opened();
    await settle();

    expect(sockets).toHaveLength(2);
    expect(minted).toBe(2);
    expect(sockets[0]!.messages()[0]).toEqual({ ticket: 'ticket-1' });
    expect(sockets[1]!.messages()[0]).toEqual({ ticket: 'ticket-2' });
    expect(sockets[1]!.sent).not.toContain(sockets[0]!.sent[0]);
  });

  it('sends whatever it was handed, without reading it', async () => {
    // The transport does not know what a credential is; the main process mints
    // one and this sends it. Which is why the narrowing of 2026-09-22 — the
    // socket now taking only a ticket — needed no change here at all.
    const { sockets } = harness({ credential: () => '{"ticket":"whatever-it-was-given"}' });
    sockets[0]!.handlers.opened();
    await settle();

    expect(sockets[0]!.messages()[0]).toEqual({ ticket: 'whatever-it-was-given' });
  });

  it('says nothing when there is nothing to prove it with', async () => {
    // A watcher on the machine itself needs no session, and a server that wants
    // one closes the socket rather than guessing.
    const { sockets } = harness({ credential: () => null });
    sockets[0]!.handlers.opened();
    await settle();

    expect(sockets[0]!.sent).toEqual([]);
  });

  it('asks nothing at all when it was given no way to ask', async () => {
    const { sockets } = harness();
    sockets[0]!.handlers.opened();
    await settle();

    expect(sockets[0]!.sent).toEqual([]);
  });

  it('does not send a ticket down a socket that dropped while it was being minted', async () => {
    // The trip to the main process and on to the server takes long enough for a
    // show network to hiccup. A ticket arriving on the wrong socket proves
    // nothing and has been spent on nothing.
    let answer: (credential: string) => void = () => undefined;
    const { sockets, current } = harness({
      credential: () => new Promise<string>((resolve) => (answer = resolve)),
    });
    current().handlers.opened();
    current().handlers.closed('the preview stream closed');

    await vi.advanceTimersByTimeAsync(500);
    answer(ticket(1));
    await settle();

    expect(sockets).toHaveLength(2);
    expect(sockets[0]!.sent).toEqual([]);
    expect(sockets[1]!.sent).toEqual([]);
  });

  it('says why when it cannot mint a ticket, and tries again', async () => {
    // The main process is gone or the server refused. Left alone the server
    // would close this socket in five seconds with nothing said, which reads as
    // a server that is not there.
    const { sockets, errors, current } = harness({
      credential: () => Promise.reject(new Error('no session')),
    });
    current().handlers.opened();
    await settle();

    expect(errors).toEqual(['this client could not prove it may watch: no session']);
    expect(sockets[0]!.closed).toBe(true);

    await vi.advanceTimersByTimeAsync(500);
    expect(sockets).toHaveLength(2);
  });
});

describe('watching', () => {
  it('passes on every picture it can read', () => {
    const { chunks, current } = harness();
    current().handlers.received(picture({ timestampUs: 1 }));
    current().handlers.received(picture({ keyframe: false, timestampUs: 2 }));

    expect(chunks.map((chunk) => chunk.timestampUs)).toEqual([1, 2]);
  });

  it('carries on past a picture it cannot read', () => {
    const { chunks, current } = harness();
    current().handlers.received(picture({ version: 9 }));
    current().handlers.received(picture({ timestampUs: 7 }));

    expect(chunks.map((chunk) => chunk.timestampUs)).toEqual([7]);
  });
});

describe('losing the stream', () => {
  it('opens another socket, waiting longer each time', () => {
    const { sockets, current, closes } = harness();

    current().handlers.closed('the preview stream closed');
    expect(closes).toEqual(['the preview stream closed']);

    vi.advanceTimersByTime(499);
    expect(sockets).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(sockets).toHaveLength(2);

    current().handlers.closed('the preview stream closed');
    vi.advanceTimersByTime(999);
    expect(sockets).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(sockets).toHaveLength(3);

    current().handlers.closed('the preview stream closed');
    vi.advanceTimersByTime(2000);
    expect(sockets).toHaveLength(4);
  });

  it('starts the backoff again after pictures have arrived', () => {
    const { sockets, current } = harness();
    current().handlers.closed('lost');
    vi.advanceTimersByTime(500);

    current().handlers.received(picture());
    current().handlers.closed('lost');

    vi.advanceTimersByTime(500);
    expect(sockets).toHaveLength(3);
  });

  it('counts a socket that fails and then closes as one reconnection', () => {
    const { sockets, current } = harness();
    current().handlers.closed('the preview stream could not be reached');
    current().handlers.closed('the preview stream closed');

    vi.advanceTimersByTime(500);
    expect(sockets).toHaveLength(2);
    vi.advanceTimersByTime(60_000);
    expect(sockets).toHaveLength(2);
  });

  it('gives up a socket that stops sending pictures', () => {
    // The last picture stays on the canvas, so a stream that stopped and a show
    // that is still running must not look the same.
    const { sockets, current, closes } = harness({ quietMs: 3000 });
    current().handlers.received(picture());

    vi.advanceTimersByTime(2999);
    expect(sockets).toHaveLength(1);

    vi.advanceTimersByTime(1);
    expect(sockets[0]!.closed).toBe(true);
    expect(closes).toEqual(['the preview stream went quiet']);

    vi.advanceTimersByTime(500);
    expect(sockets).toHaveLength(2);
  });

  it('stops for good when it is closed', () => {
    const { stream, sockets, current } = harness();
    stream.close();

    expect(sockets[0]!.closed).toBe(true);
    // A close that arrives after stopping must not start a retry.
    current().handlers.closed('the preview stream closed');
    vi.advanceTimersByTime(60_000);
    expect(sockets).toHaveLength(1);
  });
});

describe('the line under the picture', () => {
  const playing = {
    state: 'playing',
    size: { width: 1280, height: 720 },
    error: null,
    encoding: null,
  } as const;

  it('says nothing at all while the picture is playing', () => {
    // The owner, 2026-09-23: the size under the preview goes, so the picture
    // can have the room the words were taking. It said the shape and the
    // encoder, and both are answers to questions nobody asks of a preview that
    // is working — what is worth saying here is what is *wrong*.
    expect(statusLine(playing)).toBe('');
    expect(
      statusLine({
        ...playing,
        encoding: { encoder: 'libx264', hardware: false, fps: 15 },
      }),
    ).toBe('');
  });

  it('does not put an encoder in front of somebody who has no picture', () => {
    // Connecting, waiting and stopped are about whether anything is arriving
    // at all, and what it would have been encoded with does not help.
    expect(
      statusLine({
        state: 'waiting',
        size: null,
        error: null,
        encoding: { encoder: 'libx264', hardware: false, fps: 15 },
      }),
    ).toBe('Waiting for a picture to start from\u2026');
  });
});

describe('a server saying what it is encoding with', () => {
  it('passes it on, and does not mistake it for a refusal', () => {
    const { encodings, errors, current } = harness();
    current().handlers.received(
      '{"encoding":{"encoder":"libx264","hardware":false,"fps":15}}',
    );

    expect(encodings).toEqual([{ encoder: 'libx264', hardware: false, fps: 15 }]);
    expect(errors).toEqual([]);
  });

  it('says so again when a surface is restarted onto another encoder', () => {
    const { encodings, current } = harness();
    current().handlers.received(
      '{"encoding":{"encoder":"libx264","hardware":false,"fps":15}}',
    );
    current().handlers.received(
      '{"encoding":{"encoder":"h264_nvenc","hardware":true,"fps":59.94}}',
    );

    expect(encodings).toHaveLength(2);
    expect(encodings[1]).toEqual({ encoder: 'h264_nvenc', hardware: true, fps: 59.94 });
  });

  it('ignores one that is not the shape it expects rather than showing it', () => {
    // Everything on this socket is additive: a newer server may say words this
    // build has never heard. A half-understood one is not an error to put in
    // front of an operator, and it is not an encoding either.
    const { encodings, errors, current } = harness();
    for (const said of [
      '{"encoding":{"encoder":"libx264","hardware":false}}',
      '{"encoding":{"encoder":"libx264","hardware":false,"fps":0}}',
      '{"encoding":{"encoder":"libx264","hardware":"no","fps":15}}',
      '{"encoding":null}',
      '{"encoding":"libx264"}',
    ]) {
      current().handlers.received(said);
    }

    expect(encodings).toEqual([]);
    expect(errors).toEqual(Array(5).fill('the server declined the stream'));
  });
});

describe('a server that will not send pictures', () => {
  it('repeats what it said, in the words it said it in', () => {
    const { errors, current } = harness();
    current().handlers.received('{"error":"no encoder on this machine"}');

    expect(errors).toEqual(['no encoder on this machine']);
  });

  it('copes with a refusal that is not the shape it expected', () => {
    const { errors, current } = harness();
    current().handlers.received('<html>a proxy in the way</html>');

    expect(errors).toEqual(['the server declined the stream']);
  });

  it('asks a machine with no encoder again later, not at once', () => {
    // Subscribing is what asks the renderer to encode, so asking every five
    // seconds is asking a machine running a show the same question all evening.
    const { sockets, current } = harness();
    current().handlers.received('{"error":"no encoder on this machine"}');

    vi.advanceTimersByTime(DECLINED_MS - 1);
    expect(sockets).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(sockets).toHaveLength(2);
  });

  it('does not ask again when the refusal was the token', () => {
    // Waiting does not produce one.
    const { sockets, errors, current } = harness();
    current().handlers.received('{"error":"unauthenticated"}');

    expect(errors).toEqual(['unauthenticated']);
    expect(sockets[0]!.closed).toBe(true);
    vi.advanceTimersByTime(60_000);
    expect(sockets).toHaveLength(1);
  });
});
