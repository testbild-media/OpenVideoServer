// Finding servers on the network, so nobody has to type an address.
//
// The owner, 2026-09-24: "Kann man noch etwas einbauen, dass ein Client direkt
// den Server findet… muss nicht jedes Mal die IP eingeben die man vergessen kann
// oder sich in anderen Netzwerken ändern kann wenn der Server mobil ist."
//
// # Why this is written out rather than installed
//
// This client has exactly one runtime dependency. A browse-only mDNS client is
// about a hundred and fifty lines of a wire format that has not changed since
// 1987, and the alternative pulls in a tree of packages to do it — on a machine
// that goes to venues and has to keep working for years. So: a socket, a query,
// and a parser for the four record types an answer is made of.
//
// It **browses and never answers**. Nothing here claims a name, defends one or
// replies to anybody else's question, so it cannot collide with a responder
// already on the machine and cannot put anything on the network beyond its own
// question.
//
// # What a server's announcement is worth
//
// Nothing, as authority. It carries a name, a port, a version and whether the
// machine has been set up — the things a person would otherwise read off a
// label — and no credential of any kind. A client that finds a server still has
// to sign in. Anything on the segment can send one of these, so a name in this
// list is a claim and not a fact, which is why the address is shown beside it.

import { createSocket, type Socket } from 'node:dgram';
import { networkInterfaces } from 'node:os';

/** The service OpenVideoServer answers to. */
const SERVICE = '_openvideoserver._tcp.local';

/**
 * How long to let the first answers arrive before saying what was found.
 *
 * A responder on a quiet local network answers in milliseconds. Three seconds is
 * for the one that is busy, and short enough that somebody watching the connect
 * screen sees the question answered rather than a panel that never settles.
 */
const SETTLE_MS = 3_000;

/** Where multicast DNS lives. */
const GROUP = '224.0.0.251';
const PORT = 5353;

/** Record types, as the wire numbers them. */
const TYPE_A = 1;
const TYPE_PTR = 12;
const TYPE_TXT = 16;
const TYPE_SRV = 33;

/** One server, as a list of them shows it. */
export interface FoundServer {
  /** The name the server calls itself, from its own announcement. */
  name: string;
  /** Where to reach it, ready to be used as an address. */
  address: string;
  /** The API port. */
  port: number;
  /** The build it is running, when it said. */
  version: string | null;
  /**
   * Whether it still needs its first account.
   *
   * A client showing one of these offers to set it up rather than a password
   * box nothing will answer — which is the difference between a server somebody
   * commissions from the sofa and one they need a keyboard for.
   */
  needsSetup: boolean;
  /** When this client last heard from it, in milliseconds since the epoch. */
  seenAt: number;
}

/**
 * What the browse is doing, for a list that is staying empty.
 *
 * **An empty list has three causes and they need different answers**, and until
 * 2026-09-27 the client showed the same sentence for all of them: a socket that
 * would not open, a machine whose inbound multicast is dropped, and a network
 * with no server on it. The owner hit the middle one on a freshly installed
 * client and there was nothing anywhere to say which it was.
 */
export interface DiscoveryState {
  /** Why the socket could not be opened or bound, or null when it is. */
  refused: string | null;
  /** How many interfaces joined the multicast group. */
  joined: number;
  /** How many were tried, which is every IPv4 address this machine has. */
  interfaces: number;
  /** How many queries have gone out since this browse started. */
  asked: number;
  /**
   * How many packets of any kind have arrived.
   *
   * **The one that names a firewall.** The group carries every service on the
   * network, so a machine that has heard nothing at all has not heard the
   * network; a machine that has heard plenty and found no server is on a
   * network with no server on it. Those are different problems and they were
   * the same sentence.
   */
  heard: number;
}

/** A browse that is running. */
export interface Browser {
  /** Asks again, for a list an operator has just opened. */
  refresh(): void;
  /** What it is doing, for a list that is staying empty. */
  state(): DiscoveryState;
  /** Stops listening and closes the socket. */
  stop(): void;
}

/**
 * Starts looking for servers, calling `onFound` each time the list changes.
 *
 * Never rejects and never throws at the caller: a network that filters
 * multicast, an interface that has just gone away and a machine with no network
 * at all are all ordinary, and the answer to every one of them is a list that
 * stays empty while the address box keeps working.
 */
