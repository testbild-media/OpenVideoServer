// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * A typed parameter value for an effect or generator instance.
 */
export type ParameterValue =
  | {
      type: 'bool';
      value: boolean;
    }
  | {
      type: 'int';
      value: number;
    }
  | {
      type: 'float';
      value: number;
    }
  | {
      type: 'text';
      value: string;
    }
  | {
      type: 'color';
      /**
       * @minItems 4
       * @maxItems 4
       */
      value: [number, number, number, number];
    }
  | {
      type: 'choice';
      value: string;
    };

/**
 * Changes the live values on one effect instance.
 *
 * A live value sits on top of what the show configured for that instance and is never written back to it: an operator can push an effect during a number and the show still opens tomorrow at the value it was built with . Editing what it opens with is a show endpoint; this is not a shortcut to one.
 *
 * Only the keys sent are moved, for the same reason the fields of [`UpdateLayerRequest`] are optional: a client moving one control must not have to send the others, and one that sent a default for a control it was not thinking about would change something on stage.
 */
export interface UpdateEffectRequest {
  /**
   * Live values to put on top of the configured ones, by parameter key.
   *
   * Checked against the effect's manifest exactly as a configured value is: a value of the wrong type, one outside its declared range, and one for a parameter the effect never declared are all refused, and a refused request changes nothing.
   */
  overrides?: {
    [k: string]: ParameterValue;
  };
  /**
   * Whether to drop the live values this instance already had, first.
   *
   * How an operator puts an effect back the way the show was built. It needs its own field because "no live value" is an absence: there is no value a client could send that means "stop overriding this", and sending the configured value instead would only look the same until somebody edited the show.
   */
  reset?: boolean;
}
