// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * What a parameter takes, and where it starts.
 */
export type ParameterType =
  | {
      /**
       * What it is when nobody has set it.
       */
      default: number;
      kind: 'number';
      /**
       * Highest value.
       */
      max: number;
      /**
       * Lowest value.
       */
      min: number;
    }
  | {
      /**
       * What it is when nobody has set it.
       *
       * @minItems 4
       * @maxItems 4
       */
      default: [number, number, number, number];
      kind: 'colour';
    }
  | {
      /**
       * What it is when nobody has set it.
       */
      default: boolean;
      kind: 'toggle';
    }
  | {
      /**
       * What it says when nobody has set it.
       */
      default: string;
      kind: 'text';
      /**
       * The most characters it may hold, counted as `char`s rather than bytes.
       */
      max_chars: number;
    }
  | {
      /**
       * Every choice, in the order to offer them.
       */
      choices: ParameterChoice[];
      /**
       * The `id` of the one it is when nobody has set it.
       */
      default: string;
      kind: 'choice';
    };

/**
 * Every generator this build publishes.
 */
export interface GeneratorCatalogueResponse {
  /**
   * The generators.
   */
  generators: Generator[];
}
/**
 * One published generator.
 */
export interface Generator {
  /**
   * One line describing what it draws.
   */
  description: string;
  /**
   * What a source list shows.
   */
  name: string;
  /**
   * Its parameters, in the order the manifest declares them.
   */
  parameters: ModuleParameter[];
  /**
   * Stable type identifier, for example `ovs.solid_colour`.
   */
  type_id: string;
  /**
   * Version. A new parameter is a new version, never a changed one.
   */
  version: number;
}
/**
 * One parameter a generator or effect declares.
 */
export interface ModuleParameter {
  /**
   * The key it is stored under in a show.
   */
  key: string;
  /**
   * What to show beside it.
   */
  name: string;
  /**
   * What it takes.
   */
  parameter_type: ParameterType;
}
/**
 * One entry in a [`ParameterType::Choice`] list.
 */
export interface ParameterChoice {
  /**
   * The stable identifier a show stores. Never shown to an operator.
   */
  id: string;
  /**
   * What to show beside it.
   */
  name: string;
}
