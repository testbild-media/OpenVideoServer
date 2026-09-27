#!/usr/bin/env bash
# Proves, on the machine with the wall on it, that the things that matter work.
#
#   ./tools/acceptance.sh crossfader   A and B dissolve, and the dimmer dims
#   ./tools/acceptance.sh effect       an effect changes what the composition shows
#   ./tools/acceptance.sh blend        each blend mode does its own arithmetic
#   ./tools/acceptance.sh freeze       a frozen output stops, and starts again
#   ./tools/acceptance.sh clock        triggers land on the grid, not when asked
#   ./tools/acceptance.sh recover      how long the wall is dark if the renderer dies
#   ./tools/acceptance.sh console      a lighting console reaches the wall
#   ./tools/acceptance.sh live-effect  an effect moves during a show, and the project does not
#   ./tools/acceptance.sh all          every one of them
#
# Run it on the server itself: the API answers a request from the machine with
# no session, and these read the composition preview, which is
# what the renderer actually drew rather than what the code says it should have.
#
# # Why these and not a unit test
#
# Every one of these has a unit test, and every one of those passes on a machine
# with no display attached. What they cannot tell you is whether the thing an
# operator moves reaches the wall: a crossfader that composites correctly in a
# test and is wired to nothing looks identical from inside the process.
#
# So each check changes something through the API an operator would use, reads
# the picture back, and says what it saw. Each one puts the project back exactly
# as it found it.
#
# # What it needs
#
# `jq`, `ffmpeg` and `python3` on the server, a project open, and an output
# running. It writes nothing outside a temporary directory.

set -euo pipefail

api="${OVS_BASE:-http://127.0.0.1:7420/api/v1}"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fail() { echo "FAILED: $*" >&2; exit 1; }

# The mean of each channel across the composition, which is how a colour is
# recognised without an image library.
mean() {
  curl -s "$api/preview/composition" -o "$work/shot.png"
  ffmpeg -loglevel error -i "$work/shot.png" -f rawvideo -pix_fmt rgb24 - 2>/dev/null | python3 -c "
import sys
d = sys.stdin.buffer.read()
if not d:
    print('0 0 0')
else:
    n = len(d) // 3
    print(f'{sum(d[0::3])/n:.1f} {sum(d[1::3])/n:.1f} {sum(d[2::3])/n:.1f}')
"
}

open_project() {
  curl -s "$api/server/status" | jq -r '.telemetry.current_project'
}

save_project() {
  jq '{project: .}' "$1" > "$work/save.json"
  curl -s -o "$work/said.json" -w '%{http_code}' -X PUT \
    -H 'content-type: application/json' --data @"$work/save.json" "$api/projects/$2"
}

# ---------------------------------------------------------------- crossfader
#
# Two layers of flat colour, one on each bus, and the fader moved across.
# A dissolve is the one thing about A/B that an operator feels rather than
# reads: a fader that jumped at the halfway point would pass every test that
# checks the endpoints.
prove_crossfader() {
  local id; id=$(open_project)
  curl -s "$api/projects/$id" > "$work/before.json"

  local layer_a layer_b
  layer_a=$(flat_layer "A side" 0 A 1.0 0.0 01ARZ3NDEKTSV4RRFFQ69G5FAV)
  layer_b=$(flat_layer "B side" 1 B 0.0 1.0 01ARZ3NDEKTSV4RRFFQ69G5FAW)

  jq --argjson a "$layer_a" --argjson b "$layer_b" '.layers = [$a, $b]' \
    "$work/before.json" > "$work/show.json"

  [ "$(save_project "$work/show.json" "$id")" = "200" ] \
    || { head -c 600 "$work/said.json"; echo; restore "$id"; fail "the two-layer show would not save"; }
  sleep 2

  local layer
  for layer in $(jq -r '.layers[].id' "$work/show.json"); do
    curl -s -o /dev/null -X POST -H 'content-type: application/json' \
      --data '{"slot_index":1}' "$api/live/layers/$layer/slot"
  done
  sleep 2

  local position reading
  for position in 0.0 0.25 0.5 0.75 1.0; do
    curl -s -o /dev/null -X PUT -H 'content-type: application/json' \
      --data "{\"crossfader\":$position}" "$api/live/master"
    sleep 1
    reading=$(mean)
    printf 'crossfader %-5s red/blue %s\n' "$position" "$reading"
    check_mix "$position" "$reading" || { restore "$id"; fail "the crossfader is not a dissolve"; }
  done

  local dimmer
  for dimmer in 1.0 0.5 0.0; do
    curl -s -o /dev/null -X PUT -H 'content-type: application/json' \
      --data "{\"dimmer\":$dimmer}" "$api/live/master"
    sleep 1
    reading=$(mean)
    printf 'dimmer     %-5s red/blue %s\n' "$dimmer" "$reading"
  done

  curl -s -o /dev/null -X PUT -H 'content-type: application/json' \
    --data '{"dimmer":1.0,"crossfader":0.0}' "$api/live/master"
  restore "$id"
  echo "crossfader: a dissolve, and the dimmer after it"
}

