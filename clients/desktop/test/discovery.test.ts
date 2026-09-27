// Reading what a server says about itself.
//
// The network half of this was proved against the real thing: a throwaway
// `ovsd` on the reference machine, browsed from a second one, which found it
// with its name, port, version and `setup=yes`. That run also found the bug
// worth remembering — a multicast query leaves by *one* adapter unless you say
// otherwise, and on that machine it left by the VPN adapter rather than the one
// the server was on.
//
// What is covered here is the half a test can own: the wire format. A packet
// that is malformed, truncated, crafted to loop, or simply somebody else's is
// ordinary traffic on the group this listens to, and none of it may throw.

import { describe, expect, it } from 'vitest';

import { browse, wire } from '../src/main/discovery.js';

const SERVICE = '_openvideoserver._tcp.local';

/** Builds a response the way a responder does, for reading back. */
function response(options: {
  instance?: string;
  host?: string;
  port?: number;
  address?: number[];
  text?: string[];
  omit?: 'srv' | 'txt' | 'a' | 'ptr';
}): Buffer {
  const instance = options.instance ?? `OpenVideoServer.${SERVICE}`;
  const host = options.host ?? 'openvideoserver.local';
  const port = options.port ?? 7420;
  const address = options.address ?? [192, 0, 2, 10];
  const text = options.text ?? ['name=OpenVideoServer', 'version=0.0.1', 'api=1', 'setup=no'];

  const records: Buffer[] = [];

  if (options.omit !== 'ptr') {
    records.push(record(SERVICE, 12, wire.encodeName(instance)));
  }
  if (options.omit !== 'srv') {
    const data = Buffer.concat([
      Buffer.from([0, 0, 0, 0]), // priority and weight
      portOf(port),
      wire.encodeName(host),
    ]);
    records.push(record(instance, 33, data));
  }
  if (options.omit !== 'txt') {
    const strings = text.map((entry) => {
      const bytes = Buffer.from(entry, 'utf8');
      return Buffer.concat([Buffer.from([bytes.length]), bytes]);
    });
    records.push(record(instance, 16, Buffer.concat(strings)));
  }
  if (options.omit !== 'a') {
    records.push(record(host, 1, Buffer.from(address)));
  }

  const header = Buffer.alloc(12);
  header.writeUInt16BE(0, 0);
  header.writeUInt16BE(0x8400, 2); // an authoritative answer
  header.writeUInt16BE(0, 4); // no questions
  header.writeUInt16BE(records.length, 6);
  return Buffer.concat([header, ...records]);
}

function record(name: string, type: number, data: Buffer): Buffer {
  const labels = wire.encodeName(name);
  const head = Buffer.alloc(10);
  head.writeUInt16BE(type, 0);
  head.writeUInt16BE(1, 2); // class IN
  head.writeUInt32BE(120, 4); // time to live
  head.writeUInt16BE(data.length, 8);
  return Buffer.concat([labels, head, data]);
}

function portOf(port: number): Buffer {
  const buffer = Buffer.alloc(2);
  buffer.writeUInt16BE(port, 0);
  return buffer;
}

describe('the question this client asks', () => {
  it('is one standard query for the service, and nothing else', () => {
    const packet = wire.question(SERVICE, 12);
    expect(packet.readUInt16BE(2)).toBe(0); // a query, not an answer
    expect(packet.readUInt16BE(4)).toBe(1); // one question
    expect(packet.readUInt16BE(6)).toBe(0); // and no answers of our own

    // It browses and never answers: nothing here claims a name or replies to
    // anybody, so it cannot collide with a responder already on the machine.
    const { name, next } = wire.decodeName(packet, 12);
    expect(name).toBe(SERVICE);
    expect(packet.readUInt16BE(next)).toBe(12); // PTR
    expect(packet.readUInt16BE(next + 2)).toBe(1); // class IN
  });
});

