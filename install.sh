#!/bin/sh
# Installs OpenVideoServer on this Debian machine.
#
# From nothing but a network. A Debian netinst has no curl, so:
#
#   sudo apt install curl -y
#
# **The repository is private**, so every request needs a token — a fine-grained
# personal access token with read access to its contents. Nothing anonymous
# reaches it, including this file:
#
#   export OVS_TOKEN=github_pat_...
#   curl -fsSL -H "Authorization: Bearer $OVS_TOKEN" \
#        -H "Accept: application/vnd.github.raw" \
#        https://api.github.com/repos/testbild-media/OpenVideoServer/contents/install.sh \
#     | sudo -E sh
#
# `sudo -E` keeps the variable, which is what stops the token becoming an
# argument in the process list. `--token` takes one too, for a checkout.
#
# On a public repository neither is needed and the plain form works:
#
#   curl -fsSL https://raw.githubusercontent.com/testbild-media/OpenVideoServer/main/install.sh | sudo sh
#
# Either way it takes everything from the newest GitHub release: the `.deb` for
# this architecture, the checksum published beside it, and the Windows client,
# which goes where the server's own download page looks. The graphics driver
# comes from Debian's own archive.
#
# From a checkout, which builds what it installs instead of downloading it:
#
#   sudo ./install.sh
#   sudo ./install.sh --package x.deb      a package built elsewhere
#   sudo ./install.sh --url https://…      a package to download
#   sudo ./install.sh --no-driver          leave the graphics driver alone
#   sudo ./install.sh --no-client          do not fetch the Windows client
#   sudo ./install.sh --no-reboot          and leave restarting to somebody
#   sudo ./install.sh --token ghp_...      or OVS_TOKEN, for a private repository
#
# It works out which of those it is in: a checkout beside it means build, and
# anything else means fetch the newest release. Either way it installs with apt
# so the dependencies come too, and stops when the server is answering.
#
# # What it does not do
#
# **It does not create an account.** That is the one step where a password is
# typed, and a script that took one would put it in a shell history and a
# process list. The last thing it prints is the address of the page that asks
# for it, and the command for a machine with no browser.
#
# **It does not decide which socket the System screen goes on.** That is a fact
# about how the machine was built, and it is set as a connector role from the
# client or from the screen itself. The screen is installed and running on every
# machine either way; until a socket is marked for it, it draws nothing and costs
# nothing.
#
# **It does install a graphics driver**, because a renderer without one comes up
# on the processor's own graphics and shows a wall nothing — running, composing,
# and useless. On an NVIDIA machine that means enabling `contrib`, `non-free` and
# `non-free-firmware`, which is a licence decision; `--no-driver` is there for
# somebody who would rather make it themselves.
#
# What it cannot do is Secure Boot. A module built by DKMS is signed with a key
# the firmware does not know, and enrolling one needs a password chosen by a
# person and an answer at the next boot. It checks, and says.
#
# # On piping this into a shell
#
# It is convenient and it is a real trust decision: you are running whatever
# that URL serves, as root. The honest mitigations are here — the download is
# checked against the checksum published beside it, and the script says what it
# is about to install before it does. The dishonest one would be to pretend the
# pipe is safe because the file has a comment saying so. Read it first if you
# would rather; it is the same file either way.

# **POSIX `sh`, not bash, and that is the whole point of the file.** It is meant
# to be piped into a shell, and a pipe ignores the shebang: on Debian it runs
# under dash whatever the first line says. `set -o pipefail` is not POSIX and
# `BASH_SOURCE` is an array; both were here, and either would have been a syntax
# error the first time somebody ran the command written at the top of this file.
#
# Losing `pipefail` costs nothing here: the two pipelines that could fail already
# say `|| true` and are checked for emptiness, and the third feeds a checksum
# comparison that fails closed.
#
# Run under dash before being committed, which is the only way to know.
set -eu