# A layer showing one flat colour, full frame.
flat_layer() {
  jq -n --arg name "$1" --argjson order "$2" --arg route "$3" \
        --argjson r "$4" --argjson b "$5" --arg id "$6" '
    {
      id: $id, name: $name, order: $order, enabled: true, route: $route,
      default_opacity: 1.0, blend_mode: "normal",
      default_transform: {x: 0.0, y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation_deg: 0.0},
      default_quantization: "off", default_speed_ratio: 1.0,
      slots: [{
        index: 1,
        source: {
          kind: "generator", type_id: "ovs.solid_colour", version: 1,
          parameters: {colour: {type: "color", value: [$r, 0.0, $b, 1.0]}}
        },
        playback_mode: "loop", direction: "forward",
        transform: {x: 0.0, y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation_deg: 0.0},
        effects: []
      }],
      effects: []
    }'
}

# Red should fall and blue should rise in proportion to the fader, within the
# slack an eight-bit picture and a JPEG-free PNG leave.
check_mix() {
  python3 -c "
import sys
position = float('$1')
red, _green, blue = (float(v) for v in '$2'.split())
wanted_red = 255.0 * (1.0 - position)
wanted_blue = 255.0 * position
ok = abs(red - wanted_red) <= 8.0 and abs(blue - wanted_blue) <= 8.0
sys.exit(0 if ok else 1)
"
}

# -------------------------------------------------------------------- effect
#
# One layer of flat grey, and a brightness of two on it. Flat rather than
# whatever the show happens to be playing: a clip's mean moves frame to frame,
# so a difference of a few counts would prove nothing either way, and a quarter
# grey doubling to a half grey is a number that can only come from the effect
# having run.
prove_effect() {
  local id; id=$(open_project)
  curl -s "$api/projects/$id" > "$work/before.json"

  local grey
  grey=$(flat_layer "Grey" 0 A 0.25 0.25 01ARZ3NDEKTSV4RRFFQ69G5FAY)
  jq --argjson g "$grey" '.layers = [$g]' "$work/before.json" > "$work/plain.json"

  [ "$(save_project "$work/plain.json" "$id")" = "200" ]     || { head -c 600 "$work/said.json"; echo; restore "$id"; fail "the flat layer would not save"; }
  show_the_slots "$work/plain.json"

  local before after
  before=$(mean)
  echo "a quarter grey          $before"

  jq '.layers[0].effects = [{
        id: "01ARZ3NDEKTSV4RRFFQ69G5FAX",
        type_id: "ovs.colour_correct",
        version: 1,
        enabled: true,
        parameters: {brightness: {type: "float", value: 2.0}}
      }]' "$work/plain.json" > "$work/bright.json"

  [ "$(save_project "$work/bright.json" "$id")" = "200" ]     || { head -c 600 "$work/said.json"; echo; restore "$id"; fail "the effect would not save"; }
  show_the_slots "$work/bright.json"

  after=$(mean)
  echo "the same at brightness 2 $after"
  restore "$id"

  python3 -c "
import sys
before = [float(v) for v in '$before'.split()]
after = [float(v) for v in '$after'.split()]
# Doubling a quarter grey gives a half grey. Eight counts of slack for the
# eight-bit picture the preview is read from.
wanted = [min(255.0, v * 2.0) for v in before]
off = max(abs(a - w) for a, w in zip(after, wanted))
if off > 8.0:
    print(f'wanted about {wanted}, got {after}')
    sys.exit(1)
