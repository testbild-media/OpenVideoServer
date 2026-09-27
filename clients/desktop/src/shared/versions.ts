// The contract versions this client is built against.
//
// These are the TypeScript half of `crates/ovs-types/src/version.rs`. They are
// written out rather than read from the server, because the point of a version
// is what *this build* understands: a client that adopted whatever the server
// announced would negotiate itself into speaking a protocol it has never seen.
//
// `test/versions.test.ts` reads version.rs and fails when they drift, so the
// duplication cannot quietly become a disagreement.
//
// The two handshakes are here for the same reason. The realtime socket sends a
// `ClientHello` carrying a session token; a preview socket sends a ticket, which
// is the same first message with something much smaller in it. Both are parsed
// strictly at the other end, and two copies of either would eventually stop
// being one message.

import type { ClientHello } from '@ovs/schema';

/** Realtime protocol version offered in the handshake. */
export const REALTIME_PROTOCOL_VERSION = 1;

/** Show schema version this client understands. */
export const SHOW_SCHEMA_VERSION = 1;

/** Every HTTP route lives under this prefix. */
export const API_PREFIX = '/api/v1';

/** The realtime socket, which is versioned by its handshake rather than its path. */
export const REALTIME_PATH = '/realtime';

/** What this client calls itself in the handshake, for the server's logs. */
export const CLIENT_NAME = 'OpenVideoServer Desktop';

/**
 * The first message a client sends on either socket.
 *
 * Text rather than an object, because text is what goes on the wire and because
 * the main process hands one to the window as something to send rather than
 * something to read: a caller that never parses it cannot be tempted to take
 * the token out of it and use it for something else.
 */
export function clientHello(token: string, clientVersion: string): string {
  const hello: ClientHello = {
    protocol_version: REALTIME_PROTOCOL_VERSION,
    token,
    client_name: CLIENT_NAME,
    client_version: clientVersion,
  };
  return JSON.stringify(hello);
}

/**
 * The first message a watcher sends when it holds a ticket.
 *
 * The narrow half of the same handshake: `authenticated()` in stream_routes.rs
 * reads one message with an optional `ticket` and an optional `token` and tries
 * the ticket first. A ticket opens one surface for one minute and is spent by
 * being used; a session token opens everything and does not expire for hours,
 * so a window with a video decoder in it should be holding the first.
 *
 * Written out here rather than imported: `Offered` is a private struct in
 * stream_routes.rs, so the schema generator does not publish a type for it. The
 * shape is one field and it is checked by `test/stream.test.ts` against the
 * Rust that reads it.
 */
export function streamTicket(ticket: string): string {
  return JSON.stringify({ ticket });
}
