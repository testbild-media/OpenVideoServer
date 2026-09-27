#!/usr/bin/env bash
# Changes slots as fast as the server will take them, and watches the card.
#
#   ./tools/hammer-the-slots.sh              two hundred rounds
#   ./tools/hammer-the-slots.sh 600          longer
#   ./tools/hammer-the-slots.sh 200 0        as fast as the server will take them
#
# The second argument is how long to stay on a slot before leaving it, in
# milliseconds. It defaults to 400 and it matters more than the count: the
# owner's reading of this was that it takes **video** to provoke, not
# generators, and a clip that is switched away from before its decoder has
# produced a frame never gets a texture at all. A run of a thousand rounds that
# never waited would be a thousand rounds of nothing happening.
#
# Run it on the server. It drives the show that is open through the same API an
# operator's client uses, and puts every layer back on the slot it found it on.
#
# # Why this exists
#
# The owner found it by doing it: changing slots quickly filled the card, until
# the renderer was holding 7.9 GiB of 8 and the clip on the wall had been
# replaced by its layer's placeholder. Two faults, both fixed — a
# descriptor pool that never took a set back, and a texture that was leaked
# whenever binding it failed.
#
# A fix nobody can see is a fix nobody believes. This is the same thing done on
# purpose, with the one number that settles it printed as it goes.
#
# # What it changes, and what it puts back
#
# What is on the wall, for as long as it runs — this is not something to run
# during a show. Every layer goes back to the slot it was on at the end,
# whichever way the run ends.

set -uo pipefail
cd "$(dirname "$0")/.."

rounds="${1:-200}"
dwell_ms="${2:-400}"
dwell=$(awk -v ms="$dwell_ms" 'BEGIN { printf "%.3f", ms / 1000 }')
api="${OVS_BASE:-http://127.0.0.1:7420/api/v1}"

command -v jq > /dev/null || { echo "this needs jq" >&2; exit 2; }

project=$(curl -fsS "$api/server/status" | jq -r '.telemetry.current_project // empty')
[ -n "$project" ] || { echo "no project is open, so there are no slots to change" >&2; exit 1; }

# The layers that have something to change *to*, and which slot each will play.
#
# A clip in preference to a generator, wherever a layer has both. The owner's
# reading of this was that it takes video to provoke — and they are right about
# why: a generator is drawn once into a texture that is kept, while a clip
# opens a decoder and puts a new picture on the card, which is the path that
# used to leak. A run that switched between black and a generator would look
# busy and touch none of it. The first run of this script did exactly that,
# because it took each layer's *lowest* slot and on that show the lowest was
# the grid.
#
# A layer with no configured slot is skipped rather than driven: selecting one
# it does not have is refused, correctly, and a run made of refusals would
# prove nothing while looking like it had done something.
document=$(curl -fsS "$api/projects/$project")
layers=$(printf '%s' "$document" | jq -r '.layers[] | select((.slots | length) > 0) | .id')
[ -n "$layers" ] || { echo "no layer of the open project has a slot" >&2; exit 1; }

declare -A plays
clips=0
for layer in $layers; do
  plays[$layer]=$(printf '%s' "$document" | jq -r --arg l "$layer" '
    .layers[] | select(.id == $l) | .slots
    | (map(select(.source.kind == "media")) | min_by(.index))
      // (. | min_by(.index))
    | .index')
  kind=$(printf '%s' "$document" | jq -r --arg l "$layer" --arg s "${plays[$layer]}" '
    .layers[] | select(.id == $l) | .slots[] | select(.index == ($s | tonumber)) | .source.kind')
  echo "  ${layer}: slot ${plays[$layer]}, a ${kind}"
  [ "$kind" = "media" ] && clips=$(( clips + 1 ))
done

if [ "$clips" -eq 0 ]; then
  echo
  echo "none of these slots holds a clip, so this run cannot provoke what it is for" >&2
  echo "put a video in a slot and run it again" >&2
  exit 1
fi

# And where each one is now, so the show can be put back exactly.
status=$(curl -fsS "$api/server/status")
declare -A was
for layer in $layers; do
  was[$layer]=$(printf '%s' "$status" \
    | jq -r --arg l "$layer" '.telemetry.playheads[]? | select(.layer_id == $l) | .slot_index' \
    | head -1)
  [ -n "${was[$layer]}" ] || was[$layer]=0
done

restore() {
  echo
  echo "putting the show back"
  for layer in $layers; do
    curl -fsS -o /dev/null -X POST -H 'content-type: application/json' \
      --data "{\"slot_index\": ${was[$layer]}}" "$api/live/layers/$layer/slot" || true
  done
}
trap restore EXIT

# What the card is holding, in MiB, or nothing when it will not say.
vram() {
  nvidia-smi --query-compute-apps=pid,process_name,used_memory --format=csv,noheader 2>/dev/null \
    | awk -F', ' '/ovs-renderer/ {gsub(/ MiB/, "", $3); print $3}' \
    | head -1
}

start=$(vram)
echo "the renderer is holding ${start:-?} MiB before anything moves"
echo "changing slots $rounds times across $(echo "$layers" | wc -l) layer(s), ${dwell_ms}ms on each"
echo

# Slot 0 is Black on every layer and needs no configuration, so alternating
# with it is a drop and a rebuild of that layer's texture every round — which
# is the churn that used to leak, in its purest form.
every=$(( rounds / 10 )); [ "$every" -lt 1 ] && every=1
for round in $(seq 1 "$rounds"); do
  for layer in $layers; do
    if [ $(( round % 2 )) -eq 0 ]; then
      want=0
    else
      want=${plays[$layer]}
    fi
    curl -fsS -o /dev/null -X POST -H 'content-type: application/json' \
      --data "{\"slot_index\": $want}" "$api/live/layers/$layer/slot" || true
  done

  # Long enough for a decoder to have opened the file and produced a frame,
  # and for that frame to have been put on the card. Without this the run
  # measures the API and not the renderer.
  [ "$dwell_ms" -gt 0 ] && sleep "$dwell"

  if [ $(( round % every )) -eq 0 ]; then
    now=$(vram)
    printf 'after %4s rounds: %5s MiB\n' "$round" "${now:-?}"
  fi
done

echo
finish=$(vram)
echo "before: ${start:-?} MiB    after: ${finish:-?} MiB"
if [ -n "$start" ] && [ -n "$finish" ]; then
  grown=$(( finish - start ))
  echo "the card is ${grown} MiB further on than it started"
  echo
  if [ "$grown" -gt 256 ]; then
    echo "that is a leak: a texture is a few megabytes and this is hundreds"
    exit 1
  fi
  echo "nothing is being kept"
fi
