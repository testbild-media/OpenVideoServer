// The DMX-over-IP patch: what a console reaches, and where.
//
// A fixture *definition* is a published personality — a stable type, a
// footprint and a channel layout. A fixture *instance* binds one of those to
// something in this show, a universe and a start address. This
// module is every edit an operator can make to that list of instances, as plain
// functions with no window and no network.
//
// It lives in the show document, so it is saved the way everything else is:
// whole document, server validates, refusal reported against the fields it
// names. None of it is live control. A patch says which channel moves the master
// dimmer; what the dimmer is doing right now is the console's and Live Mode's,
// and nothing here reads or writes it.
//
// Four things it is careful about.
//
// **Nothing about a personality is written here.** The channel count, the
// footprint, the channel names and what each one controls all come from
// `GET /fixtures`. A patch editor that had its own idea of how wide `ovs.layer`
// is would be one that patches the next fixture on top of it.
//
// **An address suggestion is the server's answer, not arithmetic done here.**
// `GET /shows/{id}/patch/address` knows what is already patched; this module
// takes what it says and does not check it. The one thing worth knowing about
// it is that it answers from the show *on disk*, so a suggestion knows
// nothing about unsaved patch changes — which is why the panel says so.
//
// **A collision is reported, not refused.** Two fixtures overlapping in one
// universe makes the show unsaveable, so it has to be caught before the save
// — but it is a relationship between two entries rather than a bad number in
// one, and the fix may be to move either of them. So the edit goes through with
// a note, and validation marks *both* fixtures until one of them moves. A
// control that refused the second address would leave an operator rearranging a
// rig unable to put anything down.
//
// **A number the server would refuse is refused at the control.** A universe
// outside the field it is stored in, an address below one, and a fixture whose
// footprint would run off the end of a universe are all stopped here, because
// each is a single bad value with an obvious correction.

import type {
  AddressSuggestionResponse,
  EffectInstance,
  FixtureInstance,
  FixtureTarget,
  Layer,
  Personality,
  PersonalityChannel,
  Show,
  ValidationError,
} from '@ovs/schema';

import type { Edit } from './editor.js';
import { newId } from './ids.js';
import {
  MAX_DMX_ADDRESS,
  MAX_UNIVERSE,
  MIN_DMX_ADDRESS,
  MIN_FIXTURE_VERSION,
  MIN_UNIVERSE,
} from './limits.js';
import type { Advisory } from './mapping.js';

// `Applied` and `Refused` are the editor's, and so is `Edit`; these two are
// written out again rather than imported so that the runtime dependency runs
// one way only — the editor reaches into the patch for validation, and the
// patch reaches back for nothing at all.

function applied(show: Show, note?: string): Edit {
  return note === undefined ? { ok: true, show } : { ok: true, show, note };
}

function refused(reason: string): Edit {
  return { ok: false, reason };
}

// --- the catalogue -----------------------------------------------------------

/** The personalities this server publishes (`GET /fixtures`). */
export interface FixtureCatalogue {
  /**
   * Whether the catalogue has been fetched.
   *
   * Before it has, nothing can be checked against it, and reporting that a
   * personality is unpublished would be reporting this client's own ignorance
   * as a fault in the operator's document.
   */
  loaded: boolean;
  fixtures: Personality[];
}

/** A catalogue that has been asked for nothing yet. */
export function emptyFixtures(): FixtureCatalogue {
  return { loaded: false, fixtures: [] };
}

/**
 * Returns the published personality of a type, or null.
 *
 * By type alone. A personality belongs to a module, and there is one module per
 * type with no versions: "Es gibt kein Invert V2.. nirgendwo. Es ist Invert!"
 * (owner, 2026-10-18). A patch entry saved before that may still say
 * `version: 2`; it is the same fixture as the one the catalogue publishes, and
 * the number is data the document keeps rather than a second personality to
 * look for.
 */
export function findPersonality(
  catalogue: FixtureCatalogue,
  typeId: string,
): Personality | null {
  return catalogue.fixtures.find((personality) => personality.type_id === typeId) ?? null;
}

/** The personalities that can drive the show master. */
export function masterPersonalities(catalogue: FixtureCatalogue): Personality[] {
  return catalogue.fixtures.filter((personality) => personality.target === 'master');
}

/** The personalities that can drive one layer. */
export function layerPersonalities(catalogue: FixtureCatalogue): Personality[] {
  return catalogue.fixtures.filter((personality) => personality.target === 'layer');
}

