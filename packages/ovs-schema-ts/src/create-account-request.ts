// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * What an account is allowed to do.
 *
 * Ordered by authority, so `>=` is a meaningful test and [`Role::may_administer`] is the only place that decides what that means.
 */
export type Role = 'operator' | 'admin';

/**
 * Creates an account.
 *
 * Two callers, one shape. On a server with no accounts this is first-run setup and needs no session; on one that has them it is an administrator adding a colleague. What differs is the authority required, not the request.
 */
export interface CreateAccountRequest {
  /**
   * Plain password.
   */
  password: string;
  /**
   * What the account may do.
   *
   * Absent is [`Role::Operator`](ovs_types::account::Role::Operator), which is the safe half: an account created without anybody saying is an account nobody decided to trust with the machine. First-run setup is the exception and makes an administrator whatever this says, because a server whose only account cannot administer it is a server nobody can finish setting up.
   */
  role?: Role | null;
  /**
   * Login name.
   */
  username: string;
}
