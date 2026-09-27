// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * A message from a client.
 */
export type ClientMessage =
  | {
      type: 'resynchronise';
    }
  | {
      /**
       * Echoed back in the pong.
       */
      nonce: number;
      type: 'ping';
    };
