#!/usr/bin/env bash
# How many layers this wall holds, one more at a time.
#
#   ./tools/how-many-layers.sh                 up to seven, any ready clip
#   ./tools/how-many-layers.sh 7 "H264"        only clips whose name matches
#   ./tools/how-many-layers.sh 3 "Reference"
#
# Run it on the server. It puts the show back when it finishes, whichever way it
# finishes.
#
# # Why this exists
#
# `ovs-decode-cost` answers how many decoders fit in a machine's cores, without
# a card or a display. This answers the other half, which is the one an operator
# cares about: how many layers the *wall* holds. Every frame a decoder produces
# still has to be copied onto the card inside the submit the render thread waits
# for, so the two numbers are not the same and the difference between
# them is exactly the thing worth knowing.
#
# The owner's target is seven.
#
# # One clip per layer
#
# Seven layers reading one file is seven decoders sharing a page cache, which
# flatters every figure. The clips are distinct, and the run stops with a clear
# message rather than quietly reusing one if the library has too few.
#
# # What each column is
#
# - **fps** is what the output reports, sampled through the window.
# - **dropped** is frames the wall never showed, counted on the output. This is
#   the one that matters: an audience sees it.
# - **held** is passes that wanted a frame a decoder had not produced yet. The
#   wall keeps its rate and shows one frame twice, so it is a stutter rather
#   than a gap — visible on a pan, invisible on a slow dissolve.
# - **media / playback / compose** are where the render pass spent its time, in
#   microseconds, averaged over the window. At sixty a second there are 16667 of
#   them to spend between the three.
#
#   Averaged rather than read once, and that matters: `telemetry.frame` is the
#   *last* pass's timings, so a single reading can land on a pass where nothing
#   was playing. A first run of this reported four microseconds of playback on
#   three layers for exactly that reason, next to a thousand on one layer.

set -euo pipefail
cd "$(dirname "$0")/.."

api="${OVS_BASE:-http://127.0.0.1:7420/api/v1}"
most="${1:-7}"
match="${2:-}"
window="${OVS_WINDOW:-30}"
work=$(mktemp -d)

id=$(curl -fsS "$api/server/status" | jq -r '.telemetry.current_project')
[ "$id" != "null" ] && [ -n "$id" ] || { echo "no project is open" >&2; exit 1; }
curl -fsS "$api/projects/$id" > "$work/before.json"

restore() {
  if [ -s "$work/before.json" ]; then
    jq '{project: .}' "$work/before.json" > "$work/restore.json"
    curl -fsS -o /dev/null -X PUT -H 'content-type: application/json' \
      --data @"$work/restore.json" "$api/projects/$id" || true
    sleep 3
    echo
    echo "the show is back at $(curl -fsS "$api/server/status" | jq -r '.telemetry.outputs[0].actual_fps') fps"
  fi
  rm -rf "$work"
}
trap restore EXIT

mapfile -t clips < <(curl -fsS "$api/media" | jq -r --arg match "$match" '
  .items[]
  | select(.state == "ready" and .kind == "video")
  | select($match == "" or (.display_name | contains($match)))
  | .id')

if [ "${#clips[@]}" -lt "$most" ]; then
  echo "the library has ${#clips[@]} ready clip(s) matching \"$match\", and this wants $most" >&2
  echo "one clip per layer, because layers sharing a file share a page cache" >&2
  exit 1
fi

status() { curl -fsS "$api/server/status"; }

# Builds a project of `$1` layers, each on its own clip, and triggers them all.
stage() {
  local count="$1" one code
  jq -n --argjson count "$count" --argjson clips "$(printf '%s\n' "${clips[@]}" | jq -R . | jq -s .)" '
    def slot($item): {
      index: 1, source: {kind: "media", media_item_id: $item},
      playback_mode: "loop", direction: "forward",
      transform: {x: 0.0, y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation_deg: 0.0},
      effects: []
    };
    [range(0; $count) as $n | {
      # Crockford base32, which is what a ULID is: no I, L, O or U. Two digits
      # always, because twenty-six characters is the whole of the rule and a
      # tenth layer that quietly became twenty-seven is a refusal in the middle
      # of a sweep that had already taken four minutes.
      id: ("01ARZ3NDEKTSV4RRFFQ69G5H" + (100 + $n | tostring | .[1:])),
      name: ("Layer " + (($n + 1) | tostring)),
      order: $n, enabled: true, route: "A",
      default_opacity: (if $n == 0 then 1.0 else 0.5 end),
      blend_mode: "normal",
      default_transform: {x: 0.0, y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation_deg: 0.0},
      default_quantization: "off", default_speed_ratio: 1.0,
      slots: [slot($clips[$n])], effects: []
    }]' > "$work/layers.json"

  jq --slurpfile l "$work/layers.json" '.layers = $l[0]' "$work/before.json" > "$work/show.json"
  jq '{project: .}' "$work/show.json" > "$work/save.json"
  # No -f: a refusal is something to print, not something to disappear into
  # an exit code before the line that prints it.
  code=$(curl -sS -o "$work/said.json" -w '%{http_code}' -X PUT \
    -H 'content-type: application/json' --data @"$work/save.json" "$api/projects/$id")
  [ "$code" = "200" ] || { echo "the show was refused ($code): $(head -c 300 "$work/said.json")" >&2; exit 1; }
  sleep 2
  for one in $(jq -r '.layers[].id' "$work/show.json"); do
    curl -fsS -o /dev/null -X POST -H 'content-type: application/json' \
      --data '{"slot_index":1}' "$api/live/layers/$one/slot"
  done
  curl -fsS -o /dev/null -X PUT -H 'content-type: application/json' \
    --data '{"crossfader":0.0,"dimmer":1.0,"freeze":false}' "$api/live/master"
  # Long enough for every decoder to have opened its file and filled its ring.
  # A window that starts before that measures the opening rather than the show.
  sleep 8
}

echo "one clip a layer, ${window}s a window, matching \"${match:-anything}\""
echo
printf '%-8s %-8s %-9s %-7s %-8s %-10s %s\n' \
  layers fps dropped held media playback compose

for count in $(seq 1 "$most"); do
  stage "$count"

  before=$(status)
  : > "$work/samples.tsv"
  for _ in $(seq "$window"); do
    sleep 1
    status | jq -r '.telemetry.frame // {}
      | [.media_us // 0, .playback_us // 0, .compose_us // 0] | @tsv' >> "$work/samples.tsv"
  done
  after=$(status)

  fps=$(printf '%s' "$after" | jq -r '.telemetry.outputs[0].actual_fps | .*100 | round / 100')
  dropped=$(( $(printf '%s' "$after" | jq -r '.telemetry.outputs[0].dropped_frames.lifetime')
            - $(printf '%s' "$before" | jq -r '.telemetry.outputs[0].dropped_frames.lifetime') ))
  held=$(( $(printf '%s' "$after" | jq -r '.telemetry.late // 0')
         - $(printf '%s' "$before" | jq -r '.telemetry.late // 0') ))
  read -r media playback compose < <(awk -F'\t' '
    {m += $1; p += $2; c += $3; n += 1}
    END {if (n == 0) n = 1; printf "%d %d %d\n", m / n, p / n, c / n}' "$work/samples.tsv")

  printf '%-8s %-8s %-9s %-7s %-8s %-10s %s\n' \
    "$count" "$fps" "$dropped" "$held" "$media" "$playback" "$compose"
done
