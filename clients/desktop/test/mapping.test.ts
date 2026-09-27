// Master settings, the composition, and mapping the composition onto walls.
//
// Against the editing model rather than the page, like the rest of the editor
// suite: what matters about a mapping is whether the document it produces is
// one the server will take, and whether what the operator is told about it
// matches what the renderer will actually do.

import { describe, expect, it } from 'vitest';

import type { ConnectorInfo, Piece, Show, RectPx } from '@ovs/schema';

import {
  addNetworkOutput,
  addOutput,
  addPiece,
  duplicatePiece,
  movePiece,
  newId,
  openDraft,
  removeOutput,
  restoreMapping,
  setComposition,
  setMaster,
  setOutputConnector,
  renameOutput,
  setOutputOnNetwork,
  setOutputMode,
  setPiece,
  setPieceDest,
  setPieceSource,
  validate,
} from '../src/shared/editor.js';
import { stageOutput } from '../src/ui/mapping.js';
import {
  DEFAULT_BEATS_PER_BAR,
  DEFAULT_BPM,
  DEFAULT_COMPOSITION_SIZE,
  DEFAULT_CROSSFADER,
  DEFAULT_CROSSFADER_CURVE,
  DEFAULT_DIMMER,
  DEFAULT_PIECE_ORDER,
  DEFAULT_RENDER_RATE,
  DEFAULT_SPEED_SCALE,
  MAX_BPM,
  MAX_SPEED_SCALE,
  MIN_BPM,
  MIN_SPEED_SCALE,
} from '../src/shared/limits.js';
import {
  advisories,
  boundsOf,
  drawOrder,
  grown,
  isPixelExact,
  millihertzOf,
  nudged,
  overlaps,
  proportionalScale,
  rateFromMillihertz,
  scaledAbout,
  sequenceOf,
  anchorOf,
  placedAt,
  resizedAbout,
  sizedAbout,
  CENTRE_ANCHOR,
  cornersOf,
  snapGuides,
  snapPointsOf,
  snapTranslation,
  surfacePoints,
  SNAP_WITHIN_PX,
} from '../src/shared/mapping.js';
import { emptyCatalogue } from '../src/shared/modules.js';

/** A server that has reported one card with two sockets on it. */
const CONNECTORS: ConnectorInfo[] = [
  {
    gpu_pci_address: '0000:01:00.0',
    connector: 'DP-1',
    connected: true,
    model: 'Novastar MX40',
    role: 'MEDIA_OUTPUT',
    modes: [
      { size: { width: 1920, height: 1080 }, refresh_millihertz: 60_000 },
      { size: { width: 1920, height: 1080 }, refresh_millihertz: 59_940 },
    ],
  },
  {
    gpu_pci_address: '0000:01:00.0',
    connector: 'HDMI-0',
    connected: true,
    model: null,
    role: 'SERVICE_DISPLAY',
    modes: [{ size: { width: 1024, height: 600 }, refresh_millihertz: 60_000 }],
  },
];

function show(overrides: Partial<Show> = {}): Show {
  return {
    id: newId(),
    schema_version: 1,
    name: 'Nachtfahrt',
    created_at: 1_700_000_000_000,
    updated_at: 1_700_000_000_000,
    composition: {
      size: { width: 1920, height: 1080 },
      render_rate: { numerator: 60, denominator: 1 },
    },
    master: {
      default_bpm: 120,
      beats_per_bar: 4,
      default_speed_scale: 1,
      default_dimmer: 1,
      default_crossfader: 0,
      crossfader_curve: 'linear',
    },
    layers: [],
    pieces: [],
    outputs: [],
    settings: { auto_save: true, notes: '' },
    ...overrides,
  };
}

/** Applies an edit that is expected to succeed. */
function must(edit: ReturnType<typeof addPiece>): Show {
  if (!edit.ok) {
    throw new Error(`the edit was refused: ${edit.reason}`);
  }
  return edit.show;
}

/** The note an edit came back with, or nothing. */
function noteOf(edit: ReturnType<typeof addPiece>): string {
  return edit.ok ? (edit.note ?? '') : `refused: ${edit.reason}`;
}

/** A show with one output on DP-1 and no pieces yet. */
function withOutput(): { document: Show; outputId: string } {
  const document = must(addOutput(show(), CONNECTORS[0]!, CONNECTORS[0]!.modes![0]!, 'Wall'));
  return { document, outputId: document.outputs[0]!.id };
}

function pieceNamed(document: Show, name: string): Piece {
  const piece = document.pieces.find((each) => each.name === name);
  if (!piece) {
    throw new Error(`no piece called ${name}`);
  }
  return piece;
}

describe('a master setting', () => {
  it('is refused at the control when it is outside the range the model allows', () => {
    // The hole this closes. Until there was a control, a document carrying a
    // BPM the server refuses was reported by validation and could not be put
    // right, because there was nothing to put it right with.
    const document = show();

    for (const bpm of [MIN_BPM - 1, MAX_BPM + 1, 0, Number.NaN, Number.POSITIVE_INFINITY]) {
      const refused = setMaster(document, { default_bpm: bpm });
      expect(refused.ok, `${bpm} is outside ${MIN_BPM}..${MAX_BPM} and must be refused`).toBe(false);
      expect(refused.ok === false && refused.reason).toContain(`between ${MIN_BPM} and ${MAX_BPM}`);
    }
    expect(setMaster(document, { default_bpm: MIN_BPM }).ok).toBe(true);
    expect(setMaster(document, { default_bpm: MAX_BPM }).ok).toBe(true);
  });

  it('refuses a speed scale, a dimmer and a crossfader the server would refuse', () => {
    const document = show();
    expect(setMaster(document, { default_speed_scale: MIN_SPEED_SCALE - 0.1 }).ok).toBe(false);
    expect(setMaster(document, { default_speed_scale: MAX_SPEED_SCALE + 1 }).ok).toBe(false);
    expect(setMaster(document, { default_speed_scale: MAX_SPEED_SCALE }).ok).toBe(true);

    expect(setMaster(document, { default_dimmer: 1.5 }).ok).toBe(false);
    expect(setMaster(document, { default_crossfader: -0.1 }).ok).toBe(false);
    expect(setMaster(document, { default_dimmer: 0, default_crossfader: 1 }).ok).toBe(true);
  });

  it('refuses a bar that is not a whole number of beats, or none at all', () => {
    const document = show();
    expect(setMaster(document, { beats_per_bar: 0 }).ok).toBe(false);
    expect(setMaster(document, { beats_per_bar: 3.5 }).ok).toBe(false);
    expect(setMaster(document, { beats_per_bar: -4 }).ok).toBe(false);
    expect(setMaster(document, { beats_per_bar: 3 }).ok).toBe(true);
  });

  it('can be put right in a document that arrived with a bad one', () => {
    // The whole point of the control: a show written elsewhere, reported by
    // validation, and now correctable without a round trip to the server.
    const broken = show({
      master: {
        default_bpm: 5,
        beats_per_bar: 0,
        default_speed_scale: 100,
        default_dimmer: 1,
        default_crossfader: 0,
        crossfader_curve: 'linear',
      },
    });
    expect(validate(broken, emptyCatalogue()).map((problem) => problem.path)).toEqual([
      'master.default_bpm',
      'master.beats_per_bar',
      'master.default_speed_scale',
    ]);

    let fixed = must(setMaster(broken, { default_bpm: DEFAULT_BPM }));
    fixed = must(setMaster(fixed, { beats_per_bar: DEFAULT_BEATS_PER_BAR }));
    fixed = must(setMaster(fixed, { default_speed_scale: DEFAULT_SPEED_SCALE }));
    expect(validate(fixed, emptyCatalogue())).toEqual([]);
  });

  it('leaves the rest of the master alone when one setting changes', () => {
    const changed = must(setMaster(show(), { crossfader_curve: 'equal_power' }));
    expect(changed.master).toEqual({
      default_bpm: DEFAULT_BPM,
      beats_per_bar: DEFAULT_BEATS_PER_BAR,
      default_speed_scale: DEFAULT_SPEED_SCALE,
      default_dimmer: DEFAULT_DIMMER,
      default_crossfader: DEFAULT_CROSSFADER,
      crossfader_curve: 'equal_power',
    });
  });
});

