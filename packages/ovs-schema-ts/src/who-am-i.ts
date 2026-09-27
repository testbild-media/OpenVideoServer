// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * AccountId (ULID, 26 characters)
 */
export type AccountId = string;
/**
 * What an account is allowed to do.
 *
 * Ordered by authority, so `>=` is a meaningful test and [`Role::may_administer`] is the only place that decides what that means.
 */
export type Role = 'operator' | 'admin';

/**
 * Who a request is acting as.
 *
 * What the interface asks for before it draws: a page that offered user management to an operator would be a page whose buttons the server refuses.
 */
export interface WhoAmIResponse {
  /**
   * The account this session belongs to.
   */
  account: AccountSummary;
}
/**
 * One account, as everything outside the server sees it.
 *
 * **No password hash.** The field exists in storage and has no business on a wire: an interface that has it is an interface somebody can be talked into showing, and a hash is worth attacking offline.
 */
export interface AccountSummary {
  /**
   * Stable identifier.
   */
  account_id: AccountId;
  /**
   * When it was created, in milliseconds since the Unix epoch.
   */
  created_at: number;
  /**
   * Whether this is the only account that can administer the server.
   *
   * Sent rather than worked out by the reader, because the rule belongs to the server: an interface that counted admins itself would grey out the wrong button the moment the rule changed, or offer one the server then refuses.
   */
  last_administrator: boolean;
  /**
   * When it last signed in, if it ever has.
   */
  last_login_at?: number | null;
  /**
   * What this account may do.
   */
  role: Role;
  /**
   * Login name.
   */
  username: string;
}
