#!/usr/bin/env bash
# A throwaway `ovsd` for the desktop client's own session code to talk to.
#
#   ./tools/client-live.sh --serve      a throwaway server, and nothing else
#   ./tools/client-live.sh              the same, then the live suite here too
#   OVS_LIVE_PORT=7460 ./tools/client-live.sh   somewhere else, if 7450 is busy
#
# `--serve` is the one that matters, and it is what `tools/client-live.ps1`
# drives: **the client is tested on the machine the client runs on.** This
# starts the server half on the server and gets out of the way.
#
# It builds `ovsd`, starts one of its own on a spare port with an empty database
# in a temporary directory, creates the operator account, prints the address and
# waits. Everything it made is gone when it stops, however it stops.
#
# Without `--serve` it also runs `clients/desktop/test/live.test.ts` here, which
# needs Node on this machine. That is the fallback and not the normal way round:
# nothing in the product needs Node on a server, a media server should not be
# made to hold a client's packages, and the pairing this suite exists to test is
# a client on one machine and a server on another.
#
# # Why this exists
#
# `live.test.ts` is skipped unless `OVS_LIVE_SERVER` names a server, and until
# this script existed nothing in the repository ever set it. So the one suite
# that runs the client's real session code against the real Rust — the only
# place a contract both halves read differently can be caught — had never run at
# all. A test nobody runs is not a test, and a test that needs a server somebody
# has to set up by hand is a test nobody runs.
#
# # What it will not touch
#
# The server it starts is deliberately deaf and blind. It binds loopback on a
# port of its own, keeps its database and media under `mktemp -d`, reads no
# machine configuration, opens no Art-Net or sACN socket — the installed `ovsd`
# holds 6454 and 5568 and two processes cannot both have them — and is pointed
# at a renderer port with nothing behind it, so it cannot reach the renderer
# that is driving a wall. Nothing it does outlives the run, including on a
# failure: the trap stops it and deletes the directory either way.
#
# Loopback trust is turned off for it, which the installed server leaves on
#. The desktop client is always a machine away in real use, so a
# run where every request was waved through for arriving locally would be
# exercising the one path the client never takes.

set -euo pipefail
cd "$(dirname "$0")/.."

serve_only=""
serve_seconds="${OVS_LIVE_SECONDS:-900}"
if [ "${1:-}" = "--serve" ]; then
  serve_only="yes"
  [ -n "${2:-}" ] && serve_seconds="$2"
fi

port="${OVS_LIVE_PORT:-7450}"
# Somewhere with nothing behind it. `ovsd` dials this address until something
# answers, and the one thing it must never find is the renderer that is running
# a show.
renderer_port="${OVS_LIVE_RENDERER_PORT:-$((port + 1))}"
username="${OVS_LIVE_USER:-operator}"
password="${OVS_LIVE_PASSWORD:-correct horse battery staple}"
# Serving binds every interface, because the client this is for is on another
# machine. Only in `--serve`: a run that does everything here has no reason to
# be reachable from anywhere.
bind="127.0.0.1"
[ -n "$serve_only" ] && bind="0.0.0.0"
api="http://127.0.0.1:$port/api/v1"

# Only the half that runs the tests needs Node. A machine with none is the
# ordinary case on a server — nothing in the product needs one — and `--serve`
# exists so that a server never has to have one.
if [ -z "$serve_only" ]; then
  command -v npm > /dev/null && command -v npx > /dev/null || {
    echo "running the tests here needs Node and npm, and this machine has neither" >&2
    echo "use --serve and run them on the client's machine instead" >&2
    exit 2
  }
fi

work=$(mktemp -d)
server=""

