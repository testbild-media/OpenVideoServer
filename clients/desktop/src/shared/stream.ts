// Watching a surface the renderer is already drawing.
//
// The server encodes each watched surface once and pushes every picture down a
// socket (`crates/ovs-core/src/server/stream_routes.rs`): sixteen bytes of
// header and then one H.264 picture in Annex-B form, as one binary message.
// This is the other end of that.
//
// Two halves that do not touch each other:
//
//   - the transport, which is a socket, a header and what to do when the socket
//     drops. It knows nothing about pictures, so it is tested without a
//     browser.
//   - `StreamPlayer`, which is WebCodecs and nothing else. It knows nothing
//     about sockets, so a preview that cannot decode is a preview that cannot
//     decode rather than a connection nobody can diagnose.
//
// Two rules the wire imposes. Nothing that proves anything is ever in the URL —
// a query string is written into every access log and proxy cache between here
// and the server — so a watcher from the network proves itself in the first
// message (`authenticated()` in stream_routes.rs). And a message this build does
// not understand is dropped rather than thrown over: a newer server saying
// something new must not be able to stop an operator seeing the wall.
//
// This half never holds a session, and since tickets it never holds a token
// either. It is handed something that mints one credential when a socket has
// just opened, and sends it as it is: in the window that is one trip across the
// bridge to the main process, which is the only place in this client that can
// ask a server for one.

import type { StreamEncoding } from '@ovs/schema';

import { API_PREFIX } from './versions.js';

/** The header in front of every picture. */
const HEADER_BYTES = 16;

/** The only frame format this build knows. */
const FRAME_VERSION = 1;

/** Where the sockets live, under the API prefix like every other route. */
const STREAM_PATH = '/preview/stream';

/** The surface every server has, whatever it is patched to. */
export const COMPOSITION_SURFACE = 'composition';

/**
 * The same composition with everything no piece takes blacked out.
 *
 * What is left is exactly the part of the show that reaches a wall. A
 * composition is authored at a convenient size and the walls are whatever the
 * venue has, so an operator's real question — "is any of this being cut off?" —
 * is answered by looking at these two surfaces one after the other.
 *
 * Every server has it too: it is the composition surface with a mask over it
 * rather than anything a show has to configure.
 */
export const MASKED_SURFACE = 'composition-masked';

/** One picture, with what the server said about it. */
export interface StreamChunk {
  /** Whether a watcher can start from this picture. */
  keyframe: boolean;
  /** What the encoder says this picture measures. */
  width: number;
  height: number;
  /** Microseconds since the stream started. */
  timestampUs: number;
  /** H.264, Annex-B. A view into the message, not a copy of it. */
  data: Uint8Array;
}

/**
 * Reads one message off the wire.
 *
 * Returns `null` for anything this build cannot read — a message too short to
 * hold a header and a picture, or a version that arrived after this client was
 * built. Nothing throws: during a show a preview is worth exactly as much as
 * the rest of the client still running.
 */
export function parseChunk(buffer: ArrayBuffer): StreamChunk | null {
  // A header with no picture behind it is not a picture either: the decoder
  // would refuse an empty chunk and the refusal would look like a fault.
  if (buffer.byteLength <= HEADER_BYTES) {
    return null;
  }

  const header = new DataView(buffer, 0, HEADER_BYTES);
  if (header.getUint8(0) !== FRAME_VERSION) {
    return null;
  }

  return {
    keyframe: (header.getUint8(1) & 1) !== 0,
    width: header.getUint16(4, true),
    height: header.getUint16(6, true),
    // Microseconds fit in a double until the year 287396, and a timestamp the
    // decoder can use is a number rather than a bigint.
    timestampUs: Number(header.getBigUint64(8, true)),
    data: new Uint8Array(buffer, HEADER_BYTES),
  };
}

/**
 * The socket address of one surface on one server.
 *
 * `surface` is `composition` or an output's identifier, which is what the
 * telemetry calls it. Nothing that proves anything is in it: that goes in the
 * first message, never in the URL.
 */
