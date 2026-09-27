#!/usr/bin/env bash
# Makes the clip the reference workload is specified in, and puts it in the
# library the way a client would.
#
#   ./tools/make-reference-clip.sh [seconds]
#
# Run it on the server. It renders a moving 1080p60 ProRes 422 clip with ffmpeg,
# declares it, sends it in the chunks the server asks for, and verifies it.
#
# # Why this exists
#
# The reference workload is three simultaneous ProRes sources at 1920x1080 and
# 60 frames a second, and the risk is exactly that: NVIDIA's decoder does not
# do ProRes, so all three are decoded on the processor. Every measurement so far
# has used a 1080p25 clip, which is less than half the decode work — so the
# highest risk in the register has been measured against something easier than
# the thing it is about.
#
# Moving content rather than a still, because an all-intra codec's decode cost
# is roughly constant but its *disc* cost is not, and a still would flatter the
# storage figure that R1 also names.

set -euo pipefail

api="${OVS_BASE:-http://127.0.0.1:7420/api/v1}"
seconds="${1:-8}"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

clip="$work/reference.mov"
echo "rendering ${seconds}s of 1080p60 ProRes 422"
ffmpeg -loglevel error -y \
  -f lavfi -i "testsrc2=size=1920x1080:rate=60:duration=$seconds" \
  -vf "noise=alls=48:allf=t+u,format=yuv422p10le" \
  -c:v prores_ks -profile:v 2 -vendor apl0 \
  "$clip"

size=$(stat -c %s "$clip")
echo "it is $((size / 1024 / 1024)) MB, which is $((size * 8 / seconds / 1000000)) Mbit/s"

sha=$(sha256sum "$clip" | cut -d' ' -f1)

declared=$(jq -n --arg sha "$sha" --argjson size "$size" --argjson ms "$((seconds * 1000))" '
  {
    display_name: "Reference 1080p60",
    original_filename: "reference-1080p60.mov",
    sha256: $sha,
    byte_size: $size,
    kind: "video",
    dimensions: {width: 1920, height: 1080},
    duration_ms: $ms,
    frame_rate: {numerator: 60, denominator: 1},
    has_alpha: false,
    container: "mov",
    codec: "prores"
  }')

session=$(curl -s -X POST -H 'content-type: application/json' --data "$declared" "$api/media/uploads")
upload=$(printf '%s' "$session" | jq -r '.upload_id')
if [ "$upload" = "null" ] || [ -z "$upload" ]; then
  echo "the server would not take the declaration:"; printf '%s\n' "$session"; exit 1
fi

chunk=$(printf '%s' "$session" | jq -r '.chunk_size')
offset=0
while [ "$offset" -lt "$size" ]; do
  dd if="$clip" of="$work/chunk.bin" bs=1 skip="$offset" count="$chunk" \
    status=none iflag=skip_bytes,count_bytes
  sent=$(curl -s -o "$work/sent.json" -w '%{http_code}' -X PUT \
    -H 'content-type: application/octet-stream' \
    --data-binary "@$work/chunk.bin" "$api/media/uploads/$upload?offset=$offset")
  if [ "$sent" != "200" ]; then
    echo "the chunk at $offset was refused ($sent):"; head -c 300 "$work/sent.json"; echo; exit 1
  fi
  offset=$((offset + chunk))
  printf '\r  sent %d of %d MB' $((offset / 1024 / 1024)) $((size / 1024 / 1024))
done
echo

verified=$(curl -s -o "$work/verified.json" -w '%{http_code}' -X POST "$api/media/uploads/$upload/verify")
echo "verified: $verified"
[ "$verified" = "200" ] || { head -c 400 "$work/verified.json"; echo; exit 1; }

curl -s "$api/media" | jq -r '.items[] | select(.display_name == "Reference 1080p60")
  | "  \(.display_name)  \(.state)  \(.dimensions.width)x\(.dimensions.height)  \(.frame_rate.numerator)/\(.frame_rate.denominator)  \(.duration_ms)ms"'
