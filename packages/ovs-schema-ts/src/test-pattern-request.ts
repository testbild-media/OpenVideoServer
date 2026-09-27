// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * PieceId (ULID, 26 characters)
 */
export type PieceId = string;
/**
 * Which surfaces are showing the service test pattern.
 *
 * Two different questions, which is why this is not a switch.
 *
 * **Composition** asks "is the show reaching the walls, and is my mapping right?" The pattern becomes the composition, so the pieces cut it up and each wall shows its own region of it: a wall showing the middle of the pattern is mapped to the middle of the composition.
 *
 * **Outputs** asks "is this cable reaching this wall, and in what mode?" Each output draws its own pattern at its own size and rate, with its own name on it, and the mapping is bypassed entirely. An engineer reads the answer off the wall.
 *
 * Doing both at once would answer neither, so this is one setting with three values rather than two switches.
 */
export type TestPatternMode = 'OFF' | 'COMPOSITION' | 'OUTPUTS' | 'PIECES';

/**
 * Which surfaces should show the commissioning test pattern.
 */
export interface TestPatternRequest {
  /**
   * The piece drawn as the chosen one, when the mode is `PIECES`.
   *
   * One, never several: the pattern is what somebody is looking at while they line one rectangle up, and two highlighted rectangles answer a question nobody asked. Absent draws them all as unchosen.
   */
  highlighted?: PieceId | null;
  /**
   * The composition, every output, or neither.
   *
   * Not a switch, because the two patterns answer different questions: one tests the mapping, the other tests the cabling and the mode (`TestPatternMode` says which is which).
   */
  mode: TestPatternMode;
}