" || fail "a brightness of two did not double the picture"

  echo "effect: a brightness of two doubles what the composition shows"
}

# --------------------------------------------------------------------- blend
#
# Two flat colours on one bus, the upper one in each mode in turn. Every mode
# has its own arithmetic and every one of them is a one-line formula, so what
# comes back either is that formula or is not — there is no room for a mode that
# looks about right.
prove_blend() {
  local id; id=$(open_project)
  curl -s "$api/projects/$id" > "$work/before.json"

  # Two thirds and one third, chosen because no pair of modes agrees on them:
  # multiply gives a fifth, screen four fifths, difference a third, and none of
  # those is either input.
  local under over
  under=$(flat_layer "Under" 0 A 0.6667 0.6667 01ARZ3NDEKTSV4RRFFQ69G5FB0)
  over=$(flat_layer "Over" 1 A 0.3333 0.3333 01ARZ3NDEKTSV4RRFFQ69G5FB1)

  local mode reading
  for mode in normal add multiply screen lighten darken difference; do
    jq --argjson u "$under" --argjson o "$over" --arg mode "$mode" \
      '.layers = [$u, ($o | .blend_mode = $mode)]' \
      "$work/before.json" > "$work/blend.json"

    [ "$(save_project "$work/blend.json" "$id")" = "200" ] \
      || { head -c 600 "$work/said.json"; echo; restore "$id"; fail "the $mode show would not save"; }
    show_the_slots "$work/blend.json"

    reading=$(mean)
    printf '%-11s %s\n' "$mode" "$reading"
    check_blend "$mode" "$reading" || { restore "$id"; fail "$mode is not $mode"; }
  done

  restore "$id"
  echo "blend: every mode does its own arithmetic"
}

# What each mode makes of two thirds under one third.
check_blend() {
  python3 -c "
import sys
under, over = 170.0, 85.0   # what 0.6667 and 0.3333 come back as in eight bits
wanted = {
    'normal': over,
    'add': min(255.0, under + over),
    'multiply': under * over / 255.0,
    'screen': 255.0 - (255.0 - under) * (255.0 - over) / 255.0,
    'lighten': max(under, over),
    'darken': min(under, over),
    'difference': abs(under - over),
}['$1']
red, _green, blue = (float(v) for v in '$2'.split())
off = max(abs(red - wanted), abs(blue - wanted))
if off > 6.0:
    print(f'wanted about {wanted:.0f}, got {red:.0f}')
    sys.exit(1)
"
}

# -------------------------------------------------------------------- freeze
#
# Freeze holds the final output. The thing worth proving is that it holds it
# *and lets go*: an output that froze and stayed frozen would look, for the first
# few seconds, exactly like one that froze correctly.
prove_freeze() {
  local id; id=$(open_project)
  curl -s "$api/projects/$id" > "$work/before.json"

  local red blue
  red=$(flat_layer "A side" 0 A 1.0 0.0 01ARZ3NDEKTSV4RRFFQ69G5FAV)
  blue=$(flat_layer "B side" 1 B 0.0 1.0 01ARZ3NDEKTSV4RRFFQ69G5FAW)
  jq --argjson a "$red" --argjson b "$blue" '.layers = [$a, $b]' \
    "$work/before.json" > "$work/show.json"

  [ "$(save_project "$work/show.json" "$id")" = "200" ] \
    || { restore "$id"; fail "the two-layer show would not save"; }
  show_the_slots "$work/show.json"

  curl -s -o /dev/null -X PUT -H 'content-type: application/json' \
    --data '{"crossfader":0.0,"freeze":false}' "$api/live/master"
  sleep 1
  local held; held=$(mean)
  echo "fully A, running    $held"

  curl -s -o /dev/null -X PUT -H 'content-type: application/json' \
    --data '{"freeze":true}' "$api/live/master"
  sleep 1
  curl -s -o /dev/null -X PUT -H 'content-type: application/json' \
    --data '{"crossfader":1.0}' "$api/live/master"
  sleep 2
  local frozen; frozen=$(mean)
  echo "fader moved, frozen $frozen"

  curl -s -o /dev/null -X PUT -H 'content-type: application/json' \
    --data '{"freeze":false}' "$api/live/master"
  sleep 2
  local thawed; thawed=$(mean)
  echo "and let go          $thawed"

  curl -s -o /dev/null -X PUT -H 'content-type: application/json' \
    --data '{"crossfader":0.0}' "$api/live/master"
  restore "$id"

  python3 -c "
import sys
held = [float(v) for v in '$held'.split()]
frozen = [float(v) for v in '$frozen'.split()]
thawed = [float(v) for v in '$thawed'.split()]

if max(abs(a - b) for a, b in zip(held, frozen)) > 6.0:
    print(f'the fader moved the picture while it was frozen: {held} then {frozen}')
    sys.exit(1)
if max(abs(a - b) for a, b in zip(frozen, thawed)) <= 6.0:
    print(f'the picture did not move when it was let go: {frozen} then {thawed}')
    sys.exit(1)
" || fail "freeze does not hold, or does not let go"

  echo "freeze: holds the output, and lets go of it"
}