describe('resizing the composition', () => {
  it('reports what it does to the pieces that no longer fit', () => {
    // Every piece is measured against this canvas, so shrinking it is not a
    // change to one thing. A piece whose region the new canvas does not contain
    // makes the whole show unsaveable, and the operator is told which.
    const { document, outputId } = withOutput();
    let mapped = must(addPiece(document, outputId, 'Left'));
    mapped = must(addPiece(mapped, outputId, 'Right'));
    mapped = must(setPieceSource(mapped, pieceNamed(mapped, 'Left').id, {
      x: 0,
      y: 0,
      width: 640,
      height: 480,
    }));
    mapped = must(setPieceSource(mapped, pieceNamed(mapped, 'Right').id, {
      x: 1280,
      y: 0,
      width: 640,
      height: 480,
    }));

    const shrunk = setComposition(mapped, { size: { width: 1280, height: 720 } });
    expect(shrunk.ok).toBe(true);
    const note = noteOf(shrunk);
    expect(note).toContain('Right');
    expect(note).not.toContain('Left,');
    expect(note).toContain('1280×720');
    expect(note).toContain('before this show can be saved');
  });

  it('says that the pieces which still fit now take a different part of the picture', () => {
    // The consequence that sounds like nothing happening. A source rectangle is
    // in composition pixels, so the same rectangle on a different canvas is a
    // different part of the image.
    const { document, outputId } = withOutput();
    const mapped = must(addPiece(document, outputId, 'Whole'));
    const grown = setComposition(mapped, { size: { width: 3840, height: 2160 } });
    expect(noteOf(grown)).toContain('different part of the picture');
  });

  it('moves no piece to make it fit', () => {
    const { document, outputId } = withOutput();
    const mapped = must(addPiece(document, outputId, 'Whole'));
    const before = { ...pieceNamed(mapped, 'Whole').source_rect };

    const shrunk = must(setComposition(mapped, { size: { width: 640, height: 480 } }));
    expect(pieceNamed(shrunk, 'Whole').source_rect).toEqual(before);
    // And the server would refuse it, which is exactly what the note said.
    expect(validate(shrunk, emptyCatalogue())).toContainEqual({
      path: 'pieces[0].source_rect',
      message: 'must lie within the 640x480 composition',
    });
  });

  it('says nothing when the size has not changed', () => {
    const { document } = withOutput();
    const same = setComposition(document, { size: { ...document.composition.size } });
    expect(noteOf(same)).toBe('');
  });

  it('refuses a canvas with no pixels in it, as the model does', () => {
    const { document } = withOutput();
    expect(setComposition(document, { size: { width: 0, height: 1080 } }).ok).toBe(false);
    expect(setComposition(document, { size: { width: 1920, height: -4 } }).ok).toBe(false);
    expect(setComposition(document, { size: { width: 1920.5, height: 1080 } }).ok).toBe(false);
    expect(
      setComposition(document, { render_rate: { numerator: 0, denominator: 1 } }).ok,
    ).toBe(false);
  });

  it('starts a show from the canvas and rate the model starts one from', () => {
    const fresh = show();
    expect(fresh.composition.size).toEqual(DEFAULT_COMPOSITION_SIZE);
    expect(fresh.composition.render_rate).toEqual(DEFAULT_RENDER_RATE);
  });
});

describe('an output', () => {
  it('is built only from a connector the server reported', () => {
    const document = must(addOutput(show(), CONNECTORS[0]!, CONNECTORS[0]!.modes![0]!));
    const output = document.outputs[0]!;
    expect(output.identity.gpu_pci_address).toBe('0000:01:00.0');
    expect(output.identity.connector).toBe('DP-1');
    expect(output.mode.size).toEqual({ width: 1920, height: 1080 });
    expect(output.name).toBe('Novastar MX40');
    expect(validate(document, emptyCatalogue())).toEqual([]);
  });

  it('carries no EDID, because a show must never be matched by one', () => {
    // The model says the EDID fields decide nothing, and a show carrying the
    // serial of whatever was in the rack that night invites somebody to match
    // on it later.
    const document = must(addOutput(show(), CONNECTORS[0]!, CONNECTORS[0]!.modes![0]!));
    expect(Object.keys(document.outputs[0]!.identity).sort()).toEqual([
      'connector',
      'gpu_pci_address',
    ]);
  });

  it('turns the millihertz a driver reports back into the exact rate the model stores', () => {
    // 59 940 mHz is 60000/1001, not 59940/1000: the two are different rates and
    // a scheduler given the second would drift.
    const document = must(addOutput(show(), CONNECTORS[0]!, CONNECTORS[0]!.modes![1]!));
    expect(document.outputs[0]!.mode.refresh).toEqual({ numerator: 60000, denominator: 1001 });
    expect(millihertzOf(document.outputs[0]!.mode.refresh)).toBe(59_940);
  });

  it('can be moved to another socket without losing its pieces', () => {
    // A wall moved to another card on a show day is ordinary. Rebuilding the
    // output to say so would take its pieces with it, which is the whole
    // reason this is an edit rather than a delete and an add.
    const built = must(addOutput(show(), CONNECTORS[0]!, CONNECTORS[0]!.modes![0]!));
    const withPiece = must(addPiece(built, built.outputs[0]!.id));
    const moved = must(
      setOutputConnector(withPiece, built.outputs[0]!.id, '0000:01:00.0', 'DP-9'),
    );

    expect(moved.outputs[0]!.identity).toEqual({
      gpu_pci_address: '0000:01:00.0',
      connector: 'DP-9',
    });
    expect(moved.pieces).toHaveLength(1);
    expect(moved.pieces[0]!.output_id).toBe(built.outputs[0]!.id);
    expect(validate(moved, emptyCatalogue())).toEqual([]);
  });

  it('is refused onto a socket that already drives another output', () => {
    // One socket drives one wall. The same rule that refuses making a second
    // output on a taken socket has to refuse moving one onto it, or the rule
    // is only about which door was used.
    const first = must(addOutput(show(), CONNECTORS[0]!, CONNECTORS[0]!.modes![0]!));
    const both = must(
      addOutput(first, { ...CONNECTORS[0]!, connector: 'DP-2' }, CONNECTORS[0]!.modes![0]!),
    );

    const refused = setOutputConnector(both, both.outputs[1]!.id, '0000:01:00.0', 'DP-1');
    expect(refused.ok).toBe(false);
  });

  it('may be moved onto the socket it is already on', () => {
    // Otherwise saving a panel that changed nothing else would be refused for
    // a clash with itself.
    const built = must(addOutput(show(), CONNECTORS[0]!, CONNECTORS[0]!.modes![0]!));
    const again = setOutputConnector(built, built.outputs[0]!.id, '0000:01:00.0', 'DP-1');
    expect(again.ok).toBe(true);
  });

  it('is refused on the socket the server says drives the service console', () => {
    const refused = addOutput(show(), CONNECTORS[1]!, CONNECTORS[1]!.modes![0]!);
    expect(refused.ok).toBe(false);
    expect(refused.ok === false && refused.reason).toContain('service console');
  });

  it('is refused on a socket another output in this show already drives', () => {
    const { document } = withOutput();
    const refused = addOutput(document, CONNECTORS[0]!, CONNECTORS[0]!.modes![0]!);
    expect(refused.ok).toBe(false);
    expect(refused.ok === false && refused.reason).toContain('one socket drives one');
  });

  it('takes its pieces with it when it is removed, and says so', () => {
    // A piece whose output has gone makes the whole show unsaveable, and a
    // mapping that quietly lost one is a wall that goes dark.
    const { document, outputId } = withOutput();
    const mapped = must(addPiece(document, outputId, 'Whole'));
    const removed = removeOutput(mapped, outputId);

    expect(removed.ok).toBe(true);
    expect(removed.ok === true && removed.show.pieces).toEqual([]);
    expect(noteOf(removed)).toContain('Wall');
    expect(validate(removed.ok === true ? removed.show : mapped, emptyCatalogue())).toEqual([]);
  });

  it('says which pieces a smaller mode would stop drawing', () => {
    const { document, outputId } = withOutput();
    let mapped = must(addPiece(document, outputId, 'Far right'));
    mapped = must(
      setPieceDest(mapped, pieceNamed(mapped, 'Far right').id, {
        x: 1600,
        y: 0,
        width: 320,
        height: 1080,
      }),
    );

    const narrowed = setOutputMode(mapped, outputId, {
      size: { width: 1280, height: 720 },
      refresh: { numerator: 60, denominator: 1 },
    });
    expect(noteOf(narrowed)).toContain('Far right');
    expect(noteOf(narrowed)).toContain('entirely outside');
  });
});