# Everything this run made, gone, whichever way the run ended.
cleanup() {
  if [ -n "$server" ] && kill -0 "$server" 2>/dev/null; then
    kill "$server" 2>/dev/null || true
    wait "$server" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

echo "building ovsd"
cargo build --quiet -p ovs-core --bin ovsd
binary="target/debug/ovsd"
[ -x "$binary" ] || { echo "cargo built no $binary" >&2; exit 1; }

# The tree arrives here as a tar without its node_modules (tools/remote.sh), so
# the first run on a machine has to fetch them. Electron's binary is skipped: it
# is a hundred megabytes of browser, and nothing the suite imports reaches the
# two files that use it.
if [ -z "$serve_only" ] && [ ! -d node_modules/vitest ]; then
  echo "installing the client's packages, without Electron's binary"
  ELECTRON_SKIP_BINARY_DOWNLOAD=1 npm install --no-audit --no-fund
fi

echo "starting a throwaway ovsd on $bind:$port, with its database under $work"
OVS_BIND="$bind:$port" \
OVS_DATA_DIR="$work/data" \
OVS_CONFIG="$work/there-is-no-configuration.json" \
OVS_RENDERER="127.0.0.1:$renderer_port" \
OVS_DMX=off \
OVS_TRUST_LOOPBACK=0 \
OVS_LOG="${OVS_LIVE_LOG:-warn}" \
  "$binary" > "$work/ovsd.log" 2>&1 &
server=$!

# A server that died during startup — a port already taken is the usual one —
# must be reported as that rather than as thirty seconds of silence.
died() {
  echo "the throwaway server stopped before it could answer; it said:" >&2
  cat "$work/ovsd.log" >&2
  exit 1
}

echo "waiting for it to answer"
answered=""
for _ in $(seq 1 60); do
  kill -0 "$server" 2>/dev/null || died
  # Quiet rather than `-S`: a refused connection is what waiting looks like for
  # the first second, and a line of curl's own about it every half second would
  # bury the one that matters.
  if curl -fs -o /dev/null --max-time 2 "$api/server/capabilities"; then
    answered="yes"
    break
  fi
  sleep 0.5
done
[ -n "$answered" ] || { echo "$api never answered" >&2; cat "$work/ovsd.log" >&2; exit 1; }

# The operator account is made here rather than by the suite, so that
# `/server/status` can be asked for with a session — which is the endpoint worth
# waiting on, and with loopback trust off it needs one.
credentials=$(jq -n --arg u "$username" --arg p "$password" '{username: $u, password: $p}')
curl -fsS -o /dev/null -X POST -H 'content-type: application/json' \
  --data "$credentials" "$api/auth/setup"
token=$(curl -fsS -X POST -H 'content-type: application/json' \
  --data "$credentials" "$api/auth/login" | jq -r '.token')

version=$(curl -fsS -H "authorization: Bearer $token" "$api/server/status" \
  | jq -r '.telemetry.version')
echo "it is version $version, with an empty database and no renderer"

if [ -n "$serve_only" ]; then
  # The line the caller waits for. Printed after the account exists, so that a
  # client which starts the moment it sees this finds a server it can sign in
  # to rather than one that is still setting itself up.
  echo "READY $bind:$port $username"

  # It stops on its own even if whatever started it never comes back. A
  # forgotten server holding a port on a machine somebody else is using is the
  # one way this could outlive its usefulness, and a deadline costs nothing.
  echo "serving for up to ${serve_seconds}s; stop it sooner by killing $$"
  for _ in $(seq 1 "$serve_seconds"); do
    kill -0 "$server" 2>/dev/null || died
    sleep 1
  done
  echo "the deadline passed; stopping"
  exit 0
fi

echo "running the client's live suite against it"
status=0
(
  cd clients/desktop
  OVS_LIVE_SERVER="127.0.0.1:$port" \
  OVS_LIVE_USER="$username" \
  OVS_LIVE_PASSWORD="$password" \
    npx vitest run live
) || status=$?

if [ "$status" -ne 0 ]; then
  # What the server made of it, which is usually the other half of whatever the
  # suite just said.
  echo "the suite failed; the last of what the server said:" >&2
  tail -40 "$work/ovsd.log" >&2
fi

exit "$status"
