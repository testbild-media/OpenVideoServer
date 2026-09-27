// What the connection does when the network, or the server, misbehaves.

import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { ServerHello, ServerMessage, ServerTelemetry } from '@ovs/schema';
import {
  Connection,
  HEARTBEAT_MS,
  type ConnectionStatus,
  type RealtimeSocket,
  type SocketHandlers,
} from '../src/shared/connection.js';
import { REALTIME_PROTOCOL_VERSION } from '../src/shared/versions.js';

/** A socket that records what was sent and can be driven from the test. */
class FakeSocket implements RealtimeSocket {
  readonly sent: string[] = [];
  closed = false;

  constructor(readonly handlers: SocketHandlers) {}

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

  receive(value: unknown): void {
    this.handlers.received(JSON.stringify(value));
  }
}

const ACCEPTED: ServerHello = {
  result: 'accepted',
  protocol_version: REALTIME_PROTOCOL_VERSION,
  server_version: '0.0.1-test',
};

function snapshot(sequence: number): ServerMessage {
  return {
    type: 'snapshot',
    sequence,
    master: { bpm: 120, crossfader: 0, dimmer: 1, freeze: false, speed_scale: 1 },
    layers: [],
    renderer: { state: 'running' },
  };
}

/** A connection wired to a fake socket and fake timers. */
function harness() {
  const sockets: FakeSocket[] = [];
  const statuses: ConnectionStatus[] = [];
  const faults: Array<{ code: string; message: string }> = [];
  const telemetry: ServerTelemetry[] = [];
  const ndiSources: string[][] = [];
  const states: number[] = [];

  const connection = new Connection({
    url: 'ws://server/realtime',
    token: 't0ken',
    clientVersion: '0.0.1',
    open: (_url, handlers) => {
      const socket = new FakeSocket(handlers);
      sockets.push(socket);
      // A real socket opens asynchronously; this one opens when told to, so a
      // test can inspect the moment before the handshake.
      return socket;
    },
    schedule: (callback, ms) => setTimeout(callback, ms),
    cancel: (timer) => clearTimeout(timer),
    listener: {
      status: (status) => statuses.push(status),
      state: (state) => states.push(state.sequence),
      telemetry: (value) => telemetry.push(value),
      ndiSources: (value) => ndiSources.push([...value]),
      connectors: () => undefined,
      fault: (code, message) => faults.push({ code, message }),
    },
  });

  /** Opens the newest socket and completes the handshake. */
  const live = () => {
    const socket = sockets[sockets.length - 1]!;
    socket.handlers.opened();
    socket.receive(ACCEPTED);
    return socket;
  };

  return { connection, sockets, statuses, faults, telemetry, ndiSources, states, live };
}

beforeEach(() => {
  vi.useFakeTimers();
});

describe('the handshake', () => {
  it('offers the version this client speaks, and its token', () => {
    const { connection, sockets } = harness();
    connection.start();
    sockets[0]!.handlers.opened();

    expect(sockets[0]!.messages()[0]).toEqual({
      protocol_version: REALTIME_PROTOCOL_VERSION,
      token: 't0ken',
      client_name: 'OpenVideoServer Desktop',
      client_version: '0.0.1',
    });
  });

  it('goes live when the server accepts', () => {
    const { connection, statuses, live } = harness();
    connection.start();
    live();

    expect(statuses.map((status) => status.state)).toEqual(['connecting', 'live']);
  });

  it('starts from an empty mirror on every connection', () => {
    // A reconnection is a new snapshot. Anything left over from the last one
    // would be state this client cannot account for.
    const { connection, sockets, live } = harness();
    connection.start();
    live().receive(snapshot(40));
    expect(connection.state.sequence).toBe(40);

    sockets[0]!.handlers.closed('lost');
    vi.advanceTimersByTime(500);
    sockets[1]!.handlers.opened();
    sockets[1]!.receive(ACCEPTED);

    expect(connection.state.sequence).toBe(0);
    expect(connection.state.master).toBeNull();
  });
});

