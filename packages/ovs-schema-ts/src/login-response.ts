// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * AccountId (ULID, 26 characters)
 */
export type AccountId = string;

/**
 * A successful sign-in.
 */
export interface LoginResponse {
  /**
   * Which account signed in.
   *
   * V0.x has one account, but the response carries it so that a client built against this API keeps working when there are several.
   */
  account_id: AccountId;
  /**
   * When the token stops being accepted.
   */
  expires_at: number;
  /**
   * Bearer token for subsequent requests.
   */
  token: string;
}
