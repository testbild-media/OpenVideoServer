#!/usr/bin/env bash
# What Main 10 costs, against the same material at eight bits.
#
# The last item on the owner's verification list of 2026-09-23: frame time and
# dropped frames for ten-bit material against the same material as eight-bit.
# Everything else on that list is a test — `cargo test -p ovs-renderer --test
# ten_bit` and the `vulkan::offscreen` tests — because a claim that can be
# asserted should be. This one cannot: it is a number that belongs to a machine,
# and the answer on the test server is not the answer on the reference server.
#
#   ./tools/measure-ten-bit.sh                 the full sweep
#   ./tools/measure-ten-bit.sh --size 1920x1080 --frames 600
#   ./tools/measure-ten-bit.sh --keep          leave the fixtures for a rerun
#
# Run it on the machine being asked about, never through a share. It writes its
# fixtures under $TMPDIR and encodes several seconds of 4K, which takes a few
# minutes the first time and nothing on a rerun.
set -euo pipefail

cd "$(dirname "$0")/.."

frames=600
seconds=4
keep=0
sizes=()
players=(1 4)

while [ $# -gt 0 ]; do
    case "$1" in
    --frames)
        frames="$2"
        shift 2
        ;;
    --seconds)
        seconds="$2"
        shift 2
        ;;
    --size)
        sizes+=("$2")
        shift 2
        ;;
    --players)
        players=("$2")
        shift 2
        ;;
    --keep)
        keep=1
        shift
        ;;
    -h | --help)
        sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
        exit 0
        ;;
    *)
        echo "unknown option $1" >&2
        exit 2
        ;;
    esac
done

if [ ${#sizes[@]} -eq 0 ]; then
    sizes=(1920x1080 3840x2160)
fi

if ! command -v ffmpeg >/dev/null 2>&1; then
    echo "no ffmpeg on the path: the fixtures cannot be written" >&2
    exit 1
fi

work="${TMPDIR:-/tmp}/ovs-measure-ten-bit"
mkdir -p "$work"

# One encode a shape, reused across runs. x265 at 4K is slow enough that
# re-encoding it for every sweep would make the sweep about x265.
encode() {
    local name="$1" size="$2" rate="$3" pixel="$4" clip
    clip="$work/$name.mp4"
    if [ -s "$clip" ]; then
        echo "$clip"
        return
    fi
    echo "encoding $name ($size, $pixel)..." >&2
    ffmpeg -hide_banner -loglevel error -y \
        -f lavfi -i "testsrc2=size=$size:rate=$rate:duration=$seconds" \
        -c:v libx265 -pix_fmt "$pixel" -preset ultrafast \
        -x265-params "log-level=none:keyint=$rate:min-keyint=$rate:scenecut=0" \
        -color_range tv -colorspace bt709 -color_primaries bt709 -color_trc bt709 \
        -tag:v hvc1 "$clip" >&2
    echo "$clip"
}

echo "== Main 10 against the same material at eight bits =="
echo "$frames passes a clip, $seconds seconds of source, at 60 a second"
echo

cargo build --release -p ovs-renderer --bin ovs-decode-cost >&2

for size in "${sizes[@]}"; do
    for count in "${players[@]}"; do
        for pixel in yuv420p yuv420p10le; do
            name="hevc-${size}-${pixel}"
            clip="$(encode "$name" "$size" 60 "$pixel")"
            echo "---------------------------------------------------------------"
            echo "$size $pixel, $count player(s)"
            echo "---------------------------------------------------------------"
            # Only the lines that answer the question. The full report is worth
            # having when a number looks wrong, and is one `--players` run away.
            ./target/release/ovs-decode-cost "$clip" \
                --players "$count" --frames "$frames" --rate 60 |
                grep -E "cuvid|processor|TenLow|TenHigh|Eight|converting|frames held|frames a second|per frame|resident|the clip is" ||
                true
            echo
        done
    done
done

if [ "$keep" -eq 0 ]; then
    echo "clearing $work (pass --keep to reuse the fixtures)"
    rm -rf "$work"
fi
