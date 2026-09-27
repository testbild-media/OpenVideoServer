#!/usr/bin/env bash
# What one thing costs the wall, measured against the wall without it.
#
#   ./tools/cost-of.sh switch     a quantized slot change
#   ./tools/cost-of.sh switch 2   the same, with two more clips playing
#   ./tools/cost-of.sh console    the System screen sharing the card (R3)
#   ./tools/cost-of.sh effects    a chain of three on every layer
#
# Run it on the server. It puts the show back when it finishes, whichever way it
# finishes.
#
# # Why this exists
#
# The other tools here ask what the machine does. This asks what one thing
# *costs*, which is a different question and an easier one to answer wrongly.
#
# # The method, which was arrived at the hard way
#
# Every answer is a difference between two windows, and the two windows are
# **interleaved and repeated**. A first attempt at the switch measurement took
# one window of each, in order, and reported switching as cheaper than standing
# still — one frame against thirty-nine. Two things made that: the machine
# drifts over minutes (it is a laptop, it gets hot, its clocks move), and the
# script rewrote the project between conditions, which retires every player and
# starts it again. Those frames landed in the window that was supposed to be
# measuring nothing happening.
#
# So: the project is written once and left alone, the conditions alternate, and
# three rounds of each are printed rather than an average. A reader can see the
# spread and decide whether the difference is one.

set -euo pipefail
cd "$(dirname "$0")/.."

api="${OVS_BASE:-http://127.0.0.1:7420/api/v1}"
what="${1:-}"
under="${2:-0}"
work=$(mktemp -d)

id=$(curl -fsS "$api/server/status" | jq -r '.telemetry.current_project')
[ "$id" != "null" ] && [ -n "$id" ] || { echo "no project is open" >&2; exit 1; }
curl -fsS "$api/projects/$id" > "$work/before.json"

restore() {
  if [ -s "$work/before.json" ]; then
    jq '{project: .}' "$work/before.json" > "$work/restore.json"
    curl -fsS -o /dev/null -X PUT -H 'content-type: application/json' \
      --data @"$work/restore.json" "$api/projects/$id" || true
    sleep 2
    local first
    first=$(jq -r '.layers[0].id // empty' "$work/before.json")
    [ -n "$first" ] && curl -fsS -o /dev/null -X POST -H 'content-type: application/json' \
      --data '{"slot_index":1}' "$api/live/layers/$first/slot" || true
    sleep 3
    echo
    echo "the show is back at $(curl -fsS "$api/server/status" | jq -r '.telemetry.outputs[0].actual_fps') fps"
  fi
  sudo systemctl start ovs-console 2>/dev/null || true
  rm -rf "$work"
}
trap restore EXIT

dropped() {
  curl -fsS "$api/server/status" | jq -r '.telemetry.outputs[0].dropped_frames.lifetime'
}

save() {
  local layers="$1" code
  jq --slurpfile l "$layers" '.layers = $l[0]' "$work/before.json" > "$work/show.json"
  jq '{project: .}' "$work/show.json" > "$work/save.json"
  code=$(curl -fsS -o "$work/said.json" -w '%{http_code}' -X PUT \
    -H 'content-type: application/json' --data @"$work/save.json" "$api/projects/$id")
  [ "$code" = "200" ] || { echo "the show was refused ($code): $(head -c 300 "$work/said.json")" >&2; exit 1; }
  sleep 2
  local one
  for one in $(jq -r '.layers[].id' "$work/show.json"); do
    curl -fsS -o /dev/null -X POST -H 'content-type: application/json' \
      --data '{"slot_index":1}' "$api/live/layers/$one/slot"
  done
  curl -fsS -o /dev/null -X PUT -H 'content-type: application/json' \
    --data '{"crossfader":0.0,"dimmer":1.0,"freeze":false}' "$api/live/master"
  sleep 8
}

# Three rounds of two thirty-second windows, alternating. `$1` and `$2` are
# what to run during each window — a shell function taking no arguments.
compare() {
  local doing="$1" not="$2" left="$3" right="$4" round a b
  printf '%-8s %-22s %-22s\n' round "$left" "$right"
  for round in 1 2 3; do
    a=$(measure "$doing")
    b=$(measure "$not")
    printf '%-8s %-22s %-22s\n' "$round" "$a" "$b"
  done
}

measure() {
  local doing="$1" before after
  before=$(dropped)
  "$doing"
  after=$(dropped)
  echo "$((after - before)) frame(s)"
}