# How long the restart at the end waits before taking the machine.
#
# Long enough to read the line above it and press Ctrl-C, short enough that
# nobody sits watching it. Somebody who wants neither should say `--no-reboot`.
GRACE=10

REPO="testbild-media/OpenVideoServer"

if [ "$(id -u)" != "0" ]; then
  echo "this needs root: sudo $0 $*" >&2
  exit 1
fi

package=""
url=""
driver=yes
restart=yes
client=yes
# The environment first, because a token in a variable is a token that is not in
# the process list. `--token` overrides it.
token="${OVS_TOKEN:-}"
while [ $# -gt 0 ]; do
  case "$1" in
    # Taken and ignored rather than refused. It used to fetch a compositor and a
    # browser; the package carries the System screen itself now, so the flag has
    # nothing left to do — and somebody with the old command in their notes
    # should get a working server and a sentence, not an error.
    --with-screen)
      echo "--with-screen is no longer needed: the package carries the System screen."
      ;;
    --package) shift; package="${1:-}" ;;
    --url) shift; url="${1:-}" ;;
    --no-driver) driver=no ;;
    --no-reboot) restart=no ;;
    --no-client) client=no ;;
    --token) shift; token="${1:-}" ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

# A checkout is where this file sits beside the workspace it describes. Piped
# into a shell there is no such thing: `$0` is the shell's own name, which is not
# a file, so this comes out empty and the download path is taken.
here=""
if [ -f "$0" ]; then
  here=$(cd "$(dirname "$0")" && pwd)
fi
checkout=no
if [ -n "$here" ] && [ -f "$here/Cargo.toml" ] && [ -d "$here/crates" ]; then
  checkout=yes
  cd "$here"
fi

# **Nothing here may stop and ask.** This file is made to be piped into a shell,
# which means stdin is the pipe: a debconf question, or `needrestart` asking which
# services to restart, would read the rest of this script as its answer or block
# until somebody finds the machine. Debian 12 and later install `needrestart` and
# it prompts after every apt run by default, so the command documented at the top
# of this file hung the first time it was really used.
#
# `NEEDRESTART_SUSPEND` rather than its automatic mode: restarting whatever it
# thinks needs restarting, in the middle of an install, is a second surprise. The
# machine is asked to reboot at the end anyway.
DEBIAN_FRONTEND=noninteractive
NEEDRESTART_SUSPEND=1
export DEBIAN_FRONTEND NEEDRESTART_SUSPEND

# Installs without asking about configuration files either. `confdef` takes the
# package's answer where there is one and `confold` keeps what an administrator
# put there, which is what this server's own conffile wants: an edited
# `ovs-nvidia.conf` survives an upgrade.
# Asks GitHub something, with the token when there is one.
github() {
  if [ -n "$token" ]; then
    curl -fsSL -H "Authorization: Bearer $token" "$1" 2>/dev/null
  else
    curl -fsSL "$1" 2>/dev/null
  fi
}

# Downloads one thing from GitHub into a file.
#
# `Accept: application/octet-stream` is what turns an asset's API address into
# the file rather than a description of it, and it is harmless on the public
# one.
download() {
  if [ -n "$token" ]; then
    curl -fSL --progress-bar \
      -H "Authorization: Bearer $token" \
      -H "Accept: application/octet-stream" \
      "$1" -o "$2"
  else
    curl -fSL --progress-bar "$1" -o "$2"
  fi
}