describe('a piece', () => {
  it('starts as the whole composition on the whole of its output', () => {
    const { document, outputId } = withOutput();
    const piece = must(addPiece(document, outputId, 'Whole')).pieces[0]!;
    expect(piece.source_rect).toEqual({ x: 0, y: 0, width: 1920, height: 1080 });
    expect(piece.dest_rect).toEqual({ x: 0, y: 0, width: 1920, height: 1080 });
    expect(piece.order).toBe(DEFAULT_PIECE_ORDER);
    expect(piece.rotation_deg).toBe(0);
    expect(isPixelExact(piece)).toBe(true);
  });

  it('cannot be placed outside its output, because nothing of it would be drawn', () => {
    // `OutputMapping::build` skips a piece the renderer would draw nothing of,
    // so a rectangle entirely off the panel is not a mapping at all — it is a
    // rectangle in the editor with no counterpart on the wall.
    const { document, outputId } = withOutput();
    const mapped = must(addPiece(document, outputId, 'Whole'));
    const pieceId = mapped.pieces[0]!.id;

    for (const rect of [
      { x: 1920, y: 0, width: 400, height: 400 },
      { x: -400, y: 0, width: 400, height: 400 },
      { x: 0, y: 1080, width: 400, height: 400 },
      { x: 0, y: -400, width: 400, height: 400 },
    ]) {
      const refused = setPieceDest(mapped, pieceId, rect);
      expect(refused.ok, `${JSON.stringify(rect)} is entirely off the output`).toBe(false);
      expect(refused.ok === false && refused.reason).toContain('nothing of this piece would be drawn');
    }
  });

  it('may hang off an edge, which the model allows, and is told what that costs', () => {
    // Positioning a piece partly outside its output is a legitimate mapping
    // choice — `a_piece_may_hang_off_the_edge_of_its_output` in the Rust — so
    // it is reported rather than refused.
    const { document, outputId } = withOutput();
    const mapped = must(addPiece(document, outputId, 'Overhang'));
    const overhung = setPieceDest(mapped, mapped.pieces[0]!.id, {
      x: -100,
      y: 0,
      width: 400,
      height: 400,
    });

    expect(overhung.ok).toBe(true);
    expect(noteOf(overhung)).toContain('40000 px');
    expect(noteOf(overhung)).toContain('is not drawn');
  });

  it('cannot take a region the composition does not contain', () => {
    const { document, outputId } = withOutput();
    const mapped = must(addPiece(document, outputId, 'Whole'));
    const refused = setPieceSource(mapped, mapped.pieces[0]!.id, {
      x: 1000,
      y: 0,
      width: 1920,
      height: 1080,
    });

    expect(refused.ok).toBe(false);
    // The server's own words, so the message an operator reads here and the one
    // a refused save would carry are the same message.
    expect(refused.ok === false && refused.reason).toBe(
      'must lie within the 1920x1080 composition',
    );
  });

  it('cannot be given a rectangle with no pixels in it', () => {
    const { document, outputId } = withOutput();
    const mapped = must(addPiece(document, outputId, 'Whole'));
    const pieceId = mapped.pieces[0]!.id;
    expect(setPieceSource(mapped, pieceId, { x: 0, y: 0, width: 0, height: 100 }).ok).toBe(false);
    expect(setPieceDest(mapped, pieceId, { x: 0, y: 0, width: 100, height: 0 }).ok).toBe(false);
    expect(setPieceDest(mapped, pieceId, { x: 0.5, y: 0, width: 100, height: 100 }).ok).toBe(false);
  });

  it('is copied where it already is, so nobody has to undo a position nobody chose', () => {
    const { document, outputId } = withOutput();
    const mapped = must(addPiece(document, outputId, 'Panel'));
    const copied = must(duplicatePiece(mapped, mapped.pieces[0]!.id));

    expect(copied.pieces).toHaveLength(2);
    expect(copied.pieces[1]!.name).toBe('Panel copy');
    expect(copied.pieces[1]!.dest_rect).toEqual(copied.pieces[0]!.dest_rect);
    expect(copied.pieces[1]!.id).not.toBe(copied.pieces[0]!.id);
  });

  it('is refused a draw order or a rotation the document could not carry', () => {
    const { document, outputId } = withOutput();
    const mapped = must(addPiece(document, outputId, 'Whole'));
    const pieceId = mapped.pieces[0]!.id;
    expect(setPiece(mapped, pieceId, { order: -1 }).ok).toBe(false);
    expect(setPiece(mapped, pieceId, { order: 1.5 }).ok).toBe(false);
    expect(setPiece(mapped, pieceId, { rotation_deg: Number.NaN }).ok).toBe(false);
    expect(setPiece(mapped, pieceId, { rotation_deg: 90, order: 4 }).ok).toBe(true);
  });
});

