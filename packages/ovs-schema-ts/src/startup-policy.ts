// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * What a server loads after a reboot.
 *
 * This is server configuration rather than show data, but it names a show, so it lives beside the show model.
 *
 * Three modes, with `LastActive` as the default (owner decision Q6).
 */
export type StartupPolicy =
  | {
      kind: 'last_active';
    }
  | {
      kind: 'pinned';
      /**
       * The show to open.
       */
      show_id: ShowId;
    }
  | {
      kind: 'none';
    };
/**
 * ShowId (ULID, 26 characters)
 */
export type ShowId = string;
