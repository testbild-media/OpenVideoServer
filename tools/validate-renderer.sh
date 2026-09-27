#!/usr/bin/env bash
# Runs the renderer's GPU paths past the Vulkan validation layers.
#
#   ./tools/validate-renderer.sh          every path, and a non-zero exit if
#                                         anything is reported
#   ./tools/validate-renderer.sh -v       print what was reported
#
# Run it on a machine with a GPU and the validation layers installed. It needs
# no server, no display and no show: it drives `ovs-upload-cost`, which builds
# its own Vulkan device and its own pictures.
#
# # Why this exists
#
# Synchronisation is the one part of this renderer that cannot be checked by
# reading it and cannot be checked by looking at the wall. A missing barrier is
# not a crash and not a wrong picture — it is a picture that is right on this
# driver and wrong on the next one, or right at this frame rate and wrong when
# the machine is busy. The only thing that finds it is the validation layer,
# and the only thing that keeps it found is running the layer again.
#
# Two real hazards were found this way and both were invisible without it:
#
#   - `vkCmdBlitImage` reading the composition while the render pass's own
#     end-of-pass layout transition was still writing it. Every frame a preview
#     was watched. NVIDIA tolerated it.
#   - `vkCmdBeginRenderPass` loading an effect pass's attachment while the same
#     command's layout transition was writing it. Every frame of every effect.
#     Found only once `ovs-upload-cost` learnt to run a chain, because until
#     then nothing outside the show exercised those passes.
#
# # What it covers, and what it does not
#
# Covered: the composition pass, the texture upload, the readback and blit, and
# the effect ping-pong, with and without a preview being watched.
#
# Not covered: presenting. That path needs a real display, which means the
# renderer service, which means a show on a wall — so it is not something a
# check can take over. It is exercised every time the renderer runs, and a
# hazard there would need the layers enabled on the service itself.

set -uo pipefail

verbose=0
if [ "${1:-}" = "-v" ] || [ "${1:-}" = "--verbose" ]; then
  verbose=1
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

if [ -z "${VULKAN_SDK:-}" ] && ! ls /usr/share/vulkan/explicit_layer.d/*validation*.json >/dev/null 2>&1; then
  echo "the Khronos validation layer is not installed here" >&2
  echo "on Debian: apt-get install vulkan-validationlayers" >&2
  exit 1
fi

export VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation
export VK_LAYER_ENABLES=VK_VALIDATION_FEATURE_ENABLE_SYNCHRONIZATION_VALIDATION_EXT

# The build is done once, outside the timed-looking part, so a first run does
# not look like a hang.
echo "building"
if ! cargo build --release --quiet --bin ovs-upload-cost; then
  echo "could not build ovs-upload-cost" >&2
  exit 1
fi
binary="target/release/ovs-upload-cost"

# Each line is one path worth exercising. Short runs: a hazard that is there is
# there on the first frame, and ninety frames is enough to catch one that needs
# a second frame to have something to race.
cases=(
  "--layers 1"
  "--layers 3"
  "--layers 3 --readback"
  "--layers 3 --upload-once"
  "--layers 3 --effects ovs.tint"
  "--layers 3 --effects ovs.blur"
  "--layers 3 --effects ovs.colour_correct,ovs.blur,ovs.tint"
  "--layers 3 --effects ovs.blur --readback"
  "--layers 1 --size 3840x1080 --effects ovs.pixelate --readback"
)

failed=0
for case in "${cases[@]}"; do
  # shellcheck disable=SC2086
  output="$("$binary" $case --frames 90 2>&1)"
  reported="$(printf '%s\n' "$output" | grep -cE 'Validation Error|Validation Warning')"

  if [ "$reported" -eq 0 ]; then
    printf 'clean   %s\n' "$case"
  else
    printf 'REPORTS %s   (%s lines)\n' "$case" "$reported"
    failed=1
    if [ "$verbose" -eq 1 ]; then
      printf '%s\n' "$output" | grep -E 'Validation (Error|Warning)' -A 2 | sed 's/^/    /'
    fi
  fi
done

echo
if [ "$failed" -eq 0 ]; then
  echo "every path is clean"
  exit 0
fi

echo "run again with -v to see what was reported" >&2
exit 1