describe('a refusal', () => {
  it('is final for a version mismatch, and says which end is old', () => {
    const { connection, sockets, statuses } = harness();
    connection.start();
    sockets[0]!.handlers.opened();
    sockets[0]!.receive({
      result: 'incompatible_protocol',
      client_version: 1,
      server_version: 7,
    } satisfies ServerHello);

    const last = statuses[statuses.length - 1]!;
    expect(last.state).toBe('refused');
    expect(last.state === 'refused' && last.reason).toContain('7');

    // And nothing is scheduled: no socket is opened however long it waits.
    vi.advanceTimersByTime(60_000);
    expect(sockets).toHaveLength(1);
  });

  it('is final for a session the server will not accept', () => {
    const { connection, sockets, statuses } = harness();
    connection.start();
    sockets[0]!.handlers.opened();
    sockets[0]!.receive({ result: 'unauthenticated' } satisfies ServerHello);

    const last = statuses[statuses.length - 1]!;
    expect(last.state).toBe('refused');
    expect(last.state === 'refused' && last.reason).toContain('sign in');
    vi.advanceTimersByTime(60_000);
    expect(sockets).toHaveLength(1);
  });

  it('is temporary while the server is still starting', () => {
    const { connection, sockets, statuses } = harness();
    connection.start();
    sockets[0]!.handlers.opened();
    sockets[0]!.receive({ result: 'not_ready' } satisfies ServerHello);

    expect(statuses[statuses.length - 1]!.state).toBe('waiting');
    vi.advanceTimersByTime(500);
    expect(sockets).toHaveLength(2);
  });

  it('counts the refusal and the close that follows it as one event', () => {
    // The server says why and then closes. Two retries would open two sockets
    // and skip a step of the backoff, which is exactly what a struggling server
    // does not need.
    const { connection, sockets } = harness();
    connection.start();
    sockets[0]!.handlers.opened();
    sockets[0]!.receive({ result: 'not_ready' } satisfies ServerHello);
    sockets[0]!.handlers.closed('socket closed');

    vi.advanceTimersByTime(500);
    expect(sockets).toHaveLength(2);
    vi.advanceTimersByTime(5000);
    expect(sockets).toHaveLength(2);
  });
});

describe('the update stream', () => {
  it('reports the state after a snapshot and after each update', () => {
    const { connection, states, live } = harness();
    connection.start();
    const socket = live();

    socket.receive(snapshot(10));
    socket.receive({
      type: 'update',
      sequence: 11,
      change: { change: 'renderer_changed', renderer: { state: 'degraded', reason: 'hot' } },
    } satisfies ServerMessage);

    expect(states).toEqual([10, 11]);
    expect(connection.state.renderer).toEqual({ state: 'degraded', reason: 'hot' });
  });

  it('asks for a fresh snapshot on a gap, once, and applies nothing until it lands', () => {
    const { connection, faults, live } = harness();
    connection.start();
    const socket = live();
    socket.receive(snapshot(10));
    socket.sent.length = 0;

    // Two updates that do not follow. One resynchronise, not two.
    socket.receive({
      type: 'update',
      sequence: 12,
      change: { change: 'renderer_changed', renderer: { state: 'faulted', reason: 'no' } },
    } satisfies ServerMessage);
    socket.receive({
      type: 'update',
      sequence: 13,
      change: { change: 'renderer_changed', renderer: { state: 'stopped' } },
    } satisfies ServerMessage);

    expect(socket.messages()).toEqual([{ type: 'resynchronise' }]);
    expect(faults.map((fault) => fault.code)).toEqual(['gap']);
    // Neither update was applied.
    expect(connection.state.renderer).toEqual({ state: 'running' });
    expect(connection.state.sequence).toBe(10);

    // The snapshot puts it back in step.
    socket.receive(snapshot(14));
    expect(connection.state.sequence).toBe(14);
  });

  it('resynchronises when the server says this client fell behind', () => {
    const { connection, live } = harness();
    connection.start();
    const socket = live();
    socket.receive(snapshot(1));
    socket.sent.length = 0;

    socket.receive({
      type: 'fault',
      code: 'lagged',
      message: 'missed 12 updates; resynchronise',
    } satisfies ServerMessage);

    expect(socket.messages()).toEqual([{ type: 'resynchronise' }]);
  });

  it('passes the network senders on without touching the mirror', () => {
    // Outside the sequenced stream, like telemetry: each message carries the
    // whole list, so one that is missed costs nothing and needs no
    // resynchronisation. Which means it must not move the sequence either.
    const { connection, ndiSources, states, live } = harness();
    connection.start();
    const socket = live();
    socket.receive(snapshot(5));
    states.length = 0;

    socket.receive({
      type: 'ndi_sources',
      sources: ['JK-GAMING-02 (Test Pattern)'],
    } as ServerMessage);

    expect(ndiSources).toEqual([['JK-GAMING-02 (Test Pattern)']]);
    expect(states).toEqual([]);
    expect(connection.state.sequence).toBe(5);
  });

  it('passes telemetry on without touching the mirror', () => {
    const { connection, telemetry, states, live } = harness();
    connection.start();
    const socket = live();
    socket.receive(snapshot(5));
    states.length = 0;

    socket.receive({
      type: 'telemetry',
      active_decoders: 1,
      cpu_percent: 12,
      decode_errors: 0,
      disk_total_bytes: 0,
      disk_used_bytes: 0,
      ip_address: '10.0.0.2',
      outputs: [],
      prepared_decoders: 0,
      ram_percent: 30,
      renderer: { state: 'running' },
      uptime_s: 60,
      version: '0.0.1',
    } as ServerMessage);

    expect(telemetry).toHaveLength(1);
    expect(states).toEqual([]);
    expect(connection.state.sequence).toBe(5);
  });
});