describe('two pieces overlapping on one output', () => {
  /** Two pieces covering the same half of one wall. */
  function overlapping(): Show {
    const { document, outputId } = withOutput();
    let mapped = must(addPiece(document, outputId, 'Under'));
    mapped = must(addPiece(mapped, outputId, 'Over'));
    mapped = must(
      setPieceDest(mapped, pieceNamed(mapped, 'Under').id, {
        x: 0,
        y: 0,
        width: 1000,
        height: 1080,
      }),
    );
    mapped = must(
      setPieceDest(mapped, pieceNamed(mapped, 'Over').id, {
        x: 800,
        y: 0,
        width: 1000,
        height: 1080,
      }),
    );
    return mapped;
  }

  it('is not a fault, because the server takes it', () => {
    // The renderer settles it by draw order. Calling it an error would teach an
    // operator to read the red marks as advice.
    const mapped = overlapping();
    expect(validate(mapped, emptyCatalogue())).toEqual([]);
  });

  it('is reported the way the renderer resolves it: the later piece wins', () => {
    const mapped = overlapping();
    const [overlap, ...rest] = overlaps(mapped);
    expect(rest).toEqual([]);
    expect(overlap!.under.name).toBe('Under');
    expect(overlap!.over.name).toBe('Over');
    expect(overlap!.region).toEqual({ x: 800, y: 0, width: 200, height: 1080 });

    const said = advisories(mapped, CONNECTORS).map((advisory) => advisory.message);
    expect(said.join(' ')).toContain('Over is drawn over Under across 200×1080 px at 800, 0');
    expect(said.join(' ')).toContain('what reaches the wall there');
  });

  it('changes which piece wins when one is moved through the draw order', () => {
    // Draw order is the whole of what decides between them, and it is the same
    // sort `OutputMapping::build` does.
    let mapped = overlapping();
    expect(drawOrder(mapped, mapped.outputs[0]!.id).map((piece) => piece.name)).toEqual([
      'Under',
      'Over',
    ]);

    mapped = must(movePiece(mapped, pieceNamed(mapped, 'Under').id, 'later'));
    expect(drawOrder(mapped, mapped.outputs[0]!.id).map((piece) => piece.name)).toEqual([
      'Over',
      'Under',
    ]);
    expect(overlaps(mapped)[0]!.over.name).toBe('Under');
  });

  it('is not guessed at when a piece is turned, because its rectangle is not what it covers', () => {
    const mapped = overlapping();
    const turned = must(setPiece(mapped, pieceNamed(mapped, 'Over').id, { rotation_deg: 30 }));
    expect(overlaps(turned)).toEqual([]);
    expect(advisories(turned, CONNECTORS).map((advisory) => advisory.message).join(' ')).toContain(
      'is turned, so what it covers on the output is not the rectangle typed here',
    );
  });

  it('is not reported for a piece that is switched off, because it is not drawn', () => {
    const mapped = overlapping();
    const off = must(setPiece(mapped, pieceNamed(mapped, 'Over').id, { enabled: false }));
    expect(overlaps(off)).toEqual([]);
    // It keeps its place in the sequence, though: switching it back on must not
    // move it somewhere nobody put it.
    expect(sequenceOf(off, off.outputs[0]!.id).map((piece) => piece.name)).toEqual([
      'Under',
      'Over',
    ]);
  });

  it('is separated by the show order when the two share a draw order', () => {
    // The model allows two pieces at the same order and the renderer keeps the
    // order the show lists them in, so that is the thing a move has to
    // change — anything else would be an editor that appeared to do nothing.
    const mapped = overlapping();
    const outputId = mapped.outputs[0]!.id;
    expect(mapped.pieces.map((piece) => piece.order)).toEqual([
      DEFAULT_PIECE_ORDER,
      DEFAULT_PIECE_ORDER,
    ]);

    const moved = must(movePiece(mapped, pieceNamed(mapped, 'Under').id, 'later'));
    expect(moved.pieces.map((piece) => piece.name)).toEqual(['Over', 'Under']);
    expect(moved.pieces.map((piece) => piece.order)).toEqual([
      DEFAULT_PIECE_ORDER,
      DEFAULT_PIECE_ORDER,
    ]);
    expect(drawOrder(moved, outputId).map((piece) => piece.name)).toEqual(['Over', 'Under']);
  });

  it('refuses to move a piece past the end of its own output', () => {
    const mapped = overlapping();
    expect(movePiece(mapped, pieceNamed(mapped, 'Over').id, 'later').ok).toBe(false);
    expect(movePiece(mapped, pieceNamed(mapped, 'Under').id, 'earlier').ok).toBe(false);
  });
});

describe('what the server takes and the wall would still get wrong', () => {
  it('names a socket this server does not report', () => {
    const { document } = withOutput();
    const moved: Show = {
      ...document,
      outputs: [
        {
          ...document.outputs[0]!,
          identity: { gpu_pci_address: '0000:02:00.0', connector: 'DP-4' },
        },
      ],
    };
    expect(validate(moved, emptyCatalogue())).toEqual([]);
    expect(advisories(moved, CONNECTORS).map((advisory) => advisory.message).join(' ')).toContain(
      'reports no connector 0000:02:00.0/DP-4',
    );
  });

  it('says nothing about connectors at all while the server has reported none', () => {
    // An empty list means no renderer has said yet, which is not the same as a
    // machine with no sockets. Nothing here guesses at the difference.
    const { document } = withOutput();
    const said = advisories(document, []).map((advisory) => advisory.message);
    expect(said.join(' ')).not.toContain('connector');
  });

  it('says nothing about a piece being resampled, because most of them are', () => {
    // The owner, 2026-09-23: "Bei IO Mapping möchte ich keine texte wie
    // 'pieces[0].dest_rect this piece is resampled..' sehen. Das ist zu viel
    // Info."
    //
    // Any piece whose source and destination are different sizes is resampled,
    // which is most of them and is what mapping a wall *is*. The piece's own
    // row still says "resampled"; a paragraph of arithmetic under the canvas
    // for every one of them is a list nobody reads, and a list nobody reads is
    // where a real warning goes to hide.
    const { document, outputId } = withOutput();
    let mapped = must(addPiece(document, outputId, 'Panel'));
    mapped = must(
      setPieceDest(mapped, mapped.pieces[0]!.id, { x: 0, y: 0, width: 960, height: 540 }),
    );
    expect(advisories(mapped, CONNECTORS).map((advisory) => advisory.message).join(' ')).not.toContain(
      'resampled',
    );

    // And a piece that is genuinely losing part of itself still says so, which
    // is what the list is for. Hung half off the right-hand edge: the model
    // takes it — a wall can be built that way — and the wall draws less than
    // was asked for, which nothing else on the screen says.
    const spilling = must(
      setPieceDest(mapped, mapped.pieces[0]!.id, { x: 1600, y: 0, width: 960, height: 540 }),
    );
    expect(
      advisories(spilling, CONNECTORS).map((advisory) => advisory.message).join(' '),
    ).toContain('falls outside');
  });

  it('says when an output is switched off under pieces that are still mapped to it', () => {
    const { document, outputId } = withOutput();
    const mapped = must(addPiece(document, outputId, 'Panel'));
    const dark: Show = {
      ...mapped,
      outputs: [{ ...mapped.outputs[0]!, enabled: false }],
    };
    expect(advisories(dark, CONNECTORS).map((advisory) => advisory.message).join(' ')).toContain(
      'switched off, so the 1 piece on it is not drawn',
    );
  });
});

