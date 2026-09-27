// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * What to do to the machine.
 *
 * Both of these end the show. The API does not ask for confirmation — that is the console's job, in front of the operator who is about to press it — but it does insist on being told which one, because a single "power" endpoint that defaulted to something would eventually default to the wrong thing.
 */
export type PowerAction =
  | {
      action: 'reboot';
    }
  | {
      action: 'shutdown';
    };
