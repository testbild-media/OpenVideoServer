// Identifiers of the kind the model uses everywhere.
//
// Its own module because two editing models need it — the show editor and
// the DMX patch — and a client where one of them imported the other only to
// mint an identifier would be a cycle for the sake of twenty lines.

/** Crockford base32, which is what a ULID is written in. */
const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

/** Characters of timestamp, then of randomness, in a 26-character ULID. */
const ULID_TIME_CHARS = 10;
const ULID_RANDOM_CHARS = 16;

/**
 * Mints an identifier of the kind the model uses everywhere.
 *
 * A ULID: 48 bits of millisecond timestamp then 80 random bits, written in
 * Crockford base32. The server parses every identifier strictly
 * (`id.rs`, `^[0-9A-HJKMNP-TV-Z]{26}$`), so a layer or a patch entry this client
 * adds has to carry a real one rather than something that merely looks like one.
 *
 * Each byte of randomness is reduced modulo 32, which is exact — 256 is eight
 * whole alphabets — so no value is more likely than another.
 */
export function newId(now: number = Date.now()): string {
  let remaining = Math.floor(now);
  const time: string[] = [];
  for (let position = 0; position < ULID_TIME_CHARS; position += 1) {
    time.unshift(CROCKFORD[remaining % 32] ?? '0');
    remaining = Math.floor(remaining / 32);
  }

  const bytes = new Uint8Array(ULID_RANDOM_CHARS);
  globalThis.crypto.getRandomValues(bytes);
  const random = [...bytes].map((byte) => CROCKFORD[byte % 32] ?? '0');

  return [...time, ...random].join('');
}
