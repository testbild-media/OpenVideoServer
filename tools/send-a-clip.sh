#!/usr/bin/env bash
# Puts a file into the library the way a client would.
#
#   ./tools/send-a-clip.sh clip.mp4              named after the file
#   ./tools/send-a-clip.sh clip.mp4 "Opener"     named by hand
#
# Run it on the server. It reads the file with ffprobe, declares it, sends it in
# the chunks the server asks for, and verifies it.
#
# # Why this exists
#
# `make-reference-clip.sh` renders one particular clip and sends it; this sends
# whatever is in front of it. The server plays what it is given, so
# "whatever is in front of it" is now the interesting case: an H.264 export, an
# HEVC file off a camera, an MPEG-2 from a broadcast archive. This is how one
# gets onto a wall without a desktop client and without a window.
#
# Nothing here decides what the file is. ffprobe reads it, the declaration
# carries the reading, and the server probes it again and keeps its own answer.
# The one thing that is decided is video against still, because that picks which
# check the server runs — a codec whose files are usually one picture is a still
# unless the file lasts longer than one.

set -euo pipefail

api="${OVS_BASE:-http://127.0.0.1:7420/api/v1}"
file="${1:-}"
[ -n "$file" ] && [ -f "$file" ] || { echo "usage: $0 <file> [name]" >&2; exit 2; }
name="${2:-$(basename "${file%.*}")}"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

ffprobe -v error -select_streams v:0 \
  -show_entries stream=codec_name,width,height,avg_frame_rate,pix_fmt \
  -show_entries format=format_name,duration \
  -of json "$file" > "$work/probe.json"

read -r codec width height rate_n rate_d pixel container duration <<EOF
$(jq -r '
  (.streams[0] // {}) as $s | (.format // {}) as $f |
  (($s.avg_frame_rate // "0/0") | split("/")) as $r |
  [$s.codec_name // "", $s.width // 0, $s.height // 0,
   ($r[0] // "0"), ($r[1] // "1"),
   $s.pix_fmt // "", $f.format_name // "", (($f.duration // "0") | tonumber * 1000 | floor)]
  | @tsv' "$work/probe.json")
EOF

[ "$width" -gt 0 ] && [ "$height" -gt 0 ] || { echo "no picture in $file" >&2; exit 1; }

# A still's codec is one whose files are usually a single picture; a file of
# that codec lasting longer than one frame is a sequence held in it.
still=no
case "$codec" in
  png|apng|mjpeg|jpeg2000|bmp|gif|tiff|webp) [ "$duration" -le 100 ] && still=yes ;;
esac

# Whatever ffprobe's pixel format says. The server reads this for itself and
# keeps its own answer; sending it means the two can be compared in the log.
alpha=false
case "$pixel" in
  *a) alpha=true ;;
  ya*|rgba*|bgra*|argb*|abgr*|yuva*|gbra*|ayuv*) alpha=true ;;
esac

size=$(stat -c %s "$file")
sha=$(sha256sum "$file" | cut -d' ' -f1)
echo "$name: $codec ${width}x${height} $((size / 1024 / 1024)) MB, ${duration}ms, alpha=$alpha"

declared=$(jq -n \
  --arg name "$name" --arg filename "$(basename "$file")" --arg sha "$sha" \
  --argjson size "$size" --arg codec "$codec" --arg container "$container" \
  --argjson w "$width" --argjson h "$height" --argjson ms "$duration" \
  --argjson rn "$rate_n" --argjson rd "$rate_d" \
  --argjson alpha "$alpha" --arg still "$still" '
  {
    display_name: $name,
    original_filename: $filename,
    sha256: $sha,
    byte_size: $size,
    kind: (if $still == "yes" then "image" else "video" end),
    dimensions: {width: $w, height: $h},
    has_alpha: $alpha,
    container: $container,
    codec: $codec
  }
  + (if $still == "yes" then {} else
       {duration_ms: $ms}
       + (if $rn > 0 and $rd > 0 then {frame_rate: {numerator: $rn, denominator: $rd}} else {} end)
     end)')

session=$(curl -s -X POST -H 'content-type: application/json' --data "$declared" "$api/media/uploads")
case "$(printf '%s' "$session" | jq -r '.result // "accepted"')" in
  already_present)
    echo "the server already holds these bytes as $(printf '%s' "$session" | jq -r '.media_item_id')"
    exit 0 ;;
esac
upload=$(printf '%s' "$session" | jq -r '.upload_id')
if [ "$upload" = "null" ] || [ -z "$upload" ]; then
  echo "the server would not take the declaration:"; printf '%s\n' "$session"; exit 1
fi

chunk=$(printf '%s' "$session" | jq -r '.chunk_size')
offset=0
while [ "$offset" -lt "$size" ]; do
  dd if="$file" of="$work/chunk.bin" bs=1 skip="$offset" count="$chunk" \
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
if [ "$verified" != "200" ]; then
  echo "the server refused what arrived ($verified):"; head -c 400 "$work/verified.json"; echo; exit 1
fi

jq -r '"  \(.display_name)  \(.state)  \(.dimensions.width)x\(.dimensions.height)  \(.frame_rate.numerator)/\(.frame_rate.denominator)  \(.duration_ms)ms  alpha=\(.has_alpha)"' \
  "$work/verified.json"
