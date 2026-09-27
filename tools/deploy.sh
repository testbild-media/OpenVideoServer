#!/usr/bin/env bash
# Puts the binaries in this tree onto the machine they were built on.
#
#   ./tools/deploy.sh              build, install, restart, and say whether it worked
#   ./tools/deploy.sh --check      say what would change, install nothing
#
# Run it on the server, from the checkout. It needs `sudo` for two `install`
# commands and two `systemctl restart`s, and nothing else.
#
# # Why this exists
#
# A deployment done by hand is three commands in an order nobody wrote down,
# and the interesting part is not the copying — it is knowing whether the show
# came back. The renderer holds a display: if the new binary cannot open the
# card, the wall goes dark and stays dark, and the person who typed the command
# has already moved on. So this keeps what it replaced, restarts, waits for the
# wall to report a frame rate again, and puts the old binary back if it does
# not.
#
# # What it does not do
#
# It does not build on one machine and copy to another. A binary built against
# this machine's FFmpeg and this machine's driver is the only one worth
# installing on it; `tools/remote.sh build` is how a tree gets here.

set -euo pipefail
cd "$(dirname "$0")/.."

api="${OVS_BASE:-http://127.0.0.1:7420/api/v1}"
keep="${OVS_DEPLOY_KEEP:-/var/lib/ovs/previous}"
check_only=""
[ "${1:-}" = "--check" ] && check_only="yes"

# What is running now, so the report at the end has something to compare with.
running() {
  curl -fsS --max-time 2 "$api/server/status" 2>/dev/null \
    | jq -r '"\(.telemetry.version), \(.telemetry.outputs[0].actual_fps // 0 | .*100 | round / 100) fps, \(.telemetry.outputs | length) output(s)"' \
    || echo "not answering"
}

echo "before: $(running)"

# Where rustup puts it. `cargo` is installed per user and is not on the path of
# a shell that did not source `~/.cargo/env`, which on this machine includes
# the owner's own login shell — so the first real run of this script stopped at
# "cargo: Kommando nicht gefunden" before it had done anything at all.
# `tools/remote.sh` has said where cargo is since it was written; this had not.
if ! command -v cargo > /dev/null; then
  PATH="$HOME/.cargo/bin:$PATH"
  export PATH
fi
command -v cargo > /dev/null || {
  echo "no cargo on this machine, and none in ~/.cargo/bin either" >&2
  echo "this script builds what it installs, so there is nothing it can do" >&2
  exit 1
}

echo "building"

# **The FFmpeg this tree built, which is the only one there is.** No system
# FFmpeg is installed on a machine set up for this and none should be: what the
# server links has to be a build whose licence is known. Only `tools/package.sh`
# knew that; here and in `tools/remote.sh` it worked because the reference
# machine had `PKG_CONFIG_PATH` set by hand, which nothing recorded and a
# reinstall took away.
ffmpeg_dir="$PWD/build/ffmpeg-server"
if [ ! -d "$ffmpeg_dir/lib/pkgconfig" ]; then
  echo "no FFmpeg in $ffmpeg_dir; nothing here can link." >&2
  echo "  ./tools/build-ffmpeg.sh --profile server" >&2
  exit 1
fi
export PKG_CONFIG_PATH="$ffmpeg_dir/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"

# And the same RPATH the package gives its binaries, because they look for those
# libraries in the same place: `/usr/lib/openvideoserver`, which the package put
# them in. `--disable-new-dtags` asks for DT_RPATH rather than DT_RUNPATH.
export RUSTFLAGS="-C link-arg=-Wl,-rpath,/usr/lib/openvideoserver -C link-arg=-Wl,--disable-new-dtags${RUSTFLAGS:+ $RUSTFLAGS}"

# `ovs-console` too, and it is the slow one: it links a 1.4 GB Chromium. The
# build script finds the framework through `CEF_PATH`, and without that it
# downloads its own copy — one and a half gigabytes, per build directory.
cef_src="${CEF_PATH:-$HOME/.local/share/cef}"
if [ -f "$cef_src/libcef.so" ]; then
  CEF_PATH="$cef_src"
  export CEF_PATH
  cargo build --release --bin ovsd --bin ovs-renderer --bin ovs-console
  console="yes"
