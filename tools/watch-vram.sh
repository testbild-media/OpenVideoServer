#!/usr/bin/env bash
# What the renderer is holding on the card, once a second, with a line per sample.
#
#   ./tools/watch-vram.sh                 for ten minutes, to the terminal
#   ./tools/watch-vram.sh 1800            for half an hour
#   nohup ./tools/watch-vram.sh 1800 > /tmp/vram.log 2>&1 &   and walk away
#
# Run it on the server. It changes nothing and needs no privileges: it reads
# `nvidia-smi` and the server's own status endpoint.
#
# # Why this exists
#
# A card filling up is the one fault in this renderer that cannot be seen from
# inside it. Vulkan will not say how much of the device it has taken, the
# driver only says it when asked from outside, and by the time it matters every
# other thing on the machine has stopped working too — `vkCreateDevice` starts
# failing, so the tests that would have caught it cannot even open a device.
#
# So: a line a second, with the decoder count beside it, because the two
# together say whether a rise is a clip that started playing or a clip that
# stopped and took nothing with it.

set -uo pipefail

seconds="${1:-600}"
api="${OVS_BASE:-http://127.0.0.1:7420/api/v1}"

# What the renderer holds, in MiB. Empty when the driver will not say — which
# is the ordinary answer on a machine with no NVIDIA card, and on Windows.
vram() {
  nvidia-smi --query-compute-apps=pid,process_name,used_memory --format=csv,noheader 2>/dev/null \
    | awk -F', ' '/ovs-renderer/ { gsub(/ MiB/, "", $3); print $3 }' \
    | head -1
}

echo "time      vram    decoders  errors  late   what"
peak=0
for _ in $(seq 1 "$seconds"); do
  held=$(vram)
  read -r decoders errors late <<< "$(
    curl -fsS --max-time 2 "$api/server/status" 2>/dev/null \
      | jq -r '.telemetry | "\(.active_decoders) \(.decode_errors) \(.late_frames)"' \
      || echo "? ? ?"
  )"

  note=""
  if [ -n "$held" ]; then
    if [ "$held" -gt "$peak" ]; then
      # Only a new high is worth a word. A figure that goes up and down is a
      # clip starting and stopping; a figure that only ever goes up is the
      # fault this is here to catch.
      [ "$peak" -gt 0 ] && note="new high, up $(( held - peak ))"
      peak=$held
    fi
  fi

  printf '%s  %5s   %8s  %6s  %5s  %s\n' \
    "$(date +%H:%M:%S)" "${held:-?}" "$decoders" "$errors" "$late" "$note"
  sleep 1
done

echo
echo "highest the renderer held: ${peak} MiB"