describe('a rate reported in millihertz', () => {
  it('becomes the exact rational rate the model names, where there is one', () => {
    expect(rateFromMillihertz(60_000)).toEqual({ numerator: 60, denominator: 1 });
    expect(rateFromMillihertz(59_940)).toEqual({ numerator: 60000, denominator: 1001 });
    expect(rateFromMillihertz(29_970)).toEqual({ numerator: 30000, denominator: 1001 });
    expect(rateFromMillihertz(50_000)).toEqual({ numerator: 50, denominator: 1 });
  });

  it('becomes a ratio that is exactly what was reported, where it does not', () => {
    expect(rateFromMillihertz(144_000)).toEqual({ numerator: 144, denominator: 1 });
    expect(rateFromMillihertz(23_976)).toEqual({ numerator: 2997, denominator: 125 });
    expect(millihertzOf(rateFromMillihertz(23_976)!)).toBe(23_976);
  });

  it('is nothing at all when a driver reported nothing usable', () => {
    expect(rateFromMillihertz(0)).toBeNull();
    expect(rateFromMillihertz(-1)).toBeNull();
    expect(rateFromMillihertz(Number.NaN)).toBeNull();
  });
});

describe('a document with a mapping in it', () => {
  // A show written by a server this client is not the same age as. The extra
  // fields in the composition, the output and the piece are the point: nothing
  // here understands them, and nothing here may lose them.
  const raw = JSON.stringify({
    id: '01HQ0000000000000000000001',
    schema_version: 1,
    name: 'Nachtfahrt',
    created_at: 1_700_000_000_000,
    updated_at: 1_700_000_000_000,
    composition: {
      size: { width: 1920, height: 1080 },
      render_rate: { numerator: 60, denominator: 1 },
      colour_space: 'rec709',
    },
    master: {
      default_bpm: 120,
      beats_per_bar: 4,
      default_speed_scale: 1,
      default_dimmer: 1,
      default_crossfader: 0,
      crossfader_curve: 'linear',
      tap_window_ms: 4000,
    },
    layers: [],
    pieces: [
      {
        id: '01HQ0000000000000000000003',
        name: 'Wall Left Upper',
        enabled: true,
        order: 0,
        output_id: '01HQ0000000000000000000002',
        source_rect: { x: 0, y: 0, width: 960, height: 540 },
        dest_rect: { x: 0, y: 0, width: 960, height: 540 },
        rotation_deg: 0,
        blend: 'over',
      },
    ],
    outputs: [
      {
        id: '01HQ0000000000000000000002',
        name: 'LED Wall Left',
        enabled: true,
        identity: { gpu_pci_address: '0000:01:00.0', connector: 'DP-1' },
        mode: { size: { width: 1920, height: 1080 }, refresh: { numerator: 60, denominator: 1 } },
        colour_range: 'full',
      },
    ],
    settings: { auto_save: true, notes: '' },
  });

  it('loaded and saved unchanged is byte-identical', () => {
    const draft = openDraft(JSON.parse(raw) as Show);
    expect(JSON.stringify(draft.show)).toBe(raw);
  });

  it('keeps the fields this build does not understand through a mapping edit', () => {
    const document = JSON.parse(raw) as Show;
    let edited = must(setPieceDest(document, document.pieces[0]!.id, {
      x: 960,
      y: 0,
      width: 960,
      height: 540,
    }));
    edited = must(setMaster(edited, { default_bpm: 140 }));
    edited = must(setComposition(edited, { size: { width: 1920, height: 1200 } }));

    const asRead = JSON.parse(JSON.stringify(edited)) as Record<string, unknown>;
    const composition = asRead['composition'] as Record<string, unknown>;
    const master = asRead['master'] as Record<string, unknown>;
    const piece = (asRead['pieces'] as Array<Record<string, unknown>>)[0]!;
    const output = (asRead['outputs'] as Array<Record<string, unknown>>)[0]!;

    expect(composition['colour_space']).toBe('rec709');
    expect(master['tap_window_ms']).toBe(4000);
    expect(piece['blend']).toBe('over');
    expect(output['colour_range']).toBe('full');
    expect(piece['dest_rect']).toEqual({ x: 960, y: 0, width: 960, height: 540 });
    // And still in the order they arrived in, which is what makes an unchanged
    // save byte-identical in the first place.
    expect(Object.keys(piece)).toEqual(Object.keys(JSON.parse(raw).pieces[0]));
    expect(Object.keys(output)).toEqual(Object.keys(JSON.parse(raw).outputs[0]));
    expect(Object.keys(master)).toEqual(Object.keys(JSON.parse(raw).master));
  });

  it('is valid to the same rules the server applies', () => {
    expect(validate(JSON.parse(raw) as Show, emptyCatalogue())).toEqual([]);
  });

  it('is reported at the paths the server names when its mapping is broken', () => {
    const document = JSON.parse(raw) as Show;
    const broken: Show = {
      ...document,
      pieces: [
        {
          ...document.pieces[0]!,
          name: '   ',
          source_rect: { x: 1600, y: 0, width: 960, height: 540 },
          output_id: '01HQ000000000000000000000X',
        },
      ],
    };

    const problems = validate(broken, emptyCatalogue());
    expect(problems).toContainEqual({ path: 'pieces[0].name', message: 'must not be empty' });
    expect(problems).toContainEqual({
      path: 'pieces[0].source_rect',
      message: 'must lie within the 1920x1080 composition',
    });
    expect(problems).toContainEqual({
      path: 'pieces[0].output_id',
      message: 'references output 01HQ000000000000000000000X which is not configured',
    });
  });

  it('keeps a crossfader curve that is not the default', () => {
    // The fixture says `linear` and the default is `equal_power` since the
    // owner changed it on 2026-09-23 — which makes this a better test than it
    // was. A round trip that only preserved the default would preserve nothing.
    expect((JSON.parse(raw) as Show).master.crossfader_curve).toBe('linear');
    expect(DEFAULT_CROSSFADER_CURVE).not.toBe('linear');
  });
});

