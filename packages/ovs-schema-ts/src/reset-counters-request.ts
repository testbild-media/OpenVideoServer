// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Which of the operator-visible counters to put back to zero.
 *
 * The three numbers on a System screen that only mean anything relative to when somebody last looked. They are separate because they are tapped separately: an operator clearing the dropped-frame count after fixing a cable has not decided that the evening's decoder errors never happened.
 */
export type CounterGroup = 'ALL' | 'DROPPED_FRAMES' | 'LATE_FRAMES' | 'DECODE_ERRORS';

/**
 * Which counters an operator has asked to put back to zero.
 */
export interface ResetCountersRequest {
  /**
   * The group to clear. Everything, unless something narrower is named.
   */
  group?: CounterGroup & string;
}
