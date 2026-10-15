// The editor's ranges and defaults against the Rust model they were copied from.
//
// `limits.ts` exists because the generated types carry the shape of the show
// document but not its arithmetic: `SpeedRatio` is a bare number in TypeScript,
// and the 0..1 rule for a layer's opacity lives in `Layer::validate`. So the
// numbers are written out once in the client and checked here, in the same
// spirit as `versions.test.ts` and `crates/ovs-types/tests/golden_schema.rs`.
//
// What this is guarding against is an editor offering an operator a control the
// server then refuses, or starting a new layer at values the server would not
// have given it.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import {
  BLACK_SLOT_INDEX,
  BLEND_MODES,
  BLEND_MODE_LABELS,
  CONNECTOR_ROLES,
  CONNECTOR_ROLE_LABELS,
  CROSSFADER_CURVES,
  CROSSFADER_CURVE_LABELS,
  DEFAULT_BEATS_PER_BAR,
  DEFAULT_BLEND_MODE,
  DEFAULT_BPM,
  DEFAULT_COMPOSITION_SIZE,
  DEFAULT_CROSSFADER,
  DEFAULT_CROSSFADER_CURVE,
  DEFAULT_DIMMER,
  DEFAULT_OPACITY,
  DEFAULT_OUTPUT_ENABLED,
  DEFAULT_PIECE_ENABLED,
  DEFAULT_PIECE_ORDER,
  DEFAULT_PIECE_ROTATION_DEG,
  DEFAULT_PLAYBACK_DIRECTION,
  DEFAULT_PLAYBACK_MODE,
  DEFAULT_SLOT_SPEED,
  DEFAULT_RENDER_RATE,
  DEFAULT_ROUTE,
  DEFAULT_SPEED_SCALE,
  FRAME_RATES,
  IDENTITY_TRANSFORM,
  MAX_BEATS_PER_BAR,
  MAX_BPM,
  MAX_DIMENSION_PX,
  MAX_DMX_ADDRESS,
  MAX_FRACTION,
  MAX_PIECE_ORDER,
  MAX_SLOT_NAME_CHARS,
  MAX_SPEED_SCALE,
  MAX_UNIVERSE,
  MIN_BEATS_PER_BAR,
  MIN_BPM,
  MIN_DIMENSION_PX,
  MIN_DMX_ADDRESS,
  MIN_FIXTURE_VERSION,
  MIN_FRACTION,
  MIN_PIECE_ORDER,
  MIN_SLOT_BEATS,
  MIN_SPEED_SCALE,
  MIN_UNIVERSE,
  MEDIA_KINDS,
  MEDIA_KIND_LABELS,
  PLAYBACK_DIRECTIONS,
  PLAYBACK_DIRECTION_LABELS,
  PLAYBACK_MODES,
  PLAYBACK_MODE_LABELS,
  MAX_BANKS,
  MAX_SLOT_INDEX,
  ROUTES,
  ROUTE_LABELS,
  SLOTS_PER_BANK,
} from '../src/shared/limits.js';

const repository = (path: string) => fileURLToPath(new URL(`../../../${path}`, import.meta.url));
const read = (path: string) => readFileSync(repository(path), 'utf8');

const layerRs = read('crates/ovs-types/src/config/layer.rs');
const showRs = read('crates/ovs-types/src/config/show.rs');
const slotRs = read('crates/ovs-types/src/config/slot.rs');
const pieceRs = read('crates/ovs-types/src/config/piece.rs');
const outputRs = read('crates/ovs-types/src/config/output.rs');
const patchRs = read('crates/ovs-types/src/config/patch.rs');
const geometryRs = read('crates/ovs-types/src/geometry.rs');

/** A generated schema, in the shape the checks below read it. */
interface Schema {
  definitions: Record<
    string,
    {
      oneOf?: Array<{ enum?: string[] }>;
      minimum?: number;
      maximum?: number;
      properties?: Record<string, { format?: string }>;
    }
  >;
}

const showSchema = JSON.parse(read('schemas/show.schema.json')) as Schema;
// `ConnectorRole` is server configuration rather than show data, so it is
// generated beside the capabilities response rather than beside the show.
const capabilitiesSchema = JSON.parse(read('schemas/capabilities.schema.json')) as Schema;
// `MediaKind` belongs to an upload rather than to a show, and an upload is
// the one place this client has to name one.
const uploadSchema = JSON.parse(read('schemas/create-upload-request.schema.json')) as Schema;

