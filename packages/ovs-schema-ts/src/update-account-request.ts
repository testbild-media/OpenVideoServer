// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * What an account is allowed to do.
 *
 * Ordered by authority, so `>=` is a meaningful test and [`Role::may_administer`] is the only place that decides what that means.
 */
export type Role = 'operator' | 'admin';

/**
 * Changes an account.
 *
 * Every field is optional and an absent one means "leave this alone". The password is not here: it has its own request, because changing one is a different act with different authority behind it.
 */
export interface UpdateAccountRequest {
  /**
   * A new role.
   */
  role?: Role | null;
  /**
   * A new login name.
   */
  username?: string | null;
}
