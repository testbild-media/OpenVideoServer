// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Settings for one interface: the request to change them, and what a static configuration holds.
 */
export interface NetworkSettings {
  /**
   * The IPv4 address, for `static`.
   */
  address?: string | null;
  /**
   * The default gateway, for `static`. Optional: a show network often has none.
   */
  gateway?: string | null;
  /**
   * The interface, such as `eno1`.
   */
  interface: string;
  /**
   * `dhcp` or `static`.
   */
  method: string;
  /**
   * Up to three nameservers, for `static`.
   */
  nameservers?: string[];
  /**
   * The subnet mask, as `255.255.255.0` or as a prefix length such as `24`.
   */
  netmask?: string | null;
}