export function browse(
  onFound: (servers: FoundServer[], state: DiscoveryState) => void,
): Browser {
  const found = new Map<string, FoundServer>();
  let socket: Socket | null = null;
  let timer: NodeJS.Timeout | null = null;
  let settle: NodeJS.Timeout | null = null;
  const state: DiscoveryState = {
    refused: null,
    joined: 0,
    interfaces: 0,
    asked: 0,
    heard: 0,
  };

  const publish = (): void => {
    const servers = [...found.values()].sort((a, b) => a.name.localeCompare(b.name));
    onFound(servers, { ...state });
  };

  const query = (): void => {
    const live = socket;
    if (live === null) {
      return;
    }
    const packet = question(SERVICE, TYPE_PTR);

    // **Once per interface, and that is not a detail.** Left to itself the
    // system sends a multicast datagram out of one adapter — whichever the
    // routing table prefers — and on a machine with a VPN, Hyper-V or WSL on it
    // that is not the one the venue is on.
    //
    // Measured on a machine carrying both a VPN adapter and the venue's: the
    // query left by the VPN, the server on the venue side never received it, and the
    // list stayed empty with nothing anywhere saying why. A laptop is the
    // normal machine for this client and a laptop is the normal machine to have
    // three adapters on.
    const addresses = interfaceAddresses();
    if (addresses.length === 0) {
      send(live, packet);
      state.asked += 1;
      return;
    }
    askEach(live, packet, addresses);
    state.asked += 1;
  };

  try {
    // `reuseAddr` is what lets this sit on 5353 beside a responder that is
    // already there — Bonjour on a Windows machine with Adobe or iTunes on it,
    // avahi on a Linux one. Without it this would refuse to start on exactly
    // the machines most likely to have a server to find.
    socket = createSocket({ type: 'udp4', reuseAddr: true });
  } catch (error) {
    state.refused = error instanceof Error ? error.message : String(error);
    return {
      refresh: () => undefined,
      state: () => ({ ...state }),
      stop: () => undefined,
    };
  }

  const live = socket;

  live.on('error', (error) => {
    // Closed under us, or a send that failed late. The list stops growing and
    // the address box is still there — but it is written down now, because a
    // socket that died is indistinguishable from a quiet network to everybody
    // except this line.
    state.refused = error instanceof Error ? error.message : String(error);
    try {
      live.close();
    } catch {
      // Already closed.
    }
  });

  live.on('message', (message) => {
    // Before the parse, and whatever the parse makes of it. The group carries
    // every service on this network, so most of these are for somebody else —
    // which is exactly what makes the count worth having: hearing the chatter
    // and finding no server is a different machine from hearing nothing at all.
    state.heard += 1;
    try {
      const servers = read(message);
      let changed = false;
      for (const server of servers) {
        const key = `${server.address}:${server.port}`;
        const before = found.get(key);
        found.set(key, server);
        if (
          before === undefined ||
          before.name !== server.name ||
          before.version !== server.version ||
          before.needsSetup !== server.needsSetup
        ) {
          changed = true;
        }
      }
      if (changed) {
        publish();
      }
    } catch {
      // A packet this parser cannot read is a packet for somebody else. The
      // group carries every service on the network, so most of them are.
    }
  });

  live.bind(PORT, () => {
    try {
      live.setMulticastTTL(255);
      const addresses = interfaceAddresses();
      state.interfaces = addresses.length;
      for (const address of addresses) {
        try {
          live.addMembership(GROUP, address);
          state.joined += 1;
        } catch {
          // One interface of several, and the others may work. A virtual
          // adapter that cannot join is the common case on a developer's
          // machine and says nothing about the venue's network. Counted rather
          // than only shrugged at: none of them joining is a different machine
          // from one of four not joining.
        }
      }
    } catch {
      // No multicast. The socket stays open and hears nothing, which is the
      // same outcome as a network that filters it.
    }
    query();
  });

  // Asked again while the list is open. A server switched on after the client
  // is the ordinary case at a venue — the rack goes on last — and a list that
  // only ever asked once would never show it.
  timer = setInterval(query, 5_000);

  // **A sweep that found nothing is news.** Publishing only on change meant a
  // network with no server on it produced no event at all, so the connect
  // screen could not tell "still looking" from "looked, and there is nothing" —
  // and the owner, on 2026-09-24, could not tell either of those from a client
  // that did not have the feature. One answer, once, whatever it contains.
  settle = setTimeout(publish, SETTLE_MS);

  return {
    refresh: query,
    state: () => ({ ...state }),
    stop: () => {
      if (timer !== null) {
        clearInterval(timer);
        timer = null;
      }
      if (settle !== null) {
        clearTimeout(settle);
        settle = null;
      }
      socket = null;
      try {
        live.close();
      } catch {
        // Already gone.
      }
    },
  };
}