describe('losing the server', () => {
  it('retries with a growing delay and comes back live', () => {
    const { connection, sockets, statuses, live } = harness();
    connection.start();
    live().handlers.closed('socket closed');

    expect(statuses[statuses.length - 1]).toEqual({
      state: 'waiting',
      reason: 'socket closed',
      retryInMs: 500,
    });

    vi.advanceTimersByTime(500);
    expect(sockets).toHaveLength(2);

    // Still nothing there: the next wait is longer.
    sockets[1]!.handlers.closed('socket closed');
    expect(statuses[statuses.length - 1]).toMatchObject({ retryInMs: 1000 });

    vi.advanceTimersByTime(1000);
    live();
    expect(connection.status.state).toBe('live');
  });

  it('starts the backoff again after a connection that worked', () => {
    const { connection, sockets, statuses, live } = harness();
    connection.start();
    live().handlers.closed('lost');
    vi.advanceTimersByTime(500);
    live();

    sockets[sockets.length - 1]!.handlers.closed('lost');
    expect(statuses[statuses.length - 1]).toMatchObject({ retryInMs: 500 });
  });

  it('gives up on a socket that stops answering', () => {
    // A half-open connection looks exactly like a quiet one. During a show the
    // difference is a client showing state from two minutes ago.
    const { connection, sockets, live } = harness();
    connection.start();
    const socket = live();
    socket.receive(snapshot(1));
    socket.sent.length = 0;

    vi.advanceTimersByTime(HEARTBEAT_MS * 3);
    expect(socket.messages().every((message) => message['type'] === 'ping')).toBe(true);
    expect(socket.messages()).toHaveLength(3);

    // The fourth beat finds three unanswered pings and gives up.
    vi.advanceTimersByTime(HEARTBEAT_MS);
    expect(socket.closed).toBe(true);
    expect(connection.status).toMatchObject({ state: 'waiting' });

    vi.advanceTimersByTime(500);
    expect(sockets).toHaveLength(2);
  });

  it('keeps a connection that answers', () => {
    const { connection, live } = harness();
    connection.start();
    const socket = live();

    for (let beat = 0; beat < 10; beat += 1) {
      vi.advanceTimersByTime(HEARTBEAT_MS);
      socket.receive({ type: 'pong', nonce: beat + 1 } satisfies ServerMessage);
    }

    expect(socket.closed).toBe(false);
    expect(connection.status.state).toBe('live');
  });

  it('stops trying when it is told to stop', () => {
    const { connection, sockets, statuses, live } = harness();
    connection.start();
    const socket = live();

    connection.stop();

    expect(socket.closed).toBe(true);
    expect(statuses[statuses.length - 1]).toEqual({ state: 'offline' });
    // A close that arrives after stopping must not start a retry.
    socket.handlers.closed('socket closed');
    vi.advanceTimersByTime(60_000);
    expect(sockets).toHaveLength(1);
  });
});