/**
 * The personalities a module publishes, which is anything that is not the
 * master's or a layer's.
 *
 * Written as "everything else" rather than as a list, because an effect's
 * personality comes from that effect's own manifest and the target it names is
 * not a value this build's enum has. Today there are none and the panel says
 * so; the day one appears it arrives here without this file being touched.
 */
export function modulePersonalities(catalogue: FixtureCatalogue): Personality[] {
  return catalogue.fixtures.filter(
    (personality) => personality.target !== 'master' && personality.target !== 'layer',
  );
}

/**
 * The personalities that can drive one effect instance.
 *
 * By type alone, for the reason `findPersonality` gives: an instance whose
 * document still says `version: 2` is driven by the one personality its type
 * publishes, because there is no other one for it to mean.
 *
 * Separate from the panel so the matching rule is a test rather than something
 * only a running client can show. It had never been tested, and an effect that
 * matched nothing was dropped from the list without a word — twice, once here
 * and once again when the tiles were drawn.
 */
export function effectPersonalities(catalogue: FixtureCatalogue, typeId: string): Personality[] {
  return modulePersonalities(catalogue).filter((personality) => personality.type_id === typeId);
}

/**
 * Whether a personality fits what a patch entry points at.
 *
 * `FixtureDefinition::drives`, which the server checks a saved patch against: a
 * layer personality aimed at the master would give one layer's opacity fader the
 * master dimmer.
 */
export function drives(personality: Personality, target: FixtureTarget): boolean {
  switch (target.kind) {
    case 'master':
      return personality.target === 'master';
    case 'layer':
      return personality.target === 'layer';
    case 'effect':
      return personality.target === 'effect';
    case 'generator':
      return personality.target === 'generator';
  }
}

// --- reading the patch -------------------------------------------------------

/** The patch a show carries. Absent and empty mean the same thing. */
export function readPatch(show: Show): FixtureInstance[] {
  return show.fixture_patch ?? [];
}

/** Returns a patch entry by identifier, or null. */
export function findFixture(show: Show, fixtureId: string): FixtureInstance | null {
  return readPatch(show).find((fixture) => fixture.id === fixtureId) ?? null;
}

/** The path of a patch entry, in the server's words, or null. */
export function fixturePath(show: Show, fixtureId: string): string | null {
  const index = readPatch(show).findIndex((fixture) => fixture.id === fixtureId);
  return index < 0 ? null : `fixture_patch[${index}]`;
}

/** The inclusive one-based channels a fixture occupies. */
export interface ChannelRange {
  first: number;
  last: number;
}

/**
 * The channels a fixture occupies, or null when it does not fit.
 *
 * `FixtureInstance::channel_range`. Null is the answer for address zero, for a
 * footprint of nothing, and for a fixture that would run off the end of its
 * universe — all three are things the server refuses rather than trims.
 */
export function channelRange(
  fixture: Pick<FixtureInstance, 'start_address' | 'footprint'>,
): ChannelRange | null {
  const { start_address: start, footprint } = fixture;
  if (!Number.isInteger(start) || !Number.isInteger(footprint)) {
    return null;
  }
  if (start < MIN_DMX_ADDRESS || footprint < 1) {
    return null;
  }
  const last = start + footprint - 1;
  return last > MAX_DMX_ADDRESS ? null : { first: start, last };
}

/**
 * Whether two patch entries overlap in one universe.
 *
 * `FixtureInstance::collides_with`. A disabled entry never collides, so an
 * operator can park a spare patch without having to delete it — and its channels
 * go back to being free.
 */
export function collides(a: FixtureInstance, b: FixtureInstance): boolean {
  if (!a.enabled || !b.enabled || a.universe !== b.universe) {
    return false;
  }
  const left = channelRange(a);
  const right = channelRange(b);
  if (left === null || right === null) {
    return false;
  }
  return left.first <= right.last && right.first <= left.last;
}

/** Two patch entries that want the same channels, and which ones. */
export interface Collision {
  a: FixtureInstance;
  b: FixtureInstance;
  /** The channels both claim. */
  region: ChannelRange;
}

