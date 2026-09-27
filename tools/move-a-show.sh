#!/usr/bin/env bash
# Moves a show from this server to a brand new one, the way an operator would.
#
#   ./tools/move-a-show.sh
#
# Run it on the server. It starts a second server beside the running one — its
# own port, its own database and media store, no DMX sockets and no renderer —
# exports the open show, imports it there, sends the content that server says it
# is missing, and asks it to open the show.
#
# # What this proves that a unit test cannot
#
# A package written by a server that has been running a show all evening, naming
# the media it actually holds, opening on a machine that has never seen any of
# it. The unit tests build both ends in one process; this one moves a real
# twenty-four-megabyte clip between two.
#
# It changes nothing on the running server: it reads the show and the store, and
# everything it writes goes to the temporary server, which is stopped and
# deleted when it finishes.

set -euo pipefail

first="${OVS_BASE:-http://127.0.0.1:7420/api/v1}"
port="${OVS_SECOND_PORT:-7430}"
second="http://127.0.0.1:$port/api/v1"
store="${OVS_STORE:-/var/lib/ovs/media}"
ovsd="${OVSD:-/usr/bin/ovsd}"

work=$(mktemp -d)
mkdir -p "$work/second"

cleanup() {
  if [ -n "${server:-}" ]; then kill "$server" 2>/dev/null || true; fi
  rm -rf "$work"
}
trap cleanup EXIT

echo "starting a second server on $port with nothing in it"
OVS_BIND="127.0.0.1:$port" OVS_DATA_DIR="$work/second" OVS_DMX=off \
  OVS_RENDERER="127.0.0.1:7499" "$ovsd" > "$work/second.log" 2>&1 &
server=$!

for _ in $(seq 1 40); do
  if curl -s -o /dev/null "$second/server/status"; then break; fi
  sleep 0.5
done
curl -s -o /dev/null "$second/server/status" \
  || { tail -5 "$work/second.log"; echo "the second server did not come up"; exit 1; }

id=$(curl -s "$first/server/status" | jq -r '.telemetry.current_project')
echo "exporting $id"
curl -s "$first/projects/$id/export" -o "$work/package.json"
jq -r '"the package names \(.media | length) media item(s) and carries no files"' "$work/package.json"

code=$(curl -s -o "$work/imported.json" -w '%{http_code}' -X POST \
  -H 'content-type: application/json' --data @"$work/package.json" "$second/projects/import")
echo "importing into the empty server: $code"
case "$code" in
  200|201) ;;
  *) head -c 600 "$work/imported.json"; echo; exit 1 ;;
esac

jq -r '"it relinked \(.relinked | length) and is missing \(.missing | length)"' "$work/imported.json"

# Read the missing entries out first. A `while read` fed by a pipe runs in a
# subshell whose standard input is that pipe, and the first thing in the loop
# that reads standard input eats the rest of the list.
missing=$(jq -c '.missing[]' "$work/imported.json")

while IFS= read -r entry; do
  [ -n "$entry" ] || continue

  sha=$(printf '%s' "$entry" | jq -r '.sha256')
  name=$(printf '%s' "$entry" | jq -r '.display_name')
  # Found by its hash rather than by a name this script works out. A stored
  # object keeps the extension the operator's file came with, so
  # there is no longer a rule here that could agree with the server's.
  path=$(sudo -n sh -c "ls '$store/${sha:0:2}/$sha.'* 2>/dev/null" | head -1)
  if [ -z "$path" ]; then
    echo "  nothing in the store hashes to $sha"
    continue
  fi

  echo "  sending $name"
  # The store belongs to the service user, so the bytes are copied out with the
  # one privileged step this needs rather than running everything as root.
  if ! sudo -n cat "$path" > "$work/asset.bin" 2>/dev/null; then
    echo "  cannot read $path"
    continue
  fi
  echo "  copied $(stat -c %s "$work/asset.bin") bytes out of the store"

  declared=$(printf '%s' "$entry" | jq -c '{
      display_name, original_filename, sha256, byte_size,
      media_item_id, kind,
      dimensions, duration_ms, frame_rate, has_alpha, container, codec
    }')

  session=$(curl -s -X POST -H 'content-type: application/json' \
    --data "$declared" "$second/media/uploads")
  upload=$(printf '%s' "$session" | jq -r '.upload_id')
  if [ "$upload" = "null" ] || [ -z "$upload" ]; then
    echo "  the second server would not take the declaration"
    continue
  fi

  # In the chunks the server asked for, at the offsets it counts. A whole file
  # in one request is refused, and rightly: an upload that cannot be resumed is
  # one that starts again from nothing when a venue's network drops.
  chunk=$(printf '%s' "$session" | jq -r '.chunk_size')
  total=$(stat -c %s "$work/asset.bin")
  offset=0
  sent=200
  while [ "$offset" -lt "$total" ]; do
    dd if="$work/asset.bin" of="$work/chunk.bin" bs=1 skip="$offset" count="$chunk" \
      status=none iflag=skip_bytes,count_bytes
    sent=$(curl -s -o "$work/sent.json" -w '%{http_code}' -X PUT \
      -H 'content-type: application/octet-stream' \
      --data-binary "@$work/chunk.bin" "$second/media/uploads/$upload?offset=$offset")
    if [ "$sent" != "200" ]; then
      echo -n "  the chunk at $offset was refused ($sent): "
      head -c 300 "$work/sent.json"; echo
      break
    fi
    offset=$((offset + chunk))
  done
  echo "  sent $total bytes in chunks of $chunk"
  [ "$sent" = "200" ] || continue

  verified=$(curl -s -o "$work/verified.json" -w '%{http_code}' -X POST \
    "$second/media/uploads/$upload/verify")
  echo "  verified: $verified"
  if [ "$verified" != "200" ]; then
    echo -n "  the server said: "; head -c 300 "$work/verified.json"; echo
  fi
done <<< "$missing"

echo "what the second server holds now:"
curl -s "$second/media" \
  | jq -r '.items[] | "  \(.display_name)  \(.state)  \(.dimensions.width)x\(.dimensions.height)  \(.duration_ms)ms"'

opened=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$second/projects/$id/open")
echo "opening the imported show: $opened"
curl -s "$second/projects/$id" \
  | jq -r '"it has \(.layers | length) layer(s), \(.pieces | length) piece(s) and \(.outputs | length) output(s)"'
