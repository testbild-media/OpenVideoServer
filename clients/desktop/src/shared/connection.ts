// The realtime socket, and what to do when it misbehaves.
//
// The protocol's promise is handshake, snapshot, updates, in that order
//. This is the other end of that promise: it refuses to show
// state it cannot account for, and it distinguishes the three ways a connection
// ends, because they need three different things from the operator.
//
//   - the protocol does not match  -> update the client; retrying is pointless
//   - the session is not accepted  -> sign in again; retrying is pointless
//   - the network went away        -> retry, and say so while it does
//
// Nothing here touches a real WebSocket or a real clock. Both are handed in, so
// the behaviour that matters during a show — a gap, a silent connection, a
// server that comes back — is tested rather than hoped for.

import type {
  ConnectorInfo,
  ServerHello,
  ServerMessage,
  ServerTelemetry,
} from '@ovs/schema';

import { apply, emptyState, type ShowState } from './mirror.js';
import { clientHello } from './versions.js';

/** A socket this connection can drive. */
export interface RealtimeSocket {
  send(text: string): void;
  close(): void;
}

/** What a socket reports back. */
export interface SocketHandlers {
  opened(): void;
  received(text: string): void;
  /** The socket ended, for whatever reason, including one this client caused. */
  closed(reason: string): void;
}

/** Opens a socket. Injected, so the tests need neither a server nor a browser. */
export type OpenSocket = (url: string, handlers: SocketHandlers) => RealtimeSocket;

/** A scheduled callback, cancellable. */
export type Timer = ReturnType<typeof setTimeout>;

/** What the operator is told about the connection. */
export type ConnectionStatus =
  /** Not trying. */
  | { state: 'offline' }
  /** A socket is open or opening and the handshake is not finished. */
  | { state: 'connecting' }
  /** Connected, handshaken and holding a snapshot. */
  | { state: 'live'; serverVersion: string }
  /** Disconnected and waiting to try again. */
  | { state: 'waiting'; reason: string; retryInMs: number }
  /** Refused for a reason that trying again will not change. */
  | { state: 'refused'; reason: string };

/** What a connection tells the rest of the client. */
export interface ConnectionListener {
  status(status: ConnectionStatus): void;
  /** The mirror changed. Called after a snapshot and after every applied update. */
  state(state: ShowState): void;
  telemetry(telemetry: ServerTelemetry): void;
  /**
   * The NDI senders the server can see changed.
   *
   * The whole list every time, so this replaces rather than merges. Arrives
   * once after the snapshot and then whenever a sender is switched on or off.
   */
  ndiSources(sources: readonly string[]): void;
  /**
   * What this machine is wired to changed.
   *
   * A monitor swapped, a processor powered up, a cable moved. The whole list
   * every time, so this replaces rather than merges.
   */
  connectors(connectors: readonly ConnectorInfo[]): void;
  /** A fault the server reported. */
  fault(code: string, message: string): void;
}

/** How this connection is set up. */
export interface ConnectionOptions {
  url: string;
  token: string;
  clientVersion: string;
  open: OpenSocket;
  listener: ConnectionListener;
  /** Defaults to `setTimeout`/`clearTimeout`. */
  schedule?: (callback: () => void, ms: number) => Timer;
  cancel?: (timer: Timer) => void;
}

/** Reconnection delays, in order; the last one repeats. */
const BACKOFF_MS = [500, 1000, 2000, 5000, 10_000] as const;

/** How often a ping goes out on an idle connection. */
export const HEARTBEAT_MS = 5000;

/**
 * How long a ping may go unanswered before the connection is treated as dead.
 *
 * A socket whose other end has gone without a FIN looks exactly like a quiet
 * one. During a show the difference is a client showing state from two minutes
 * ago, so it is worth a packet every five seconds to find out.
 */
export const HEARTBEAT_TIMEOUT_MS = 12_000;

/** One connection to one server. */
export class Connection {
  #socket: RealtimeSocket | null = null;
  #status: ConnectionStatus = { state: 'offline' };
  #state: ShowState = emptyState();
  #handshaken = false;
  #stopped = true;
  #attempt = 0;
  /** A resynchronisation has been asked for and the snapshot has not arrived. */
  #awaitingSnapshot = false;
  #reconnect: Timer | null = null;
  #heartbeat: Timer | null = null;
  #pingNonce = 0;
  /** When a ping was last answered, on the injected clock's terms. */
  #outstandingPings = 0;

  private readonly schedule: (callback: () => void, ms: number) => Timer;
  private readonly cancel: (timer: Timer) => void;

  constructor(private readonly options: ConnectionOptions) {
    this.schedule = options.schedule ?? ((callback, ms) => setTimeout(callback, ms));
    this.cancel = options.cancel ?? ((timer) => clearTimeout(timer));
  }

  /** What the operator is currently being told. */
  get status(): ConnectionStatus {
    return this.#status;
  }

  /** The mirror. */
  get state(): ShowState {
    return this.#state;
  }

  /** Opens the connection, and keeps it open. */
  start(): void {
    if (!this.#stopped) {
      return;
    }
    this.#stopped = false;
    this.#attempt = 0;
    this.#connect();
  }

  /** Closes it, and stops trying. */
  stop(): void {
    this.#stopped = true;
    this.#clearTimers();
    const socket = this.#socket;
    this.#socket = null;
    this.#handshaken = false;
    socket?.close();
    this.#announce({ state: 'offline' });
  }

  #connect(): void {
    this.#handshaken = false;
    this.#awaitingSnapshot = false;
    this.#outstandingPings = 0;
    this.#announce({ state: 'connecting' });