/** Every pair of patch entries whose channels overlap, each pair once. */
export function collisions(show: Show): Collision[] {
  const patch = readPatch(show);
  const found: Collision[] = [];
  for (const [index, a] of patch.entries()) {
    for (const b of patch.slice(index + 1)) {
      if (!collides(a, b)) {
        continue;
      }
      const left = channelRange(a);
      const right = channelRange(b);
      if (left === null || right === null) {
        continue;
      }
      found.push({
        a,
        b,
        region: {
          first: Math.max(left.first, right.first),
          last: Math.min(left.last, right.last),
        },
      });
    }
  }
  return found;
}

/** Every universe this patch touches, lowest first. */
export function universesOf(show: Show): number[] {
  return [...new Set(readPatch(show).map((fixture) => fixture.universe))].sort(
    (a, b) => a - b,
  );
}

/** The entries in one universe, in address order. */
export function fixturesIn(show: Show, universe: number): FixtureInstance[] {
  return readPatch(show)
    .filter((fixture) => fixture.universe === universe)
    .sort((a, b) => a.start_address - b.start_address);
}

/** The patch entry pointing at one layer, or null. */
export function fixtureForLayer(show: Show, layerId: string): FixtureInstance | null {
  return (
    readPatch(show).find(
      (fixture) => fixture.target.kind === 'layer' && fixture.target.layer_id === layerId,
    ) ?? null
  );
}

/** The show's master fixture, or null when nothing is patched to it. */
export function masterFixture(show: Show): FixtureInstance | null {
  return readPatch(show).find((fixture) => fixture.target.kind === 'master') ?? null;
}

/** The patch entry pointing at one effect instance, or null. */
export function fixtureForEffect(show: Show, effectId: string): FixtureInstance | null {
  return (
    readPatch(show).find(
      (fixture) =>
        fixture.target.kind === 'effect' && fixture.target.effect_instance_id === effectId,
    ) ?? null
  );
}

/** One effect instance in the document, and where it sits. */
export interface EffectPlace {
  instance: EffectInstance;
  layer: Layer;
  /** Left out for a layer's own chain. */
  slotIndex?: number;
}

/** Every effect instance in the document, layer chains first. */
export function effectPlaces(show: Show): EffectPlace[] {
  const found: EffectPlace[] = [];
  for (const layer of show.layers) {
    for (const instance of layer.effects) {
      found.push({ instance, layer });
    }
    for (const slot of layer.slots) {
      for (const instance of slot.effects) {
        found.push({ instance, layer, slotIndex: slot.index });
      }
    }
  }
  return found;
}

/**
 * What a patch entry drives, in words.
 *
 * A layer that is not in this show is named by its identifier rather than
 * invented: that is a document written elsewhere, validation says so, and there
 * is nothing here that knows what the layer was called.
 */
export function describeTarget(show: Show, target: FixtureTarget): string {
  switch (target.kind) {
    case 'master':
      return 'Master';
    case 'layer': {
      const layer = show.layers.find((each) => each.id === target.layer_id);
      return layer ? `Layer · ${layer.name}` : `Layer ${target.layer_id}`;
    }
    case 'effect': {
      const place = effectPlaces(show).find(
        (each) => each.instance.id === target.effect_instance_id,
      );
      if (!place) {
        return `Effect ${target.effect_instance_id}`;
      }
      const where =
        place.slotIndex === undefined
          ? place.layer.name
          : `${place.layer.name}, slot ${place.slotIndex}`;
      return `Effect · ${place.instance.type_id} on ${where}`;
    }
    case 'generator': {
      const place = generatorPlaces(show).find(
        (each) => each.instanceId === target.generator_instance_id,
      );
      if (!place) {
        return `Generator ${target.generator_instance_id}`;
      }
      return `Generator · ${place.typeId} on ${place.layer.name}, slot ${place.slotIndex}`;
    }
  }
}

/**
 * Every generator instance in a show, with where it is.
 *
 * A generator is a slot's source rather than a chain entry, so there is one per
 * slot at most and its place is the slot's.
 */
export function generatorPlaces(show: Show): GeneratorPlace[] {
  const places: GeneratorPlace[] = [];
  for (const layer of show.layers) {
    for (const slot of layer.slots ?? []) {
      if (slot.source.kind === 'generator') {
        places.push({
          layer,
          slotIndex: slot.index,
          // Absent on a show written before generators had an identity;
          // the server mints one on its next save, and until then there is
          // nothing a patch could point at.
          instanceId: slot.source.instance_id ?? '',
          typeId: slot.source.type_id,
        });
      }
    }
  }
  return places;
}