describe('several pieces at once', () => {
  // The owner, 2026-09-23: "wenn man mehrere Objekte gleichzeitig markieren
  // kann und dann im richtigen Verhältnis, sprich ohne das seitenverhältnis
  // kaputt zu machen an den ecken skalieren könnte."

  const panels: RectPx[] = [
    { x: 0, y: 0, width: 100, height: 200 },
    { x: 100, y: 0, width: 100, height: 200 },
    { x: 200, y: 0, width: 100, height: 200 },
  ];

  it('takes three panels in a row as one box', () => {
    expect(boundsOf(panels)).toEqual({ x: 0, y: 0, width: 300, height: 200 });
    expect(boundsOf([])).toBeNull();
  });

  it('scales a corner by one number, so nothing changes shape', () => {
    // A drag of the bottom-right corner out and down. Both axes take the same
    // scale, which is the whole point: an LED wall's proportions are not the
    // operator's to change by a pixel while reaching for a size.
    const bounds = boundsOf(panels)!;
    const anchor = { x: bounds.x, y: bounds.y };
    const corner = { x: bounds.x + bounds.width, y: bounds.y + bounds.height };
    const scale = proportionalScale(corner, anchor, 150, 100);

    const scaled = panels.map((panel) => scaledAbout(panel, anchor, scale));
    const ratios = scaled.map((panel) => panel.width / panel.height);
    for (const ratio of ratios) {
      expect(ratio).toBeCloseTo(100 / 200, 5);
    }

    // And they still abut: the arrangement survives the gesture.
    expect(scaled[0]!.x + scaled[0]!.width).toBe(scaled[1]!.x);
    expect(scaled[1]!.x + scaled[1]!.width).toBe(scaled[2]!.x);
  });

  it('follows the pointer even when it is dragged off the diagonal', () => {
    // Pulling mostly sideways still grows the box, because what is followed is
    // the movement showed onto the arm rather than one axis of it.
    const bounds = boundsOf(panels)!;
    const anchor = { x: bounds.x, y: bounds.y };
    const corner = { x: bounds.x + bounds.width, y: bounds.y + bounds.height };

    expect(proportionalScale(corner, anchor, 100, 0)).toBeGreaterThan(1);
    expect(proportionalScale(corner, anchor, -100, 0)).toBeLessThan(1);
    expect(proportionalScale(corner, anchor, 0, 0)).toBe(1);
  });

  it('pins the far corner, so a corner drag grows towards the pointer', () => {
    const one: RectPx = { x: 50, y: 50, width: 100, height: 100 };
    const anchor = { x: 50, y: 50 };
    expect(scaledAbout(one, anchor, 2)).toEqual({ x: 50, y: 50, width: 200, height: 200 });
    expect(scaledAbout(one, anchor, 0.5)).toEqual({ x: 50, y: 50, width: 50, height: 50 });
  });

  it('never scales a rectangle away to nothing', () => {
    // `Piece::validate` refuses one, and a gesture that ends in a refusal
    // every time is not a control.
    const one: RectPx = { x: 0, y: 0, width: 100, height: 100 };
    const tiny = scaledAbout(one, { x: 0, y: 0 }, 0);
    expect(tiny.width).toBeGreaterThanOrEqual(1);
    expect(tiny.height).toBeGreaterThanOrEqual(1);
  });

  it('nudges and grows by whole pixels', () => {
    const one: RectPx = { x: 10, y: 20, width: 100, height: 50 };
    expect(nudged(one, -1, 10)).toEqual({ x: 9, y: 30, width: 100, height: 50 });
    // From the top-left, so the position somebody just typed stays put while
    // they adjust the size.
    expect(grown(one, 5, -5)).toEqual({ x: 10, y: 20, width: 105, height: 45 });
    expect(grown(one, 0, -1000).height).toBeGreaterThanOrEqual(1);
  });
});
describe('a mapping made live', () => {
  // The owner, 2026-09-23: the mapping is to be made live, so the output being
  // changed is the output being looked at. Every rectangle reaches the wall as
  // it is dragged, which means the way out of a session of dragging is not an
  // undo of the last gesture but a restore of what the session opened with.

  it('puts every rectangle back where the session found it', () => {
    const { document: opened, outputId } = withOutput();
    const start = must(addPiece(opened, outputId, 'Left'));
    const before = { outputs: start.outputs, pieces: start.pieces };

    // Half an hour of lining a wall up: one piece moved and another added, all
    // of it already on the wall.
    const moved = must(
      setPieceDest(start, pieceNamed(start, 'Left').id, {
        x: 37,
        y: 12,
        width: 900,
        height: 1000,
      }),
    );
    const grownBy = must(addPiece(moved, outputId, 'Right'));

    const back = must(restoreMapping(grownBy, before.outputs, before.pieces));

    expect(back.pieces).toEqual(before.pieces);
    expect(back.outputs).toEqual(before.outputs);
  });

  it('brings an output back with the pieces that were drawing to it', () => {
    // The two travel together for the same reason the request carries both: a
    // mapping restored piece by piece would, for a moment, name a wall that is
    // not there.
    const { document: opened, outputId } = withOutput();
    const start = must(addPiece(opened, outputId, 'Left'));
    const before = { outputs: start.outputs, pieces: start.pieces };

    const without = must(removeOutput(start, outputId));
    expect(without.pieces).toHaveLength(0);

    const back = must(restoreMapping(without, before.outputs, before.pieces));
    expect(back.outputs).toHaveLength(1);
    expect(back.pieces).toHaveLength(1);
    expect(validate(back, emptyCatalogue())).toEqual([]);
  });

  it('leaves everything that is not the mapping alone', () => {
    // It is one edit against the document in hand, not a reload: a name typed
    // in the Show tab while the wall was being lined up is still typed.
    const { document: opened, outputId } = withOutput();
    const start = must(addPiece(opened, outputId, 'Left'));
    const renamed = { ...start, name: 'Nachtfahrt II' };

    const back = must(restoreMapping(renamed, start.outputs, start.pieces));

    expect(back.name).toBe('Nachtfahrt II');
    expect(back.master).toEqual(start.master);
    expect(back.composition).toEqual(start.composition);
    expect(back.settings).toEqual(start.settings);
  });
});