else
  echo "  no Chromium framework in $cef_src; building without the System screen"
  echo "  (export-cef-dir puts one there; see crates/ovs-console/Cargo.toml)"
  cargo build --release --bin ovsd --bin ovs-renderer
  console=""
fi

for binary in ovsd ovs-renderer ${console:+ovs-console}; do
  built="target/release/$binary"
  [ -x "$built" ] || { echo "cargo built no $built" >&2; exit 1; }
  if cmp -s "$built" "/usr/bin/$binary"; then
    echo "  $binary is already what is installed"
  else
    echo "  $binary differs from what is installed"
  fi
done

if [ -n "$check_only" ]; then
  echo "checking only, so nothing was installed"
  exit 0
fi

# Kept where the state directory is, because that is the one place on this
# machine that belongs to the server and is backed up with it.
sudo mkdir -p "$keep"
for binary in ovsd ovs-renderer ovs-console; do
  if [ -x "/usr/bin/$binary" ]; then
    sudo cp -p "/usr/bin/$binary" "$keep/$binary"
  fi
done
echo "what was there is in $keep"

echo "installing"
sudo install -m 0755 target/release/ovsd /usr/bin/ovsd
sudo install -m 0755 target/release/ovs-renderer /usr/bin/ovs-renderer

# **The System screen, and the Chromium it is drawn with.**
#
# The framework is 1.5 GB and is not a system library: it goes beside the rest of
# this product rather than into /usr/lib, so nothing else on the machine can pick
# it up by accident and an uninstall takes it with it. `ovs-console.service`
# names this path in both `CEF_PATH` and `LD_LIBRARY_PATH`, which answer
# different questions — where the resources are, and where the library is.
#
# Compared before copying, because it is a gigabyte and a half and most deploys
# change none of it.
if [ -n "$console" ]; then
  sudo install -m 0755 target/release/ovs-console /usr/bin/ovs-console
  cef_dst="/usr/lib/openvideoserver/cef"
  if sudo cmp -s "$cef_src/libcef.so" "$cef_dst/libcef.so"; then
    echo "  the Chromium framework is already what is installed"
  else
    echo "  carrying the Chromium framework ($(du -sh "$cef_src/libcef.so" | cut -f1) of library)"
    sudo install -d -m 0755 "$cef_dst"
    # By name, because the distribution also holds headers, a CMake project, a
    # sample program and a SUID sandbox helper this does not use — nearly 200 MB
    # of things a running console never opens.
    for f in libcef.so icudtl.dat resources.pak chrome_100_percent.pak \
             chrome_200_percent.pak v8_context_snapshot.bin libEGL.so \
             libGLESv2.so libvulkan.so.1 libvk_swiftshader.so \
             vk_swiftshader_icd.json CREDITS.html; do
      [ -f "$cef_src/$f" ] && sudo install -m 0644 "$cef_src/$f" "$cef_dst/$f"
    done
    sudo rm -rf "$cef_dst/locales"
    sudo cp -r "$cef_src/locales" "$cef_dst/"
    sudo chmod -R a+rX "$cef_dst"
  fi
else
  echo "  no System screen in this build; /usr/bin/ovs-console left as it was"
fi