/** One generator instance in the document, and where it sits. */
export interface GeneratorPlace {
  layer: Layer;
  slotIndex: number;
  instanceId: string;
  /**
   * The generator's type, which is all a personality is matched by.
   *
   * The slot's `version` is left behind on purpose. There is one module per
   * type and it has no versions (owner, 2026-10-18), so a show that saved its
   * strobe as `version: 2` is patched with the channels of the one Strobe the
   * catalogue publishes.
   */
  typeId: string;
}

/**
 * The personalities that can drive one generator instance.
 *
 * The twin of [`effectPersonalities`]. A generator is a slot's source rather
 * than a chain entry, and the catalogue treats both as modules, so the lookup
 * is the same and only where it is found differs.
 */
export function generatorPersonalities(
  catalogue: FixtureCatalogue,
  typeId: string,
): Personality[] {
  return modulePersonalities(catalogue).filter((personality) => personality.type_id === typeId);
}

/** What a patch row calls a fixture: the personality's name, or its type. */
export function fixtureName(fixture: FixtureInstance, catalogue: FixtureCatalogue): string {
  return findPersonality(catalogue, fixture.type_id)?.name ?? fixture.type_id;
}

/**
 * The absolute address of one channel of a patched fixture.
 *
 * What somebody programming a desk writes down. A personality's offsets are
 * one-based within the fixture, so the fixture's own start address is channel
 * one of it.
 */
export function absoluteChannel(fixture: FixtureInstance, channel: PersonalityChannel): number {
  return fixture.start_address + channel.offset - 1;
}

// --- editing the patch -------------------------------------------------------

/** Where a fixture sits. */
export interface PatchAt {
  universe: number;
  startAddress: number;
}

/**
 * Patches a personality to something in this show.
 *
 * Everything about the fixture comes from the personality the server published:
 * the type, the footprint, and the `version` field the document stores, which
 * is the catalogue's own number written down as data. Nothing about it is a
 * number chosen here, which is what lets a personality this build has never
 * heard of be patched correctly.
 */
export function patchFixture(
  show: Show,
  catalogue: FixtureCatalogue,
  personality: Personality,
  target: FixtureTarget,
  at: PatchAt,
): Edit {
  if (!findPersonality(catalogue, personality.type_id)) {
    return refused(`this server does not publish ${personality.type_id}`);
  }
  if (!drives(personality, target)) {
    return refused(`${personality.type_id} cannot drive what this patch entry points at`);
  }

  const wrong = refuseTarget(show, target);
  if (wrong !== null) {
    return refused(wrong);
  }
  const bad = refuseAt(at, personality.footprint);
  if (bad !== null) {
    return refused(bad);
  }

  const fixture: FixtureInstance = {
    id: newId(),
    type_id: personality.type_id,
    version: personality.version,
    target,
    universe: at.universe,
    start_address: at.startAddress,
    footprint: personality.footprint,
    enabled: true,
  };

  const next: Show = { ...show, fixture_patch: [...readPatch(show), fixture] };
  return applied(next, collisionNote(next, fixture));
}

/** Takes a fixture out of the patch. Nothing else in the document changes. */
export function unpatchFixture(show: Show, fixtureId: string): Edit {
  if (!findFixture(show, fixtureId)) {
    return refused('that fixture is not in this patch');
  }
  return applied({
    ...show,
    fixture_patch: readPatch(show).filter((fixture) => fixture.id !== fixtureId),
  });
}

/**
 * Moves a fixture to another universe or another address.
 *
 * Nothing else about the entry changes: the identifier a console's show file
 * never sees, the personality, the version, what it drives and whether it is
 * enabled all stay exactly as they were, along with anything in the entry this
 * build has never heard of. Re-addressing a fixture is one of the two things
 * this page exists for, and an operator doing it has not asked to repatch it.
 */
export function moveFixture(show: Show, fixtureId: string, to: PatchAt): Edit {
  const fixture = findFixture(show, fixtureId);
  if (!fixture) {
    return refused('that fixture is not in this patch');
  }
  const bad = refuseAt(to, fixture.footprint);
  if (bad !== null) {
    return refused(bad);
  }

  const moved: FixtureInstance = {
    ...fixture,
    universe: to.universe,
    start_address: to.startAddress,
  };
  const next: Show = {
    ...show,
    fixture_patch: readPatch(show).map((each) => (each.id === fixtureId ? moved : each)),
  };
  return applied(next, collisionNote(next, moved));
}

