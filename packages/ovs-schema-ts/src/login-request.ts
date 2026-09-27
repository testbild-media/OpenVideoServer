// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Credentials for a sign-in.
 *
 * `Debug` is implemented by hand so a password cannot reach a log through a derived formatter.
 */
export interface LoginRequest {
  /**
   * Plain password, sent over TLS and never stored.
   */
  password: string;
  /**
   * Login name.
   */
  username: string;
}