export function streamUrl(baseUrl: string, surface: string): string {
  const url = new URL(`${API_PREFIX}${STREAM_PATH}/${encodeURIComponent(surface)}`, baseUrl);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  return url.toString();
}

// --- the transport -----------------------------------------------------------

/** A socket a stream can drive. */
export interface StreamSocket {
  send(text: string): void;
  close(): void;
}

/** What a stream socket reports back. */
export interface StreamSocketHandlers {
  opened(): void;
  /** One message: a picture, or something the server said in words. */
  received(data: ArrayBuffer | string): void;
  /** The socket ended, for whatever reason, including one this client caused. */
  closed(reason: string): void;
}

/** Opens a socket. Injected, so the tests need neither a server nor a browser. */
export type OpenStreamSocket = (url: string, handlers: StreamSocketHandlers) => StreamSocket;

/** A scheduled callback, cancellable. */
export type Timer = ReturnType<typeof setTimeout>;

/**
 * Opens a real `WebSocket`.
 *
 * The default, and the only part of the transport that needs a browser.
 */
export const openWebSocket: OpenStreamSocket = (url, handlers) => {
  const socket = new globalThis.WebSocket(url);
  // Pictures are bytes. Without this they arrive as blobs and every frame costs
  // a trip through an asynchronous reader before it can be decoded.
  socket.binaryType = 'arraybuffer';

  let failure = '';
  socket.onopen = () => handlers.opened();
  socket.onmessage = (event: MessageEvent<ArrayBuffer | string>) => handlers.received(event.data);
  socket.onerror = () => {
    // A browser says nothing about why on purpose. The close that follows is
    // where this is reported, so it is only remembered here.
    failure = 'the preview stream could not be reached';
  };
  socket.onclose = () => handlers.closed(failure === '' ? 'the preview stream closed' : failure);

  return {
    send: (text) => {
      if (socket.readyState === globalThis.WebSocket.OPEN) {
        socket.send(text);
      }
    },
    close: () => {
      // Handlers first: closing a socket fires `close`, and that one is this
      // client's own doing rather than a stream that went away.
      socket.onopen = null;
      socket.onmessage = null;
      socket.onerror = null;
      socket.onclose = null;
      socket.close();
    },
  };
};

/** Waits between attempts, in order; the last one repeats. */
export const STREAM_BACKOFF_MS = [500, 1000, 2000, 5000, 10_000] as const;

/**
 * How long an open socket may go without a picture.
 *
 * The renderer encodes every frame it draws while somebody is watching, so a
 * silence this long is a stream that has stopped rather than a quiet passage.
 * It matters because the last picture stays on the canvas: nothing in this
 * client may quietly go on showing a wall that is no longer there.
 */
export const QUIET_MS = 3000;

/**
 * How long to wait after the server has said it cannot stream this surface.
 *
 * That is a machine with no encoder, most often — something a renderer restart
 * can change and five seconds cannot. Subscribing is what asks the renderer to
 * encode, so asking every five seconds is asking a machine running a show the
 * same question all evening.
 */
export const DECLINED_MS = 30_000;

/** What the server says when a watcher from the network has not proved it may watch. */
const UNAUTHENTICATED = 'unauthenticated';

/**
 * Mints the first message this watcher sends, for one socket.
 *
 * Asked for once per socket rather than once per stream, and that is not an
 * optimisation to skip. A ticket is spent the first time it is used
 * (`spend_stream_ticket`), so a second socket offering the first socket's ticket
 * is refused; and a credential minted for a session that has since ended must
 * not be sent again either. One socket, one mint.
 *
 * Allowed to be slow because in the window it crosses a process boundary and
 * then a network: the server gives a watcher from the network five seconds to
 * prove itself (`TOKEN_TIMEOUT` in stream_routes.rs).
 *
 * `null` sends nothing, which is what a watcher on the machine itself does: it
 * needs no session, and it is also the honest answer when this
 * client has nothing to prove with.
 */
export type MintCredential = () => string | null | Promise<string | null>;