/**
 * Parks a fixture, or puts it back.
 *
 * A parked entry keeps its universe and address and holds no channels, which is
 * what parking is for: an operator takes a fixture out of the rig for a night
 * without losing where it was, and the channels are free for something else
 * meanwhile.
 */
export function setFixtureEnabled(show: Show, fixtureId: string, enabled: boolean): Edit {
  const fixture = findFixture(show, fixtureId);
  if (!fixture) {
    return refused('that fixture is not in this patch');
  }

  const changed: FixtureInstance = { ...fixture, enabled };
  const next: Show = {
    ...show,
    fixture_patch: readPatch(show).map((each) => (each.id === fixtureId ? changed : each)),
  };
  return applied(next, enabled ? collisionNote(next, changed) : undefined);
}

/**
 * Why a universe number cannot be stored, or null.
 *
 * Exported because the control has to refuse it too: a number the server would
 * not take should not survive being typed, and the field it is stored in is the
 * whole of the rule.
 */
export function refuseUniverse(universe: number): string | null {
  return Number.isInteger(universe) && universe >= MIN_UNIVERSE && universe <= MAX_UNIVERSE
    ? null
    : `a universe is a whole number between ${MIN_UNIVERSE} and ${MAX_UNIVERSE}`;
}

/** Why a fixture of this size cannot start at this address, or null. */
export function refuseAddress(startAddress: number, footprint: number): string | null {
  if (!Number.isInteger(startAddress) || startAddress < MIN_DMX_ADDRESS) {
    return `a start address is a whole number, at least ${MIN_DMX_ADDRESS}`;
  }
  if (channelRange({ start_address: startAddress, footprint }) === null) {
    // The server's own sentence, so an operator who meets it twice meets it in
    // the same words both times.
    return `address ${startAddress} with a footprint of ${footprint} does not fit in a ${MAX_DMX_ADDRESS}-channel universe`;
  }
  return null;
}

/** Why a universe and an address cannot hold a fixture of this size, or null. */
function refuseAt(at: PatchAt, footprint: number): string | null {
  return refuseUniverse(at.universe) ?? refuseAddress(at.startAddress, footprint);
}

/**
 * Why this show cannot have another fixture pointing there, or null.
 *
 * Two of these are the server's rules and one is not. A second master is
 * refused by `validate_patch`, and so is a layer this show does not have. A
 * *second* fixture on one layer the server would take, and it is refused here
 * anyway: there is one Layer Fixture per patched layer, and two of them is
 * two stretches of a console's channels quietly driving the same layer, which
 * nobody would find by looking at the desk. The same reasoning refuses a second
 * output on one socket.
 */
function refuseTarget(show: Show, target: FixtureTarget): string | null {
  switch (target.kind) {
    case 'master':
      return masterFixture(show) === null
        ? null
        : 'this show already has a master fixture, and there is exactly one. Move it or unpatch it instead.';
    case 'layer': {
      const layer = show.layers.find((each) => each.id === target.layer_id);
      if (!layer) {
        return 'that layer is not in this show';
      }
      return fixtureForLayer(show, layer.id) === null
        ? null
        : `${layer.name} already has a fixture. Move it or unpatch it instead.`;
    }
    case 'effect': {
      const place = effectPlaces(show).find(
        (each) => each.instance.id === target.effect_instance_id,
      );
      if (!place) {
        return 'that effect instance is not in this show';
      }
      return fixtureForEffect(show, place.instance.id) === null
        ? null
        : `${place.instance.type_id} already has a fixture. Move it or unpatch it instead.`;
    }
    case 'generator': {
      const place = generatorPlaces(show).find(
        (each) => each.instanceId === target.generator_instance_id,
      );
      if (!place) {
        return 'that generator instance is not in this show';
      }
      return fixtureForGenerator(show, place.instanceId) === null
        ? null
        : `${place.typeId} already has a fixture. Move it or unpatch it instead.`;
    }
  }
}

/** The fixture patched to one generator instance, if there is one. */
export function fixtureForGenerator(
  show: Show,
  generatorInstanceId: string,
): FixtureInstance | null {
  return (
    readPatch(show).find(
      (fixture) =>
        fixture.target.kind === 'generator' &&
        fixture.target.generator_instance_id === generatorInstanceId,
    ) ?? null
  );
}

/**
 * What to tell an operator whose edit landed on top of something, or nothing.
 *
 * Said at the moment it happens as well as marked on both fixtures afterwards,
 * because the entry that was already there may be several rows away.
 */
