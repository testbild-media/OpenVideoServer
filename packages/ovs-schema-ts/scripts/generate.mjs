// Generates TypeScript types from the committed JSON Schema documents.
//
// The schemas themselves come from the Rust types in crates/ovs-types, so the
// chain is: Rust type -> JSON Schema -> TypeScript. There is exactly one
// definition of the project model, and the desktop client cannot drift from it.
//
//   node scripts/generate.mjs           write the types
//   node scripts/generate.mjs --check   fail if the committed types are stale
//
// The --check form is what CI runs. It is the TypeScript half of the guard that
// crates/ovs-types/tests/golden_schema.rs provides for the schemas themselves.

import { readdir, readFile, writeFile, mkdir } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { compile } from 'json-schema-to-typescript';

const here = dirname(fileURLToPath(import.meta.url));
const schemaDir = resolve(here, '..', '..', '..', 'schemas');
const outDir = resolve(here, '..', 'src');
const checkOnly = process.argv.includes('--check');

const HEADER = [
  '// Generated from the OpenVideoServer shared schema. Do not edit by hand.',
  '// Regenerate with: npm run generate --workspace @ovs/schema',
  '',
].join('\n');

/** Turns `media-item.schema.json` into `media-item.ts`. */
const moduleNameFor = (schemaFile) => schemaFile.replace(/\.schema\.json$/, '');

/**
 * Finds the top-level declarations in one generated module.
 *
 * Every schema is compiled on its own, so each shared type it references is
 * written out again in that module: `LayerLive` exists in `layer-live.ts`, in
 * `state-snapshot.ts` and in `server-message.ts`. That is right for the modules
 * — each is a complete, standalone reading of one schema — but it means the
 * package index cannot simply re-export all of them, so the index has to know
 * which names each module declares.
 *
 * Returns a map of name to declaration text; the text is what the duplicate
 * check below compares.
 */
function declarations(text) {
  const lines = text.replace(/\r\n/g, '\n').split('\n');
  const found = new Map();
  for (let index = 0; index < lines.length; index += 1) {
    const match = /^export (?:type|interface) ([A-Za-z0-9_]+)/.exec(lines[index]);
    if (!match) {
      continue;
    }
    // Read to the end of the declaration: an interface closes when its braces
    // balance, a type alias when a line ends the statement at depth zero.
    const body = [];
    let depth = 0;
    for (; index < lines.length; index += 1) {
      const line = lines[index];
      body.push(line);
      for (const character of line) {
        if (character === '{') {
          depth += 1;
        } else if (character === '}') {
          depth -= 1;
        }
      }
      const trimmed = line.trimEnd();
      if (depth === 0 && (trimmed.endsWith(';') || trimmed === '}')) {
        break;
      }
    }
    found.set(match[1], body.join('\n'));
  }
  return found;
}

/**
 * Builds the package index: every name exported exactly once.
 *
 * `export *` from every module is ambiguous — a shared type is declared in each
 * module that references it — and TypeScript refuses it, so a client could not
 * import from the package at all. Naming each type's module resolves that, and
 * taking the first module that declares it is deterministic because the schema
 * files are sorted.
 *
 * The copies have to agree. They do today, because they are compiled from one
 * set of schemas which are themselves generated from one set of Rust types; if
 * they ever stop agreeing that is a fault in that chain rather than something
 * to paper over by picking one, so this throws instead.
 */
function buildIndex(modules) {
  const owner = new Map();
  const exports = new Map();

  for (const [moduleName, contents] of modules) {
    for (const [name, body] of declarations(contents)) {
      const seen = owner.get(name);
      if (!seen) {
        owner.set(name, { moduleName, body });
        const names = exports.get(moduleName) ?? [];
        names.push(name);
        exports.set(moduleName, names);
      } else if (seen.body !== body) {
        throw new Error(
          `${name} is declared differently in ${seen.moduleName}.ts and ${moduleName}.ts. ` +
            'The schemas they were compiled from disagree about a shared type; fix that ' +
            'rather than the generator.',
        );
      }
    }
  }

  const lines = [HEADER];
  for (const [moduleName, names] of exports) {
    // Types only, so the index leaves nothing behind at runtime and a client
    // compiled with `verbatimModuleSyntax` can import from it.
    lines.push('export type {');
    for (const name of names) {
      lines.push(`  ${name},`);
    }
    lines.push(`} from './${moduleName}.js';`);
  }
  lines.push('');
  return lines.join('\n');
}

async function generate() {
  if (!existsSync(schemaDir)) {
    throw new Error(
      `schema directory ${schemaDir} not found; run ` +
        '`cargo run -p ovs-types --bin ovs-schema-gen -- schemas` first',
    );
  }

  const schemaFiles = (await readdir(schemaDir))
    .filter((name) => name.endsWith('.schema.json'))
    .sort();

  if (schemaFiles.length === 0) {
    throw new Error(`no schema documents in ${schemaDir}`);
  }

  const outputs = new Map();
  const modules = new Map();

  for (const schemaFile of schemaFiles) {
    const schema = JSON.parse(await readFile(join(schemaDir, schemaFile), 'utf8'));
    const compiled = await compile(schema, schema.title ?? moduleNameFor(schemaFile), {
      bannerComment: '',
      additionalProperties: false,
      style: { singleQuote: true },
    });
    modules.set(moduleNameFor(schemaFile), compiled);
    outputs.set(`${moduleNameFor(schemaFile)}.ts`, `${HEADER}${compiled}`);
  }

  outputs.set('index.ts', buildIndex(modules));

  return outputs;
}

const outputs = await generate();

if (checkOnly) {
  const stale = [];
  for (const [name, contents] of outputs) {
    const path = join(outDir, name);
    const committed = existsSync(path) ? await readFile(path, 'utf8') : null;
    if (committed?.replace(/\r\n/g, '\n') !== contents) {
      stale.push(name);
    }
  }
  if (stale.length > 0) {
    console.error(
      'The generated TypeScript is out of date with the shared schema:\n  - ' +
        stale.join('\n  - ') +
        '\n\nRun `npm run generate --workspace @ovs/schema` and commit the result.',
    );
    process.exit(1);
  }
  console.log(`${outputs.size} generated TypeScript modules are up to date.`);
} else {
  await mkdir(outDir, { recursive: true });
  for (const [name, contents] of outputs) {
    await writeFile(join(outDir, name), contents, 'utf8');
  }
  console.log(`Wrote ${outputs.size} TypeScript modules to ${outDir}`);
}
