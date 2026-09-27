// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * ShowId (ULID, 26 characters)
 */
export type ShowId = string;

/**
 * One entry in the show list.
 */
export interface ShowListEntry {
  /**
   * When it was created.
   */
  created_at: number;
  /**
   * Where it is filed in this server's list.
   *
   * Slash-separated, empty for the root. Not part of the document: where a show is filed is a fact about this server's list rather than about the show (owner, 2026-09-23).
   */
  folder?: string;
  /**
   * Stable identifier.
   */
  id: ShowId;
  /**
   * Whether this server build can open it.
   *
   * A show it cannot open is still listed, so it does not look deleted.
   */
  loadable: boolean;
  /**
   * Operator-visible name.
   */
  name: string;
  /**
   * Schema version the stored show uses.
   */
  schema_version: number;
  /**
   * When it was last saved.
   */
  updated_at: number;
}