    this.#socket = this.options.open(this.options.url, {
      opened: () => this.#onOpen(),
      received: (text) => this.#onText(text),
      closed: (reason) => this.#onClose(reason),
    });
  }

  #onOpen(): void {
    // The token goes in the handshake rather than a header or a query string:
    // the one would not survive a browser, the other would be written into
    // every access log between here and the server.
    this.#socket?.send(clientHello(this.options.token, this.options.clientVersion));
  }

  #onText(text: string): void {
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch {
      this.options.listener.fault('malformed', 'the server sent something this client cannot read');
      return;
    }

    if (!this.#handshaken) {
      this.#onHello(parsed as ServerHello);
      return;
    }

    this.#onMessage(parsed as ServerMessage);
  }

  #onHello(hello: ServerHello): void {
    // A refusal closes the socket cleanly at the far end, so there is nothing
    // to close here — only something to say. Two of the three are final: the
    // operator has to do something before another attempt could succeed.
    switch (hello.result) {
      case 'accepted':
        this.#handshaken = true;
        this.#attempt = 0;
        this.#state = emptyState();
        this.#announce({ state: 'live', serverVersion: hello.server_version });
        this.#startHeartbeat();
        break;
      case 'incompatible_protocol':
        this.#refuse(
          `this client speaks realtime protocol ${hello.client_version} and the server speaks ` +
            `${hello.server_version}; update whichever is older`,
        );
        break;
      case 'unauthenticated':
        this.#refuse('the server did not accept this session; sign in again');
        break;
      case 'not_ready':
        // Temporary by definition: the server is still starting.
        this.#retry('the server is still starting');
        break;
    }
  }

  #onMessage(message: ServerMessage): void {
    if (message.type === 'telemetry') {
      this.options.listener.telemetry(message);
      return;
    }

    if (message.type === 'ndi_sources') {
      this.options.listener.ndiSources(message.sources);
      return;
    }

    if (message.type === 'connectors') {
      this.options.listener.connectors(message.connectors);
      return;
    }

    if (message.type === 'pong') {
      this.#outstandingPings = 0;
      return;
    }

    if (message.type === 'fault') {
      this.options.listener.fault(message.code, message.message);
      if (message.code === 'lagged') {
        // This client fell behind and the server stopped pretending otherwise.
        this.#resynchronise();
      }
      return;
    }

    if (message.type === 'media_progress') {
      return;
    }

    if (this.#awaitingSnapshot && message.type !== 'snapshot') {
      // Everything between asking for a snapshot and getting one is a gap by
      // construction. Applying it would be wrong and asking again for each
      // would be a storm.
      return;
    }

    const applied = apply(this.#state, message);
    switch (applied.kind) {
      case 'applied':
        this.#awaitingSnapshot = false;
        this.options.listener.state(this.#state);
        break;
      case 'gap':
        this.options.listener.fault(
          'gap',
          `update ${applied.received} does not follow ${applied.expected - 1}; resynchronising`,
        );
        this.#resynchronise();
        break;
      case 'ignored':
        break;
    }
  }

  /** Asks for a fresh snapshot rather than guessing at what was missed. */
  #resynchronise(): void {
    if (this.#awaitingSnapshot) {
      return;
    }
    this.#awaitingSnapshot = true;
    this.#send({ type: 'resynchronise' });
  }

  #onClose(reason: string): void {
    this.#socket = null;
    this.#handshaken = false;
    this.#stopHeartbeat();

    if (this.#stopped || this.#status.state === 'refused') {
      // Either this client closed it, or the server already said why.
      return;
    }
    this.#retry(reason === '' ? 'the connection to the server was lost' : reason);
  }

  #retry(reason: string): void {
    if (this.#stopped || this.#reconnect !== null) {
      // Already waiting. A refusal the server explains and the close that
      // follows it are one event, not two, and treating them as two would open
      // two sockets and skip a step of the backoff.
      return;
    }
    const delay = BACKOFF_MS[Math.min(this.#attempt, BACKOFF_MS.length - 1)] ?? 10_000;
    this.#attempt += 1;
    this.#announce({ state: 'waiting', reason, retryInMs: delay });
    this.#reconnect = this.schedule(() => {
      this.#reconnect = null;
      this.#connect();
    }, delay);
  }

  #refuse(reason: string): void {
    this.#stopped = true;
    this.#clearTimers();
    this.#socket = null;
    this.#announce({ state: 'refused', reason });
  }

  #startHeartbeat(): void {
    this.#stopHeartbeat();
    const beat = () => {
      if (this.#outstandingPings * HEARTBEAT_MS >= HEARTBEAT_TIMEOUT_MS) {
        // Nothing has come back for long enough that this is not a quiet
        // connection, it is a dead one.
        const socket = this.#socket;
        this.#socket = null;
        this.#stopHeartbeat();
        socket?.close();
        this.#handshaken = false;
        this.#retry('the server stopped answering');
        return;
      }
      this.#pingNonce += 1;
      this.#outstandingPings += 1;
      this.#send({ type: 'ping', nonce: this.#pingNonce });
      this.#heartbeat = this.schedule(beat, HEARTBEAT_MS);
    };
    this.#heartbeat = this.schedule(beat, HEARTBEAT_MS);
  }

  #stopHeartbeat(): void {
    if (this.#heartbeat !== null) {
      this.cancel(this.#heartbeat);
      this.#heartbeat = null;
    }
    this.#outstandingPings = 0;
  }

  #clearTimers(): void {
    if (this.#reconnect !== null) {
      this.cancel(this.#reconnect);
      this.#reconnect = null;
    }
    this.#stopHeartbeat();
  }

  #send(value: unknown): void {
    this.#socket?.send(JSON.stringify(value));
  }

  #announce(status: ConnectionStatus): void {
    this.#status = status;
    this.options.listener.status(status);
  }
}
