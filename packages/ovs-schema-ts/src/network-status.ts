// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * The machine's wired network interfaces, and any change on trial.
 *
 * The owner, 2026-10-19: "Network Settings via Admin GUI, also ob DHCP/Static inkl IP Subnetz Gateway DNS etc." — and on the safety net, five minutes to confirm before the previous settings come back by themselves.
 */
export interface NetworkStatus {
  /**
   * Every wired interface, in name order. Not the loopback, not a VPN and not a radio: none of those is configured here.
   */
  interfaces: NetworkInterface[];
  /**
   * The nameservers the machine uses now, whichever interface set them.
   */
  nameservers: string[];
  /**
   * The last change and where it got, if one has ever been made here.
   */
  trial?: NetworkTrial | null;
  /**
   * Why nothing can be changed from here, where nothing can: a machine whose network is not run by `ifupdown` is changed where it is run.
   */
  unavailable?: string | null;
}
/**
 * One wired interface, as it is and as it is configured.
 */
export interface NetworkInterface {
  /**
   * The IPv4 addresses it has now, as `address/prefix`.
   */
  addresses: string[];
  /**
   * The configured static settings, where the method is `static`.
   */
  configured?: NetworkSettings | null;
  /**
   * The default gateway through it now, if there is one.
   */
  gateway?: string | null;
  /**
   * Whether a cable with something at the other end is plugged in.
   */
  link: boolean;
  /**
   * Its hardware address.
   */
  mac: string;
  /**
   * How it is configured to come up: `dhcp`, `static`, or `none` where the configuration does not mention it.
   */
  method: string;
  /**
   * Its name, such as `eno1`.
   */
  name: string;
  /**
   * The link's speed in megabits a second, where the driver says.
   */
  speed_mbps?: number | null;
}
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
/**
 * A change to the network, and how far it got.
 *
 * Written by the machine's own network helper, which runs apart from the server: it has to be able to put the previous settings back when nobody can reach the server any more.
 */
export interface NetworkTrial {
  /**
   * When it last wrote this, as UTC in ISO 8601.
   */
  at: string;
  /**
   * When the previous settings come back by themselves, in seconds since the epoch, while it is `waiting`.
   */
  deadline?: number | null;
  /**
   * What it is doing, or why it stopped, in a sentence.
   */
  detail: string;
  /**
   * Which change this is.
   */
  id: string;
  /**
   * The interface it changed.
   */
  interface: string;
  /**
   * `applying`, `waiting` for somebody to keep it, then `kept`, `restored`, `failed` or `refused`.
   */
  stage: string;
  /**
   * What it set, in a few words: `static 10.110.0.50/24` or `dhcp`.
   */
  summary: string;
}