/** How a stream is set up. */
export interface StreamOptions {
  /** From `streamUrl`. */
  url: string;
  /**
   * Proves this watcher may watch, for a server that is not this machine.
   *
   * Absent sends nothing at all, for a caller that knows it has nothing to
   * prove and would rather not ask.
   */
  credential?: MintCredential;
  /** One picture. */
  onChunk: (chunk: StreamChunk) => void;
  /** Something went wrong, in words an operator can be shown. */
  onError?: (reason: string) => void;
  /**
   * What the surface turned out to be encoded with.
   *
   * Arrives a moment after the socket opens, and again if the surface is
   * restarted onto another encoder. A software preview is capped at fifteen
   * frames a second so that watching a wall cannot cost the show, and this is
   * how an operator can tell that from a wall that is stuttering
   * (owner decision Q13).
   */
  onEncoding?: (encoding: StreamEncoding) => void;
  /** The socket ended. Another attempt follows unless the reason was final. */
  onClose?: (reason: string) => void;
  /** Defaults to `openWebSocket`. */
  open?: OpenStreamSocket;
  /** Defaults to `setTimeout`/`clearTimeout`. */
  schedule?: (callback: () => void, ms: number) => Timer;
  cancel?: (timer: Timer) => void;
  /** Defaults to `QUIET_MS`. Zero switches the check off. */
  quietMs?: number;
}

/** A stream that is running. */
export interface StreamHandle {
  /** Stops watching, for good. Safe to call twice. */
  close(): void;
}

/**
 * Watches one surface, and keeps watching.
 *
 * A socket that drops is reopened on a bounded backoff, because the ordinary
 * reason a preview stops during a show is a network that came back a second
 * later. A socket the server refuses for a reason waiting cannot change is not
 * reopened at all.
 */
export function openStream(options: StreamOptions): StreamHandle {
  return new Stream(options);
}

/** One socket to one surface, reopened as often as it takes. */
class Stream implements StreamHandle {
  #socket: StreamSocket | null = null;
  #retry: Timer | null = null;
  #quiet: Timer | null = null;
  #attempt = 0;
  #stopped = false;

  private readonly open: OpenStreamSocket;
  private readonly schedule: (callback: () => void, ms: number) => Timer;
  private readonly cancel: (timer: Timer) => void;
  private readonly quietMs: number;

  constructor(private readonly options: StreamOptions) {
    this.open = options.open ?? openWebSocket;
    this.schedule = options.schedule ?? ((callback, ms) => setTimeout(callback, ms));
    this.cancel = options.cancel ?? ((timer) => clearTimeout(timer));
    this.quietMs = options.quietMs ?? QUIET_MS;
    this.#connect();
  }

  close(): void {
    this.#stopped = true;
    this.#clearTimers();
    const socket = this.#socket;
    this.#socket = null;
    socket?.close();
  }