describe('snapping one piece to another', () => {
  // The owner, 2026-09-23: "ein Snap Tool. Das sollte innerhalb von 5px Ecke an
  // Ecke einrasten, egal welche Rotation. Optional auch an Kanten einrasten auf
  // einer Achse und auch Mittelpunkte an jeder Kante sollte ein Einrastpunkt
  // sein."

  const panel: RectPx = { x: 0, y: 0, width: 200, height: 100 };

  it('offers nine points: the corners, the edge middles and the centre', () => {
    const points = snapPointsOf(panel);
    expect(points).toHaveLength(9);
    expect(points.slice(0, 4)).toEqual([
      { x: 0, y: 0 },
      { x: 200, y: 0 },
      { x: 200, y: 100 },
      { x: 0, y: 100 },
    ]);
    // The middle of each edge, in the order the edges run.
    expect(points.slice(4, 8)).toEqual([
      { x: 100, y: 0 },
      { x: 200, y: 50 },
      { x: 100, y: 100 },
      { x: 0, y: 50 },
    ]);
    expect(points[8]).toEqual({ x: 100, y: 50 });
  });

  it('turns its corners about the anchor, so a piece at an angle offers the corners it has', () => {
    // A quarter turn about the middle of a 200x100 panel: the corners swap
    // their arms, which is a shape 100 wide and 200 tall about the same centre.
    const turned = cornersOf(panel, 90);
    expect(turned[0]!.x).toBeCloseTo(150, 5);
    expect(turned[0]!.y).toBeCloseTo(-50, 5);
    expect(turned[2]!.x).toBeCloseTo(50, 5);
    expect(turned[2]!.y).toBeCloseTo(150, 5);

    // And about a corner it stays put, which is the point of an anchor.
    const hinged = cornersOf(panel, 90, { x: 0, y: 0 });
    expect(hinged[0]).toEqual({ x: 0, y: 0 });
  });

  it('brings a corner onto a corner when both axes are within reach', () => {
    const moving = snapPointsOf({ x: 203, y: 2, width: 200, height: 100 });
    const fixed = snapPointsOf(panel);
    expect(snapTranslation(moving, fixed, SNAP_WITHIN_PX)).toEqual({ dx: -3, dy: -2 });
  });

  it('slides along one axis when only that one lines up', () => {
    // Level with the panel's top edge, but a long way along it: the pieces
    // line up on y and nothing pulls on x, which is what edge snapping is.
    const moving = snapPointsOf({ x: 900, y: 3, width: 200, height: 100 });
    const fixed = snapPointsOf(panel);
    expect(snapTranslation(moving, fixed, SNAP_WITHIN_PX)).toEqual({ dx: 0, dy: -3 });
  });

  it('takes the nearest point on each axis, not the first it meets', () => {
    const fixed = [
      { x: 0, y: 0 },
      { x: 4, y: 0 },
    ];
    expect(snapTranslation([{ x: 3, y: 0 }], fixed, SNAP_WITHIN_PX)).toEqual({ dx: 1, dy: 0 });
  });

  it('pulls on nothing that is out of reach', () => {
    const moving = snapPointsOf({ x: 900, y: 900, width: 10, height: 10 });
    expect(snapTranslation(moving, snapPointsOf(panel), SNAP_WITHIN_PX)).toEqual({ dx: 0, dy: 0 });
  });

  it('snaps to the surface itself, which is what a piece is lined up against most', () => {
    const wall = surfacePoints({ width: 1920, height: 1080 });
    // A piece three pixels shy of the bottom-right corner of the wall.
    const moving = snapPointsOf({ x: 1717, y: 977, width: 200, height: 100 });
    expect(snapTranslation(moving, wall, SNAP_WITHIN_PX)).toEqual({ dx: 3, dy: 3 });
  });

  it('finds a turned corner beside a square one', () => {
    // The whole of "egal welche Rotation": a panel hung at forty-five degrees
    // has a corner somewhere the rectangle's own numbers never mention, and
    // that is the corner an operator is lining up against.
    const hinged = snapPointsOf({ x: 0, y: 0, width: 100, height: 100 }, 45);
    const lowest = hinged.reduce((best, point) => (point.y > best.y ? point : best));
    const moving = snapPointsOf({
      x: Math.round(lowest.x) + 2,
      y: Math.round(lowest.y) + 2,
      width: 40,
      height: 40,
    });
    const snap = snapTranslation(moving, hinged, SNAP_WITHIN_PX);
    expect(Math.abs(snap.dx)).toBeLessThanOrEqual(SNAP_WITHIN_PX);
    expect(Math.abs(snap.dy)).toBeLessThanOrEqual(SNAP_WITHIN_PX);
    expect(snap.dx === 0 && snap.dy === 0).toBe(false);
  });
});

describe('the anchor as a piece\u2019s origin', () => {
  // The owner, 2026-09-23: "Wenn ich den Anchor auf Center setze, ist mein xy
  // Nullpunkt immer noch oben Links und meine Breite und Höhe geht auch von
  // Oben Links aus als eigentlich von der Mitte aus. Der Anchor soll genau das
  // machen wofür er da ist."

  const panel: RectPx = { x: 100, y: 200, width: 400, height: 200 };
  const topLeft = { x: 0, y: 0 };
  const bottomRight = { x: 1, y: 1 };

  it('says where the piece is, from the anchor rather than from the corner', () => {
    expect(anchorOf(panel, CENTRE_ANCHOR)).toEqual({ x: 300, y: 300 });
    expect(anchorOf(panel, topLeft)).toEqual({ x: 100, y: 200 });
    expect(anchorOf(panel, bottomRight)).toEqual({ x: 500, y: 400 });
  });

  it('puts the anchor where it is told, and the rectangle follows', () => {
    const moved = placedAt(panel, CENTRE_ANCHOR, { x: 960, y: 540 });
    expect(anchorOf(moved, CENTRE_ANCHOR)).toEqual({ x: 960, y: 540 });
    expect(moved).toEqual({ x: 760, y: 440, width: 400, height: 200 });

    // The same point asked of a top-left anchor is the corner itself.
    expect(placedAt(panel, topLeft, { x: 960, y: 540 })).toEqual({
      x: 960,
      y: 540,
      width: 400,
      height: 200,
    });
  });

  it('grows about the anchor rather than off the left edge', () => {
    // Twice as wide about the centre: the middle does not move, both edges do.
    const wider = sizedAbout(panel, CENTRE_ANCHOR, 800, 200);
    expect(anchorOf(wider, CENTRE_ANCHOR)).toEqual({ x: 300, y: 300 });
    expect(wider).toEqual({ x: -100, y: 200, width: 800, height: 200 });

    // And anchored top-left it grows to the right, which is the other thing an
    // anchor is for.
    expect(sizedAbout(panel, topLeft, 800, 200)).toEqual({
      x: 100,
      y: 200,
      width: 800,
      height: 200,
    });
  });

  it('drags an edge about the anchor, so a centred piece opens both ways', () => {
    // The right edge pulled fifty out. About the centre the piece grows a
    // hundred — fifty each way — and its middle has not moved.
    const pulled = resizedAbout(panel, CENTRE_ANCHOR, 'e', 50, 0);
    expect(pulled.width).toBe(500);
    expect(anchorOf(pulled, CENTRE_ANCHOR)).toEqual({ x: 300, y: 300 });

    // The same pull on a piece anchored top-left moves one edge, which is the
    // behaviour everybody already knows.
    const familiar = resizedAbout(panel, topLeft, 'e', 50, 0);
    expect(familiar).toEqual({ x: 100, y: 200, width: 450, height: 200 });
  });

  it('leaves an edge the anchor is standing on alone', () => {
    // There is nothing to the right of an anchor on the right edge to lengthen,
    // and the arithmetic for it is a division by nought.
    const pinned = resizedAbout(panel, { x: 1, y: 0.5 }, 'e', 50, 0);
    expect(pinned).toEqual(panel);
  });

  it('keeps a corner drag about the anchor too', () => {
    // Both axes at once, which is what a corner is.
    const grown = resizedAbout(panel, CENTRE_ANCHOR, 'se', 50, 25);
    expect(grown.width).toBe(500);
    expect(grown.height).toBe(250);
    expect(anchorOf(grown, CENTRE_ANCHOR)).toEqual({ x: 300, y: 300 });
  });

  it('never shrinks a piece out of existence', () => {
    const tiny = resizedAbout(panel, CENTRE_ANCHOR, 'e', -10_000, 0);
    expect(tiny.width).toBeGreaterThan(0);
    expect(tiny.height).toBe(200);
  });
});