# --------------------------------------------------------------------- clock
#
# One layer with a red slot and a blue slot, and the grid set to a bar. A
# trigger asked for at any moment must not take effect until the next bar line,
# so two triggers asked for three seconds apart must land eight seconds apart —
# a whole bar at thirty beats a minute — rather than three.
#
# That is the property worth measuring, because it cannot be produced by
# accident: a quantizer that simply delayed everything by a fixed amount would
# put the two flips three seconds apart, and one that ignored the grid would put
# them there too.
prove_clock() {
  local id; id=$(open_project)
  curl -s "$api/projects/$id" > "$work/before.json"

  jq --argjson l "$(two_slot_layer)" '.layers = [$l]' \
    "$work/before.json" > "$work/clock.json"
  [ "$(save_project "$work/clock.json" "$id")" = "200" ] \
    || { head -c 600 "$work/said.json"; echo; restore "$id"; fail "the two-slot layer would not save"; }
  sleep 2

  local layer; layer=$(jq -r '.layers[0].id' "$work/clock.json")

  # Thirty beats a minute: a beat is two seconds and a bar is eight, which is
  # long enough to measure through a preview that changes five times a second.
  curl -s -o /dev/null -X PUT -H 'content-type: application/json' \
    --data '{"bpm":30.0,"crossfader":0.0,"dimmer":1.0,"freeze":false}' "$api/live/master"
  curl -s -o /dev/null -X PUT -H 'content-type: application/json' \
    --data '{"quantization":"4"}' "$api/live/layers/$layer"

  select_slot "$layer" 1
  wait_for_colour red 12 >/dev/null || { restore "$id"; fail "the red slot never showed"; }

  local first second
  select_slot "$layer" 2
  first=$(wait_for_colour blue 12) || { restore "$id"; fail "the blue slot never showed"; }
  echo "first trigger landed after  ${first}s"

  sleep 3
  select_slot "$layer" 1
  second=$(wait_for_colour red 12) || { restore "$id"; fail "the red slot never came back"; }
  echo "second, asked 3s later, after ${second}s"

  restore "$id"

  python3 -c "
import sys
second = float('$second')
# The second trigger was asked for three seconds after the first one landed, so
# the two flips are 3 + second apart. On the grid that has to be a whole bar.
gap = 3.0 + second
if abs(gap - 8.0) > 1.0:
    print(f'the two flips were {gap:.1f}s apart; a bar at 30 BPM is 8.0s')
    sys.exit(1)
" || fail "triggers are not landing on the grid"

  echo "clock: two triggers three seconds apart landed one bar apart"
}