# The unit files too, since a change to one is easy to forget and silent.
#
# **Every one of them, because this used to compare two and install three.**
# `ovs-console.service` was not among the two, so a change to it alone left this
# block unentered: nothing installed, nothing reloaded, and a deploy that reported
# success at everything it did do. A fix written, committed and deployed did not
# reach the machine, and the only evidence was the absence of one line of output.
units_changed=""
for unit in deploy/systemd/*.service; do
  if ! sudo cmp -s "$unit" "/etc/systemd/system/$(basename "$unit")"; then
    units_changed="$units_changed $(basename "$unit")"
  fi
done

if [ -n "$units_changed" ]; then
  echo "the unit files changed as well:$units_changed"

  # **Checked before they are installed.** `systemd-analyze verify` reads a unit
  # the way systemd will and says what it does not understand — and what it does
  # not understand, it *ignores*, silently, at boot. It found
  # `StartLimitIntervalSec` sitting in `[Service]` in two of these, where it does
  # nothing, under a comment saying the service must never give up.
  #
  # Said rather than refused. On a machine that has not installed these binaries
  # yet it also reports that they are not executable, which is true and is not a
  # reason to stop a first deploy.
  if command -v systemd-analyze > /dev/null; then
    (cd deploy/systemd && systemd-analyze verify ./*.service) 2>&1 \
      | grep -v "is not executable" \
      | sed 's/^/  /' || true
  fi

  sudo install -m 0644 deploy/systemd/*.service /etc/systemd/system/
  sudo systemctl daemon-reload
fi

# **The configuration directory, owned by the core.** Without this, Save on the
# System screen fails with "this server cannot write
# /etc/openvideoserver/config.json" (owner, 2026-09-25) -- and it fails on exactly
# the machines that have been running longest.
#
# The unit's `ConfigurationDirectory=` makes it, but only when it is not already
# there: systemd deliberately leaves an existing one's ownership alone, because
# configuration belongs to the administrator. A machine that had this directory
# before the unit gained that line therefore keeps it root-owned for ever.
#
# The directory, not the file. The server replaces config.json by writing beside
# it and renaming, which needs write permission on the directory and none on the
# file, so an administrator's own root-owned config.json stays theirs and stays
# replaceable. 0755 because the renderer and the kiosk run as other users and
# both read it; nothing in it is secret, it describes sockets.
sudo install -d -m 0755 /etc/openvideoserver
sudo chown ovs:ovs /etc/openvideoserver

# **What an NVIDIA card needs before it will give up more than one output.**
#
# `modeset=0`, or the card's KMS driver owns the connectors and `VK_KHR_display`
# cannot take the one it is asked for: one output comes up, a second does not, and
# nothing says why. `deploy/modprobe.d/ovs-nvidia.conf` says the rest.
#
# **This is the one step here that cannot take effect while the show is running.**
# The file is installed, the initramfs is rebuilt, and the reboot is *said* rather
# than taken — a deploy that rebooted the machine would be a deploy nobody could
# run during a rehearsal.
#
# Installed on every machine, not only one with such a card: modprobe ignores
# options for a module that is not there, and a machine that grows a card later
# should already have them.
boot_note=""
if ! sudo cmp -s deploy/modprobe.d/ovs-nvidia.conf /etc/modprobe.d/ovs-nvidia.conf; then
  echo "the card's module options changed"
  sudo install -d -m 0755 /etc/modprobe.d
  sudo install -m 0644 deploy/modprobe.d/ovs-nvidia.conf /etc/modprobe.d/ovs-nvidia.conf
  # These modules load from the initramfs, so the file on its own changes nothing.
  # In /usr/sbin, which is not on the path of a shell that did not log in — the
  # same trap `cargo` was found in above.
  if [ -x /usr/sbin/update-initramfs ]; then
    echo "  rebuilding the initramfs"
    sudo /usr/sbin/update-initramfs -u > /dev/null 2>&1 || true
  fi
fi

# Whether this machine has an NVIDIA display adapter. Asked of sysfs rather than
# of `lspci`, which a minimal Debian does not have installed.
has_nvidia_card() {
  for dev in /sys/bus/pci/devices/*; do
    [ -r "$dev/vendor" ] || continue
    [ "$(cat "$dev/vendor")" = "0x10de" ] || continue
    case "$(cat "$dev/class" 2> /dev/null)" in
      0x0300*) return 0 ;;
    esac
  done
  return 1
}

# What the *running* kernel has, which is the only thing the renderer cares
# about. The parameter is a bool: it reads `Y` or `N`, and it is root-readable
# only.
#
# Asked only where there is a card **and a driver loaded to ask**. Without the
# module there is nothing to read and nothing true to say — a machine with the
# card and no driver would otherwise be sent to reboot for a setting that cannot
# be in force yet.
if has_nvidia_card && [ -d /sys/module/nvidia_drm ]; then
  modeset=$(sudo cat /sys/module/nvidia_drm/parameters/modeset 2> /dev/null || echo "?")
  case "$modeset" in
    N | n | 0) ;;
    *)
      boot_note="this card is still running with KMS modesetting on (modeset=$modeset), so it \
will give up one output and no more; that needs a reboot"
      ;;
  esac
fi

# Restarting and stopping the machine. `crates/ovs-core/src/power.rs` runs
# `systemctl reboot`, which is a D-Bus call to logind, and polkit decides whether
# an unprivileged user may make it. Without this the System screen's two buttons
# reach the server, run the command, and are refused with exit status 1 (owner,
# 2026-09-25). The rule itself says what it grants and what it deliberately does
# not. polkitd watches the directory, so nothing has to be restarted.
#
# /etc rather than /usr/share, because a hand install has no package to own a
# file under /usr.
sudo install -d -m 0755 /etc/polkit-1/rules.d
sudo install -m 0644 deploy/polkit/50-openvideoserver-power.rules /etc/polkit-1/rules.d/

# The client, so the machine running the show can hand it out. The same rule
# `tools/package.sh` uses: whatever is in `dist/client` goes, and a checkout with
# nothing there installs nothing — a page offering a download the server has not
# got is worse than a page offering none.
#
# The point is the evening where somebody needs the client on a second laptop and
# the venue's wifi is a captive portal: the server's address is already on a
# screen. `crates/ovs-core/src/server/downloads.rs` hands out what lands here.
sudo install -d -m 0755 /usr/share/openvideoserver/client
if compgen -G "dist/client/*" > /dev/null; then
  sudo install -m 0644 dist/client/* /usr/share/openvideoserver/client/
  echo "carrying $(ls dist/client | wc -l) client file(s)"
else
  echo "no client in dist/client; this server will offer none"
fi

echo "restarting"
# **The order no longer matters, and that is the point.**
#
# It used to: the console was a compositor, it held a card, and a renderer
# started while it still had one found nothing to present on. The console asks
# the graphics stack for nothing now. It listens on a socket the renderer
# connects to and retries for ever; the renderer retries that socket for ever;
# and each reloads or reconnects when the other comes back. Started in any order,
# both arrive at the same place.
#
# The console is still first, because it is the one whose failure is visible from
# the front of the rack and it may as well have the head start.
sudo systemctl restart ovs-console || true
sudo systemctl restart ovs-renderer
sudo systemctl restart ovsd

# The wall is what decides whether this worked. A frame rate near the mode, from
# an output that is present, is the only answer that counts — a server that
# answers while every wall is dark is not a working deployment.
echo "waiting for the wall"
lit=""
for _ in $(seq 1 40); do
  sleep 1
  fps=$(curl -fsS --max-time 2 "$api/server/status" 2>/dev/null \
    | jq -r '.telemetry.outputs[0].actual_fps // 0' 2>/dev/null || echo 0)
  if awk -v f="$fps" 'BEGIN { exit !(f > 10) }'; then
    lit="yes"
    break
  fi
done

if [ -z "$lit" ]; then
  echo "the wall did not come back; putting back what was there" >&2
  for binary in ovsd ovs-renderer; do
    [ -x "$keep/$binary" ] && sudo install -m 0755 "$keep/$binary" "/usr/bin/$binary"
  done
  sudo systemctl restart ovs-renderer ovsd
  sleep 5
  echo "after rolling back: $(running)" >&2
  journalctl -u ovs-renderer -n 20 --no-pager >&2 || true
  exit 1
fi

echo "after:  $(running)"
# Last, so it is the line still on the screen when this finishes. A wall that
# came back is not the same as a machine that can carry two of them.
if [ -n "$boot_note" ]; then
  echo ""
  echo "  ⚠ $boot_note"
  echo ""
fi
curl -fsS "$api/server/status" | jq -c '{
  renderer: .telemetry.renderer.state,
  playing: .telemetry.active_decoders,
  late: .telemetry.late_frames,
  frame: .telemetry.frame
}'
