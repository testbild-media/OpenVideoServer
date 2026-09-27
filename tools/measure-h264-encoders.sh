#!/usr/bin/env bash
# Measures libopenh264 against libx264 at the software preview's own settings.
#
#   ./tools/measure-h264-encoders.sh
#   ./tools/measure-h264-encoders.sh --source CLIP --frames 300 --size 1280x720
#   ./tools/measure-h264-encoders.sh --openh264 DIR
#
# # Why this exists
#
# The server's software preview encoder is `libx264`, which is GPL, and the
# server links libavcodec rather than running it, so a server anybody may have a
# copy of would have to be GPL too (owner decision Q18). The LGPL replacement is
# `libopenh264`: BSD, and Constrained Baseline, which means no CABAC and no
# B-frames — so it compresses worse, by an amount nobody had measured.
#
# The owner's instruction was **erst messen**. This is the measurement.
#
# # What it measures, and what it deliberately does not
#
# Both encoders are given the settings `stream.rs` gives the real one: 4:2:0,
# a target bitrate with a maxrate and a half-second buffer, a keyframe every
# `KEYFRAME_SECONDS`, no B-frames, and whatever each library calls "do not wait
# for the next frame". Everything else is left at its default, because a tuned
# comparison would measure the tuning.
#
# The input is raw frames on disk, not a clip. Encoding from a clip measures the
# decoder as well, and the decoder is the same either way, so it would be noise
# in both columns and a smaller apparent difference between them.
#
# Three numbers per encoder, because they answer different questions:
#
#   - **cores** — CPU seconds per second of media, for the encoder alone. Not
#     comparable with `ovs-preview-cost`'s 2.52 cores, which is the whole
#     preview path at sixty frames a second including the RGBA-to-4:2:0
#     conversion the renderer does per frame. What is comparable is the ratio
#     between the two columns, which is the question being asked.
#   - **realtime** — media seconds per wall second. Whether one surface keeps up
#     *at all* in however many threads the library uses by itself. An encoder
#     can be cheap in cores and still too slow in one thread.
#   - **SSIM / PSNR** — against the frames that went in, at the same bitrate.
#     What the operator watching the preview actually loses.
#
# Run it on the machine whose numbers you want. The test machine has twenty
# cores and the reference server has six, so a cores figure measured
# here is a cores figure there, but a realtime figure is not.

set -euo pipefail
cd "$(dirname "$0")/.."

# Numbers are read by awk and written into a document. A locale that writes
# 0,15 gives both of those something they parse as zero.
export LC_ALL=C

source_clip="$HOME/clips/reference-prores.mov"
openh264_dir="build/ffmpeg-openh264"
frames=300
size=1280x720
kbps=6000
fps=15
# `KEYFRAME_SECONDS` in stream.rs. A keyframe every two seconds.
keyframe_seconds=2

while [ $# -gt 0 ]; do
  case "$1" in
    --source) source_clip="${2:?--source wants a file}"; shift 2 ;;
    --openh264) openh264_dir="${2:?--openh264 wants a directory}"; shift 2 ;;
    --frames) frames="${2:?--frames wants a number}"; shift 2 ;;
    --size) size="${2:?--size wants WxH}"; shift 2 ;;
    --bitrate) kbps="${2:?--bitrate wants kbps}"; shift 2 ;;
    --fps) fps="${2:?--fps wants a number}"; shift 2 ;;
    -h|--help) sed -n '2,45p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done

system_ffmpeg=$(command -v ffmpeg || true)
openh264_ffmpeg="$openh264_dir/ffmpeg"

if [ -z "$system_ffmpeg" ]; then
  echo "no ffmpeg on the path, and libx264 is measured with the system one" >&2
  exit 1
fi
if [ ! -x "$openh264_ffmpeg" ]; then
  echo "no ffmpeg at $openh264_ffmpeg" >&2
  echo "build one first:  ./tools/build-ffmpeg.sh --profile openh264" >&2
  exit 1
fi
if [ ! -r "$source_clip" ]; then
  echo "cannot read $source_clip; name one with --source" >&2
  exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

raw="$work/source.yuv"
media_seconds=$(awk -v f="$frames" -v r="$fps" 'BEGIN { printf "%.4f", f / r }')

echo "preparing $frames frames of $size at ${fps}fps from $(basename "$source_clip")"
# Looped, because the reference clips are six seconds long and six seconds is
# not enough to measure a rate against. The same frames go to both encoders, so
# the repetition is in both columns; it flatters neither, and a keyframe every
# two seconds means the loop point is not a structural advantage to either.
"$system_ffmpeg" -hide_banner -loglevel error -y \
  -stream_loop -1 -i "$source_clip" \
  -vf "scale=${size/x/:},fps=$fps,format=yuv420p" \
  -frames:v "$frames" \
  -f rawvideo "$raw"