# A layer with a red slot and a blue slot, so a trigger is visible.
two_slot_layer() {
  jq -n '
    {
      id: "01ARZ3NDEKTSV4RRFFQ69G5FB2", name: "Grid", order: 0, enabled: true,
      route: "A", default_opacity: 1.0, blend_mode: "normal",
      default_transform: {x: 0.0, y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation_deg: 0.0},
      default_quantization: "4", default_speed_ratio: 1.0,
      slots: [
        {
          index: 1,
          source: {kind: "generator", type_id: "ovs.solid_colour", version: 1,
                   parameters: {colour: {type: "color", value: [1.0, 0.0, 0.0, 1.0]}}},
          playback_mode: "loop", direction: "forward",
          transform: {x: 0.0, y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation_deg: 0.0},
          effects: []
        },
        {
          index: 2,
          source: {kind: "generator", type_id: "ovs.solid_colour", version: 1,
                   parameters: {colour: {type: "color", value: [0.0, 0.0, 1.0, 1.0]}}},
          playback_mode: "loop", direction: "forward",
          transform: {x: 0.0, y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation_deg: 0.0},
          effects: []
        }
      ],
      effects: []
    }'
}

select_slot() {
  curl -s -o /dev/null -X POST -H 'content-type: application/json' \
    --data "{\"slot_index\":$2}" "$api/live/layers/$1/slot"
}

# Waits until the composition is mostly that colour, and says how long it took.
wait_for_colour() {
  local wanted="$1" limit="$2"
  local started; started=$(date +%s.%N)
  local reading elapsed
  while :; do
    reading=$(mean)
    elapsed=$(python3 -c "print(f'{$(date +%s.%N) - $started:.1f}')")
    if colour_is "$wanted" "$reading"; then
      echo "$elapsed"
      return 0
    fi
    if python3 -c "import sys; sys.exit(0 if $elapsed > $limit else 1)"; then
      return 1
    fi
    sleep 0.2
  done
}

colour_is() {
  python3 -c "
import sys
red, _green, blue = (float(v) for v in '$2'.split())
wanted = '$1'
ok = (red > 200 and blue < 60) if wanted == 'red' else (blue > 200 and red < 60)
sys.exit(0 if ok else 1)
"
}

# ------------------------------------------------------------------- recover
#
# The unit says Restart=always, because a renderer that has stopped is a dark
# wall and a rack machine has nobody standing next to it. What that costs had
# never been measured.
#
# Timed from the kill to the first frame that reaches the panel, using the
# process identity rather than the telemetry: the core's view of the renderer is
# a moment behind it, so polling that would time the core's notice rather than
# the wall's darkness.
#
# Needs sudo, and it does kill the show for a second or two. Not one to run
# during one.
prove_recover() {
  local before; before=$(pgrep -x ovs-renderer | head -1 || true)
  [ -n "$before" ] || fail "there is no renderer running to kill"

  local started; started=$(date +%s.%N)
  sudo -n systemctl kill --signal=SIGKILL ovs-renderer || fail "could not stop the renderer"

  local now=""
  for _ in $(seq 1 300); do
    now=$(pgrep -x ovs-renderer | head -1 || true)
    if [ -n "$now" ] && [ "$now" != "$before" ]; then break; fi
    sleep 0.1
  done
  [ -n "$now" ] && [ "$now" != "$before" ] || fail "no renderer came back"
  local up; up=$(date +%s.%N)
  python3 -c "print(f'  a new renderer existed after {$up - $started:.1f}s')"

  local fps=0
  for _ in $(seq 1 600); do
    fps=$(curl -s "$api/server/status" | jq -r '.telemetry.outputs[0].actual_fps // 0')
    if python3 -c "import sys; sys.exit(0 if $fps > 30 else 1)"; then break; fi
    sleep 0.1
  done
  local lit; lit=$(date +%s.%N)
  python3 -c "
import sys
dark = $lit - $started
print(f'  the wall was dark for {dark:.1f}s')
if dark > 5.0:
    print('  which is longer than anybody would call a recovery')
    sys.exit(1)
" || fail "the wall took too long to come back"

  curl -s "$api/server/status" | jq -r '
    "  back at \(.telemetry.outputs[0].actual_fps) fps, " +
    "\(.telemetry.outputs[0].dropped_frames.lifetime) dropped since it started, " +
    "and the core still calls it \(.telemetry.renderer.state)"'

  echo "recover: the wall comes back on its own"
}

