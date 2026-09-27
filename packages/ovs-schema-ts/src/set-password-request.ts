// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Sets an account's password.
 *
 * **`current` is what separates the two ways in.** Somebody changing their own password proves they know it; an administrator resetting somebody else's does not have it and does not need it. A request that leaves it out for itself is refused, which is what stops a borrowed unlocked screen from becoming a stolen account.
 */
export interface SetPasswordRequest {
  /**
   * The password in use now, when changing one's own.
   */
  current?: string | null;
  /**
   * The new password.
   */
  password: string;
}