# What the frames really are, rather than what was asked for: a clip shorter
# than `--frames` would otherwise be measured as though it were not.
bytes=$(stat -c %s "$raw")
width=${size%x*}
height=${size#*x}
actual=$((bytes / (width * height * 3 / 2)))
if [ "$actual" -lt "$frames" ]; then
  echo "  the clip gave $actual frames, not $frames"
  frames=$actual
  media_seconds=$(awk -v f="$frames" -v r="$fps" 'BEGIN { printf "%.4f", f / r }')
fi

# --- one measured encode ----------------------------------------------------
#
# Timed with the shell's own `time` rather than `/usr/bin/time`, which is a
# separate Debian package and not installed on the machine this measures. The
# command's own output goes to the void so that what comes back is three
# numbers and nothing else: user, system, wall.

timed() {
  local TIMEFORMAT='%3U %3S %3R' where="$1"
  shift
  { time "$@" >/dev/null 2>"$where"; } 2>&1
}

# Into a `.mov`, because the measurement build has the muxers the client needs
# and the client writes ProRes in MOV — there is no mp4 muxer in it, and adding
# one to the shipped build to make a measurement tidier would be the wrong way
# round.
encode() {
  local binary="$1" encoder="$2" out="$3"
  shift 3

  timed "$out.log" "$binary" -hide_banner -loglevel error -y \
    -f rawvideo -pix_fmt yuv420p -s "$size" -r "$fps" -i "$raw" \
    -c:v "$encoder" \
    -b:v "${kbps}k" -maxrate "${kbps}k" -bufsize "$((kbps / 2))k" \
    -g "$((fps * keyframe_seconds))" -bf 0 \
    "$@" \
    "$out"
}

# --- the quality of what came out -------------------------------------------

# The filters print a line per frame and one summary line, and the summary is on
# stderr while the per-frame stats are on stdout. Reading the last line of stdout
# gets the *final frame*, which is a number that looks plausible and is not the
# answer — it was 0.76 here where the average was much higher.
quality() {
  local encoded="$1" filter="$2"
  "$system_ffmpeg" -hide_banner -loglevel info \
    -f rawvideo -pix_fmt yuv420p -s "$size" -r "$fps" -i "$raw" \
    -i "$encoded" \
    -lavfi "[1:v]format=yuv420p[enc];[0:v][enc]$filter" \
    -f null - 2>&1 >/dev/null | grep -E "Parsed_$filter" | tail -1
}

# --- and the floor the two share --------------------------------------------
#
# Reading 400MB off disk and handing it to a null muxer costs something. It is
# the same something in both columns, and subtracting it would be arithmetic on
# two measurements rather than a measurement, so it is reported instead.

echo "measuring the floor (read and discard, no encoder)"
read -r floor_user floor_sys floor_wall < <(
  timed "$work/floor.log" "$system_ffmpeg" -hide_banner -loglevel error -y \
    -f rawvideo -pix_fmt yuv420p -s "$size" -r "$fps" -i "$raw" \
    -f null -
)

echo "measuring libx264 (system ffmpeg)"
read -r x264_user x264_sys x264_wall < <(
  encode "$system_ffmpeg" libx264 "$work/x264.mov" \
    -preset ultrafast -tune zerolatency
)

echo "measuring libopenh264 ($openh264_dir)"
read -r oh_user oh_sys oh_wall < <(
  encode "$openh264_ffmpeg" libopenh264 "$work/openh264.mov" \
    -rc_mode bitrate -allow_skip_frames 0
)

for produced in "$work/x264.mov" "$work/openh264.mov"; do
  if [ ! -s "$produced" ]; then
    echo "$(basename "$produced") is empty: the encode failed, and said:" >&2
    cat "$produced.log" >&2
    exit 1
  fi
done

# --- the table --------------------------------------------------------------

report() {
  local name="$1" user="$2" sys="$3" wall="$4" file="$5"
  local ssim psnr bitrate
  ssim=$(quality "$file" ssim | sed -n 's/.*All:\([0-9.]*\).*/\1/p')
  psnr=$(quality "$file" psnr | sed -n 's/.*average:\([0-9.]*\).*/\1/p')
  bitrate=$(awk -v b="$(stat -c %s "$file")" -v s="$media_seconds" \
    'BEGIN { printf "%.0f", b * 8 / s / 1000 }')

  awk -v n="$name" -v u="$user" -v s="$sys" -v w="$wall" -v m="$media_seconds" \
    -v ssim="${ssim:-?}" -v psnr="${psnr:-?}" -v kbps="$bitrate" \
    'BEGIN {
       printf "| %-12s | %6.2f | %7.2f | %6.2f | %6s | %6s | %7s |\n",
         n, (u + s) / m, m / w, u + s, ssim, psnr, kbps
     }'
}

echo
echo "$frames frames, $size, ${fps}fps, ${kbps}kbps target, $(nproc) cores available"
echo "floor: $(awk -v u="$floor_user" -v s="$floor_sys" -v m="$media_seconds" \
  'BEGIN { printf "%.2f", (u + s) / m }') cores just to read the frames"
echo
echo "| encoder      |  cores | realtime |    cpu |   ssim |   psnr | kbps    |"
echo "|--------------|--------|----------|--------|--------|--------|---------|"
report libx264 "$x264_user" "$x264_sys" "$x264_wall" "$work/x264.mov"
report libopenh264 "$oh_user" "$oh_sys" "$oh_wall" "$work/openh264.mov"
echo
echo "cores    = CPU seconds per second of media, encoder only. Not the whole"
echo "           preview path: ovs-preview-cost measures that. The ratio is"
echo "           what this answers."
echo "realtime = media seconds per wall second, in whatever threads the library"
echo "           uses by itself; below 1.00 means one surface cannot keep up"