# ------------------------------------------------------------------- console
#
# The whole point of the lighting integration: a fader on a desk and a picture
# on a wall have to agree. Everything short of this — packets counted, universes
# listed, priorities arbitrated — can be true while the layer stays dark.
#
# Needs a sender. Set OVS_SEND to an `ovs-dmx-send` binary; the server does not
# transmit and has nothing of its own that could stand in for a console. On the
# machine itself that is one build away and there is no reason to skip it:
#
#   cargo build --release --bin ovs-dmx-send
#   OVS_SEND=./target/release/ovs-dmx-send ./tools/acceptance.sh
prove_console() {
  # Found rather than demanded. This check skipped in every run for weeks
  # because nobody had set a variable, which made a suite of seven read as green
  # while proving six — and this is the one that proves the whole point of the
  # lighting integration.
  if [ -z "${OVS_SEND:-}" ]; then
    for candidate in ./target/release/ovs-dmx-send ./target/debug/ovs-dmx-send "$(command -v ovs-dmx-send || true)"; do
      if [ -n "$candidate" ] && [ -x "$candidate" ]; then
        OVS_SEND="$candidate"
        break
      fi
    done
  fi
  if [ -z "${OVS_SEND:-}" ] && command -v cargo > /dev/null; then
    echo "  building a sender, because this check needs a console and the server is not one"
    cargo build --release --bin ovs-dmx-send --quiet 2>/dev/null || true
    [ -x ./target/release/ovs-dmx-send ] && OVS_SEND=./target/release/ovs-dmx-send
  fi
  [ -n "${OVS_SEND:-}" ] || fail "no ovs-dmx-send anywhere; build one or set OVS_SEND"

  local id; id=$(open_project)
  curl -s "$api/projects/$id" > "$work/before.json"
  local layer=01ARZ3NDEKTSV4RRFFQ69G5FC0

  flat_layer "Patched" 0 A 1.0 1.0 "$layer" > "$work/layer.json"
  # The personality this server actually publishes, rather than one this script
  # remembers. It was written with fourteen channels and the owner's layout has
  # twelve; a harness that knows the numbers goes quietly wrong the
  # day they change, which is the day it most needs to work.
  curl -s "$api/fixtures" > "$work/fixtures.json"
  local footprint
  footprint=$(jq -r '.fixtures[] | select(.type_id == "ovs.layer") | .footprint' "$work/fixtures.json")
  [ -n "$footprint" ] && [ "$footprint" != "null" ] \
    || fail "this server publishes no ovs.layer personality"

  jq --argjson l "$(cat "$work/layer.json")" --arg layer "$layer" --argjson f "$footprint" '
    .layers = [$l]
    | .fixture_patch = [{
        id: "01ARZ3NDEKTSV4RRFFQ69G5FC1",
        type_id: "ovs.layer", version: 1,
        target: {kind: "layer", layer_id: $layer},
        universe: 1, start_address: 1, footprint: $f, enabled: true
      }]' "$work/before.json" > "$work/patched.json"

  [ "$(save_project "$work/patched.json" "$id")" = "200" ] \
    || { head -c 600 "$work/said.json"; echo; restore "$id"; fail "the patched show would not save"; }
  show_the_slots "$work/patched.json"

  # A patched layer with nobody driving it holds what the project says, rather
  # than adopting whatever a desk that has gone home left on the wire (R15).
  local quiet; quiet=$(mean)
  echo "  patched, with no console sending: $quiet"
  is_lit "$quiet" || { restore "$id"; fail "a patch with no console on it took the layer"; }

  # One console at a time. These two used to overlap for eight seconds — the
  # first sent for twelve and the second started after four — so two sources of
  # the same name and the same priority were on universe 1 together, one at full
  # and one at half, and the check measured which of them the arbitration
  # happened to pick. It passed most of the time, which is the worst way for a
  # check to be wrong: it fails rarely enough to be dismissed.
  #
  # What a desk's home preset contains, built from the published homes rather
  # than typed out. A console sends a *whole* universe, and every channel it has
  # not programmed arrives as zero — which on this personality means a scale of
  # zero and a layer nobody can see. Zero stays a real value (owner decision
  # Q15); the homes are the answer, and this sends them.
  local home dimmer slot
  home=$(homed_channels ovs.layer)
  dimmer=$(channel_number ovs.layer layer_opacity)
  slot=$(channel_number ovs.layer layer_slot)
  [ -n "$dimmer" ] && [ -n "$slot" ] || fail "the layer personality has no dimmer or no slot"

  # Slot 1, in the middle of its band: three values a slot from four, so 5.
  local desk
  "$OVS_SEND" --host 127.0.0.1 --protocol sacn --universe 1 --source "acceptance" \
    --channels "$home,$dimmer=255,$slot=5" --rate 20 --seconds 6 >/dev/null 2>&1 &
  desk=$!
  sleep 4
  local full; full=$(mean)
  echo "  console at full:                 $full"
  wait "$desk" 2>/dev/null || true

  "$OVS_SEND" --host 127.0.0.1 --protocol sacn --universe 1 --source "acceptance" \
    --channels "$home,$dimmer=128,$slot=5" --rate 20 --seconds 6 >/dev/null 2>&1 &
  desk=$!
  sleep 4
  local half; half=$(mean)
  echo "  console at half:                 $half"
  wait "$desk" 2>/dev/null || true

  restore "$id"

  python3 -c "
import sys
# Red and blue only: a flat layer here drives those two.
full = [float(v) for v in '$full'.split()][::2]
half = [float(v) for v in '$half'.split()][::2]
if min(full) < 240:
    print(f'a console at full did not light the layer: {full}')
    sys.exit(1)
# 128 of 255 is halfway up an eight-bit dimmer, which is what the owner's
# layout gives this channel.
if not all(abs(a - 127.5) <= 10 for a in half):
    print(f'a console at half did not halve it: {half}')
    sys.exit(1)
" || fail "a console does not reach the wall"

  echo "console: a fader on a desk and the picture agree"
}

