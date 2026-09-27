#!/usr/bin/env bash
# Watches a running show and writes down what it did.
#
#   ./tools/soak.sh                     watch until stopped, sampling every 10s
#   ./tools/soak.sh 30 /tmp/soak.csv    every 30s, into that file
#
# Run it on the server itself: the status endpoint answers a request from the
# machine with no session, and a sample taken across the network
# measures the network as much as the show.
#
# # Why this exists
#
# The failures that end an evening are not the ones a test suite finds. They are
# a frame rate that drifts after four hours, memory that grows a megabyte a
# minute, a card that throttles when the room warms up, a decoder that leaks a
# handle per loop. None of those are visible in a run that lasts a minute, and
# all of them are obvious in a column of numbers taken over a night.
#
# So this writes a row per sample and nothing else. It does not judge, it does
# not restart anything, and it never touches the show: reading a status is the
# only thing it does to the server.
#
# # Reading the result
#
# `--summary <file>` prints what the numbers did rather than the numbers:
# whether frames were dropped after the start, how far the measured rate sat
# from the one the mode asks for, whether memory finished higher than it began,
# and the hottest the processor and the card got.

set -euo pipefail

interval="${1:-10}"
out="${2:-soak-$(date +%Y%m%d-%H%M%S).csv}"
base="${OVS_BASE:-http://127.0.0.1:7420/api/v1}"

if [ "${1:-}" = "--summary" ]; then
  file="${2:?usage: $0 --summary <file>}"
  python3 - "$file" <<'PY'
import csv, sys

rows = list(csv.DictReader(open(sys.argv[1])))
if len(rows) < 2:
    print("not enough samples to say anything")
    raise SystemExit(0)

def numbers(key):
    out = []
    for row in rows:
        try:
            out.append(float(row[key]))
        except (KeyError, TypeError, ValueError):
            pass
    return out

# Either process restarting mid-soak is the most interesting thing in the file,
# and it also breaks every difference taken across it: counters start again at
# zero. The server says so through its uptime; the renderer reports none, so it
# is recognised by being a different process — its frame counter going backwards
# is the same evidence, and misses a restart that had nothing to count. The run
# is cut into stretches at each restart and the longest one is read.
def went_backwards(previous, row, key):
    try:
        return float(row[key]) < float(previous[key])
    except (KeyError, TypeError, ValueError):
        return False


def became_another(previous, row, key):
    was, now = previous.get(key), row.get(key)
    return bool(was) and bool(now) and was != now


stretches, current = [], [rows[0]]
server_restarts = renderer_restarts = 0
for previous, row in zip(rows, rows[1:]):
    server = went_backwards(previous, row, "uptime_s")
    renderer = became_another(previous, row, "renderer_pid") or went_backwards(
        previous, row, "dropped_lifetime"
    )
    if server or renderer:
        server_restarts += int(server)
        renderer_restarts += int(renderer and not server)
        stretches.append(current)
        current = []
    current.append(row)
stretches.append(current)

if server_restarts:
    print(f"the server restarted {server_restarts} time(s) during this run")
if renderer_restarts:
    print(f"the renderer restarted {renderer_restarts} time(s) during this run")

rows = max(stretches, key=len)
if len(rows) < 2:
    print("no stretch long enough to say anything")
    raise SystemExit(0)

missed = sum(1 for row in rows if not row.get("uptime_s"))
if missed:
    print(f"{missed} sample(s) got no answer at all")

first, last = rows[0], rows[-1]
hours = (float(last["uptime_s"]) - float(first["uptime_s"])) / 3600

print(f"{len(rows)} samples over {hours:.2f} hours, unbroken")

# Frames dropped after the first sample. Dropping some while a display sets its
# mode is the display coming up, not the show missing frames, and the first
# sample is taken after that.
#
# A counter that went backwards was put back to zero by somebody — the operator
# taps it on the System screen, or a client asks. Said rather than printed as a
# negative, because a negative reads as a fault and it is the opposite: it is
# somebody deciding the board should read true from here.
def counted(key):
    try:
        moved = int(float(last[key])) - int(float(first[key]))
    except (KeyError, TypeError, ValueError):
        return "?"
    return str(moved) if moved >= 0 else "cleared"


print(
    f"dropped frames: {counted('dropped_lifetime')}   "
    f"late: {counted('late_frames')}   "
    f"decoder errors: {counted('decode_errors')}"
)

fps = numbers("actual_fps")
wanted = numbers("wanted_fps")
if fps and wanted:
    drift = [a - b for a, b in zip(fps, wanted)]
    worst = max(drift, key=abs)
    mean = sum(drift) / len(drift)
    print(f"rate against the mode: mean {mean:+.4f} fps, worst {worst:+.4f} fps")