# One asset's address in `$release`, by the name it was published under.
#
# **Which address depends on the token.** The API answers with two for every
# asset. `browser_download_url` is the public one and needs nothing;
# `url` is the API's own, and it is the only one that works for a private
# repository. The browser URL ignores a token and answers 404 there — which
# reads exactly like a release that carries no such asset, and would have sent
# somebody looking for a broken release rather than a missing header.
#
# Read with awk rather than a grep because an asset carries its *uploader's*
# API url as well as its own, and a grep for `"url"` finds whichever came last.
# The asset's own is the one ending `/releases/assets/<n>`; `name` comes before
# it in the object and `browser_download_url` comes last, so by the time that
# line is reached both belong to the same asset.
asset_named() {
  printf '%s' "$release" | tr ',' '\n' | awk -v want="$1" -v api="${token:+yes}" '
    /"url": *"https:\/\/api\.github\.com\/repos\/.*\/releases\/assets\/[0-9]+"/ {
      match($0, /https:[^"]*/)
      mine = substr($0, RSTART, RLENGTH)
    }
    /"name": *"/ {
      line = $0
      sub(/.*"name": *"/, "", line)
      sub(/".*/, "", line)
      name = line
    }
    /"browser_download_url": *"/ {
      match($0, /https:[^"]*/)
      web = substr($0, RSTART, RLENGTH)
      if (name ~ want) {
        print (api == "yes" ? mine : web)
        exit
      }
    }
  '
}

# The same asset's published name, which is what it is saved as.
#
# Needed because an API address ends in a number: `basename` on it would save
# the Windows client as `47`.
asset_name() {
  printf '%s' "$release" | tr ',' '\n' | awk -v want="$1" '
    /"name": *"/ {
      line = $0
      sub(/.*"name": *"/, "", line)
      sub(/".*/, "", line)
      name = line
    }
    /"browser_download_url": *"/ {
      if (name ~ want) {
        print name
        exit
      }
    }
  '
}

apt_install() {
  apt-get install -y \
    -o Dpkg::Options::=--force-confdef \
    -o Dpkg::Options::=--force-confold \
    "$@"
}

for tool in curl apt-get; do
  command -v "$tool" > /dev/null || {
    echo "$tool is missing. This installer needs it." >&2
    exit 1
  }
done

# --- where the package comes from ----------------------------------------------

if [ -z "$package" ] && [ -z "$url" ] && [ "$checkout" = yes ] && command -v cargo > /dev/null; then
  echo "==> building the package from this checkout"
  # Built as whoever invoked sudo: a release build as root leaves a target
  # directory they cannot write to afterwards.
  if [ -n "${SUDO_USER:-}" ]; then
    sudo -u "$SUDO_USER" ./tools/package.sh
  else
    ./tools/package.sh
  fi
  package=$(ls -t target/package/openvideoserver_*.deb 2>/dev/null | head -1 || true)
fi

if [ -z "$package" ]; then
  architecture=$(dpkg --print-architecture)

  if [ -n "$url" ]; then
    # Named outright, which is the one form that cannot go stale.
    asset="$url"
    echo "==> taking the package from $asset"
  else
    echo "==> looking for the newest release for $architecture"

    release=$(github "https://api.github.com/repos/$REPO/releases/latest" || true)
    asset=$(asset_named "^openvideoserver_.*_${architecture}\\\\.deb\$")
    sums=$(asset_named "^openvideoserver_.*_${architecture}\\\\.deb\\\\.sha256\$")
    # The Windows client from the same release, taken below once the package
    # has made the directory it goes in. Read here because this is where the
    # release is in hand, and asking for it twice would be asking GitHub for
    # the same answer twice and hoping it is the same one.
    client_asset=$(asset_named "\\\\.exe\$")
    client_name=$(asset_name "\\\\.exe\$")
    package_name=$(asset_name "^openvideoserver_.*_${architecture}\\\\.deb\$")
  fi

  if [ -z "$asset" ]; then
    echo "" >&2
    echo "No published release carries a package for $architecture." >&2
    echo "" >&2
    if [ -z "$token" ]; then
      echo "This repository is private, so an anonymous request sees no" >&2
      echo "releases at all — which looks exactly like this. Give it a token:" >&2
      echo "  sudo ./install.sh --token github_pat_...   (or set OVS_TOKEN)" >&2
      echo "" >&2
    fi
    echo "Name one to download:" >&2
    echo "  sudo ./install.sh --url https://example/openvideoserver_x.y.z_$architecture.deb" >&2
    echo "" >&2
    echo "Or build one from a checkout:" >&2
    echo "  git clone https://github.com/$REPO.git" >&2
    echo "  cd OpenVideoServer && sudo ./install.sh" >&2
    exit 1
  fi

  download=$(mktemp -d)
  trap 'rm -rf "$download"' EXIT
  # By the name it was published under, not by the last part of the address:
  # an asset's API address ends in a number, and `basename` on one would save
  # the package as `312`.
  name="${package_name:-$(basename "$asset")}"
  package="$download/$name"

  echo "==> downloading $name"
  download "$asset" "$package"

  # Checked against the checksum published beside it. A download that arrived
  # wrong is the one failure mode a package manager cannot catch for you, and a
  # server built from a truncated file fails later and stranger.
  # Its own asset in the release rather than the package's address with
  # `.sha256` stuck on the end — which was true of a browser URL and is not
  # true of an API one.
  if [ -n "${sums:-}" ] && download "$sums" "$package.sha256" 2>/dev/null; then
    echo "==> checking it"
    expected=$(awk '{print $1}' "$package.sha256")
    actual=$(sha256sum "$package" | awk '{print $1}')
    if [ "$expected" != "$actual" ]; then
      echo "the download does not match its published checksum; refusing it" >&2
      echo "  published $expected" >&2
      echo "  got       $actual" >&2
      exit 1
    fi
  else
    # **Said plainly, because this is the weak point.** apt will not catch a
    # package that arrived wrong or was replaced on the way: it installs what it
    # is given, as root. A checksum published beside the file closes it, and
    # over http there is nothing else that would.
    echo "    (no checksum published beside it; installing unverified)" >&2
    case "$asset" in
      http://*)
        echo "    and it came over http, so anything on the path could have" >&2
        echo "    replaced it. Publish a .sha256 beside the package." >&2
        ;;
    esac
  fi
fi

if [ ! -f "$package" ]; then
  echo "no package to install." >&2
  exit 1
fi

# --- installing -----------------------------------------------------------------

echo "==> installing $(basename "$package")"
apt-get update -qq || true
# `apt install ./file.deb` rather than `dpkg -i`, because apt resolves the
# dependencies the package declares and dpkg only complains about them.
apt_install "$(readlink -f "$package")"

# --- the client the server hands out ---------------------------------------------
#
# The package carries a Windows client when the release that built it had one.
# It does not when the package was built by hand from a checkout with nothing in
# `dist/client`, and it does not for a package built before that job existed —
# and then the server comes up with a download page that offers nothing, which
# is the one thing that page exists not to do.
#
# Never fatal. A server with no client beside it is a working server, and the
# machine this is running on is the one that cannot use the file anyway.

client_dir=/usr/share/openvideoserver/client

if [ "$client" = yes ] && [ -n "${client_asset:-}" ]; then
  if [ -n "$(ls -A "$client_dir" 2>/dev/null)" ]; then
    echo "==> the package already carries a client; leaving it"
  else
    client_file="${client_name:-$(basename "$client_asset")}"
    echo "==> fetching the Windows client: $client_file"
    mkdir -p "$client_dir"
    if download "$client_asset" "$client_dir/$client_file"; then
      chmod 0644 "$client_dir/$client_file"
      echo "    the server's download page will offer it"
    else
      # Left tidy rather than half-downloaded: `downloads.rs` lists whatever is
      # in this directory by name and size, so a truncated file would be
      # offered as though it were the client.
      rm -f "$client_dir/$client_file"
      echo "    could not fetch it; the server will offer no client" >&2
    fi
  fi
elif [ "$client" = yes ] && [ -z "${client_asset:-}" ] && [ -z "$(ls -A "$client_dir" 2>/dev/null)" ]; then
  echo "==> no Windows client in this release and none in the package"
  echo "    the server's download page will be empty until one is put in"
  echo "    $client_dir"
fi

# --- the graphics driver ---------------------------------------------------------

# Whether this machine has an NVIDIA display adapter. Asked of sysfs rather than
# of `lspci`, which a minimal Debian does not have installed.
has_nvidia_card() {
  for device in /sys/bus/pci/devices/*; do
    [ -r "$device/vendor" ] || continue
    [ "$(cat "$device/vendor")" = "0x10de" ] || continue
    case "$(cat "$device/class" 2> /dev/null)" in
      0x0300*) return 0 ;;
    esac
  done
  return 1
}

# Whether the firmware is enforcing Secure Boot.
#
# Read out of the EFI variable rather than from `mokutil`, which a fresh Debian
# does not have: the value is four bytes of attributes and then one byte, and 1
# is on. `od` is in coreutils and is always there.
secure_boot_on() {
  for variable in /sys/firmware/efi/efivars/SecureBoot-*; do
    [ -r "$variable" ] || continue
    [ "$(od -An -t u1 -j 4 -N 1 "$variable" 2> /dev/null | tr -d ' ')" = "1" ] && return 0
    return 1
  done
  return 1
}

# Makes sure apt can see the components named, in either source format.
#
# **Both, because Debian changed which it writes.** A machine installed as
# Debian 13 has `/etc/apt/sources.list.d/*.sources` in deb822 form; one upgraded
# from older has the one-line `/etc/apt/sources.list`; a machine can have both.
# Only the entries that already carry `main` are touched — a third-party
# repository is not this installer's to widen.
#
# Idempotent, and a file is copied to `<name>.ovs-backup` before its first
# change, so running this twice leaves the backup as the original.
components_changed=no

# Adds the components to one file, on the lines that already offer `main` and do
# not already carry them.
#
# `$2` is what such a line starts with: `Components:` in the deb822 files, `deb`
# in the one-line one. It is anchored, so `deb-src` and a commented-out line are
# both left alone.
#
# **Per line, not per file.** The first version asked whether the file mentioned
# a component anywhere — and Debian's own one-line sources carry `main
# non-free-firmware` on some lines and `main` on others, so the line that needed
# widening did not get it. Caught by running it against both formats.
#
# Edited in a copy and compared, so the file is written only when something
# changed, and backed up once, before its first change.
widen_file() {
  target=$1
  starts=$2
  shift 2
  [ -f "$target" ] || return 0

  work=$(mktemp)
  cat "$target" > "$work"
  for want in "$@"; do
    sed -i -E \
      "/^$starts([[:space:]].*)?[[:space:]]main([[:space:]]|$)/ {
         /[[:space:]]$want([[:space:]]|$)/! s/$/ $want/
       }" "$work"
  done

  if cmp -s "$work" "$target"; then
    rm -f "$work"
    return 0
  fi

  [ -f "$target.ovs-backup" ] || cp -p "$target" "$target.ovs-backup"
  # Written through rather than moved, so the file keeps its own ownership and
  # mode.
  cat "$work" > "$target"
  rm -f "$work"
  components_changed=yes
}

ensure_components() {
  for file in /etc/apt/sources.list.d/*.sources; do
    widen_file "$file" "Components:" "$@"
  done
  widen_file /etc/apt/sources.list "deb" "$@"
}

needs_restart=no
if [ "$driver" = no ]; then
  echo "==> leaving the graphics driver alone, as asked"
elif has_nvidia_card && [ ! -e /dev/nvidiactl ]; then
  echo "==> this machine has an NVIDIA card and no driver; installing one"

  # The proprietary driver is spread across all three: the driver and its DKMS
  # module are non-free, the helpers that switch GLX over are contrib, and the
  # firmware blobs are non-free-firmware. Asked of Debian rather than assumed —
  # this file used to name two of the three and would have failed.
  ensure_components contrib non-free non-free-firmware
  if [ "$components_changed" = yes ]; then
    echo "    enabled contrib, non-free and non-free-firmware"
    apt-get update -qq || true
  fi

  # **Said before it happens, because it is about to go quiet.** DKMS compiles
  # a kernel module against the running kernel, which prints almost nothing and
  # takes minutes on a laptop. The owner watched it do that and reasonably
  # concluded the install had died — and stopping it is the one response that
  # would have broken something.
  echo ""
  echo "    This builds a kernel module with DKMS. It takes a few minutes and"
  echo "    says almost nothing for most of them, which is normal and is not a"
  echo "    hang. Watch it from another terminal if you would rather see it:"
  echo "      sudo tail -f /var/log/apt/term.log"
  echo ""

  apt_install \
    "linux-headers-$(dpkg --print-architecture)" \
    nvidia-driver \
    firmware-misc-nonfree

  echo "==> the driver is installed"
  needs_restart=yes

  if secure_boot_on; then
    echo ""
    echo "  ⚠ Secure Boot is on, so the module DKMS just built is signed with a"
    echo "    key the firmware does not trust and will not load. Enrolling one"
    echo "    needs a password you choose and an answer at the next boot:"
    echo "      sudo mokutil --import /var/lib/dkms/mok.pub"
    echo "      sudo reboot        # then: Enroll MOK, Continue, that password"
    echo ""
  fi
elif has_nvidia_card; then
  echo "==> the NVIDIA driver is already loaded"
else
  # Intel and AMD are driven by Mesa, which is in main and carries no licence
  # question. The package cannot depend on it: which one a machine needs is a
  # fact about the machine, not about this software.
  echo "==> installing the Vulkan driver for this machine's graphics"
  apt_install mesa-vulkan-drivers || true
fi

# --- is it actually up? ----------------------------------------------------------

echo "==> waiting for the server"
address=$(hostname -I 2>/dev/null | tr ' ' '\n' | grep -v '^$' | head -1 || true)
[ -n "$address" ] || address="127.0.0.1"

up=no
for _ in $(seq 1 30); do
  if curl -fsS --max-time 2 "http://127.0.0.1:7420/api/v1/server/capabilities" > /dev/null 2>&1; then
    up=yes
    break
  fi
  sleep 1
done

echo ""
if [ "$up" = yes ]; then
  echo "The server is answering."
else
  echo "The server is installed but is not answering yet. What it says:"
  echo "  journalctl -u ovsd -n 40"
  echo "  journalctl -u ovs-renderer -n 40"
fi

# Whether the pages made it onto port 80. A failure there is survivable and the
# server says so itself, but somebody who has just run an installer should be
# told rather than left to find the pages missing.
if curl -fsS --max-time 2 "http://127.0.0.1/status.json" > /dev/null 2>&1; then
  echo ""
  echo "  Set it up at   http://$address/"
else
  echo ""
  echo "  Set it up at   http://$address:7420/"
  echo "  (port 80 is not being served; something else may own it)"
fi

echo ""
echo "  The first account you create is an administrator."
echo "  Without a browser to hand:"
echo "    sudo -u ovs ovsd --add-account <name> <password>"
echo ""

# **Last, and outside the question of which port answered.** This used to sit
# inside the port 80 branch, so a machine whose pages had not come up got neither
# the restart nor the notice that it needed one — which is the machine most
# likely to need both.
if [ "$needs_restart" = yes ]; then
  echo "  The graphics driver was just installed, so this machine has to restart"
  echo "  before the renderer can drive a wall. Nouveau is still loaded until it"
  echo "  does, and the two modules cannot both have the card."
  if [ "$restart" = no ]; then
    echo ""
    echo "    sudo reboot"
    echo ""
    exit 0
  fi
  echo ""
  echo "  Restarting in $GRACE seconds. Ctrl-C now to stay up."
  sleep "$GRACE"
  systemctl reboot
fi
