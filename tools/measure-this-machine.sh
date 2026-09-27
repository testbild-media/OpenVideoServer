#!/usr/bin/env bash
# What a machine can do, before there is a show on it.
#
#   ./tools/measure-this-machine.sh                 everything it can answer
#   ./tools/measure-this-machine.sh /media/clip.mov also how many decoders fit
#
# Run it on the machine being asked about. It needs a GPU and a build tree.
# It needs **no server, no display, no show and no install**: every part of it
# is a binary run out of `target/release`, so it can be run on a machine that
# is in production without touching anything that is in production.
#
# # Why this exists
#
# The reference server (owner decision Q9) is a 2016 six-core machine
# with one RTX 3060 Ti and no integrated adapter, and **nothing has ever run on
# it**. Every number recorded so far was taken on the test machine, which is a 2023
# fourteen-core laptop with a 4070 and two adapters. Behaviour transfers between
# the two; throughput does not.
#
# The v0.1 acceptance is a real comparison against the CasparCG setup on that
# machine, and walking into it without knowing what the machine holds would mean
# discovering it during the comparison. This is the thing to run first, and the
# output is meant to be handed back whole.
#
# # What each part answers
#
#   decoders   how many clips this processor can decode at once
#   upload     what it costs to put a frame on this card and draw it (R2)
#   effects    what a chain of effects costs on top of that
#   preview    what encoding a preview costs, in hardware and in software
#   validation whether this driver is happy with what the renderer records
#
# # What it does not answer
#
# Whether the wall holds sixty. That needs the wall, a show and real content —
# `tools/how-many-layers.sh` and `tools/reference-load.sh`, both of which need a
# running server. This measures the parts; those measure the whole.

set -uo pipefail

clip="${1:-}"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

rule() { printf '\n== %s %s\n\n' "$1" "$(printf '=%.0s' $(seq 1 $((66 - ${#1}))))"; }

rule "this machine"

# The facts a number is worthless without. A compose figure from a machine
# whose bus negotiated at a quarter of its capability is a fact about the
# morning rather than about the renderer, and that has actually happened here.
if [ -r /proc/cpuinfo ]; then
  model="$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//')"
  cores="$(grep -c '^processor' /proc/cpuinfo)"
  printf '%-12s %s\n' "processor" "${model:-unknown}"
  printf '%-12s %s threads\n' "threads" "$cores"
fi
if [ -r /proc/meminfo ]; then
  kb="$(awk '/MemTotal/ {print $2}' /proc/meminfo)"
  printf '%-12s %s GiB\n' "memory" "$((kb / 1024 / 1024))"
fi
if command -v nvidia-smi >/dev/null 2>&1; then
  printf '%-12s %s\n' "card" "$(nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv,noheader 2>/dev/null | head -1)"
fi
printf '%-12s %s\n' "kernel" "$(uname -sr)"
printf '%-12s %s\n' "date" "$(date -Is)"

rule "building"
# Every binary at once, so the compile is not mistaken for the measurement.
if ! cargo build --release --quiet \
  --bin ovs-decode-cost --bin ovs-upload-cost --bin ovs-preview-cost; then
  echo "could not build the measuring binaries" >&2
  exit 1
fi
echo "built"

if [ -n "$clip" ]; then
  rule "decoders — how many clips this processor holds"
  if [ ! -r "$clip" ]; then
    echo "cannot read $clip" >&2
  else
    for players in 1 3 5 7; do
      echo "--- $players player(s) ---"
      ./target/release/ovs-decode-cost "$clip" --players "$players" --frames 600 2>&1 |
        grep -viE '^$' | tail -6
    done
  fi
else
  rule "decoders — skipped"
  echo "pass a clip to measure decoding:"
  echo "    ./tools/measure-this-machine.sh /path/to/a/real/clip.mov"
  echo "the clip should be one this show would actually play — the codec is the"
  echo "whole question, and a synthetic file answers about the wrong one."
fi

rule "upload and compose — what a frame costs on this card"
for layers in 1 3 5 7; do
  printf -- '--- %s layer(s), 1080p ---\n' "$layers"
  ./target/release/ovs-upload-cost --layers "$layers" --frames 300 2>&1 | tail -4
done

rule "effects — what a chain costs on top"
# Uploads excluded, because the question here is the drawing. At 4K, where the
# card is actually busy: at 1080p on a fast card the chain disappears into the
# noise and the comparison says nothing.
for chain in "" "ovs.blur" "ovs.colour_correct,ovs.blur,ovs.tint"; do
  if [ -z "$chain" ]; then
    printf -- '--- 3 layers, 4K, no effects ---\n'
    ./target/release/ovs-upload-cost --layers 3 --size 3840x2160 --frames 200 --upload-once 2>&1 | tail -4
  else
    printf -- '--- 3 layers, 4K, %s ---\n' "$chain"
    ./target/release/ovs-upload-cost --layers 3 --size 3840x2160 --frames 200 --upload-once --effects "$chain" 2>&1 | tail -4
  fi
done

rule "preview — what somebody watching costs"
# stderr dropped: the encoders log their own statistics there, and a page of
# libx264 macroblock percentages buries the two lines that are the answer.
./target/release/ovs-preview-cost --frames 300 2>/dev/null

rule "validation — whether this driver is happy"
# Last, because it is slow and because it is the one whose answer is a yes or a
# no rather than a number.
if bash tools/validate-renderer.sh; then
  :
else
  echo
  echo "this driver reports hazards the test machine's does not."
  echo "run ./tools/validate-renderer.sh -v and keep the output."
fi

rule "done"
echo "Hand this whole output back. The numbers matter less than the machine"
echo "they came from, and the two are only together here."
