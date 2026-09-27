// Why the connect screen found no servers, in the words that lead somewhere.
//
// Its own module because `ui/app.ts` is the page's entry point and runs on
// import: a test could not have called this without starting a client. Four
// branches picking the one sentence an operator reads when nothing is working
// is the shape that gets one branch wrong and is never noticed.

import type { DiscoveryState } from '../main/discovery.js';

/**
 * Why the list is empty, in the words that lead somewhere.
 *
 * **Three machines, three answers.** A socket that never opened, a machine
 * whose inbound multicast is dropped, and a network with no server on it used
 * to read identically — "None found yet" — and the middle one is the common
 * case on a client that has just been installed, because the firewall has not
 * been told about it yet.
 *
 * The packet count is what tells them apart, and it counts packets for anybody:
 * the group carries every service on the network, so silence is the machine and
 * chatter without a server is the network.
 */
export function whyEmpty(state: DiscoveryState | undefined): string {
  if (state === undefined) {
    return 'None found yet. Type the address instead, or check the server is on this network.';
  }
  if (state.refused !== null) {
    return `This client could not listen for servers: ${state.refused}. Type the address instead.`;
  }
  if (state.interfaces > 0 && state.joined === 0) {
    return (
      'This client could not join the discovery group on any of this machine’s ' +
      `${state.interfaces} network adapters, so it cannot hear servers announce themselves. ` +
      'Type the address instead.'
    );
  }
  if (state.asked > 1 && state.heard === 0) {
    return (
      `Asked ${state.asked} times and heard nothing at all on this network — not even other ` +
      'services, which normally answer. Something on this machine is dropping it, usually the ' +
      'firewall: allow OpenVideoServer, or type the address instead.'
    );
  }
  if (state.asked > 1) {
    return (
      `Asked ${state.asked} times and heard ${state.heard} answers, none of them a server. ` +
      'Type the address instead, or check the server is on this network.'
    );
  }
  return 'Looking…';
}
