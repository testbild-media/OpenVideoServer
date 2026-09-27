// What the connect screen says when it found no servers.
//
// The owner installed the client on a second machine on 2026-09-27 and it found
// nothing, while the server's own log showed it announcing itself correctly on
// its real interface. One sentence covered every reason, so there was nothing
// to go on — and the reason that matters most, a firewall on the client's own
// machine, is invisible from everywhere except this count.
//
// Four branches picking the one sentence somebody reads when nothing is
// working. Each one has to lead somewhere different, or they may as well be the
// sentence they replaced.

import { describe, expect, it } from 'vitest';

import { whyEmpty } from '../src/shared/discovery-note.js';
import type { DiscoveryState } from '../src/main/discovery.js';

const working: DiscoveryState = {
  refused: null,
  joined: 1,
  interfaces: 1,
  asked: 4,
  heard: 12,
};

describe('why the list is empty', () => {
  it('names the error when the socket never opened', () => {
    const said = whyEmpty({ ...working, refused: 'EADDRINUSE: address already in use' });
    expect(said).toContain('EADDRINUSE');
    // And still says what to do, because a reason without a way forward is a
    // worse message than none.
    expect(said).toContain('address');
  });

  it('says so when no adapter could join the group', () => {
    // Three adapters and none of them joined: the machine has network and
    // cannot hear multicast on any of it.
    const said = whyEmpty({ ...working, interfaces: 3, joined: 0 });
    expect(said).toContain('3');
    expect(said.toLowerCase()).toContain('adapters');
  });

  it('blames this machine when nothing at all has been heard', () => {
    // **The one the owner hit.** The group carries every service on the
    // network, so hearing nothing after several questions is not a quiet
    // network — it is a machine that is not being allowed to hear.
    const said = whyEmpty({ ...working, heard: 0 });
    expect(said.toLowerCase()).toContain('firewall');
    expect(said).toContain('4');
  });

  it('blames the network when it heard plenty and no server', () => {
    const said = whyEmpty(working);
    expect(said.toLowerCase()).not.toContain('firewall');
    expect(said).toContain('12');
    expect(said.toLowerCase()).toContain('this network');
  });

  it('says it is still looking before it has asked twice', () => {
    // One question is not evidence of anything. A client that accused the
    // firewall a tenth of a second after starting would be wrong most of the
    // time, and wrong in a way that sends somebody into their network settings.
    expect(whyEmpty({ ...working, asked: 1, heard: 0 })).toBe('Looking…');
  });

  it('falls back to something useful when it knows nothing', () => {
    // A window that opened before the first answer arrived.
    expect(whyEmpty(undefined)).toContain('Type the address');
  });
});
