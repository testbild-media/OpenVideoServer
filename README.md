# OpenVideoServer

A dedicated live media server for events, LED walls, shows and installations.

> Setup in the client. Run the show from the lighting console. The server keeps
> running independently.

OpenVideoServer combines an autonomous Debian server, a desktop client for
configuration and media management, Art-Net or sACN as the primary live-control
interface, a local rack service display, layer-based composition, beat-synchronised
playback, effects and generators, and rectangular pieces for LED-wall mapping.

**Status: in development, and it runs.** On the Debian test machine it drives a
Sony LMD-2110W at 59.94 fps through `VK_KHR_display` with no compositor between
it and the panel, plays ProRes — three simultaneous 1080p60 sources with no late
frames — composites layers across three buses with a crossfader, a master dimmer
and freeze, takes Art-Net and sACN from a console across a real switch, and
serves a live H.264 preview of every surface at about thirty milliseconds. The
desktop client edits shows, maps pieces onto walls, ingests media, patches a
console and runs the show.

Every number above was measured on a test machine, which is not the server
this product is specified against. For throughput that difference matters, and
nothing here should be read as a figure for other hardware.

## Installing the server

On a Debian machine with nothing on it but a network. A Debian netinst has no
`curl`, so that is the first line:

```bash
sudo apt install curl -y
```

**This repository is private**, so every request needs a token — a fine-grained
personal access token with read access to its contents. Nothing anonymous
reaches it, including the script itself:

```bash
export OVS_TOKEN=github_pat_...
curl -fsSL -H "Authorization: Bearer $OVS_TOKEN" -H "Accept: application/vnd.github.raw" https://api.github.com/repos/testbild-media/OpenVideoServer/contents/install.sh | sudo -E sh
```

`sudo -E` keeps the variable, which is what stops the token becoming an
argument in the process list. From a checkout, `--token` takes one instead.

The script is [install.sh](install.sh) in the root of this repository. Nothing
else is needed and nothing else is fetched from anywhere but this repository's
releases.

Were the repository public, the short form would work and no token would be
involved — the script asks for one only when it has one:

```bash
curl -fsSL https://raw.githubusercontent.com/testbild-media/OpenVideoServer/main/install.sh | sudo sh
```

It does all of this:

| | |
|---|---|
| the server | the newest `.deb` for this architecture, from the latest release |
| the checksum | the `.sha256` published beside it, checked before anything is installed |
| the Windows client | into `/usr/share/openvideoserver/client`, so the server can hand it out |
| the graphics driver | NVIDIA, from Debian's own archive, with the modeset setting the renderer needs |
| the restart | once, at the end, because a driver that was just installed is not the one running |

Each of the last three can be left out: `--no-client`, `--no-driver`,
`--no-reboot`. `--url` names a package to install instead of looking for one,
and `--package` installs one that is already on the machine. `--token` is the
other way to give it the token, for a checkout rather than a pipe.

Piping a script into a root shell is a real trust decision, and the honest
mitigation is the checksum rather than a comment saying the pipe is safe. Read
it first if you would rather — it is the same file either way, and run from a
checkout it builds what it installs instead of downloading anything.

## Installing the client

The client is a Windows application and a separate machine from the server. Two
places to get the same file:

* **From the server**, on its own page at `http://<server>/` — which is the
  point of it being in the package. The evening somebody needs the client on a
  second laptop and the venue's wifi is a captive portal, the machine running
  the show has the installer on it and its address is already on a screen.
