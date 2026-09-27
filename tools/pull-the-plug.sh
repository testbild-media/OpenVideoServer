#!/usr/bin/env bash
# Takes things away from a running show and measures what the wall does.
#
#   ./tools/pull-the-plug.sh              every case, in order
#   ./tools/pull-the-plug.sh renderer     one of them
#
# Run it on the server. It needs `sudo` to stop and start units, and it puts
# everything back whatever happens.
#
# # Why this exists
#
# Every other check in this directory asks what the machine does when it is
# working. This one asks what it does when a piece of it is not — which is the
# only question an evening actually turns on. A media server that survives a
# renderer crash and one that does not look identical until the night it
# happens.
#
# # The invariant being tested
#
# The renderer drives the panel without waiting to be told anything.
# So the core going away must not touch the wall at all: no black frame,
# no dropped frame, no change of rate. The renderer going away *does* take the
# wall, and what is measured there is how long for and whether the show that
# comes back is the show that was running.
#
# # What it is not
#
# It cannot pull an HDMI cable or a network lead, and it does not pretend to.
# What it takes away is processes and sockets, which is most of what actually
# fails: a unit that OOMs, a driver that wedges, a core that was restarted
# during a get-in with the show already up.

set -euo pipefail

api="${OVS_BASE:-http://127.0.0.1:7420/api/v1}"
only="${1:-all}"

# The wall's own numbers. Asked of the renderer through the core, which is the
# only way in — so a reading that fails while the core is down is expected and
# says so rather than counting as a dark wall.
fps() {
  curl -fsS --max-time 2 "$api/server/status" 2>/dev/null \
    | jq -r '.telemetry.outputs[0].actual_fps // 0' 2>/dev/null || echo "unreachable"
}

dropped() {
  curl -fsS --max-time 2 "$api/server/status" 2>/dev/null \
    | jq -r '.telemetry.outputs[0].dropped_frames.lifetime // 0' 2>/dev/null || echo 0
}

# Lit means the core can see a renderer that is running *and* an output that is
# presenting. The frame rate alone is not enough: the core keeps the last
# telemetry it was sent, so a dead renderer's wall reads at its old rate for as
# long as nobody asks the right question. `renderer.state` is the right
# question — it goes to `unreachable` the moment the socket drops.
lit() {
  local state reading
  state=$(curl -fsS --max-time 2 "$api/server/status" 2>/dev/null     | jq -r '.telemetry.renderer.state // "gone"' 2>/dev/null || echo gone)
  [ "$state" = "running" ] || return 1
  reading=$(fps)
  [ "$reading" != "unreachable" ] && LC_ALL=C awk -v f="$reading" 'BEGIN { exit !(f > 10) }'
}

settle() {
  for _ in $(seq 1 60); do
    lit && return 0
    sleep 0.5
  done
  return 1
}

echo "before anything: $(fps) fps"
settle || { echo "the wall is not lit to begin with; nothing to measure" >&2; exit 1; }

# --- the renderer, killed outright ------------------------------------------

renderer_killed() {
  echo
  echo "== the renderer, killed with SIGKILL =="
  local before after pid began dark
  before=$(dropped)
  pid=$(systemctl show ovs-renderer -p MainPID --value)
  echo "  it is $pid, and the wall is at $(fps) fps"

  # Waited for, because the core notices the socket drop and only then is the
  # clock honest: measuring from the kill to a reading that was already stale
  # would report nought.
  local noticed

  began=$(date +%s.%N)
  sudo kill -9 "$pid"

  noticed=""
  for _ in $(seq 1 100); do
    if ! lit; then
      noticed="yes"
      break
    fi
    sleep 0.1
  done
  [ -n "$noticed" ] || echo "  (the core never reported it gone, so the gap below is a floor)"

  # Dark from the moment it died until an output reports a rate again. systemd's
  # restart delay is most of this and that is the honest number: what an
  # audience sees is the whole gap, not the part this software is responsible
  # for.
  if ! settle; then
    echo "  the wall did not come back" >&2
    return 1
  fi
  dark=$(echo "$(date +%s.%N) $began" | LC_ALL=C awk '{printf "%.1f", $1 - $2}')
  after=$(dropped)

  echo "  dark for ${dark}s, back at $(fps) fps"
  echo "  the counter went $before to $after, which is the restart and not a fault"
  echo "  the show: $(curl -fsS "$api/server/status" | jq -r '.telemetry.current_project_name // "none"')"
}

