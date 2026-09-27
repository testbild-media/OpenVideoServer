// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * FixtureInstanceId (ULID, 26 characters)
 */
export type FixtureInstanceId = string;

/**
 * What a patch editor asks when it is about to add a fixture.
 */
export interface AddressSuggestionRequest {
  /**
   * How many channels the fixture takes.
   */
  footprint: number;
  /**
   * The fixture being re-addressed, if this is a move rather than an add.
   *
   * A fixture does not collide with where it already is, so without this an editor could not suggest the address a fixture already has.
   */
  moving?: FixtureInstanceId | null;
  /**
   * Which universe to look in.
   */
  universe: number;
}