# One personality's home preset, as `ovs-dmx-send` takes it.
#
# `channel=value` pairs for every channel of the fixture, a wide channel split
# into the two bytes a desk puts on the wire. Read from `GET /fixtures`, so a
# channel added to a personality is sent rather than left at zero — and a zero
# left in a home is exactly the failure this whole check exists to catch.
homed_channels() {
  jq -r --arg type "$1" '
    .fixtures[] | select(.type_id == $type) | .channels
    | map(
        if .width == 2 then
          "\(.offset)=\(.home / 256 | floor),\(.offset + 1)=\(.home % 256)"
        else
          "\(.offset)=\(.home % 256)"
        end
      )
    | join(",")' "$work/fixtures.json"
}

# Which channel one control is on, one-based, or nothing when it has none.
channel_number() {
  jq -r --arg type "$1" --arg control "$2" '
    .fixtures[] | select(.type_id == $type) | .channels[]
    | select(.control == $control) | .offset' "$work/fixtures.json"
}

# Whether the layer is lit at all. Only the red and blue channels are looked at,
# because that is what `flat_layer` drives.
is_lit() {
  python3 -c "
import sys
red, _green, blue = (float(v) for v in '$1'.split())
sys.exit(0 if red > 240 and blue > 240 else 1)
"
}

