// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * How a channel's bytes become a value.
 */
export type ChannelEncodingInfo =
  | {
      encoding: 'ranged8';
      /**
       * The value at `255`.
       */
      max: number;
      /**
       * The value at `0`.
       */
      min: number;
    }
  | {
      encoding: 'ranged16';
      /**
       * The value at `65535`.
       */
      max: number;
      /**
       * The value at `0`.
       */
      min: number;
    }
  | {
      encoding: 'index8';
    }
  | {
      encoding: 'switch';
      /**
       * The lowest value that counts as on.
       */
      threshold: number;
    }
  | {
      /**
       * The bands, lowest first.
       */
      bands: ChannelBand[];
      encoding: 'stepped';
    };
/**
 * What kind of thing a personality can drive.
 */
export type PersonalityTarget = 'master' | 'layer' | 'effect' | 'generator';

/**
 * Every personality this build publishes.
 */
export interface FixtureCatalogueResponse {
  /**
   * The personalities, in the order this build publishes them.
   */
  fixtures: Personality[];
}
/**
 * One published personality.
 */
export interface Personality {
  /**
   * Its channels, in address order.
   */
  channels: PersonalityChannel[];
  /**
   * How many channels an instance occupies.
   */
  footprint: number;
  /**
   * What a console's personality list shows.
   */
  name: string;
  /**
   * What a patch entry using it must point at.
   */
  target: PersonalityTarget;
  /**
   * Stable type identifier, for example `ovs.master`.
   */
  type_id: string;
  /**
   * Always one: there is one personality of each type. A patch entry still carries the number, so it is still served.
   */
  version: number;
}
/**
 * One channel of a personality.
 */
export interface PersonalityChannel {
  /**
   * The stable machine name of what it controls, for example `layer_opacity`.
   */
  control: string;
  /**
   * How its bytes become a value.
   */
  encoding: ChannelEncodingInfo;
  /**
   * What to send to put this channel at rest.
   *
   * Raw, in the units the channel is sent in: the whole sixteen-bit number for a wide channel, one byte for a narrow one. A console homes a fixture by sending numbers rather than by understanding them.
   *
   * Zero still means zero everywhere else (owner decision Q15). This is not a neutral value the server substitutes for one; it is what a desk should put in its own home or default preset so a freshly patched layer is visible and its normal size instead of a wall of zeros.
   */
  home: number;
  /**
   * Whether it fires on a rising edge rather than holding a value.
   */
  momentary: boolean;
  /**
   * What a console's channel list shows.
   */
  name: string;
  /**
   * One-based offset within the fixture.
   */
  offset: number;
  /**
   * How many channels it occupies.
   */
  width: number;
}
/**
 * One band of a stepped channel.
 *
 * The part a console programmer cannot work out from a channel list, and the part a client's patch editor needs to draw the fader's labels.
 */
export interface ChannelBand {
  /**
   * Lowest DMX value in this band.
   */
  from: number;
  /**
   * Highest DMX value in this band.
   */
  to: number;
  /**
   * What this band selects, spelled as the rest of the API spells it.
   */
  value: string;
}
