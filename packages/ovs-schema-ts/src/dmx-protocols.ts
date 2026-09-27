// Generated from the OpenVideoServer shared schema. Do not edit by hand.
// Regenerate with: npm run generate --workspace @ovs/schema
/**
 * Which DMX-over-IP protocols reach this show.
 *
 * Two switches rather than one, because a rig is not one thing: a venue's house desk may speak sACN while a visiting console brings Art-Net, and an operator who wants one of them to stop wants the other to keep going (owner, 2026-09-23).
 *
 * **Switched off means it does not drive anything, not that it is not listened for.** The sockets stay open and the status keeps saying what is on the wire, exactly as `MasterLive::dmx_enabled` does for both at once: an operator taking a protocol off the show still needs to see that the desk is sending, or they cannot tell "I switched it off" from "the cable is out".
 *
 * A server setting rather than a show's. Which protocols a building speaks is a fact about the building, and carrying it in a show would mean a show taken to another venue arrives with the wrong answer.
 */
export interface DmxProtocols {
  /**
   * Whether Art-Net drives this show.
   */
  artnet: boolean;
  /**
   * Whether sACN drives this show.
   */
  sacn: boolean;
}