describe('what a snap says about itself', () => {
  // The owner, 2026-09-23: "Markiere bitte den Punkt oder die Linie wo ein Snap
  // greift… eine Magenta Linie für Linie an Linie oder Punkt für Ecke an Ecke."
  //
  // Which means the arithmetic has to say *which* it was. A line and a dot are
  // different claims: one says two edges met on an axis, the other says a
  // corner met a corner.

  const panel: RectPx = { x: 0, y: 0, width: 200, height: 100 };

  it('names the corner when one point answers both axes', () => {
    const moving = snapPointsOf({ x: 203, y: 2, width: 200, height: 100 });
    const guides = snapGuides(moving, snapPointsOf(panel), SNAP_WITHIN_PX);

    expect(guides).toMatchObject({ dx: -3, dy: -2, atX: 200, atY: 0 });
    expect(guides.corner).toEqual({ x: 200, y: 0 });
  });

  it('names the line when only one axis answers', () => {
    const moving = snapPointsOf({ x: 900, y: 3, width: 200, height: 100 });
    const guides = snapGuides(moving, snapPointsOf(panel), SNAP_WITHIN_PX);

    expect(guides).toMatchObject({ dx: 0, dy: -3, atX: null, atY: 0 });
    expect(guides.corner).toBeNull();
  });

  it('claims no corner when two axes caught on two different points', () => {
    // One fixed point supplies the x, another the y. They cross, but nothing
    // is *at* the crossing, and a dot there would say something untrue.
    const fixed = [
      { x: 100, y: 5000 },
      { x: 5000, y: 100 },
    ];
    const guides = snapGuides([{ x: 102, y: 98 }], fixed, SNAP_WITHIN_PX);

    expect(guides).toMatchObject({ dx: -2, dy: 2, atX: 100, atY: 100 });
    expect(guides.corner).toBeNull();
  });

  it('says nothing when nothing is within reach', () => {
    const guides = snapGuides(
      snapPointsOf({ x: 900, y: 900, width: 10, height: 10 }),
      snapPointsOf(panel),
      SNAP_WITHIN_PX,
    );
    expect(guides).toEqual({ dx: 0, dy: 0, atX: null, atY: null, corner: null });
  });
});

describe('an output that goes on the network', () => {
  const mode = { size: { width: 1280, height: 720 }, refresh: { numerator: 60, denominator: 1 } };

  it('publishes under its own name, and takes no socket', () => {
    const document = must(addNetworkOutput(show(), 'Wall Left', mode));
    const output = document.outputs[0]!;
    // One name. The server puts its own machine in front of it, so storing the
    // announced name would rename every stream in a show the day it moved.
    expect(output.name).toBe('Wall Left');
    expect(output.on_the_network).toBe(true);
    // An output cannot be a socket and a stream at once, and the server refuses
    // a document that claims both.
    expect(output.identity).toEqual({});
    expect(output.mode.size).toEqual({ width: 1280, height: 720 });
  });

  it('refuses a stream nobody could find', () => {
    expect(noteOf(addNetworkOutput(show(), '   ', mode))).toContain('refused');
  });

  it('refuses a second output publishing the same name', () => {
    // The fault this catches would be found last of all: everything works, both
    // streams are on the network, and whoever picks one in another room has no
    // way to tell which of the two they got.
    const document = must(addNetworkOutput(show(), 'Programme', mode));
    expect(noteOf(addNetworkOutput(document, 'Programme', mode))).toContain('refused');
    expect(noteOf(addNetworkOutput(document, '  Programme  ', mode))).toContain('refused');
  });

  it('refuses a rename onto another stream, because that is a rename out there', () => {
    let document = must(addNetworkOutput(show(), 'Programme', mode));
    document = must(addNetworkOutput(document, 'Preview', mode));
    const first = document.outputs[0]!.id;

    document = must(renameOutput(document, first, 'Main programme'));
    expect(document.outputs[0]!.name).toBe('Main programme');

    expect(noteOf(renameOutput(document, first, 'Preview'))).toContain('refused');
  });

  it('moves a wall onto the network and back', () => {
    // Which is the whole reason the destination is a field and not a decision
    // made once: a wall unplugged for an evening has a feed that still has to
    // go somewhere, and it keeps its pieces either way (owner, 2026-09-25).
    const { document, outputId } = withOutput();
    expect(document.outputs[0]!.on_the_network).not.toBe(true);

    const published = must(setOutputOnNetwork(document, outputId));
    expect(published.outputs[0]!.on_the_network).toBe(true);
    expect(published.outputs[0]!.identity).toEqual({});

    const back = must(
      setOutputConnector(published, outputId, CONNECTORS[0]!.gpu_pci_address, CONNECTORS[0]!.connector),
    );
    expect(back.outputs[0]!.on_the_network).toBe(false);
    expect(back.outputs[0]!.identity.connector).toBe(CONNECTORS[0]!.connector);
  });

  it('refuses to publish a wall under a name already on the network', () => {
    let document = must(addNetworkOutput(show(), 'Wall', mode));
    document = must(addOutput(document, CONNECTORS[0]!, CONNECTORS[0]!.modes![0]!, 'Wall'));
    const wall = document.outputs[1]!.id;
    expect(noteOf(setOutputOnNetwork(document, wall))).toContain('refused');
  });
});

describe('the OUTPUT stage', () => {
  /** A show with two walls, each carrying one piece. */
  // A second wall of its own: CONNECTORS[1] is the Service Display, which
  // `addOutput` refuses on purpose — a socket carrying the System screen is not
  // one a show may have.
  const SECOND: ConnectorInfo = {
    gpu_pci_address: '0000:01:00.0',
    connector: 'DP-4',
    connected: true,
    model: 'Internal Display',
    role: 'MEDIA_OUTPUT',
    modes: [{ size: { width: 2560, height: 1440 }, refresh_millihertz: 60_000 }],
  };

  const twoWalls = (): Show => {
    let built = must(addOutput(show(), CONNECTORS[0]!, CONNECTORS[0]!.modes![0]!, 'First'));
    built = must(addOutput(built, SECOND, SECOND.modes![0]!, 'Second'));
    built = must(addPiece(built, built.outputs[0]!.id, 'On the first'));
    built = must(addPiece(built, built.outputs[1]!.id, 'On the second'));
    return built;
  };

  it('draws the wall that is selected', () => {
    // **The fault this is here for** (owner, 2026-09-25: "wenn ich im IO Mapping
    // auf den Monitor klicke, wird immer nur der erste Monitor angezeigt").
    // Selecting a wall clears the selected piece, so reading the piece alone left
    // the stage on the first wall in the list.
    const show = twoWalls();
    const second = show.outputs[1]!;

    expect(stageOutput(show, second.id, null)?.id).toBe(second.id);
  });

  it('draws the wall the selected piece is on when no wall is selected', () => {
    // How an operator dragging a piece gets that piece's wall without choosing
    // it separately.
    const show = twoWalls();
    const onTheSecond = show.pieces.find((piece) => piece.output_id === show.outputs[1]!.id)!;

    expect(stageOutput(show, null, onTheSecond.id)?.id).toBe(show.outputs[1]!.id);
  });

  it('draws the first wall when nothing is selected', () => {
    // A show that has just been opened shows something rather than an empty
    // frame.
    const show = twoWalls();
    expect(stageOutput(show, null, null)?.id).toBe(show.outputs[0]!.id);
  });

  it('draws nothing when there are no walls', () => {
    // Which is what "No outputs" is for, and is different from "the first of
    // none".
    expect(stageOutput(show(), null, null)).toBeNull();
  });

  it('falls back when the selection names something the show no longer has', () => {
    // Deleting a wall leaves whatever was selected pointing at nothing, and the
    // stage has to land somewhere rather than on undefined.
    const show = twoWalls();
    expect(stageOutput(show, newId(), null)?.id).toBe(show.outputs[0]!.id);
    expect(stageOutput(show, null, newId())?.id).toBe(show.outputs[0]!.id);
  });
});
