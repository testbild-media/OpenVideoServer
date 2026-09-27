// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * The folders the show list is organised into.
 *
 * Its own endpoint rather than a field on the list, because `GET /shows` answers with an array and this API is additive within a major version: a field may be added to an entry, but the shape of a response is not repurposed under a client that is already reading it.
 *
 * Sent whole rather than worked out from the shows, because an operator makes a folder in order to put a show in it: one derived from its contents could not exist between being made and being filled.
 */
export interface ShowFoldersResponse {
  /**
   * Every folder, in path order. The root is not in it.
   */
  folders?: string[];
}
