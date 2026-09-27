// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * The first message a client sends.
 *
 * `Debug` is implemented by hand so a session token cannot reach a log through a derived formatter.
 */
export interface ClientHello {
  /**
   * Human-readable client name, for diagnostics.
   */
  client_name: string;
  /**
   * Client build version, for diagnostics.
   */
  client_version: string;
  /**
   * Realtime protocol version the client speaks.
   */
  protocol_version: number;
  /**
   * Session token obtained from the HTTP API.
   *
   * Carried in the handshake rather than in a header or a query string. A browser cannot set headers on a WebSocket handshake, and a query string would write the token into every access log and proxy cache along the way.
   */
  token: string;
}