# The machine's figure includes whatever else was running; the two processes'
# own figures are the ones a leak would show in.
passes = [
    (numbers("media_us"), "stills"),
    (numbers("playback_us"), "clips"),
    (numbers("compose_us"), "drawing"),
]
if any(column for column, _ in passes):
    parts = []
    for column, name in passes:
        if column:
            parts.append(f"{name} {sum(column) / len(column):.0f} us")
    worst = max(
        (sum(values) for values in zip(*(column for column, _ in passes if column))),
        default=0,
    )
    print(f"the pass: {', '.join(parts)} on average, worst {worst} us of a frame")

for label, key in (("the machine", "ram_used_mb"), ("the renderer", "renderer_mb"), ("the core", "core_mb")):
    values = numbers(key)
    if len(values) >= 2:
        print(f"{label}: {values[0]:.0f} MB to {values[-1]:.0f} MB ({values[-1] - values[0]:+.0f} MB)")

vram = numbers("vram_used_mb")
if len(vram) >= 2:
    print(f"card memory: {vram[0]:.0f} MB to {vram[-1]:.0f} MB ({vram[-1] - vram[0]:+.0f} MB)")

for label, key in (("processor", "cpu_temp_c"), ("card", "gpu_temp_c")):
    values = [v for v in numbers(key) if v > 0]
    if values:
        print(f"{label} temperature: {min(values):.0f} to {max(values):.0f} C")
PY
  exit 0
fi

echo "sampling $base every ${interval}s into $out; stop it when you have seen enough" >&2

# The two processes are measured on their own as well as the machine. A machine
# figure includes whatever else is running — a build, a browser, the console —
# and a leak in the renderer would be invisible inside it.
# Which process it is. A restart leaves the counters at zero and the memory
# where it was, so nothing else in a row says one happened; a different number
# here says it exactly.
pid_of() {
  pgrep -x "$1" | head -1 || true
}

rss_mb() {
  pid=$(pgrep -x "$1" | head -1 || true)
  if [ -n "${pid:-}" ] && [ -r "/proc/$pid/statm" ]; then
    awk -v page="$(getconf PAGE_SIZE)" '{printf "%d", $2 * page / 1048576}' "/proc/$pid/statm"
  fi
}

printf 'at,uptime_s,cpu_percent,cpu_temp_c,ram_used_mb,gpu_percent,gpu_temp_c,vram_used_mb,active_decoders,decode_errors,late_frames,outputs,actual_fps,wanted_fps,dropped_lifetime,media_us,playback_us,compose_us,renderer_mb,core_mb,renderer_pid\n' > "$out"

while true; do
  # A sample that could not be taken is a row saying so rather than a gap: a
  # server that stopped answering for ten minutes is the most interesting thing
  # a soak can find, and it must not look like the script was not running.
  status="$(curl -s --max-time 5 -H 'x-ovs-local: 1' "$base/server/status" || true)"

  if [ -z "$status" ]; then
    printf '%s,,,,,,,,,,,,,,,,,,,,\n' "$(date -Iseconds)" >> "$out"
  else
    printf '%s,%s,%s,%s,%s\n' "$(date -Iseconds)" "$(printf '%s' "$status" | jq -r '
      .telemetry as $t
      | ($t.outputs // []) as $o
      | ($o[0] // {}) as $first
      | [
          $t.uptime_s,
          ($t.cpu_percent // 0 | . * 100 | round / 100),
          ($t.cpu_temperature_c // 0),
          (($t.ram_used_bytes // 0) / 1048576 | round),
          ($t.gpu_percent // 0),
          ($t.gpu_temperature_c // 0),
          (($t.vram_used_bytes // 0) / 1048576 | round),
          $t.active_decoders,
          $t.decode_errors,
          $t.late_frames,
          ($o | length),
          (($first.actual_fps // 0) * 1000 | round / 1000),
          (((($first.refresh // {}).numerator // 0) / ((($first.refresh // {}).denominator // 1))) * 1000 | round / 1000),
          (($first.dropped_frames // {}).lifetime // 0),
          # Where the last frame went, which the renderer could not say until
          # it was asked to time its own pass. Zero rather than blank when
          # there is no renderer, so a column stays a number.
          (($t.frame // {}).media_us // 0),
          (($t.frame // {}).playback_us // 0),
          (($t.frame // {}).compose_us // 0)
        ] | @csv')" "$(rss_mb ovs-renderer)" "$(rss_mb ovsd)" "$(pid_of ovs-renderer)" >> "$out"
  fi

  sleep "$interval"
done
