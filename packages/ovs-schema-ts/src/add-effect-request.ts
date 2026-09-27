// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Adds an effect to the end of a layer's or a slot's chain.
 *
 * **Appended, never replacing.** An effect dropped on a slot or a layer stacks on top of what is there (owner, 2026-09-23): a chain is an order, and dropping a second effect is not a request to undo the first.
 *
 * No parameters. Every one of them has a default in the effect's own manifest, and a client that sent copies of those defaults would be writing down values nobody chose — which then stop following the manifest the day it changes. They are set afterwards, where they are set today.
 */
export interface AddEffectRequest {
  /**
   * Stable effect type identifier, for example `ovs.blur`.
   */
  type_id: string;
  /**
   * The manifest version to configure it against.
   *
   * Sent rather than assumed to be the newest. Published manifests are immutable, and an effect whose version was chosen by the server would change behaviour under a show that never asked it to.
   */
  version: number;
}
