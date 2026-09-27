// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * The server's answer to a hello.
 *
 * One flat set of outcomes rather than "accepted or rejected, and separately why". Nothing downstream acts on the grouping — a client has to do something different for each specific outcome, update, sign in again or wait — and a tag inside a tag is a shape a generated client cannot describe faithfully, which is how a contract starts drifting from the code it was generated from.
 */
export type ServerHello =
  | {
      /**
       * Protocol version in use.
       */
      protocol_version: number;
      result: 'accepted';
      /**
       * Server build version, for diagnostics.
       */
      server_version: string;
    }
  | {
      /**
       * The version the client offered.
       */
      client_version: number;
      result: 'incompatible_protocol';
      /**
       * The version this server speaks.
       */
      server_version: number;
    }
  | {
      result: 'unauthenticated';
    }
  | {
      result: 'not_ready';
    };
