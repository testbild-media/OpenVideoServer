#!/usr/bin/env bash
# Three clips at once, which is the workload this product is specified against.
#
#   ./tools/reference-load.sh [seconds] [clip name] [layers]
#
# Run it on the server. It builds a show with three layers all playing, watches
# the frame rate, the dropped and late counts and the decoder count for a while,
# and puts the project back.
#
# # Why three
#
# Three simultaneous ProRes sources at 60 fps are the reference workload,
# and this is why it is the interesting number:
# NVIDIA's decoder does not do ProRes, so every one of those three is decoded on
# a processor from 2016. Everything else in this server has been measured with
# one clip playing, which tells you nothing about the case it was designed for.
#
# # What it does not do
#
# It does not transcode anything or fetch anything. It plays what is already in
# the library: the heaviest clips it can find, one per layer, falling back to
# repeating the heaviest when the library holds fewer than there are layers.
#
# Which clip goes on which layer is not a detail. Three layers reading one file
# share a page cache and, measured on the test machine, hold sixty frames a
# second; three layers reading three files do not. Naming a clip on the command
# line puts that one on every layer, which is the old behaviour and the one to
# use when the question is about decoding rather than about reading.

set -euo pipefail

api="${OVS_BASE:-http://127.0.0.1:7420/api/v1}"
seconds="${1:-60}"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

id=$(curl -s "$api/server/status" | jq -r '.telemetry.current_project')
curl -s "$api/projects/$id" > "$work/before.json"

# The heaviest clips in the library by pixels a second, because that is what the
# reference workload is about — or one named on the command line, on every layer.
wanted="${2:-}"
curl -s "$api/media" > "$work/library.json"
heaviest() {
  jq -r --arg wanted "$wanted" '
    [ .items[]
      | select(.state == "ready" and .kind == "video")
      | select($wanted == "" or .display_name == $wanted)
    ]
    | sort_by(-(.dimensions.width * .dimensions.height
                * (.frame_rate.numerator / (.frame_rate.denominator // 1))))
    | .[].id' "$work/library.json"
}
mapfile -t clips < <(heaviest)
[ "${#clips[@]}" -gt 0 ] || { echo "no playable clip in the library"; exit 1; }

# One clip per layer where the library has them, the heaviest repeated where it
# does not. A named clip goes on every layer.
clip_for() {
  if [ -n "$wanted" ] || [ "$1" -ge "${#clips[@]}" ]; then
    printf '%s' "${clips[0]}"
  else
    printf '%s' "${clips[$1]}"
  fi
}
# One, two or three, so the shape of a limit can be seen rather than guessed: a
# decoder that cannot manage one on its own is a different problem from three
# that cannot share a machine.
count="${3:-3}"
echo "on $count layer(s):"
for order in $(seq 0 $((count - 1))); do
  jq -r --arg i "$(clip_for "$order")" --argjson n "$((order + 1))" '.items[] | select(.id == $i)
    | "  layer \($n) plays \(.display_name // "a clip with no name"), \(.dimensions.width)x\(.dimensions.height) at \(.frame_rate.numerator / (.frame_rate.denominator // 1))"' "$work/library.json"
done

layer() {
  # $1 order, $2 id, $3 opacity
  jq -n --argjson order "$1" --arg id "$2" --argjson opacity "$3" --arg item "$(clip_for "$1")" '
    {
      id: $id, name: ("Layer " + ($order + 1 | tostring)), order: $order,
      enabled: true, route: "A", default_opacity: $opacity, blend_mode: "normal",
      default_transform: {x: 0.0, y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation_deg: 0.0},
      default_quantization: "off", default_speed_ratio: 1.0,
      slots: [{
        index: 1,
        source: {kind: "media", media_item_id: $item},
        playback_mode: "loop", direction: "forward",
        transform: {x: 0.0, y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation_deg: 0.0},
        effects: []
      }],
      effects: []
    }'
}

# Three different speeds, so the three decoders are at different points in the
# file and none of them can be sharing a frame with another.
jq --argjson a "$(layer 0 01ARZ3NDEKTSV4RRFFQ69G5FD0 1.0)" \
   --argjson b "$(layer 1 01ARZ3NDEKTSV4RRFFQ69G5FD1 0.5)" \
   --argjson c "$(layer 2 01ARZ3NDEKTSV4RRFFQ69G5FD2 0.33)" \
   --argjson count "$count" \
   '.layers = [($a | .default_speed_ratio = 1.0),
               ($b | .default_speed_ratio = 0.5),
               ($c | .default_speed_ratio = 2.0)][:$count]' \
  "$work/before.json" > "$work/three.json"

jq '{project: .}' "$work/three.json" > "$work/save.json"
code=$(curl -s -o "$work/said.json" -w '%{http_code}' -X PUT \
  -H 'content-type: application/json' --data @"$work/save.json" "$api/projects/$id")
echo "saving the three-layer show: $code"
[ "$code" = "200" ] || { head -c 600 "$work/said.json"; echo; exit 1; }

sleep 2
for layer_id in $(jq -r '.layers[].id' "$work/three.json"); do
  curl -s -o /dev/null -X POST -H 'content-type: application/json' \
    --data '{"slot_index":1}' "$api/live/layers/$layer_id/slot"
done
curl -s -o /dev/null -X PUT -H 'content-type: application/json' \
  --data '{"crossfader":0.0,"dimmer":1.0,"freeze":false}' "$api/live/master"

echo "settling"
sleep 6

read -r start_dropped start_late start_errors <<< "$(curl -s "$api/server/status" | jq -r '
  [.telemetry.outputs[0].dropped_frames.lifetime, .telemetry.late_frames, .telemetry.decode_errors] | @tsv')"

printf '%-9s %-7s %-8s %-6s %-6s %s\n' at fps decoders cpu% rendercpu 'dropped/late/errors'
for _ in $(seq 1 $((seconds / 5))); do
  sleep 5
  curl -s "$api/server/status" > "$work/now.json"
  renderer=$(ps -o pcpu= -C ovs-renderer | tr -d ' ' || echo '?')
  jq -r --arg r "$renderer" --argjson d "$start_dropped" --argjson l "$start_late" --argjson e "$start_errors" '
    .telemetry as $t
    | [ (now | strflocaltime("%H:%M:%S")),
        ($t.outputs[0].actual_fps * 100 | round / 100 | tostring),
        ($t.active_decoders | tostring),
        ($t.cpu_percent * 10 | round / 10 | tostring),
        $r,
        (($t.outputs[0].dropped_frames.lifetime - $d | tostring) + "/" +
         ($t.late_frames - $l | tostring) + "/" +
         ($t.decode_errors - $e | tostring))
      ] | @tsv' "$work/now.json" \
    | awk -F'\t' '{printf "%-9s %-7s %-8s %-6s %-6s %s\n", $1, $2, $3, $4, $5, $6}'
done

jq '{project: .}' "$work/before.json" > "$work/restore.json"
curl -s -o /dev/null -X PUT -H 'content-type: application/json' \
  --data @"$work/restore.json" "$api/projects/$id"
sleep 1
restored=$(jq -r '.layers[0].id' "$work/before.json")
curl -s -o /dev/null -X POST -H 'content-type: application/json' \
  --data '{"slot_index":1}' "$api/live/layers/$restored/slot"
echo "restored"