  #connect(): void {
    this.#socket = this.open(this.options.url, {
      opened: () => this.#onOpen(),
      received: (data) => this.#onMessage(data),
      closed: (reason) => this.#onClose(reason),
    });
    this.#expectPicture();
  }

  #onOpen(): void {
    const mint = this.options.credential;
    if (mint === undefined) {
      // A watcher on the machine itself needs no session, and a server that
      // wants one closes the socket rather than guessing.
      return;
    }

    // The socket that opened is the socket that must carry the credential.
    // Minting one takes a trip across a process boundary and then to the server,
    // and by the time it comes back this stream may be on its next attempt: a
    // ticket sent down a socket nobody is listening to is a ticket spent on
    // nothing, and the server counts whatever arrives first as the answer it was
    // waiting for.
    const socket = this.#socket;

    void (async () => {
      let credential: string | null;
      try {
        credential = await mint();
      } catch (error) {
        // Nothing to offer on this socket, so the server will close it in five
        // seconds anyway. Saying so now costs the operator one line instead of
        // five seconds of a blank panel.
        const reason = `this client could not prove it may watch: ${message(error)}`;
        this.options.onError?.(reason);
        this.#drop(reason);
        return;
      }

      if (credential === null || credential === '' || this.#stopped || this.#socket !== socket) {
        return;
      }
      socket?.send(credential);
    })();
  }

  #onMessage(data: ArrayBuffer | string): void {
    if (typeof data === 'string') {
      this.#onSaid(data);
      return;
    }

    const chunk = parseChunk(data);
    if (chunk === null) {
      // A newer server said something this build does not read. The next
      // picture it sends may well be one that it does.
      return;
    }

    // A stream that has worked starts the backoff again, however long ago that
    // was: the next drop is usually a network that comes straight back.
    this.#attempt = 0;
    this.#expectPicture();
    this.options.onChunk(chunk);
  }

  /** The server saying something in words rather than in pictures. */
  #onSaid(text: string): void {
    let said = 'the server declined the stream';
    try {
      const body = JSON.parse(text) as { error?: unknown; encoding?: unknown };

      // Not an error at all: what this surface is being encoded with. Handled
      // first and returned from, because everything below this treats a text
      // message as bad news.
      const encoding = readEncoding(body.encoding);
      if (encoding !== null) {
        this.options.onEncoding?.(encoding);
        return;
      }

      if (typeof body.error === 'string') {
        said = body.error;
      }
    } catch {
      // Not the server's error shape, and not a picture either. The line above
      // will do.
    }

    this.options.onError?.(said);

    if (said === UNAUTHENTICATED) {
      // Waiting does not produce a token. Said once, and then nothing: a socket
      // reopened every ten seconds to be refused again is noise in a log
      // somebody will have to read during a show.
      this.close();
      this.options.onClose?.(said);
      return;
    }

    this.#drop(said, DECLINED_MS);
  }

  #onClose(reason: string): void {
    if (this.#stopped) {
      return;
    }
    this.#drop(reason);
  }

  /** Lets go of this attempt and schedules the next. */
  #drop(reason: string, wait?: number): void {
    if (this.#stopped || this.#retry !== null) {
      // Already waiting. A socket that fails reports its error and then its
      // close, and that is one failure and one reconnection.
      return;
    }

    const socket = this.#socket;
    this.#socket = null;
    socket?.close();
    this.#clearQuiet();

    const delay =
      wait ?? STREAM_BACKOFF_MS[Math.min(this.#attempt, STREAM_BACKOFF_MS.length - 1)] ?? 10_000;
    this.#attempt += 1;
    this.options.onClose?.(reason);

    this.#retry = this.schedule(() => {
      this.#retry = null;
      this.#connect();
    }, delay);
  }

  /** Restarts the wait for the next picture. */
  #expectPicture(): void {
    this.#clearQuiet();
    if (this.quietMs <= 0) {
      return;
    }
    this.#quiet = this.schedule(() => {
      this.#quiet = null;
      this.#drop('the preview stream went quiet');
    }, this.quietMs);
  }

  #clearQuiet(): void {
    if (this.#quiet !== null) {
      this.cancel(this.#quiet);
      this.#quiet = null;
    }
  }

  #clearTimers(): void {
    if (this.#retry !== null) {
      this.cancel(this.#retry);
      this.#retry = null;
    }
    this.#clearQuiet();
  }
}

/**
 * One encoding as the server sent it, or null when that is not what this is.
 *
 * Checked rather than cast. Everything on this socket is additive — a newer
 * server may say words this build has never heard — so a message that does not
 * have this shape is not this message, and is not an error either.
 */
function readEncoding(value: unknown): StreamEncoding | null {
  if (typeof value !== 'object' || value === null) {
    return null;
  }
  const { encoder, hardware, fps } = value as Partial<StreamEncoding>;
  if (typeof encoder !== 'string' || typeof hardware !== 'boolean') {
    return null;
  }
  if (typeof fps !== 'number' || !Number.isFinite(fps) || fps <= 0) {
    return null;
  }
  return { encoder, hardware, fps };
}

// --- the picture -------------------------------------------------------------

/**
 * What the server's encoder produces: H.264 baseline, level 3.0.
 *
 * The profile a machine with any hardware decoder at all can decode.
 */
