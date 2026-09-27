// A query leaves by every adapter, not six times by the last one.
//
// The bug this exists for, found on 2026-09-27: `setMulticastInterface` is a
// setsockopt and takes effect at once, while `socket.send` queues the datagram
// and returns. So a loop that sets and sends, six times, set the interface six
// times and *then* transmitted six datagrams — every one of them out of
// whichever interface was set last.
//
// On the owner's machine the last of six was `vEthernet (WSL)`, so the server
// never saw a question, while the client heard multicast on the real adapter
// perfectly well. On the machine this was written on there are two adapters and
// the working one is last, which is why it looked correct there.
//
// The property is an ordering, so the test is about ordering: every send must
// be preceded by the interface it belongs to, with nothing in between.

import { describe, expect, it } from 'vitest';

import { askEach, type Sender } from '../src/main/discovery.js';

/**
 * A socket that transmits when libuv would, which is later.
 *
 * **This is the whole test.** The first version of this recorded which
 * interface was set at the moment `send` was *called*, and it passed against
 * the broken loop — because in that loop the setsockopt and the call are
 * adjacent and only the transmission is late. A fake that cannot be wrong in
 * the way the real thing was wrong proves nothing.
 *
 * So `send` records nothing when called. It records, when the datagram
 * actually goes, whichever interface is in effect at that moment — which is
 * exactly what the operating system does with it.
 */
function recorder(options: { throwOn?: string; failSend?: string } = {}) {
  const calls: string[] = [];
  let current: string | null = null;
  const socket: Sender = {
    setMulticastInterface(address) {
      if (options.throwOn === address) {
        throw new Error(`no multicast on ${address}`);
      }
      calls.push(`interface:${address}`);
      current = address;
    },
    send(_packet, _offset, _length, _port, _address, done) {
      const failing = options.failSend !== undefined && options.failSend === current;
      setTimeout(() => {
        calls.push(`send:${current ?? 'none'}`);
        // A send that fails still calls back, which is what keeps the chain
        // moving. This is the case that would hang if it did not.
        done?.(failing ? new Error('unreachable') : null);
      }, 0);
    },
  };
  return { calls, socket };
}

const packet = Buffer.from([1, 2, 3]);
const SIX = [
  '172.31.1.2',
  '10.11.0.101',
  '2.0.0.10',
  '10.100.0.10',
  '172.18.128.1',
  '172.17.96.1',
];

const settled = (run: (done: () => void) => void): Promise<void> =>
  new Promise((resolve) => run(() => resolve()));

describe('asking out of every interface', () => {
  it('sends once per interface, each after its own', async () => {
    // **The regression.** Before this, the six interfaces were set and then six
    // datagrams went out, all of them attributed to the last one.
    const { calls, socket } = recorder();
    await settled((done) => askEach(socket, packet, SIX, done));

    expect(calls).toEqual([
      'interface:172.31.1.2',
      'send:172.31.1.2',
      'interface:10.11.0.101',
      'send:10.11.0.101',
      'interface:2.0.0.10',
      'send:2.0.0.10',
      'interface:10.100.0.10',
      'send:10.100.0.10',
      'interface:172.18.128.1',
      'send:172.18.128.1',
      'interface:172.17.96.1',
      'send:172.17.96.1',
    ]);
  });

  it('carries on past an interface that cannot be set', async () => {
    // A virtual adapter that cannot carry multicast is the common case on a
    // developer's machine, and the one after it may be the venue's.
    const { calls, socket } = recorder({ throwOn: '2.0.0.10' });
    await settled((done) => askEach(socket, packet, SIX, done));

    expect(calls).not.toContain('interface:2.0.0.10');
    expect(calls).toContain('send:10.11.0.101');
    expect(calls).toContain('send:172.17.96.1');
    expect(calls.filter((call) => call.startsWith('send:'))).toHaveLength(5);
  });

  it('carries on past a send that fails', async () => {
    // The one that would hang rather than misbehave: a failed send still calls
    // back, and the chain is what the next interface is waiting on.
    const { calls, socket } = recorder({ failSend: '10.11.0.101' });
    await settled((done) => askEach(socket, packet, SIX, done));

    expect(calls.filter((call) => call.startsWith('send:'))).toHaveLength(6);
  });

  it('finishes when there is nothing to ask from', async () => {
    const { calls, socket } = recorder();
    await settled((done) => askEach(socket, packet, [], done));
    expect(calls).toEqual([]);
  });
});
