// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * An error returned by any endpoint.
 *
 * A single shape for every failure, so a client has one thing to handle rather than one per endpoint.
 */
export interface ApiError {
  /**
   * Short machine-readable code, for example `show_not_found`.
   */
  code: string;
  /**
   * Field-level problems, when the request failed validation.
   */
  details?: ValidationError[];
  /**
   * Human-readable description, suitable for showing an operator.
   */
  message: string;
}
/**
 * A single validation problem, located by a dotted path into the model.
 */
export interface ValidationError {
  /**
   * Human-readable description of what is wrong.
   */
  message: string;
  /**
   * Dotted path to the offending value, for example `layers[2].slots[0].index`.
   */
  path: string;
}
