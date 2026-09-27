// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * What happened to a request that the machine restart or stop.
 */
export interface PowerResponse {
  /**
   * Whether the machine accepted it.
   *
   * False is ordinary rather than exceptional: a server that has not been given permission to reboot itself is a server that will not, and saying so beats an operator pressing a button that does nothing.
   */
  accepted: boolean;
  /**
   * What was run, or why nothing was.
   */
  detail: string;
}