export const PREVIEW_CODEC = 'avc1.42E01E';

/** What the panel can say about the picture. */
export interface StreamStatus {
  /** Where the picture has got to. */
  state:
    | /** Not watching. */ 'stopped'
    | /** A socket is open or on its way. */ 'connecting'
    | /** Connected, and nothing to start decoding from yet. */ 'waiting'
    | /** Pictures are being decoded. */ 'playing';
  /** What the pictures measure, once one has been decoded. */
  size: { width: number; height: number } | null;
  /** The last thing that went wrong, or null when nothing has since. */
  error: string | null;
  /**
   * What the server is encoding this surface with, once it has said.
   *
   * Null until then, and on a server too old to say. Drawn so that a preview
   * running at a quarter speed reads as a decision rather than as a fault.
   */
  encoding: StreamEncoding | null;
}

/** The status as a line to put in front of an operator. */
export function statusLine(status: StreamStatus): string {
  switch (status.state) {
    case 'stopped':
      return status.error ?? 'The live preview is off.';
    case 'connecting':
      return status.error === null
        ? 'Connecting to the live preview…'
        : `Reconnecting: ${status.error}`;
    case 'waiting':
      return 'Waiting for a picture to start from…';
    case 'playing':
      // **Nothing at all while it is playing** (owner, 2026-09-23): the size
      // and the encoding were a line of text under the picture, and the
      // picture is what the line was taking room from. What is worth saying
      // here is what is *wrong*, and a preview that is playing has nothing
      // wrong with it.
      return '';
  }
}

/**
 * How the encoding reads on one line.
 *
 * The rate first, because that is the thing an operator is looking at and
 * wondering about; the encoder second, because that is what a log will be
 * searched for afterwards.
 */
export function encodingLine(encoding: StreamEncoding): string {
  const rate = Number.isInteger(encoding.fps) ? String(encoding.fps) : encoding.fps.toFixed(1);
  return `${rate} fps, ${encoding.hardware ? 'card' : 'software'} (${encoding.encoder})`;
}

/** How a player is set up. */
export interface StreamPlayerOptions {
  /** Draws one picture. The frame is closed as soon as this returns. */
  onFrame: (frame: VideoFrame) => void;
  /** Called whenever the status changes, and only then. */
  onStatus: (status: StreamStatus) => void;
  /** Builds a decoder. Injected, so this needs no WebCodecs to exercise. */
  newDecoder?: (init: VideoDecoderInit) => VideoDecoder;
}

/**
 * Turns chunks into pictures.
 *
 * It waits for a keyframe before it configures anything, because a stream is
 * joined wherever the show happens to be and everything before the first
 * keyframe describes pictures this decoder has never seen. It rebuilds itself
 * when the surface changes shape and when the decoder gives up, because both
 * happen mid-show and neither is a reason to show an operator a black
 * rectangle with no explanation.
 */
export class StreamPlayer {
  #decoder: VideoDecoder | null = null;
  /** What the decoder was configured with, when there is a decoder. */
  #configured: { width: number; height: number } | null = null;
  #status: StreamStatus = { state: 'connecting', size: null, error: null, encoding: null };
  #closed = false;

  private readonly newDecoder: (init: VideoDecoderInit) => VideoDecoder;

  constructor(private readonly options: StreamPlayerOptions) {
    this.newDecoder = options.newDecoder ?? ((init) => new VideoDecoder(init));
  }

  /** What the panel is currently being told. */
  get status(): StreamStatus {
    return this.#status;
  }

  /** Takes one picture off the wire. */
  feed(chunk: StreamChunk): void {
    if (this.#closed) {
      return;
    }

    if (
      this.#configured !== null &&
      (this.#configured.width !== chunk.width || this.#configured.height !== chunk.height)
    ) {
      // The surface changed shape mid-stream. The server announces that in the
      // next picture rather than out of band, so this is the only warning there
      // is, and a decoder configured for the old shape would reject every
      // picture from here on.
      this.#release();
    }

    if (this.#decoder === null) {
      if (!chunk.keyframe) {
        this.#report({ ...this.#status, state: 'waiting' });
        return;
      }
      if (!this.#start(chunk)) {
        return;
      }
    }

    try {
      this.#decoder?.decode(
        new EncodedVideoChunk({
          type: chunk.keyframe ? 'key' : 'delta',
          timestamp: chunk.timestampUs,
          data: chunk.data,
        }),
      );
    } catch (error) {
      this.#rebuild(`the picture could not be decoded: ${message(error)}`);
    }
  }