/** The largest value a `u32` field can carry, which bounds several of these. */
const U32_MAX = 4_294_967_295;
/** And a `u16`, which bounds a universe number. */
const U16_MAX = 65_535;

/** Reads a numeric `pub const`, including one written as a division. */
function constant(source: string, name: string): number {
  const match = new RegExp(`pub const ${name}: \\w+ = ([^;]+);`).exec(source);
  if (!match?.[1]) {
    throw new Error(`${name} is not in the Rust; the client's copy has nothing to check against`);
  }
  return match[1]
    .split('/')
    .map((part) => Number(part.trim()))
    .reduce((left, right) => left / right);
}

/** Every variant of a Rust enum, in declaration order. */
function variants(source: string, name: string): Array<{ name: string; isDefault: boolean }> {
  const start = source.indexOf(`pub enum ${name} {`);
  if (start < 0) {
    throw new Error(`enum ${name} is not in the Rust`);
  }
  const end = source.indexOf('\n}', start);
  const body = source.slice(start, end);

  const found: Array<{ name: string; isDefault: boolean }> = [];
  let pendingDefault = false;
  for (const line of body.split('\n').slice(1)) {
    if (line.trim() === '#[default]') {
      pendingDefault = true;
      continue;
    }
    const variant = /^ {4}([A-Z][A-Za-z0-9]*)\s*[,{(]/.exec(line);
    if (variant?.[1]) {
      found.push({ name: variant[1], isDefault: pendingDefault });
      pendingDefault = false;
    }
  }
  return found;
}

/** The values of a schema enum, in the order the Rust declares them. */
function schemaValues(name: string, schema: Schema = showSchema): string[] {
  const definition = schema.definitions[name];
  if (!definition?.oneOf) {
    throw new Error(`${name} is not an enum in the generated schema`);
  }
  return definition.oneOf.map((variant) => {
    const value = variant.enum?.[0];
    if (value === undefined) {
      throw new Error(`a variant of ${name} has no value in the generated schema`);
    }
    return value;
  });
}

/** The serialised name of the variant the Rust marks `#[default]`. */
function defaultValueOf(source: string, name: string, schema: Schema = showSchema): string {
  const declared = variants(source, name);
  const position = declared.findIndex((variant) => variant.isDefault);
  if (position < 0) {
    throw new Error(`${name} has no #[default] variant`);
  }
  const values = schemaValues(name, schema);
  const value = values[position];
  if (value === undefined) {
    throw new Error(`${name} declares ${declared.length} variants but the schema has ${values.length}`);
  }
  return value;
}

/**
 * One field of a Rust struct literal, as it is written.
 *
 * Used for the `Default` implementations, which is where what a new show
 * starts from actually lives: `MasterConfig::default`, `Composition::default`,
 * `Piece::new` and `OutputConfig::new`.
 */
function literalField(source: string, block: string, field: string): string {
  const start = source.indexOf(block);
  if (start < 0) {
    throw new Error(`${block} is not in the Rust`);
  }
  // Up to the last comma on the line, so a value with a comma inside it —
  // `SizePx::new(1920, 1080)` — comes back whole.
  const match = new RegExp(`\\n\\s+${field}: (.+),\\r?\\n`).exec(source.slice(start));
  if (!match?.[1]) {
    throw new Error(`${block} does not set ${field}`);
  }
  return match[1].trim();
}

/** Every `FrameRate` constant the model names, in declaration order. */
function declaredRates(): Array<{ label: string; numerator: number; denominator: number }> {
  const found = [
    ...geometryRs.matchAll(
      /pub const FPS_([0-9_]+): Self = Self \{\s*numerator: (\d+),\s*denominator: (\d+),\s*\};/g,
    ),
  ];
  if (found.length === 0) {
    throw new Error('no FrameRate constants in the Rust; the client has nothing to check against');
  }
  return found.map((match) => ({
    // `FPS_29_97` is the rate commonly written 29.97, which is what the
    // constant is named after and what a chooser should say.
    label: (match[1] ?? '').replace('_', '.'),
    numerator: Number(match[2]),
    denominator: Number(match[3]),
  }));
}

describe('the ranges the editor enforces', () => {
  it('bound a slot’s beat count the way the model does', () => {
    // One upwards, with no ceiling but the clip's own length (owner,
    // 2026-09-23). The floor is the only end that needs a number, and it is the
    // one the editor puts on the spinner.
    expect(MIN_SLOT_BEATS).toBe(constant(slotRs, 'MIN_SLOT_BEATS'));
  });

  it('hold a pad’s name to the length the model does', () => {
    // The field in OPTIONS stops at this, so that a name an operator can type
    // is a name the server will take. A client that let them type past it
    // would refuse the save after the typing rather than before.
    expect(MAX_SLOT_NAME_CHARS).toBe(constant(slotRs, 'MAX_SLOT_NAME_CHARS'));
  });

  it('bound the master clock the way the model does', () => {
    expect(MIN_BPM).toBe(constant(showRs, 'MIN_BPM'));
    expect(MAX_BPM).toBe(constant(showRs, 'MAX_BPM'));
    expect(MIN_SPEED_SCALE).toBe(constant(showRs, 'MIN_SPEED_SCALE'));
    expect(MAX_SPEED_SCALE).toBe(constant(showRs, 'MAX_SPEED_SCALE'));
  });

  it('use the 0..1 range the model checks opacity and the master against', () => {
    expect(layerRs).toContain(`(${MIN_FRACTION.toFixed(1)}..=${MAX_FRACTION.toFixed(1)}).contains(&self.default_opacity)`);
    expect(showRs).toContain(`(${MIN_FRACTION.toFixed(1)}..=${MAX_FRACTION.toFixed(1)}).contains(&value)`);
  });

  it('bank slots the way the matrix does, and reserve the same index for black', () => {
    expect(BLACK_SLOT_INDEX).toBe(constant(layerRs, 'BLACK_SLOT_INDEX'));
    expect(SLOTS_PER_BANK).toBe(constant(layerRs, 'SLOTS_PER_BANK'));
  });

  it('stops where the model stops: seven banks, and eighty-four slots', () => {
    // The owner's ceiling (2026-09-23). A client that offered a slot past it
    // would turn a rule into a failed request in front of an operator.
    expect(MAX_BANKS).toBe(constant(layerRs, 'MAX_BANKS'));
    expect(MAX_SLOT_INDEX).toBe(SLOTS_PER_BANK * MAX_BANKS);
    expect(layerRs).toContain(
      'MAX_SLOT_INDEX: u16 = Self::SLOTS_PER_BANK * Self::MAX_BANKS',
    );
  });

  it('bound a bar the way the model does, and at the width of the field it is stored in', () => {
    // `MasterConfig::validate` refuses zero and says nothing else, so the
    // bottom of the range is one; the top is the field, which is a `u32`, and a
    // larger number is one the document could not carry back.
    expect(showRs).toContain('if self.beats_per_bar == 0 {');
    expect(MIN_BEATS_PER_BAR).toBe(1);
    expect(showRs).toContain('pub beats_per_bar: u32,');
    expect(showSchema.definitions['MasterConfig']?.properties?.['beats_per_bar']?.format).toBe(
      'uint32',
    );
    expect(MAX_BEATS_PER_BAR).toBe(U32_MAX);
  });

  it('bound a surface the way the model does, and at the width of its fields', () => {
    // `SizePx::validate` and `RectPx::validate` both refuse a zero width or
    // height, which is the whole of what the model says about how small a
    // surface may be.
    expect(geometryRs).toContain('if self.width == 0 || self.height == 0 {');
    expect(MIN_DIMENSION_PX).toBe(1);
    for (const field of ['width', 'height'] as const) {
      expect(showSchema.definitions['SizePx']?.properties?.[field]?.format).toBe('uint32');
      expect(showSchema.definitions['RectPx']?.properties?.[field]?.format).toBe('uint32');
    }
    expect(MAX_DIMENSION_PX).toBe(U32_MAX);
  });

  it('bound a draw order at the width of the field a piece stores it in', () => {
    expect(pieceRs).toContain('pub order: u32,');
    expect(showSchema.definitions['Piece']?.properties?.['order']?.format).toBe('uint32');
    expect(MIN_PIECE_ORDER).toBe(0);
    expect(MAX_PIECE_ORDER).toBe(U32_MAX);
  });

  it('bound a DMX address the way the patch model does', () => {
    // A universe has 512 channels and addressing is one-based. A patch editor
    // that offered an address outside that would be offering an operator a
    // fixture the server then refuses — and a fixture nothing on stage moves.
    expect(MAX_DMX_ADDRESS).toBe(constant(patchRs, 'MAX_DMX_ADDRESS'));
    expect(patchRs).toContain('if self.start_address == 0 || self.footprint == 0 {');
    expect(MIN_DMX_ADDRESS).toBe(1);
  });

  it('bound a universe at the width of the field a patch entry stores it in', () => {
    // The model says nothing else about a universe number: Art-Net counts from
    // zero and sACN from one, and which is which is the operator's to settle
    // with their console rather than this client's to narrow.
    expect(patchRs).toContain('pub universe: u16,');
    expect(showSchema.definitions['FixtureInstance']?.properties?.['universe']?.format).toBe(
      'uint16',
    );
    expect(MIN_UNIVERSE).toBe(0);
    expect(MAX_UNIVERSE).toBe(U16_MAX);
  });

  it('start fixture definition versions where the model starts them', () => {
    expect(patchRs).toContain('fixture definition versions start at 1');
    expect(MIN_FIXTURE_VERSION).toBe(1);
  });
});

describe('the menus the editor offers', () => {
  for (const [name, values, labels] of [
    ['Route', ROUTES, ROUTE_LABELS],
    ['BlendMode', BLEND_MODES, BLEND_MODE_LABELS],
    ['PlaybackMode', PLAYBACK_MODES, PLAYBACK_MODE_LABELS],
    ['PlaybackDirection', PLAYBACK_DIRECTIONS, PLAYBACK_DIRECTION_LABELS],
    ['CrossfaderCurve', CROSSFADER_CURVES, CROSSFADER_CURVE_LABELS],
  ] as const) {
    it(`list every ${name} the model has, in the order it declares them`, () => {
      // Order is not cosmetic: it is the order the DMX range mapping uses and
      // the order the Live Mode reference reads in.
      expect([...values]).toEqual(schemaValues(name));
      expect(Object.keys(labels).sort()).toEqual([...values].sort());
    });
  }

  it('list every ConnectorRole the model has, in the order it declares them', () => {
    // Read only: what a socket is for describes the machine, is set on the
    // server, and must never be inferred from what is plugged in.
    // The list still has to be the model's, because a role this client
    // did not recognise would be a socket it drew nothing sensible about.
    expect([...CONNECTOR_ROLES]).toEqual(schemaValues('ConnectorRole', capabilitiesSchema));
    expect(Object.keys(CONNECTOR_ROLE_LABELS).sort()).toEqual([...CONNECTOR_ROLES].sort());
  });

  it('list every MediaKind the model has, in the order it declares them', () => {
    // Offered on one screen only: a machine with no prober cannot say whether a
    // file is a clip or a picture, and that decides which of the server's two
    // checks it faces. It used to ask which internal profile the file was,
    // which meant asking an operator to recognise a codec.
    expect([...MEDIA_KINDS]).toEqual(schemaValues('MediaKind', uploadSchema));
    expect(Object.keys(MEDIA_KIND_LABELS).sort()).toEqual([...MEDIA_KINDS].sort());
  });

  it('offer the frame rates the model names, with the names it gives them', () => {
    // An output's rate comes from the mode list a connector reported; these are
    // the rates the model itself declares constants for, which is what the
    // composition's render rate is chosen from and what a reported rate in
    // millihertz is matched back against.
    const declared = declaredRates();
    expect(FRAME_RATES.map((named) => named.label)).toEqual(declared.map((rate) => rate.label));
    expect(FRAME_RATES.map((named) => named.rate)).toEqual(
      declared.map((rate) => ({ numerator: rate.numerator, denominator: rate.denominator })),
    );
  });

});

describe('what a new layer and a new slot start from', () => {
  it('are the variants the model marks as its defaults', () => {
    expect(DEFAULT_ROUTE).toBe(defaultValueOf(layerRs, 'Route'));
    expect(DEFAULT_BLEND_MODE).toBe(defaultValueOf(layerRs, 'BlendMode'));
    expect(DEFAULT_PLAYBACK_MODE).toBe(defaultValueOf(slotRs, 'PlaybackMode'));
    expect(DEFAULT_PLAYBACK_DIRECTION).toBe(defaultValueOf(slotRs, 'PlaybackDirection'));
  });

  it('are the numbers `Layer::new` starts a layer at', () => {
    expect(layerRs).toContain(`default_opacity: ${DEFAULT_OPACITY.toFixed(1)},`);
  });

  it('are the rate `Slot::with_source` starts a slot at', () => {
    // A slot dropped on a pad plays at the speed it was made at. The client and
    // the server have to agree on that, or a pad set up here and a pad set up
    // there are two different pads.
    expect(slotRs).toContain(`speed: ${DEFAULT_SLOT_SPEED.toFixed(1)},`);
  });

  it('are the identity transform the model defines', () => {
    const block = /pub const IDENTITY: Self = Self \{([^}]+)\}/.exec(geometryRs)?.[1] ?? '';
    const declared = Object.fromEntries(
      [...block.matchAll(/(\w+): (-?[\d.]+)/g)].map((match) => [match[1], Number(match[2])]),
    );
    expect(declared).toEqual({ ...IDENTITY_TRANSFORM });
  });
});

describe('what the master, the composition, a piece and an output start from', () => {
  const masterDefault = 'impl Default for MasterConfig {';
  const compositionDefault = 'impl Default for Composition {';

  it('are the numbers `MasterConfig::default` gives a show', () => {
    // A show created here and a show created on the server have to be the
    // same show, or the two disagree about a document neither of them
    // changed.
    expect(literalField(showRs, masterDefault, 'default_bpm')).toBe(
      `${DEFAULT_BPM.toFixed(1)}`,
    );
    expect(literalField(showRs, masterDefault, 'beats_per_bar')).toBe(
      String(DEFAULT_BEATS_PER_BAR),
    );
    expect(literalField(showRs, masterDefault, 'default_speed_scale')).toBe(
      `${DEFAULT_SPEED_SCALE.toFixed(1)}`,
    );
    expect(literalField(showRs, masterDefault, 'default_dimmer')).toBe(
      `${DEFAULT_DIMMER.toFixed(1)}`,
    );
    expect(literalField(showRs, masterDefault, 'default_crossfader')).toBe(
      `${DEFAULT_CROSSFADER.toFixed(1)}`,
    );
    expect(DEFAULT_CROSSFADER_CURVE).toBe(defaultValueOf(showRs, 'CrossfaderCurve'));
  });

  it('are the canvas and the rate `Composition::default` gives a show', () => {
    expect(literalField(showRs, compositionDefault, 'size')).toBe(
      `SizePx::new(${DEFAULT_COMPOSITION_SIZE.width}, ${DEFAULT_COMPOSITION_SIZE.height})`,
    );
    // Named rather than written out, so this checks both that the default is
    // `FPS_60` and that `FPS_60` is what this client thinks it is.
    const named = literalField(showRs, compositionDefault, 'render_rate');
    expect(named).toBe('FrameRate::FPS_60');
    const sixty = declaredRates().find((rate) => rate.label === '60');
    expect(DEFAULT_RENDER_RATE).toEqual({
      numerator: sixty?.numerator,
      denominator: sixty?.denominator,
    });
  });

  it('are what `Piece::new` and `OutputConfig::new` create', () => {
    // The rectangles are deliberately not checked, because there are none to
    // check: `Piece::new` takes them, and this client takes them from the
    // composition and the output the document already holds.
    expect(literalField(pieceRs, 'pub fn new(', 'enabled')).toBe(String(DEFAULT_PIECE_ENABLED));
    expect(literalField(pieceRs, 'pub fn new(', 'order')).toBe(String(DEFAULT_PIECE_ORDER));
    expect(literalField(pieceRs, 'pub fn new(', 'rotation_deg')).toBe(
      `${DEFAULT_PIECE_ROTATION_DEG.toFixed(1)}`,
    );
    expect(literalField(outputRs, 'pub fn new(', 'enabled')).toBe(String(DEFAULT_OUTPUT_ENABLED));
  });
});