# --- the core, taken away and given back ------------------------------------

core_stopped() {
  echo
  echo "== the core, stopped while the show runs =="
  # Read straight off the renderer's own log, because the core is the usual way
  # in and it is the thing being taken away. One line per output per second.
  local before
  before=$(sudo journalctl -u ovs-renderer -n 1 --no-pager -o short-unix | awk '{print $1}')
  echo "  the wall is at $(fps) fps"

  sudo systemctl stop ovsd
  echo "  ovsd is $(systemctl is-active ovsd); the status endpoint is now $(fps)"

  # Twelve seconds with nothing to tell it anything. The renderer should not
  # notice: it holds the display and the show.
  sleep 12

  # What the renderer said about itself while the core was gone. It writes one
  # line per output per second, so this is the wall's own account of the twelve
  # seconds in which nothing could tell it anything — which is the whole point
  # of the invariant. The escapes go first: the log is coloured for a terminal
  # and a colour code between `fps` and its value defeats any pattern.
  local window=/tmp/ovs-renderer-window.txt
  sudo journalctl -u ovs-renderer --since "@$before" --no-pager -o cat \
    | sed 's/\x1b\[[0-9;]*m//g' > "$window"
  local lines
  lines=$(grep -c 'fps=' "$window" || true)
  if [ "$lines" -eq 0 ]; then
    echo "  it said nothing at all while the core was down, which is not the invariant" >&2
  else
    grep -oE 'fps=[0-9.]+' "$window" | cut -d= -f2 \
      | LC_ALL=C awk '
          NR == 1 { low = $1; high = $1 }
          { if ($1 < low) low = $1; if ($1 > high) high = $1; total += $1 }
          END { printf "  it reported %d second(s) at %.2f to %.2f fps, mean %.2f\n", NR, low, high, total / NR }'
    local missed
    missed=$(grep -oE 'dropped=[0-9]+' "$window" | cut -d= -f2 | LC_ALL=C awk '{ total += $1 } END { print total + 0 }')
    echo "  and dropped $missed frame(s) across those seconds"
  fi
  rm -f "$window"

  sudo systemctl start ovsd
  settle || { echo "  the wall did not report again after the core came back" >&2; return 1; }
  echo "  the core is back, the wall is at $(fps) fps"
  echo "  the show: $(curl -fsS "$api/server/status" | jq -r '.telemetry.current_project_name // "none"')"
  echo "  renderer as the core sees it: $(curl -fsS "$api/server/status" | jq -r '.telemetry.renderer.state')"
}

# --- the core, killed rather than asked -------------------------------------

core_killed() {
  echo
  echo "== the core, killed with SIGKILL =="
  local pid
  pid=$(systemctl show ovsd -p MainPID --value)
  echo "  it is $pid, and the wall is at $(fps) fps"
  sudo kill -9 "$pid"
  sleep 8
  echo "  ovsd is $(systemctl is-active ovsd) and the wall is at $(fps) fps"
  settle || { echo "  the wall did not report again" >&2; return 1; }
  echo "  the show: $(curl -fsS "$api/server/status" | jq -r '.telemetry.current_project_name // "none"')"
}

put_back() {
  sudo systemctl start ovsd ovs-renderer 2>/dev/null || true
  if settle; then
    echo
    echo "everything is back: $(fps) fps, $(curl -fsS "$api/server/status" | jq -r '.telemetry.active_decoders') playing"
  else
    echo "the wall is not lit; look at it" >&2
  fi
}
trap put_back EXIT

case "$only" in
  renderer) renderer_killed ;;
  core) core_stopped ;;
  core-killed) core_killed ;;
  all)
    renderer_killed
    core_stopped
    core_killed
    ;;
  *) echo "usage: $0 [all|renderer|core|core-killed]" >&2; exit 2 ;;
esac