describe('reading a server out of a response', () => {
  it('takes the name, the address, the port and what it says about itself', () => {
    const [server] = wire.read(response({}));
    expect(server).toEqual({
      name: 'OpenVideoServer',
      address: '192.0.2.10',
      port: 7420,
      version: '0.0.1',
      needsSetup: false,
      seenAt: expect.any(Number),
    });
  });

  it('knows a server that has never been set up', () => {
    // The difference between offering a password box nothing will answer and
    // offering to commission the machine.
    const [server] = wire.read(
      response({ text: ['name=Rack 1', 'version=0.2.0', 'setup=yes'] }),
    );
    expect(server?.needsSetup).toBe(true);
    expect(server?.name).toBe('Rack 1');
  });

  it('drops anything it cannot reach rather than showing half of it', () => {
    // A responder may answer a PTR question with only the pointer and leave the
    // rest for a follow-up. An entry with a name and no address is a row an
    // operator cannot click.
    expect(wire.read(response({ omit: 'srv' }))).toEqual([]);
    expect(wire.read(response({ omit: 'a' }))).toEqual([]);
    expect(wire.read(response({ omit: 'ptr' }))).toEqual([]);

    // Without the text it is still reachable, so it is still offered — under
    // the instance name, which is the one thing there is.
    const [server] = wire.read(response({ omit: 'txt' }));
    expect(server?.address).toBe('192.0.2.10');
    expect(server?.name).toBe('OpenVideoServer');
    expect(server?.version).toBeNull();
  });

  it('ignores every other service on the group', () => {
    // A printer, a television, a phone. This listens to a group that carries
    // all of them and most of what arrives is somebody else's.
    const printer = Buffer.concat([
      Buffer.alloc(12),
      record('_ipp._tcp.local', 12, wire.encodeName('Office.  _ipp._tcp.local')),
    ]);
    printer.writeUInt16BE(1, 6);
    expect(wire.read(printer)).toEqual([]);
  });
});

describe('a packet that is not what it claims', () => {
  it('never throws, whatever is in it', () => {
    const good = response({});
    // Every truncation of a real packet, which is what a lost fragment or a
    // hostile sender produces.
    for (let length = 0; length < good.length; length += 1) {
      expect(() => wire.read(good.subarray(0, length))).not.toThrow();
    }
    expect(() => wire.read(Buffer.alloc(0))).not.toThrow();
    expect(() => wire.read(Buffer.from([0xff, 0xff, 0xff]))).not.toThrow();
  });

  it('does not follow a compression pointer into a loop', () => {
    // **The one genuinely dangerous shape.** A name may end in a pointer to an
    // earlier offset, and one that points at itself is four bytes that hang a
    // client for ever. Anything on the segment can send it.
    const looping = Buffer.alloc(20);
    looping.writeUInt16BE(0, 4);
    looping.writeUInt16BE(1, 6);
    // A name at offset 12 that points back to offset 12.
    looping.writeUInt8(0xc0, 12);
    looping.writeUInt8(12, 13);

    const started = Date.now();
    expect(() => wire.decodeName(looping, 12)).not.toThrow();
    expect(() => wire.read(looping)).not.toThrow();
    expect(Date.now() - started).toBeLessThan(1_000);
  });

  it('reads a name that legitimately uses a pointer', () => {
    // The same mechanism, used as intended: a real response compresses every
    // repeated suffix, so refusing pointers outright would read nothing.
    const message = Buffer.alloc(64);
    const first = wire.encodeName('openvideoserver.local');
    first.copy(message, 12);
    const at = 12 + first.length;
    message.writeUInt8(4, at);
    message.write('rack', at + 1, 'utf8');
    message.writeUInt8(0xc0, at + 5);
    message.writeUInt8(12, at + 6);

    expect(wire.decodeName(message, at).name).toBe('rack.openvideoserver.local');
  });

  it('reads the text it is given and ignores what it cannot', () => {
    const properties = wire.readText(
      Buffer.concat([
        Buffer.from([7]),
        Buffer.from('name=hi'),
        Buffer.from([4]),
        Buffer.from('bare'), // no '=' at all
        Buffer.from([9]),
        Buffer.from('a=b=c=d=e'), // several, and only the first splits
      ]),
    );
    expect(properties.get('name')).toBe('hi');
    expect(properties.has('bare')).toBe(false);
    expect(properties.get('a')).toBe('b=c=d=e');
  });
});

describe('a sweep that finds nothing', () => {
  it('still reports, so the screen can stop saying it is looking', async () => {
    // **The half that made the client silent.** Publishing only on change meant
    // a network with no server on it produced no event at all, and the connect
    // screen could not tell "still looking" from "looked, and there is
    // nothing" — which the owner met on 2026-09-24 as a panel that said
    // nothing whatsoever.
    //
    // What is asserted is that an answer arrives, not what is in it: this runs
    // on a developer machine, a build server and a venue network, and on one of
    // those there may well be a real server listening.
    const heard = await new Promise<number>((resolve) => {
      const waiting = setTimeout(() => {
        browser.stop();
        resolve(-1);
      }, 8_000);
      const browser = browse((servers) => {
        clearTimeout(waiting);
        browser.stop();
        resolve(servers.length);
      });
    });

    expect(heard).toBeGreaterThanOrEqual(0);
  }, 12_000);
});
