// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * A permission to watch one preview stream, for a short while.
 *
 * Minted by a request that carried a session, and narrower than one on purpose: it opens one surface, it expires within the minute, and it is spent the first time it is used. A client that has this cannot load a show, move a fader or stop the machine.
 */
export interface StreamTicketResponse {
  /**
   * When it stops working, in milliseconds since the epoch.
   */
  expires_at: number;
  /**
   * The ticket, to be sent as the first message on the stream socket.
   */
  ticket: string;
}