* **From the releases**, at
  [github.com/testbild-media/OpenVideoServer/releases](https://github.com/testbild-media/OpenVideoServer/releases),
  which is where to start when there is no server yet. Signed in, since the
  repository is private.

The server's own page needs no token: the installer puts the client on the
machine, and the machine hands it out.

It is not code-signed, so Windows shows SmartScreen's "unrecognised app" the
first time. Said here rather than discovered: an unsigned installer nobody
warned you about looks exactly like a tampered one.

**It does not create an account.** That is the one step where a password is
typed, and a script that took one would leave it in a shell history and a
process list. The server's own page asks for it; the first account made is an
administrator, because somebody has to be able to make the second.

The server then answers on port 80 with its status — what it is playing, how
hard it is working, and where to download the client — and carries a small
administration area behind a sign-in for accounts and system settings.
[deploy/README.md](deploy/README.md) is the full account, including what the
package deliberately will not do.

**The client is a separate machine.** It configures a show and runs one; it is
not installed on the server and the server does not need it to keep playing.

## Documentation

The server documents itself where it can be checked: `GET /api/v1/fixtures`
returns every DMX personality this build publishes, and `/api/v1/generators`
and `/api/v1/effects` every module with its parameters. Those are the answers
that matter to somebody programming a console or building a show, and they come
from the running build rather than from a file that might be older than it.

`deploy/README.md` covers installing, upgrading and what the package will not
do. `SECURITY.md` says what this server assumes about the network it is on.

The product specification and the engineering notes are not published.

## Repository layout

```text
crates/          Rust workspace: server core, renderer, media, DMX, shared types
clients/desktop/ Electron + TypeScript desktop client
service-console/ 1024x600 touch UI for the local rack display
schemas/         JSON Schema generated from crates/ovs-types (committed)
packages/        TypeScript packages, including types generated from schemas/
deploy/          systemd units and Debian packaging
```

Crates and packages appear as the phase that needs them begins.

## Building

Requires the Rust toolchain pinned in `rust-toolchain.toml` and Node.js 20 or
newer. FFmpeg is needed later, for media ingest.

```bash
cargo build --workspace
cargo test --workspace
```

## Running the server

```bash
OVS_BIND=127.0.0.1:7420 OVS_DATA_DIR=./run cargo run -p ovs-core --bin ovsd
```

| Variable | Default | Meaning |
|---|---|---|
| `OVS_BIND` | `0.0.0.0:7420` | address to listen on |
| `OVS_DATA_DIR` | `/var/lib/ovs` | where the database lives |
| `OVS_LOG` | `info` | log filter |

A fresh server reports `configured: false` from
`GET /api/v1/server/capabilities` and accepts one `POST /api/v1/auth/setup` to
create the operator account. After that, `POST /api/v1/auth/login` returns a
bearer token for the API and for the realtime handshake.

`deploy/systemd/ovsd.service` is the production unit.

## Running the renderer

```bash
OVS_RENDERER_BIND=127.0.0.1:7421 cargo run -p ovs-renderer --bin ovs-renderer
```

| Variable | Default | Meaning |
|---|---|---|
| `OVS_RENDERER_BIND` | `127.0.0.1:7421` | control channel address |
| `OVS_RENDERER_OUTPUTS` | `1` | headless outputs to drive |
| `OVS_LOG` | `info` | log filter |

The renderer listens and the core connects to it, not the other way round. That
is why stopping `ovsd` does not stop the show: the renderer keeps rendering the
last state it was given. The once-a-second log line reports the measured rate
and the dropped-frame counters.

With no display named it renders headless, which is what the command above
does and what the tests use. On a machine with a panel attached it takes the
connector directly through `VK_KHR_display`, with no compositor between it and
the screen — `ovs-renderer --displays` lists what this machine is plugged
into.

## The shared schema

The Rust types in `crates/ovs-types` are the single definition of the show
model, the media model, live state and telemetry. JSON Schema and TypeScript are
generated from them, and both generated layers are committed and guarded by
tests.

After changing a shared type:

```bash
cargo run -p ovs-proto --bin ovs-schema-gen -- schemas
npm run schema:generate
```

`cargo test` and `npm run schema:check` fail until both are regenerated. If the
change alters a persisted contract, bump the matching version in
`crates/ovs-types/src/version.rs` and add a migration in the same commit.

## Before pushing

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
npm run schema:check
```

## Licence

Copyright 2026 testbild.media / Jason Kuhr. Apache-2.0 — see [LICENSE](LICENSE).

The source here links to nothing on its own. A *built* server is a different
question, because it links FFmpeg, and on Debian FFmpeg is a GPL build.
[NOTICE](NOTICE) sets out what that means for a package somebody hands on, and
what already exists to deal with it.