/**
 * What a datagram socket has to look like to be asked, for a test to stand in.
 *
 * Three methods of `dgram.Socket`, because the property under test is the order
 * they are called in and nothing else about a socket matters to it.
 */
export interface Sender {
  setMulticastInterface(address: string): void;
  send(
    packet: Buffer,
    offset: number,
    length: number,
    port: number,
    address: string,
    done?: (error: Error | null) => void,
  ): void;
}

/**
 * Asks once out of every interface, one at a time.
 *
 * **One at a time is the whole point.** `setMulticastInterface` is a setsockopt
 * and takes effect at once; `socket.send` queues the datagram and returns. A
 * loop that sets and sends, six times, therefore sets the interface six times
 * and *then* transmits six datagrams — every one of them out of whichever
 * interface was set last.
 *
 * The owner's machine has six adapters and the working one is second, so every
 * question went out of `vEthernet (WSL (Hyper-V firewall))` and the server
 * never saw one (2026-09-27). The machine this was written on has two, with the
 * working one last, which is why it looked fine here.
 *
 * So each send waits for the one before it to be handed to the operating
 * system. A send that fails is not a reason to stop: the next interface may be
 * the venue's.
 */
export function askEach(
  socket: Sender,
  packet: Buffer,
  addresses: readonly string[],
  done?: () => void,
): void {
  const step = (at: number): void => {
    if (at >= addresses.length) {
      done?.();
      return;
    }
    const from = addresses[at];
    try {
      if (from !== undefined) {
        socket.setMulticastInterface(from);
      }
    } catch {
      // An interface that went away between the last question and this one, or
      // one that cannot carry multicast at all.
      step(at + 1);
      return;
    }
    try {
      socket.send(packet, 0, packet.length, PORT, GROUP, () => step(at + 1));
    } catch {
      // Threw before it queued anything, so nothing will call back.
      step(at + 1);
    }
  };
  step(0);
}

/** Sends one query, out of whichever interface the system prefers. */
function send(socket: Socket, packet: Buffer): void {
  try {
    socket.send(packet, 0, packet.length, PORT, GROUP);
  } catch {
    // No route at all. The next tick tries again.
  }
}

/** Every IPv4 address this machine has, for joining the group on each. */
function interfaceAddresses(): string[] {
  const addresses: string[] = [];
  for (const entries of Object.values(networkInterfaces())) {
    for (const entry of entries ?? []) {
      if (entry.family === 'IPv4' && !entry.internal) {
        addresses.push(entry.address);
      }
    }
  }
  // An empty list means "let the system choose", which is right on a machine
  // with one interface and the only thing possible on one with none.
  return addresses;
}

// --- the wire ----------------------------------------------------------------

/** Builds a query packet for one name and type. */
function question(name: string, type: number): Buffer {
  const labels = encodeName(name);
  const packet = Buffer.alloc(12 + labels.length + 4);
  // Id 0: a multicast query is answered to the group, so nothing matches a
  // reply to a request and an id would be noise.
  packet.writeUInt16BE(0, 0);
  packet.writeUInt16BE(0, 2); // flags: a standard query
  packet.writeUInt16BE(1, 4); // one question
  labels.copy(packet, 12);
  packet.writeUInt16BE(type, 12 + labels.length);
  packet.writeUInt16BE(1, 14 + labels.length); // class IN
  return packet;
}

/** Encodes a dotted name as length-prefixed labels. */
function encodeName(name: string): Buffer {
  const parts = name.split('.').filter((part) => part.length > 0);
  const size = parts.reduce((total, part) => total + 1 + Buffer.byteLength(part), 1);
  const buffer = Buffer.alloc(size);
  let at = 0;
  for (const part of parts) {
    const bytes = Buffer.from(part, 'utf8');
    buffer.writeUInt8(bytes.length, at);
    bytes.copy(buffer, at + 1);
    at += 1 + bytes.length;
  }
  buffer.writeUInt8(0, at);
  return buffer;
}

/** One record, reduced to what is needed here. */
interface Record {
  name: string;
  type: number;
  data: Buffer;
  at: number;
}

/**
 * Reads a response and returns whatever servers it describes.
 *
 * A response may carry all of the records for a server or only some: a
 * responder answering a PTR question puts the SRV, TXT and A records in the
 * additional section as a courtesy, and is allowed not to. Anything incomplete
 * is dropped rather than half-shown — an entry with a name and no address is a
 * row an operator cannot click.
 */