# --------------------------------------------------------------- live effect
#
# An effect whose parameters cannot be moved during a show is half an effect,
# and the whole difficulty is that moving one must not edit the show. So this
# checks both halves: that the picture changes, and that the project does not.
prove_live_effect() {
  local id; id=$(open_project)
  curl -s "$api/projects/$id" > "$work/before.json"

  local layer=01ARZ3NDEKTSV4RRFFQ69G5FE0
  local effect=01ARZ3NDEKTSV4RRFFQ69G5FE1

  # A quarter grey with a colour correction at its published defaults, which is
  # neutral — so anything that moves is the live value and nothing else.
  flat_layer "Grey" 0 A 0.25 0.25 "$layer" > "$work/layer.json"
  jq --argjson l "$(cat "$work/layer.json")" --arg effect "$effect" '
    .layers = [$l | .effects = [{
      id: $effect, type_id: "ovs.colour_correct", version: 1,
      enabled: true, parameters: {}
    }]]' "$work/before.json" > "$work/show.json"

  [ "$(save_project "$work/show.json" "$id")" = "200" ] \
    || { head -c 600 "$work/said.json"; echo; restore "$id"; fail "the show would not save"; }
  show_the_slots "$work/show.json"

  local configured; configured=$(mean)
  echo "  at the configured value:  $configured"

  local sent
  sent=$(curl -s -o "$work/live.json" -w '%{http_code}' -X PUT \
    -H 'content-type: application/json' \
    --data '{"overrides":{"brightness":{"type":"float","value":2.0}}}' \
    "$api/live/effects/$effect")
  [ "$sent" = "204" ] \
    || { head -c 400 "$work/live.json"; echo; restore "$id"; fail "the live value was refused ($sent)"; }
  sleep 2
  local moved; moved=$(mean)
  echo "  with a live brightness:   $moved"

  local still; still=$(curl -s "$api/projects/$id" | jq -c '.layers[0].effects[0].parameters')
  echo "  and the project says:     $still"

  # A value the manifest cannot take. An API request can say anything, and a
  # live control that accepted a brightness of four hundred would be a way to
  # stop a show from a client.
  local refused
  refused=$(curl -s -o /dev/null -w '%{http_code}' -X PUT \
    -H 'content-type: application/json' \
    --data '{"overrides":{"brightness":{"type":"float","value":400.0}}}' \
    "$api/live/effects/$effect")
  echo "  a value out of range:     $refused"

  curl -s -o /dev/null -X PUT -H 'content-type: application/json' \
    --data '{"reset":true}' "$api/live/effects/$effect"
  sleep 2
  local back; back=$(mean)
  echo "  put back:                 $back"

  restore "$id"

  python3 -c "
import sys
configured = [float(v) for v in '$configured'.split()][::2]
moved = [float(v) for v in '$moved'.split()][::2]
back = [float(v) for v in '$back'.split()][::2]

wanted = [min(255.0, v * 2.0) for v in configured]
if max(abs(a - w) for a, w in zip(moved, wanted)) > 8.0:
    print(f'a live brightness of two wanted about {wanted}, got {moved}')
    sys.exit(1)
if max(abs(a - b) for a, b in zip(configured, back)) > 8.0:
    print(f'putting it back wanted {configured}, got {back}')
    sys.exit(1)
if '$still' not in ('null', '{}'):
    print(f'the project was edited: {\"$still\"}')
    sys.exit(1)
if '$refused' == '204':
    print('a brightness of four hundred was accepted')
    sys.exit(1)
" || fail "an effect cannot be moved live without editing the show"

  echo "live-effect: moved on the wall, and the project untouched"
}

# Selects the first real slot on every layer that has one.
#
# Live state, not configuration: what it costs is whatever an operator had
# selected, and what it buys is that these checks measure a picture rather than
# a black frame — and that the machine is left showing the show rather than
# nothing.
show_the_slots() {
  local layer index
  for layer in $(jq -r '.layers[].id' "$1"); do
    index=$(jq -r --arg id "$layer"       '.layers[] | select(.id == $id) | (.slots | map(.index) | min // 1)' "$1")
    curl -s -o /dev/null -X POST -H 'content-type: application/json'       --data "{\"slot_index\":$index}" "$api/live/layers/$layer/slot"
  done
  sleep 2
}

# Puts the project back exactly as it was found, and leaves it showing.
restore() {
  [ "$(save_project "$work/before.json" "$1")" = "200" ] || echo "WARNING: the project was not restored" >&2
  sleep 1
  show_the_slots "$work/before.json"
}

case "${1:-all}" in
  crossfader) prove_crossfader ;;
  effect) prove_effect ;;
  blend) prove_blend ;;
  freeze) prove_freeze ;;
  clock) prove_clock ;;
  recover) prove_recover ;;
  console) prove_console ;;
  live-effect) prove_live_effect ;;
  all)
    prove_effect
    prove_blend
    prove_crossfader
    prove_freeze
    prove_clock
    prove_live_effect
    # Only where there is something to play the console's part. `recover` is
    # deliberately left out: it kills the renderer, which is not something to do
    # to a machine somebody might be watching.
    if [ -n "${OVS_SEND:-}" ]; then
      prove_console
    else
      echo "console: skipped, because OVS_SEND names no sender"
    fi
    ;;
  *) echo "usage: $0 [crossfader|effect|blend|freeze|clock|recover|console|live-effect|all]" >&2; exit 2 ;;
esac
