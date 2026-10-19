// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * What a slot plays.
 *
 * New source types are added as variants here (capture, DeckLink, NDI and HTML as expected future types). Everything downstream of a source treats them identically, so adding one must not require changes to the layer, composition, piece or output model.
 */
export type SourceRef =
  | {
      kind: 'media';
      /**
       * Logical media identity.
       */
      media_item_id: MediaItemId;
    }
  | {
      /**
       * This instance's own identity.
       *
       * What a patched fixture points at and what a live value is keyed by, the same way an effect instance's is.
       *
       * Optional on the wire, so that a client need not invent one and a show written before generators had one still opens. The server gives it a real identity **when the show is saved**, because a patch entry points at this instance and lives in the same document: an identity that existed only in the loaded state would be minted again on the next open, and every fixture patched against the previous one would be pointing at a generator that no longer exists. A document already on disk without one gains it when it is opened (`Show::assign_missing_ids`).
       *
       * Nil rather than a fresh one as the `serde` default, because a default is written into the published schema and a schema that changes on every build can never be checked against a committed one — and left out of the wire form entirely when it is nil, so that "no identity" never reads as "the identity of twenty-six zeroes".
       */
      instance_id?: GeneratorInstanceId;
      kind: 'generator';
      /**
       * Whether an administrator has locked this generator in its slot.
       *
       * A locked generator cannot be taken out of its slot or replaced by anybody, and only an administrator can lock or unlock one; see [`crate::control`]. Left out of a document when it is not locked.
       */
      locked?: boolean;
      /**
       * Generator parameters as defined by its manifest.
       */
      parameters?: {
        [k: string]: ParameterValue;
      };
      /**
       * Stable generator type identifier, for example `ovs.solid_colour`.
       */
      type_id: string;
      /**
       * Manifest version of that generator.
       */
      version: number;
    }
  | {
      kind: 'ndi';
      /**
       * The full NDI source name, as the network announces it.
       *
       * `MACHINE (Source Name)` is the shape NDI uses, and it is carried whole rather than split: the two halves are one identifier and a client that took them apart would have to put them back together exactly to find the source again.
       */
      name: string;
    };
/**
 * MediaItemId (ULID, 26 characters)
 */
export type MediaItemId = string;
/**
 * GeneratorInstanceId (ULID, 26 characters)
 */
export type GeneratorInstanceId = string;
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
 * Result of assigning or clearing a slot.
 */
export interface AssignSlotResponse {
  /**
   * What the slot held before, or `None` when it was empty.
   *
   * Returned rather than assumed so that a client can say what it replaced after the fact. The asking happens before the request is sent, in front of the operator; this is the record of what was actually there, which is not always what the client thought.
   */
  replaced?: SourceRef | null;
  /**
   * When the server recorded the edit.
   */
  updated_at: number;
}
