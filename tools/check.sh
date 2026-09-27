#!/usr/bin/env bash
# Runs every check CI runs, in the same order, from the repository root.
#
#   ./tools/check.sh
#
# Stops at the first failure so the output shows the problem rather than
# scrolling past it.

set -euo pipefail
cd "$(dirname "$0")/.."

step() {
  printf '\n=== %s ===\n' "$1"
  shift
  "$@"
}

step 'Formatting'  cargo fmt --all -- --check
step 'Lint'        cargo clippy --all-targets --workspace -- -D warnings
step 'Tests'       cargo test --workspace
step 'Schema (TS)' npm run schema:check
step 'Client types' npm run client:typecheck
step 'Client tests' npm run client:test

printf '\nAll checks passed.\n'
