#!/usr/bin/env bash
# Runs the checks on a real Linux machine, over SSH.
#
#   OVS_REMOTE=ovs-test ./tools/remote.sh            fmt, clippy and the tests
#   OVS_REMOTE=ovs-test ./tools/remote.sh test       the tests alone
#   OVS_REMOTE=ovs-test ./tools/remote.sh displays   what that machine is plugged into
#   OVS_REMOTE=ovs-test ./tools/remote.sh -- <cmd>   anything else, in the tree
#   OVS_REMOTE=ovs-test ./tools/remote.sh fetch <p>  copy a path back from there
#
# Debian is the production target and Windows is where development happens
#. Everything that can only be true on Linux — the display path, the
# service units, what a real GPU driver offers — is true or not on the machine
# that has them, so this makes running there one command rather than a ritual.
#
# The tree is sent with tar rather than git: what matters is what is in the
# working directory now, not what has been committed. The directories that are
# shipped whole are cleared first, because tar adds and overwrites but never
# removes — see `SENT_WHOLE`.
#
# Run `check` here before calling anything finished, not only the tests. Code
# behind `cfg(unix)` — the decoder, the encoder, the media probe — is not
# compiled on Windows at all, so a Windows clippy run says nothing about it and
# has twice passed a lint error straight through to this machine.

set -euo pipefail
cd "$(dirname "$0")/.."

remote="${OVS_REMOTE:-}"
if [ -z "$remote" ]; then
  echo "OVS_REMOTE is not set: name the ssh host to run on, for example ovs-test" >&2
  exit 2
fi

# Its own directory on the far side, so a machine can hold more than one of
# these without them treading on each other.
target="${OVS_REMOTE_DIR:-openvideoserver}"

# The directories that are shipped whole, and are therefore safe to clear
# before the archive lands.
#
# **Clearing them is the point.** `tar` adds and overwrites; it never removes,
# so a file deleted or renamed here stayed on the far side for ever. A test
# renamed in one commit went on running there in its old form for three more,
# failing against a harness that had since been fixed, and the failure named a
# file that no longer exists in this repository.
#
# `target/` is deliberately not on the list: it is the build cache, it is
# enormous, and clearing it would turn every run into a full rebuild.
SENT_WHOLE="crates clients docs packages schemas tools"

send() {
  ssh -o BatchMode=yes "$remote" \
    "mkdir -p '$target' && cd '$target' && rm -rf $SENT_WHOLE"
  # `docs/NDISDK` is hundreds of megabytes of vendor installers that change
  # never and are needed once. Excluded by name rather than by size, because
  # what makes them not worth copying every time is what they are.
  tar czf - \
    --exclude=./target \
    --exclude=./node_modules --exclude=./build \
    --exclude=./.git \
    --exclude='*.exe' \
    --exclude='./docs/NDISDK' \
    . | ssh -o BatchMode=yes "$remote" "tar xzf - -C '$target'"
}

run() {
  # `cargo` is installed per user by rustup and is not on a non-login shell's
  # path, which is why this says where it is rather than assuming.
  # `CEF_PATH` names the Chromium Embedded Framework `ovs-console` links
  # against. Without it that crate's build script downloads and extracts its own
  # copy — one and a half gigabytes, per build directory — which turns a
  # thirty-second build into a long one and does it again on every clean.
  #
  # `PKG_CONFIG_PATH` names the FFmpeg **this tree built**, and it has to: there
  # is no system FFmpeg on a machine set up for this and there must not be, since
  # the point of `tools/build-ffmpeg.sh` is that what the server links is a build
  # whose licence is known. This was missing, and every build here worked only
  # because the reference machine had it set by hand — until that machine was
  # reinstalled and the first build stopped at "Package libavutil was not found".
  #
  # Its `lib` goes on `LD_LIBRARY_PATH` as well, because a test binary has to
  # load at run time what the compiler found at build time.
  # The directory is entered first, so the FFmpeg path is derived from where the
  # tree actually is: `OVS_REMOTE_DIR` takes an absolute path as well as one
  # relative to the home directory, and `$HOME/$target` is wrong for the first.
  ssh -o BatchMode=yes "$remote" \
    "cd '$target' || exit 1; \
     export PATH=\$HOME/.cargo/bin:\$PATH; \
     export CEF_PATH=\$HOME/.local/share/cef; \
     export FFMPEG_DIR=\$PWD/build/ffmpeg-server; \
     export PKG_CONFIG_PATH=\$FFMPEG_DIR/lib/pkgconfig\${PKG_CONFIG_PATH:+:\$PKG_CONFIG_PATH}; \
     export LD_LIBRARY_PATH=\$CEF_PATH:\$FFMPEG_DIR/lib\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}; \
     $1"
}

command="${1:-check}"
case "$command" in
  check)
    send
    run "cargo fmt --all -- --check"
    run "cargo clippy --all-targets --workspace -- -D warnings"
    run "cargo test --workspace"
    ;;
  test)
    send
    run "cargo test --workspace"
    ;;
  build)
    send
    run "cargo build --release --bin ovsd --bin ovs-renderer"
    ;;
  displays)
    send
    run "cargo build --release --bin ovs-renderer --quiet && ./target/release/ovs-renderer --displays"
    ;;
  fetch)
    # The generated artefacts are all produced by cargo, and cargo only exists
    # on the far side. Regenerating one therefore means bringing it back, and
    # doing that by hand is how a committed document ends up one build stale.
    shift
    if [ "$#" -eq 0 ]; then
      echo "fetch: name at least one path inside the tree, for example Cargo.lock" >&2
      exit 2
    fi
    for path in "$@"; do
      case "$path" in
        /* | *..*)
          echo "fetch: $path is not a path inside the tree" >&2
          exit 2
          ;;
      esac
      # Into the parent rather than onto the path itself: scp of a directory
      # onto an existing directory nests it, which is a silent wrong answer.
      parent="$(dirname "$path")"
      mkdir -p "$parent"
      scp -q -o BatchMode=yes -r "$remote:$target/$path" "$parent"
      echo "fetched $path"
    done
    ;;
  --)
    shift
    send
    run "$*"
    ;;
  *)
    echo "usage: OVS_REMOTE=<host> $0 [check|test|build|displays|fetch <path>...|-- <command>]" >&2
    exit 2
    ;;
esac
