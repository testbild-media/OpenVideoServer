// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * What deleting a media item would affect.
 */
export interface MediaUsageResponse {
  /**
   * Names of shows that reference this item.
   *
   * Deleting media must warn where it is used rather than silently breaking an archived show.
   */
  used_by_shows: string[];
}