still() { sleep 30; }

# --- a quantized slot change -------------------------------------------------

SWITCHING_LAYER=01ARZ3NDEKTSV4RRFFQ69G5FT0

switching() {
  local slot=2 _
  for _ in $(seq 10); do
    curl -fsS -o /dev/null -X POST -H 'content-type: application/json' \
      --data "{\"slot_index\":$slot}" "$api/live/layers/$SWITCHING_LAYER/slot"
    slot=$(( slot == 2 ? 1 : 2 ))
    sleep 3
  done
}

cost_of_switch() {
  mapfile -t clips < <(curl -fsS "$api/media" | jq -r '
    .items[] | select(.state == "ready" and .kind == "video") | .id')
  [ "${#clips[@]}" -ge 2 ] || { echo "needs two playable clips in the library" >&2; exit 1; }

  jq -n --arg layer "$SWITCHING_LAYER" --arg a "${clips[0]}" --arg b "${clips[1]}" \
        --argjson under "$under" '
    def slot($i; $item): {
      index: $i, source: {kind: "media", media_item_id: $item},
      playback_mode: "loop", direction: "forward",
      transform: {x: 0.0, y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation_deg: 0.0},
      effects: []
    };
    def layer($n; $id; $name; $quantize; $slots): {
      id: $id, name: $name, order: $n, enabled: true, route: "A",
      default_opacity: 1.0, blend_mode: "normal",
      default_transform: {x: 0.0, y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation_deg: 0.0},
      default_quantization: $quantize, default_speed_ratio: 1.0,
      slots: $slots, effects: []
    };
    [layer(0; $layer; "Switching"; "4"; [slot(1; $a), slot(2; $b)])]
    + ([ layer(1; "01ARZ3NDEKTSV4RRFFQ69G5FT1"; "Load 1"; "off"; [slot(1; $a)]),
         layer(2; "01ARZ3NDEKTSV4RRFFQ69G5FT2"; "Load 2"; "off"; [slot(1; $b)]) ][:$under])
  ' > "$work/layers.json"

  save "$work/layers.json"
  echo "a slot change quantized to a bar, ten in each window, $under other clip(s) playing"
  echo
  compare switching still "switching" "left alone"
}

# --- the System screen -------------------------------------------------------

cost_of_console() {
  echo "the show as it is, with the System screen up and with it stopped"
  echo
  local round a b
  printf '%-8s %-22s %-22s\n' round "console up" "console stopped"
  for round in 1 2 3; do
    sudo systemctl start ovs-console 2>/dev/null || true
    sleep 6
    a=$(measure still)
    sudo systemctl stop ovs-console 2>/dev/null || true
    sleep 6
    b=$(measure still)
    printf '%-8s %-22s %-22s\n' "$round" "$a" "$b"
  done
  sudo systemctl start ovs-console 2>/dev/null || true
}

# --- an effect chain ---------------------------------------------------------

cost_of_effects() {
  jq '[ .layers[] | . + {effects: [
      {id: "01ARZ3NDEKTSV4RRFFQ69G5EE1", type_id: "ovs.colour_correct", version: 1, enabled: true,
       parameters: {brightness: {type: "float", value: 1.1}}},
      {id: "01ARZ3NDEKTSV4RRFFQ69G5EE2", type_id: "ovs.tint", version: 1, enabled: true,
       parameters: {amount: {type: "float", value: 0.4}}},
      {id: "01ARZ3NDEKTSV4RRFFQ69G5EE3", type_id: "ovs.pixelate", version: 1, enabled: true,
       parameters: {block_px: {type: "float", value: 6.0}}}
    ]} ]' "$work/before.json" > "$work/with.json"
  jq '[ .layers[] | . + {effects: []} ]' "$work/before.json" > "$work/without.json"

  echo "the show as it is, with three effects on every layer and with none"
  echo
  local round a b
  printf '%-8s %-22s %-22s\n' round "three each" "none"
  for round in 1 2 3; do
    save "$work/with.json" > /dev/null
    a=$(measure still)
    save "$work/without.json" > /dev/null
    b=$(measure still)
    printf '%-8s %-22s %-22s\n' "$round" "$a" "$b"
  done
}

case "$what" in
  switch) cost_of_switch ;;
  console) cost_of_console ;;
  effects) cost_of_effects ;;
  *) echo "usage: $0 [switch [others]|console|effects]" >&2; exit 2 ;;
esac
