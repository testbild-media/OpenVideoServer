// The effect and generator catalogues, and what they say about a parameter.
//
// The server publishes both (`GET /effects`, `GET /generators`) from the same
// manifests it validates a saved show against, so a control drawn from one
// of these is a control the server will accept. Nothing in this file knows the
// name of a single effect: the type, the range and the default of every
// parameter come out of the catalogue, which is the whole point of having one.
//
// The checks below mirror `ParameterDefinition::refuse` and
// `config::manifest::problems` in `crates/ovs-types`. They exist so an operator
// is told at the control rather than at the save; the server's answer is still
// the one that decides, and a disagreement is the server's to win.

import type { Effect, Generator, ModuleParameter, ParameterValue } from '@ovs/schema';

/** Parameter values as a show stores them, keyed by the manifest's names. */
export type ParameterMap = Record<string, ParameterValue>;

/** A module an operator can configure: an effect or a generator. */
export interface Module {
  type_id: string;
  version: number;
  name: string;
  description: string;
  parameters: ModuleParameter[];
}

/** What this server publishes. */
export interface Catalogue {
  /**
   * Whether the catalogues have been fetched.
   *
   * Before they have, nothing can be checked against them, and reporting that
   * an effect is unpublished would be reporting the client's own ignorance as
   * a fault in the operator's document.
   */
  loaded: boolean;
  effects: Effect[];
  generators: Generator[];
}

/** A catalogue that has been asked for nothing yet. */
export function emptyCatalogue(): Catalogue {
  return { loaded: false, effects: [], generators: [] };
}

/** Returns a published effect by type and version, or null. */
export function findEffect(catalogue: Catalogue, typeId: string, version: number): Effect | null {
  return (
    catalogue.effects.find((effect) => effect.type_id === typeId && effect.version === version) ??
    null
  );
}

/** Returns a published generator by type and version, or null. */
export function findGenerator(
  catalogue: Catalogue,
  typeId: string,
  version: number,
): Generator | null {
  return (
    catalogue.generators.find(
      (generator) => generator.type_id === typeId && generator.version === version,
    ) ?? null
  );
}

/**
 * The value a module is published with, for a parameter nobody has set.
 *
 * Mirrors `ParameterDefinition::default_value`, including which
 * `ParameterValue` variant each kind is written as: a number is stored as a
 * float, because that is what the server writes and a show that disagreed
 * would differ from one the server made.
 */
export function defaultValue(parameter: ModuleParameter): ParameterValue {
  const type = parameter.parameter_type;
  switch (type.kind) {
    case 'number':
      return { type: 'float', value: type.default };
    case 'colour':
      return { type: 'color', value: type.default };
    case 'toggle':
      return { type: 'bool', value: type.default };
    case 'choice':
      return { type: 'choice', value: type.default };
    case 'text':
      return { type: 'text', value: type.default };
  }
}

/**
 * The value in force for a parameter: what is set, or what it was published with.
 *
 * Mirrors `config::manifest::value`. A show that leaves a parameter out is
 * not incomplete; it is a show that wants the published default.
 */
export function parameterValue(
  module: Module,
  values: ParameterMap | undefined,
  key: string,
): ParameterValue | null {
  const set = values?.[key];
  if (set !== undefined) {
    return set;
  }
  const declared = module.parameters.find((parameter) => parameter.key === key);
  return declared ? defaultValue(declared) : null;
}

/**
 * The value in force: what has been moved, what is configured, what was published.
 *
 * Mirrors `EffectValues::value`, which asks the same three places in the same
 * order and takes the first answer. A live value is not merged into the
 * configured map on either side of the wire: the show keeps what it was
 * built with, so closing the show and opening it again gives that back.
 *
 * `live` is what a control source has moved since the show opened. A
 * parameter absent from it has no live value at all — it is not "live at its
 * configured value" — which is why this falls through to `parameterValue`
 * rather than treating the absence as a number.
 */
export function valueInForce(
  module: Module,
  configured: ParameterMap | undefined,
  live: ParameterMap | undefined,
  key: string,
): ParameterValue | null {
  const moved = live?.[key];
  return moved === undefined ? parameterValue(module, configured, key) : moved;
}

/**
 * Why a value is not usable for a parameter, or null when it is.
 *
 * Mirrors `ParameterDefinition::refuse`, including the refusal rather than the
 * clamp: clamping is what a live control does with a fader that has gone too
 * far, and this is a document being edited, where the right answer is to say
 * what is wrong.
 */
export function refuse(parameter: ModuleParameter, value: ParameterValue): string | null {
  const type = parameter.parameter_type;
  switch (type.kind) {
    case 'number': {
      if (value.type !== 'float' && value.type !== 'int') {
        return 'must be a number';
      }
      const number = value.value;
      return Number.isFinite(number) && number >= type.min && number <= type.max
        ? null
        : `must be a number between ${type.min} and ${type.max}`;
    }
    case 'colour': {
      if (value.type !== 'color') {
        return 'must be a colour';
      }
      // The 0..1 bound is the model's, not the parameter's: a colour is linear
      // RGBA and there is no other range it could be in.
      return value.value.every((channel) => Number.isFinite(channel) && channel >= 0 && channel <= 1)
        ? null
        : 'must be four channels between 0 and 1';
    }
    case 'toggle':
      return value.type === 'bool' ? null : 'must be on or off';
    case 'text': {
      if (value.type !== 'text') {
        return 'must be text';
      }
      // Characters rather than bytes, as the server counts them: somebody who
      // typed thirty letters typed thirty letters whether or not any of them
      // was an umlaut.
      const length = [...value.value].length;
      return length > type.max_chars
        ? `must be at most ${type.max_chars} characters, not ${length}`
        : null;
    }
    case 'choice': {
      const named = type.choices.map((choice) => choice.id).join(', ');
      if (value.type !== 'choice') {
        return `must be one of ${named}`;
      }
      // By identifier, never by label and never by position: a label is a
      // thing that gets reworded and a position is a thing that moves.
      return type.choices.some((choice) => choice.id === value.value)
        ? null
        : `must be one of ${named}`;
    }
  }
}

/** One thing wrong with a set of parameter values. */
export interface ParameterProblem {
  key: string;
  reason: string;
}

/**
 * Everything wrong with a set of values, in the manifest's own terms.
 *
 * Mirrors `config::manifest::problems`. A value for something the module never
 * declared is a problem too: it is nearly always a typo or a show written
 * against another version, and ignoring it means an operator setting a control
 * and seeing nothing happen.
 */
export function parameterProblems(
  module: Module,
  values: ParameterMap | undefined,
): ParameterProblem[] {
  const problems: ParameterProblem[] = [];
  for (const [key, value] of Object.entries(values ?? {})) {
    const declared = module.parameters.find((parameter) => parameter.key === key);
    if (!declared) {
      problems.push({
        key,
        reason: `${module.type_id} version ${module.version} has no such parameter`,
      });
      continue;
    }
    const reason = refuse(declared, value);
    if (reason !== null) {
      problems.push({ key, reason });
    }
  }
  return problems;
}
