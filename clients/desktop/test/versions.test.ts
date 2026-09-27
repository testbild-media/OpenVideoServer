// The client's contract versions against the Rust ones they were copied from.
//
// The generated types cannot drift, because they are generated. These constants
// can, because they are not — so this reads the Rust and fails when they
// disagree, in the same spirit as crates/ovs-types/tests/golden_schema.rs.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import {
  API_PREFIX,
  SHOW_SCHEMA_VERSION,
  REALTIME_PROTOCOL_VERSION,
} from '../src/shared/versions.js';

const versionRs = fileURLToPath(
  new URL('../../../crates/ovs-types/src/version.rs', import.meta.url),
);

/** Reads `pub const NAME: u32 = 7;` out of version.rs. */
function rustConstant(name: string): number {
  const source = readFileSync(versionRs, 'utf8');
  const match = new RegExp(`pub const ${name}: u32 = (\\d+);`).exec(source);
  if (!match) {
    throw new Error(`${name} is not in version.rs; the client's copy has nothing to check against`);
  }
  return Number(match[1]);
}

describe('the versions this client is built against', () => {
  it('speaks the realtime protocol the server speaks', () => {
    expect(REALTIME_PROTOCOL_VERSION).toBe(rustConstant('REALTIME_PROTOCOL_VERSION'));
  });

  it('understands the show schema the server writes', () => {
    expect(SHOW_SCHEMA_VERSION).toBe(rustConstant('SHOW_SCHEMA_VERSION'));
  });

  it('calls the API by the prefix the server serves it under', () => {
    expect(API_PREFIX).toBe(`/api/v${rustConstant('API_VERSION')}`);
  });

});
