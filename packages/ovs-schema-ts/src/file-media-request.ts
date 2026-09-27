// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Renames one media item, or moves it to another folder, or both.
 *
 * **Neither touches the file.** A display name is a label and a folder is a label; the asset keeps its object key and the content hash it is found by, so a show that plays this item goes on playing it (owner, 2026-09-23: "organising media must not move the real files").
 *
 * A field left out means "leave this alone", so a rename need not send a folder and a move need not send a name.
 */
export interface FileMediaRequest {
  /**
   * The name the library shows.
   */
  display_name?: string | null;
  /**
   * Where it is filed. Empty is the root.
   */
  folder?: string | null;
}