  /**
   * The pictures stopped arriving.
   *
   * For whatever the transport says went wrong: the decoder is let go, because
   * whatever comes back will have to start from a keyframe anyway.
   */
  failed(reason: string): void {
    if (this.#closed) {
      return;
    }
    this.#release();
    this.#report({ ...this.#status, state: 'connecting', error: reason });
  }

  /** Stops decoding. Safe to call twice. */
  close(): void {
    this.#closed = true;
    this.#release();
    this.#report({ ...this.#status, state: 'stopped' });
  }

  /** Builds a decoder for the shape this picture says it is. */
  #start(chunk: StreamChunk): boolean {
    try {
      const decoder = this.newDecoder({
        output: (frame) => this.#paint(frame),
        error: (error) => this.#rebuild(`the decoder stopped: ${message(error)}`),
      });
      decoder.configure({
        codec: PREVIEW_CODEC,
        // No `description`: that is what tells WebCodecs to expect Annex-B with
        // the parameter sets in front of every keyframe rather than a
        // description handed over once at the start.
        optimizeForLatency: true,
        codedWidth: chunk.width,
        codedHeight: chunk.height,
      });
      this.#decoder = decoder;
      this.#configured = { width: chunk.width, height: chunk.height };
      this.#report({ ...this.#status, state: 'waiting' });
      return true;
    } catch (error) {
      this.#rebuild(`the decoder could not be started: ${message(error)}`);
      return false;
    }
  }

  /**
   * Records what the server said this surface is being encoded with.
   *
   * Kept on the status rather than reported on its own, so that a panel drawing
   * one line has one thing to draw. A surface restarted onto another encoder
   * sends this again and the line changes with it.
   */
  setEncoding(encoding: StreamEncoding | null): void {
    if (this.#closed) {
      return;
    }
    this.#report({ ...this.#status, encoding });
  }

  /** Draws one decoded picture, and lets go of it. */
  #paint(frame: VideoFrame): void {
    try {
      if (this.#closed) {
        return;
      }
      this.#report({
        ...this.#status,
        state: 'playing',
        // From the decoded frame rather than from the header: H.264 carries its
        // own cropping, and what was drawn is the honest answer to what shape
        // the wall is.
        size: { width: frame.displayWidth, height: frame.displayHeight },
        error: null,
      });
      this.options.onFrame(frame);
    } finally {
      // A frame that is not closed holds one of the decoder's buffers, and a
      // decoder with no buffers left stops within seconds.
      frame.close();
    }
  }

  /** Lets the decoder go and waits for a keyframe to build the next one. */
  #rebuild(reason: string): void {
    this.#release();
    if (this.#closed) {
      return;
    }
    this.#report({ ...this.#status, state: 'waiting', error: reason });
  }

  #release(): void {
    const decoder = this.#decoder;
    this.#decoder = null;
    this.#configured = null;
    try {
      // A decoder that has already errored is closed, and closing one twice
      // throws.
      if (decoder && decoder.state !== 'closed') {
        decoder.close();
      }
    } catch {
      // It is not this player's any more either way.
    }
  }

  #report(status: StreamStatus): void {
    if (
      status.state === this.#status.state &&
      status.error === this.#status.error &&
      status.size?.width === this.#status.size?.width &&
      status.size?.height === this.#status.size?.height
    ) {
      // Nothing changed. A panel redrawn sixty times a second says nothing a
      // panel redrawn when something happens does not.
      return;
    }
    this.#status = status;
    this.options.onStatus(status);
  }
}

/** Whatever was thrown, as something an operator can be shown. */
function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