function collisionNote(show: Show, fixture: FixtureInstance): string | undefined {
  const hit = readPatch(show).filter(
    (other) => other.id !== fixture.id && collides(fixture, other),
  );
  if (hit.length === 0) {
    return undefined;
  }
  const range = channelRange(fixture);
  const where = range === null ? '' : ` over channels ${range.first}–${range.last}`;
  return `this fixture now overlaps ${hit.length === 1 ? '' : `${hit.length} fixtures, including `}${describeTarget(show, hit[0]!.target)} in universe ${fixture.universe}${where}. The server refuses a show with an overlap in it.`;
}

// --- what the server would refuse --------------------------------------------

/** How a problem is reported: the same shape and the same paths the server uses. */
type Fault = (path: string, message: string) => void;

/**
 * Every rule `Show::validate_patch` applies, at the paths it names.
 *
 * Not a second opinion — the server decides — but the patch is the one part of
 * a show where the mistake an operator makes is invisible until a show, so
 * it is worth catching at the control. The messages are the server's own words
 * wherever the server has words for it, so meeting a problem here and meeting
 * it again in a refused save reads as one problem rather than two.
 *
 * One deliberate difference: a collision is reported on *both* fixtures. The
 * server reports it once, against the earlier of the two, which is enough for a
 * list at the bottom of a screen and not enough for a row an operator is looking
 * at. Either of the two may be the one that should move.
 */
export function validatePatch(
  show: Show,
  catalogue: FixtureCatalogue,
  fault: Fault,
): void {
  const patch = readPatch(show);
  const layers = new Set(show.layers.map((layer) => layer.id));
  let masters = 0;

  for (const [index, fixture] of patch.entries()) {
    const path = `fixture_patch[${index}]`;

    if (fixture.type_id.trim() === '') {
      fault(`${path}.type_id`, 'must not be empty');
    }
    if (fixture.version < MIN_FIXTURE_VERSION) {
      fault(`${path}.version`, 'fixture definition versions start at 1');
    }
    if (fixture.footprint === 0) {
      fault(`${path}.footprint`, 'must occupy at least one channel');
    }
    if (channelRange(fixture) === null) {
      fault(
        `${path}.start_address`,
        `address ${fixture.start_address} with a footprint of ${fixture.footprint} does not fit in a ${MAX_DMX_ADDRESS}-channel universe`,
      );
    }

    switch (fixture.target.kind) {
      case 'master':
        masters += 1;
        break;
      case 'layer':
        if (!layers.has(fixture.target.layer_id)) {
          fault(
            `${path}.target.layer_id`,
            `references layer ${fixture.target.layer_id} which is not in this show`,
          );
        }
        break;
      case 'effect':
        // The server says nothing about this one, so neither does validation:
        // an entry pointing at an effect instance that is not here is a
        // document written elsewhere, and it is reported as an advisory rather
        // than as a refusal the server would not make.
        break;
    }

    checkPersonality(fixture, path, catalogue, fault);

    for (const [other, twin] of patch.entries()) {
      if (other === index || !collides(fixture, twin)) {
        continue;
      }
      fault(
        `${path}.start_address`,
        `collides with fixture_patch[${other}] in universe ${fixture.universe}`,
      );
    }
  }

  if (masters > 1) {
    fault('fixture_patch', `a show has exactly one master fixture, found ${masters}`);
  }
}

/**
 * Checks a patch entry against the personality it names.
 *
 * `check_personality`. A patch that disagrees with the published personality
 * means every fixture after it is offset, and the first anyone would know of it
 * is a fader doing the wrong thing during a show.
 */
function checkPersonality(
  fixture: FixtureInstance,
  path: string,
  catalogue: FixtureCatalogue,
  fault: Fault,
): void {
  if (!catalogue.loaded) {
    return;
  }
  // By type alone, so an entry saved as `version: 2` is checked against the one
  // personality its type publishes rather than reported as unpublished.
  const published = findPersonality(catalogue, fixture.type_id);
  if (published === null) {
    // An effect's personality comes from that effect's own manifest, so nothing
    // here can check it, and the server does not either.
    if (fixture.target.kind !== 'effect' && fixture.type_id.trim() !== '') {
      fault(`${path}.type_id`, `this build does not publish ${fixture.type_id}`);
    }
    return;
  }
  if (published.footprint !== fixture.footprint) {
    fault(
      `${path}.footprint`,
      `${fixture.type_id} occupies ${published.footprint} channels, not ${fixture.footprint}`,
    );
  }
  if (!drives(published, fixture.target)) {
    fault(`${path}.target`, `${fixture.type_id} cannot drive what this patch entry points at`);
  }
}

