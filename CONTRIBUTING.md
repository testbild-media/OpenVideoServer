# Contributing

## Before you start

This project is built to a written specification held by the project owner,
and the specification wins. It is not published, so a change that alters what
the product *does* — rather than how it does it — needs settling in an issue
first. That is not bureaucracy: it is the only way to find out whether a
behaviour is deliberate before arguing about it in review.

## What you need

- The Rust toolchain pinned in `rust-toolchain.toml`. Do not change the pin in
  a feature branch.
- Node.js 20 or newer, for the desktop client.
- A Debian machine for anything that touches the renderer, the media pipeline
  or Vulkan. Those parts are not developed on Windows or macOS: they talk to
  DRM connectors and FFmpeg builds that only exist there. The rest of the
  workspace builds anywhere.

## A Debian machine from nothing

Written out because the reference machine was rebuilt on 2026-09-26 and nothing
here said how. Every command below was the one actually used.

```bash
sudo apt install build-essential pkg-config nasm cmake git libclang-dev \
  zlib1g-dev libsnappy-dev libopenh264-dev dpkg-dev fakeroot
```

`libclang-dev` is what `bindgen` loads to read FFmpeg's headers. Without it the
build stops at "Unable to find libclang", from inside `ffmpeg-sys-next` — a crate
whose name gives no hint that clang is involved.

`nasm` is not optional: `tools/build-ffmpeg.sh` refuses without an assembler
rather than quietly producing a build that decodes several times slower. The
three `-dev` libraries are what the server profile's `configure` enables, and
`dpkg-dev` and `fakeroot` are what `tools/package.sh` checks for.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
```

No version is needed: `rust-toolchain.toml` pins it, and the first `cargo`
in the tree installs that toolchain with the components it names.

```bash
./tools/build-ffmpeg.sh --profile server
```

A few minutes. It fetches FFmpeg and `nv-codec-headers` itself and ends by
verifying what it built, including that no GPL component got in.

### The Chromium the System screen draws with

```bash
cargo install export-cef-dir --version "=$(sed -n 's/^cef = .*"=\([0-9.]*\)".*/\1/p' \
  crates/ovs-console/Cargo.toml)"
export-cef-dir -f ~/.local/share/cef
```

**Install the tool at the same version as the `cef` crate, and this is the
whole reason for the line above.** `export-cef-dir` decides which CEF
distribution to fetch from *its own* version, and plain `cargo install
export-cef-dir` gives the newest — which on 2026-09-26 was `154.1.0+154.0.26`
and would have fetched CEF 154.0.26 for bindings pinned at `154.0.0`. Pinned to
match, it fetches 154.0.23, which is the one those bindings describe.

A binding set that does not match the library it calls is how CEF aborts inside
`CefInitialize` with nothing in the log, which is the failure this repository has
already paid for once — see the comment on the pin in
`crates/ovs-console/Cargo.toml`.

It is 1.5 GB and lands in `~/.local/share/cef`, where `tools/deploy.sh` and
`tools/package.sh` both look. Nothing else needs it: only `ovs-console` links it.

## Before you push

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm run schema:check
```

and, in `clients/desktop`:

```bash
npx tsc --noEmit
npm test
```

CI runs all of it. `-D warnings` is not negotiable and neither is `cargo fmt`:
a diff full of formatting is a diff nobody can review.

## The shared schema is generated, not written

The Rust types in `crates/ovs-types` are the single definition of the show
model, the media model, live state and telemetry. JSON Schema and TypeScript
are generated from them and both generated layers are committed. After changing
a shared type:

```bash
cargo run -p ovs-proto --bin ovs-schema-gen -- schemas
npm run schema:generate
```

Tests fail until you do. If the change alters a contract that is already on
disk somewhere, bump the matching version in `crates/ovs-types/src/version.rs`
and add a migration in the same commit — a migration that arrives one commit
later is a machine that will not start.

Migrations are forward-only and ordered, and their names are recorded in the
database. Renaming a migration file that has already run on a real machine
breaks that machine. Add a new one instead.

## Tests

A test earns its place by failing when something is actually wrong. Tests that
assert the code does what the code does are worse than no tests, because they
have to be edited every time the code changes and they never object.

Anything about timing, throughput or frame rate is **measured, not asserted**.
If you claim something is fast, say on what machine, with what clip, and what
the number was. The test machine and the machine this product is specified
against are not the same, and a figure from one is not a figure for the other.

## Commits

Write the subject as a sentence that states what is now true:

```
A project was never a thing: it is a show, and only the loaded one is editable
```

not `fix: rename project to show`. The body says why, what it cost, and what
you decided not to do. Somebody reading it in two years wants the reasoning,
which is the one thing they cannot recover from the diff.

One change per commit. A rename and a bug fix in the same commit means neither
can be reverted alone.

## Things that will be sent back

- A hand-written `Depends:` line in the Debian package. Dependencies are read
  out of the binaries by `dpkg-shlibdeps`. The one exception is documented
  where it lives, with the reason.
- A stand-in that pretends to work. A decoder that returns colour bars, a
  measurement that returns a plausible number, a handler that logs success
  without doing anything — all of these look like progress and are worse than
  an error, because the failure surfaces somewhere else, later, wearing a
  different fault's face.
- Silent catches. If something cannot be done, say so where somebody will read
  it.
- Comments that restate the line below them. Comments earn their place by
  saying why, or by naming what was tried and did not work.