function read(message: Buffer): FoundServer[] {
  const answers = parse(message);

  // **Matched without case and shown with it.** Names on this wire are
  // case-insensitive, so every comparison here lowercases — and a server called
  // "Bühne Links" that is read back as "bühne links" is a server an operator
  // does not recognise in their own list. The key and the label are two things.
  const instances = new Map<string, string>();
  for (const record of answers) {
    if (record.type === TYPE_PTR && record.name.toLowerCase() === SERVICE) {
      const instance = decodeName(message, record.at).name;
      instances.set(instance.toLowerCase(), instance);
    }
  }
  if (instances.size === 0) {
    return [];
  }

  const addresses = new Map<string, string>();
  for (const record of answers) {
    if (record.type === TYPE_A && record.data.length === 4) {
      addresses.set(record.name.toLowerCase(), [...record.data].join('.'));
    }
  }

  const servers: FoundServer[] = [];
  for (const [instance, spelled] of instances) {
    const srv = answers.find(
      (record) => record.type === TYPE_SRV && record.name.toLowerCase() === instance,
    );
    if (srv === undefined || srv.data.length < 7) {
      continue;
    }
    const port = srv.data.readUInt16BE(4);
    const target = decodeName(message, srv.at + 6).name.toLowerCase();
    const address = addresses.get(target);
    if (address === undefined) {
      continue;
    }

    const txt = answers.find(
      (record) => record.type === TYPE_TXT && record.name.toLowerCase() === instance,
    );
    const properties = txt === undefined ? new Map<string, string>() : readText(txt.data);

    servers.push({
      name: properties.get('name') ?? spelled.split('.')[0] ?? 'OpenVideoServer',
      address,
      port,
      version: properties.get('version') ?? null,
      needsSetup: properties.get('setup') === 'yes',
      seenAt: Date.now(),
    });
  }
  return servers;
}

/** Splits a message into its records, questions skipped. */
function parse(message: Buffer): Record[] {
  if (message.length < 12) {
    return [];
  }
  const questions = message.readUInt16BE(4);
  const counts =
    message.readUInt16BE(6) + message.readUInt16BE(8) + message.readUInt16BE(10);

  let at = 12;
  for (let i = 0; i < questions; i += 1) {
    at = decodeName(message, at).next + 4;
  }

  const records: Record[] = [];
  for (let i = 0; i < counts && at < message.length; i += 1) {
    const { name, next } = decodeName(message, at);
    if (next + 10 > message.length) {
      break;
    }
    const type = message.readUInt16BE(next);
    const length = message.readUInt16BE(next + 8);
    const start = next + 10;
    if (start + length > message.length) {
      break;
    }
    records.push({ name, type, data: message.subarray(start, start + length), at: start });
    at = start + length;
  }
  return records;
}

/**
 * Reads a name, following compression pointers.
 *
 * The one genuinely fiddly part of this format: a name may end in a pointer to
 * an earlier offset, and a packet crafted to point at itself would loop for
 * ever. The jump budget is what stops that — a real name needs one or two.
 */
function decodeName(message: Buffer, from: number): { name: string; next: number } {
  const parts: string[] = [];
  let at = from;
  let next: number | null = null;
  let jumps = 0;

  while (at < message.length) {
    const length = message.readUInt8(at);
    if (length === 0) {
      at += 1;
      break;
    }
    if ((length & 0xc0) === 0xc0) {
      if (at + 1 >= message.length) {
        break;
      }
      if (next === null) {
        next = at + 2;
      }
      jumps += 1;
      if (jumps > 16) {
        break;
      }
      at = ((length & 0x3f) << 8) | message.readUInt8(at + 1);
      continue;
    }
    if (at + 1 + length > message.length) {
      break;
    }
    parts.push(message.subarray(at + 1, at + 1 + length).toString('utf8'));
    at += 1 + length;
  }

  return { name: parts.join('.'), next: next ?? at };
}

/** Reads the key/value pairs out of a TXT record. */
function readText(data: Buffer): Map<string, string> {
  const properties = new Map<string, string>();
  let at = 0;
  while (at < data.length) {
    const length = data.readUInt8(at);
    at += 1;
    if (length === 0 || at + length > data.length) {
      break;
    }
    const entry = data.subarray(at, at + length).toString('utf8');
    at += length;
    const split = entry.indexOf('=');
    if (split > 0) {
      properties.set(entry.slice(0, split), entry.slice(split + 1));
    }
  }
  return properties;
}

/** Exposed for the tests, which read real captured packets. */
export const wire = { question, encodeName, decodeName, parse, read, readText };