/** The patch's problems on their own, for a caller that wants them as a list. */
export function patchProblems(
  show: Show,
  catalogue: FixtureCatalogue,
): ValidationError[] {
  const found: ValidationError[] = [];
  validatePatch(show, catalogue, (path, message) => found.push({ path, message }));
  return found;
}

// --- what is true and is not a fault -----------------------------------------

/**
 * Everything about this patch an operator would want to know and that a save
 * will not mention.
 *
 * The same separation the mapping editor makes: validation says what the server
 * would refuse, this says what it would accept and a show would still get
 * wrong. A parked fixture is a console fader that reaches nothing; a layer with
 * no fixture is a layer the desk cannot touch; no master fixture is a show with
 * no dimmer, no crossfader and no tempo from the console.
 *
 * Nothing is said at all about a show with an empty patch. Plenty of shows
 * never see a console, and telling one of them that none of its layers is
 * patched would be a page of complaints about a decision nobody made.
 */
export function patchAdvisories(show: Show, catalogue: FixtureCatalogue): Advisory[] {
  const patch = readPatch(show);
  const found: Advisory[] = [];
  if (patch.length === 0) {
    return found;
  }

  const places = effectPlaces(show);
  for (const [index, fixture] of patch.entries()) {
    if (!fixture.enabled) {
      found.push({
        path: `fixture_patch[${index}].enabled`,
        message: `${fixtureName(fixture, catalogue)} is parked: it holds no channels, and a console moving them reaches nothing`,
      });
    }
    // An entry pointing at an effect instance that is not in this document.
    // The server takes such a show — it checks nothing about an effect
    // target — so this is said rather than refused.
    const target = fixture.target;
    if (
      target.kind === 'effect' &&
      !places.some((place) => place.instance.id === target.effect_instance_id)
    ) {
      found.push({
        path: `fixture_patch[${index}].target`,
        message: `this entry drives effect ${target.effect_instance_id}, which is not in this show`,
      });
    }
  }

  if (masterFixture(show) === null) {
    found.push({
      path: 'fixture_patch',
      message:
        'nothing is patched to the master, so the dimmer, the crossfader, BPM, tap and freeze cannot be reached from a console',
    });
  }

  for (const [index, layer] of show.layers.entries()) {
    if (fixtureForLayer(show, layer.id) === null) {
      found.push({
        path: `layers[${index}]`,
        message: `${layer.name} has no fixture, so a console cannot reach it`,
      });
    }
  }

  return found;
}

// --- the server's address suggestion -----------------------------------------

/** Where the server says a fixture fits, or that it does not. */
export type Suggestion =
  | { ok: true; universe: number; startAddress: number }
  | { ok: false; universe: number; reason: string };

/**
 * Reads what `GET /shows/{id}/patch/address` answered.
 *
 * Taken as given. The server knows what is already patched and this does not
 * check its arithmetic: a client that recomputed the answer would be a second
 * opinion about the one question the endpoint exists to settle, and the two
 * would disagree the first time either changed.
 *
 * No room is a real answer rather than a failure — a universe has 512 channels
 * and a show can want more than that — so it comes back as a reason rather than
 * as a number this client made up.
 */
export function readSuggestion(
  response: AddressSuggestionResponse,
  footprint: number,
): Suggestion {
  const address = response.start_address;
  if (address === null || address === undefined) {
    return {
      ok: false,
      universe: response.universe,
      reason: `universe ${response.universe} has no room for a ${footprint}-channel fixture`,
    };
  }
  return { ok: true, universe: response.universe, startAddress: address };
}

/**
 * What a suggestion is worth while the document has unsaved changes.
 *
 * `suggest_address` answers from the show on disk, not the one being edited
 * (`show_routes::suggest_address`), so a fixture patched a minute ago and not
 * yet saved is not in the answer. That is not a reason to compute the address
 * here instead; it is a reason to say so, and to let the collision marks catch
 * what is left.
 */
export const SUGGESTION_IS_OF_THE_SAVED_SHOW =
  'The server answers from the show as it has it on disk, so unsaved patch changes are not in this suggestion.';
