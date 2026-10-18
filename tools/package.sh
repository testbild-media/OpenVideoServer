#!/usr/bin/env bash
# Builds the Debian package a server is installed from.
#
#   ./tools/package.sh                 build and package
#   ./tools/package.sh --no-build      package what is already built
#
# Run it on a Debian machine of the same release the package is for: the
# dependencies are read out of the binaries that were built there, not written
# down by hand, so a package built on trixie says what trixie's libraries are
# called.
#
# # Why the dependencies are derived
#
# `ovs-renderer` links against thirty-odd shared libraries — FFmpeg, Vulkan,
# DRM, and everything those pull in. A hand-written `Depends:` line is a list
# that is wrong the first time anything is upgraded, and wrong in the worst way:
# the package installs and the renderer will not start. `dpkg-shlibdeps` reads
# the binaries and asks dpkg which package owns each library, which is the same
# question an operator would have to answer by hand at three in the morning.
#
# # It carries its own FFmpeg
#
# Debian's FFmpeg is GPL-2.0-or-later: its copyright file says the default
# packages are built with GPL sources. Depending on it would ship an Apache-2.0
# program against GPL libraries. So the package carries the licence-clean build
# `tools/build-ffmpeg.sh --profile server` produces, in a private directory
# where it cannot shadow the machine's own.
#
# The binaries find it through an **old-style `DT_RPATH`**, not the `DT_RUNPATH`
# the linker emits by default. The difference decides whether this works:
# `RUNPATH` is consulted only for the object that declares it, so `libavcodec`
# would go looking for `libavutil` down the system path and find Debian's —
# half of one build and half of another, in a process that decodes video.
# `RPATH` is inherited by everything loaded underneath.
#
# # What is in it
#
# The two binaries, their FFmpeg, the three unit files, the Windows client
# installer if one has been put in `dist/client`, and nothing else. The database, the media store
# and the machine's configuration are created on first start and are not the
# package's to own — removing the package must never take a show with it.

set -euo pipefail

cd "$(dirname "$0")/.."

build=yes
# Before the arguments are read, so that `--ndi=` overrides the environment and
# not the other way round. It was set after the loop until 2026-10-18, which
# threw away every `--ndi=`: the release workflow fetched the runtime, handed it
# over, and every package since the runtime was meant to be carried (0.2.31 to
# 0.2.41) shipped without it, saying so in one line of a build log nobody
# reads. Found on a fresh location server whose renderer reported no NDI. Why
# there is a runtime to name at all is further down, where it is staged.
ndi_runtime="${OVS_NDI_RUNTIME:-}"
for argument in "$@"; do
  case "$argument" in
    --no-build) build=no ;;
    --ndi=*) ndi_runtime="${argument#--ndi=}" ;;
    *) echo "unknown option: $argument" >&2; exit 2 ;;
  esac
done

upstream=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

# **The revision, which is what tells two packages of one version apart.**
#
# Debian puts it after a hyphen, and `dpkg` orders by it: `0.2.0-7` is newer
# than `0.2.0-6`, so a new build is an upgrade rather than a reinstall. Before
# this, every build of 0.2.0 was `openvideoserver_0.2.0_amd64.deb`, apt
# considered the installed one already current, and two packages that were
# different software were indistinguishable by name, by apt and by eye. That
# cost two installs on 2026-10-10 and the wrong conclusion both times.
#
# `OVS_PACKAGE_REVISION` is the CI run number: monotonic, already there, and
# nobody has to remember to bump it. A build made by hand claims no number it
# did not earn — `0~<date>` sorts below every CI build, which is correct, since
# a package somebody made on their own machine should never look newer than
# one that came out of the pipeline.
revision="${OVS_PACKAGE_REVISION:-}"
if [ -z "$revision" ]; then
  revision="0~$(date -u +%Y%m%d%H%M%S)"
fi
version="${upstream}-${revision}"

architecture=$(dpkg --print-architecture)
package="openvideoserver"
staging="target/package/${package}_${version}_${architecture}"

# The FFmpeg this package ships. Built rather than found, because what makes
# it the right one is how it was configured, and a build lying around from
# something else is not evidence of that.
# **The NDI runtime, when whoever is building has one to give.**
#
# The SDK licence grants it at 2a - "a nonexclusive royalty-free license ... to
# distribute ... object code included in the SDK solely as used by such
# Products" - and the SDK documentation says where it goes: "include the NDI
# DLLs as part of your own application and keep them in your application
# folders ... Please do not install your NDI DLLs into the system path for this
# reason." This package has such a folder, with its own FFmpeg in it and an
# RPATH pointing there, and the renderer opens the library by bare name, so a
# copy in that folder is found before any the machine happens to have. Proved
# on PC05 on 2026-10-15: with one in each place, the renderer mapped this one.
#
# **It is not in this repository and must not be.** The repository is public;
# the runtime belongs in the product it is bundled with, not loose in a source
# tree. It is named at build time instead - by `--ndi=`, or `OVS_NDI_RUNTIME` -
# so a build without one simply makes the package this project made before,
# with no NDI and a line in the log saying so. Both are read at the top, before
# the arguments.

ffmpeg_dir="$PWD/build/ffmpeg-server"
if [ ! -f "$ffmpeg_dir/lib/libavcodec.so" ] && [ ! -f "$ffmpeg_dir/lib/libavcodec.so.61" ]; then
  echo "no licence-clean FFmpeg in $ffmpeg_dir; building one"
  ./tools/build-ffmpeg.sh --profile server
else
  echo "checking the FFmpeg in $ffmpeg_dir"
  ./tools/build-ffmpeg.sh --profile server --check "$ffmpeg_dir"
fi

for tool in dpkg-deb dpkg-shlibdeps fakeroot; do
  command -v "$tool" >/dev/null || {
    echo "$tool is missing. sudo apt install dpkg-dev fakeroot" >&2
    exit 1
  }
done

# Where the libraries will live on the installed machine. Private, so this
# package can never change what anything else on the machine links against.
readonly PRIVATE_LIB="/usr/lib/$package"

# **The Chromium the System screen draws with, which this package carries.**
#
# Refused rather than skipped. The package installs `ovs-console.service` and
# creates the account it runs as, so a package without the framework is a machine
# whose System screen cannot start — which is exactly what it shipped as until
# 2026-09-26. If this is missing, the answer is to get a framework, not to build a
# package that is quietly missing a third of the product.
cef_src="${CEF_PATH:-$HOME/.local/share/cef}"
if [ ! -f "$cef_src/libcef.so" ]; then
  echo "no Chromium framework in $cef_src, so this package would install a" >&2
  echo "System screen that cannot start." >&2
  echo "" >&2
  echo "  export CEF_PATH=/path/to/cef   (or put one in ~/.local/share/cef)" >&2
  echo "" >&2
  echo "crates/ovs-console/Cargo.toml says where one comes from." >&2
  exit 1
fi
export CEF_PATH="$cef_src"

if [ "$build" = yes ]; then
  echo "building against $ffmpeg_dir"
  # `--disable-new-dtags` asks for DT_RPATH rather than DT_RUNPATH; the header
  # above says why that is the whole trick.
  export PKG_CONFIG_PATH="$ffmpeg_dir/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
  export RUSTFLAGS="-C link-arg=-Wl,-rpath,$PRIVATE_LIB -C link-arg=-Wl,--disable-new-dtags${RUSTFLAGS:+ $RUSTFLAGS}"
  cargo build --release -p ovs-core --bin ovsd
  cargo build --release -p ovs-renderer --bin ovs-renderer
  # The slow one: it links a gigabyte and a half of Chromium.
  cargo build --release -p ovs-console --bin ovs-console
fi

for binary in target/release/ovsd target/release/ovs-renderer target/release/ovs-console; do
  [ -x "$binary" ] || { echo "$binary is not built. Run without --no-build." >&2; exit 1; }
done

echo "laying out $staging"
rm -rf "$staging"
install -d "$staging/DEBIAN"
install -d "$staging/usr/bin"
install -d "$staging/lib/systemd/system"
install -d "$staging/usr/share/openvideoserver/client"
install -d "$staging/usr/share/polkit-1/rules.d"
install -d "$staging$PRIVATE_LIB"
install -d "$staging/usr/share/doc/$package"

install -m 0755 target/release/ovsd "$staging/usr/bin/ovsd"
install -m 0755 target/release/ovs-renderer "$staging/usr/bin/ovs-renderer"
install -m 0644 deploy/systemd/*.service "$staging/lib/systemd/system/"

# Restarting and stopping the machine. `crates/ovs-core/src/power.rs` runs
# `systemctl reboot`, which is a D-Bus call to logind, and polkit decides whether
# an unprivileged user may make it. Without this the System screen's two buttons
# reach the server, run the command, and are refused with exit status 1 (owner,
# 2026-09-25). The rule itself says what it grants and what it deliberately does
# not. polkitd watches the directory, so nothing has to be restarted.
#
# /usr/share is where a package's rules belong, and an administrator overrides
# them from /etc/polkit-1/rules.d without this file being in their way.
install -m 0644 deploy/polkit/50-openvideoserver-power.rules \
  "$staging/usr/share/polkit-1/rules.d/"

# **The card's module options**, which are what an NVIDIA card needs before it
# will give up more than one output. A conffile, so an administrator who edits it
# keeps their edit across an upgrade — `deploy/debian/conffiles` lists it and
# `postinst` rebuilds the initramfs and says when a reboot is owed.
#
# Carried on every machine, not only one with such a card: modprobe ignores
# options for a module that is not there.
install -d "$staging/etc/modprobe.d"
install -m 0644 deploy/modprobe.d/ovs-nvidia.conf \
  "$staging/etc/modprobe.d/ovs-nvidia.conf"

# **The System screen, and the Chromium it draws with.**
#
# `ovs-console.service` names `/usr/lib/openvideoserver/cef` in both `CEF_PATH`
# and `LD_LIBRARY_PATH`, which answer different questions — where the resources
# are, and where the library is — so this has to land at exactly that path.
#
# By name, not wholesale: the distribution also holds headers, a CMake project, a
# sample program and a SUID sandbox helper a running console never opens. The same
# list `tools/deploy.sh` installs, for the same reason.
install -d "$staging$PRIVATE_LIB/cef"
install -m 0755 target/release/ovs-console "$staging/usr/bin/ovs-console"
for part in libcef.so icudtl.dat resources.pak chrome_100_percent.pak \
  chrome_200_percent.pak v8_context_snapshot.bin libEGL.so \
  libGLESv2.so libvulkan.so.1 libvk_swiftshader.so \
  vk_swiftshader_icd.json CREDITS.html; do
  [ -f "$cef_src/$part" ] && install -m 0644 "$cef_src/$part" "$staging$PRIVATE_LIB/cef/$part"
done
cp -r "$cef_src/locales" "$staging$PRIVATE_LIB/cef/"
chmod -R a+rX "$staging$PRIVATE_LIB/cef"

# **The one thing in here that is stripped, and it has to be.** `libcef.so`
# carries its debug information: 1386 MB with it, 443 MB without, which xz takes
# to about 107 MB. A package nobody can copy to a venue is a package nobody has.
# Nothing else is touched — the product's own binaries keep their symbols, because
# a backtrace from a rack at midnight is worth more than the megabytes.
if command -v strip > /dev/null; then
  before=$(du -h "$staging$PRIVATE_LIB/cef/libcef.so" | cut -f1)
  strip --strip-debug "$staging$PRIVATE_LIB/cef/libcef.so"
  echo "stripped the Chromium framework: $before -> $(du -h "$staging$PRIVATE_LIB/cef/libcef.so" | cut -f1)"
else
  echo "no strip on this machine, so this package will carry 1.4 GB of Chromium" >&2
  echo "debug information. sudo apt install binutils" >&2
fi

# Chromium's licence material beside FFmpeg's, for the same reason: shipping
# somebody else's library and not its licence is the one obligation that is
# trivial to forget. It is in the framework directory too, where the browser can
# serve it.
[ -f "$cef_src/CREDITS.html" ] && install -m 0644 "$cef_src/CREDITS.html" \
  "$staging/usr/share/doc/$package/CHROMIUM-CREDITS.html"

# The libraries themselves, and the symlinks beside them: a SONAME link is what
# the loader actually opens, and copying only the real files gives a package
# that installs and a renderer that cannot start.
cp -a "$ffmpeg_dir"/lib/lib*.so* "$staging$PRIVATE_LIB/"
chmod 0644 "$staging$PRIVATE_LIB"/lib*.so.*.* 2>/dev/null || true

# FFmpeg's own licence material, beside the package's. Shipping a library and
# not its licence is the one thing LGPL asks for that is trivial to forget.
for licence in "$ffmpeg_dir"/COPYING* "$ffmpeg_dir"/LICENSE*; do
  [ -f "$licence" ] && install -m 0644 "$licence" \
    "$staging/usr/share/doc/$package/FFMPEG-$(basename "$licence")"
done
[ -f "$ffmpeg_dir/CONFIGURATION.txt" ] && install -m 0644 \
  "$ffmpeg_dir/CONFIGURATION.txt" "$staging/usr/share/doc/$package/FFMPEG-CONFIGURATION.txt"

# The NDI runtime, under the name the renderer opens. `readlink -f` because
# what is named may be the SDK's symlink, and a symlink into a directory the
# package does not carry would be a library that is not there.
if [ -n "$ndi_runtime" ]; then
  ndi_file=$(readlink -f "$ndi_runtime" 2>/dev/null || true)
  if [ ! -f "$ndi_file" ]; then
    echo "--ndi names $ndi_runtime, which is not a file" >&2
    exit 1
  fi
  # The soname it will be opened by, read from the library rather than assumed:
  # a future SDK with a different one would otherwise be staged under a name
  # nothing looks for, and NDI would go quiet with the file sitting right there.
  ndi_soname=$(objdump -p "$ndi_file" 2>/dev/null | awk '/SONAME/ { print $2 }')
  : "${ndi_soname:=libndi.so.6}"
  install -m 0644 "$ndi_file" "$staging$PRIVATE_LIB/$ndi_soname"
  echo "carrying the NDI runtime as $ndi_soname ($(du -h "$ndi_file" | cut -f1))"

  # Its licence material, beside FFmpeg's and Chromium's, for the same reason:
  # shipping somebody else's library and not its licence is the one obligation
  # that is trivial to forget. 3d of the SDK licence requires the copyright
  # notice to travel with it, and the components inside the runtime have their
  # own licences to carry.
  #
  # Looked for near the library rather than at a fixed place, because what
  # names it may be an unpacked SDK, a folder a release workflow made, or one
  # file somebody pointed at. Found none is said out loud: a package that
  # carries the runtime and not its licences is one nobody should hand on, and
  # silence is how that happens.
  ndi_licences=0
  for licence in "$(dirname "$ndi_file")"/*licen* \
                 "$(dirname "$ndi_file")"/../*licen* \
                 "$(dirname "$ndi_file")"/../../licenses/* \
                 "$(dirname "$ndi_file")"/../../*License*; do
    if [ -f "$licence" ]; then
      install -m 0644 "$licence" \
        "$staging/usr/share/doc/$package/NDI-$(basename "$licence")"
      ndi_licences=$(( ndi_licences + 1 ))
    fi
  done
  if [ "$ndi_licences" -eq 0 ]; then
    echo "" >&2
    echo "the NDI runtime is being carried and no licence text was found near" >&2
    echo "$ndi_file. The SDK licence requires its notices to travel with it." >&2
    echo "Put libndi_licenses.txt beside the library and build again." >&2
    exit 1
  fi
  echo "and $ndi_licences NDI licence file(s) beside it"
else
  echo "no NDI runtime named; this package will carry none"
fi

# It has to be the build that was just staged, not the machine's. Checked here
# rather than trusted, because the failure is silent: a binary that found
# Debian's libavcodec works perfectly and is the wrong licence.
for binary in ovsd ovs-renderer; do
  if ! readelf -d "$staging/usr/bin/$binary" | grep -q "(RPATH).*$PRIVATE_LIB"; then
    echo "$binary has no RPATH pointing at $PRIVATE_LIB; it would link the machine's FFmpeg" >&2
    exit 1
  fi
done
install -m 0644 LICENSE "$staging/usr/share/doc/$package/copyright" 2>/dev/null || true
# **And NOTICE, which was not carried at all until 2026-10-15.** It is the file
# that says what the carried FFmpeg obliges anybody redistributing this, and
# since the package may now carry the NDI runtime it is also where 3d of that
# licence's terms live. A package that leaves it behind is a package whose
# obligations travel only in a source tree nobody installing it has.
install -m 0644 NOTICE "$staging/usr/share/doc/$package/NOTICE" 2>/dev/null || true
install -m 0644 deploy/README.md "$staging/usr/share/doc/$package/README.md"

# The client, if somebody has put one here. The owner asked for it in the
# package (2026-09-24), and a server with no installer beside it simply offers
# no download rather than promising one it has not got.
if compgen -G "dist/client/*" > /dev/null; then
  install -m 0644 dist/client/* "$staging/usr/share/openvideoserver/client/"
  echo "including $(ls dist/client | wc -l) client file(s)"
else
  echo "no client in dist/client; the package will carry none"
fi

# What the binaries actually need, asked of dpkg rather than guessed.
echo "working out the dependencies"
mkdir -p "$staging/DEBIAN"
cat > "$staging/DEBIAN/control" <<CONTROL
Package: $package
Version: $version
Architecture: $architecture
Maintainer: OpenVideoServer <noreply@openvideoserver.invalid>
Section: video
Priority: optional
Homepage: https://github.com/testbild-media/OpenVideoServer
Description: Media server for live video shows
 Runs one show at a time on a machine with real outputs: a layer and slot
 matrix driven from a client over the network or from a lighting console over
 Art-Net or sACN.
 .
 This package carries the core and the renderer as two services, so restarting
 or upgrading one does not interrupt the other.
CONTROL

# `dpkg-shlibdeps` wants a debian/control beside it, and writes what it finds
# into debian/substvars. Both live in a scratch directory that is thrown away.
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/debian"
cat > "$scratch/debian/control" <<'SHLIBS'
Source: openvideoserver
Package: openvideoserver
Architecture: any
SHLIBS
# Asked twice, because these are two different questions.
#
# **What do the binaries need?** With `-l` pointing at the libraries this
# package carries, so they resolve here instead of against a Debian package —
# the whole reason for shipping them is not to depend on Debian's. If FFmpeg
# still appears in this answer, something linked the machine's copy, and the
# guard below refuses to build such a package.
# **Kept, not discarded.** This used to end `2>/dev/null`, and when the answer
# came back empty there was nothing at all to say why — which is exactly what a
# container without Chromium's libraries produces, and exactly how one silent
# exit cost two CI runs to explain.
( cd "$scratch" && dpkg-shlibdeps -O --ignore-missing-info \
    -l"$OLDPWD/$staging$PRIVATE_LIB" -l"$OLDPWD/$staging$PRIVATE_LIB/cef" \
    "$OLDPWD/$staging/usr/bin/ovsd" "$OLDPWD/$staging/usr/bin/ovs-renderer" \
    "$OLDPWD/$staging/usr/bin/ovs-console" \
    2>"$scratch/binaries.log" ) > "$scratch/depends" || true

shlibs=$(sed -n 's/^shlibs:Depends=//p' "$scratch/depends" | head -1)

# **What do the libraries we ship need?** Their FFmpeg dependencies are each
# other — satisfied inside the package, and proved by the RPATH check above —
# so those are dropped. Everything else is real and nothing else would find it:
# this build's `libavcodec` needs `libopenh264.so.8`, the software encoder this
# licence change was made for, and a package that did not say so would install
# onto a machine where the renderer cannot load libavcodec at all.
#
# A second invocation rather than more arguments to the first, because
# `dpkg-shlibdeps` resolves a *library* target's own needs down the system path
# whatever `-l` says. One run would report Debian's libavcodec on libavformat's
# behalf, and the guard could not tell that from the fault it is there to catch.
carried=()
for library in "$staging$PRIVATE_LIB"/lib*.so.*.*; do
  [ -f "$library" ] && carried+=("$OLDPWD/$library")
done
# **And Chromium, which is the reason this matters most.** `ovs-console` names
# only `libcef.so`; everything the browser actually needs — X11, NSS, ATK, cups,
# gbm, pango, alsa, thirty-two libraries of it — is named by `libcef.so` and by
# nothing this package builds. A package that did not ask would install onto a
# minimal Debian and leave a System screen that cannot load its own library.
[ -f "$staging$PRIVATE_LIB/cef/libcef.so" ] \
  && carried+=("$OLDPWD/$staging$PRIVATE_LIB/cef/libcef.so")

# **And NDI, for the same reason, more quietly.** It is opened at run time and
# never linked, so `dpkg-shlibdeps` would not hear about it from the binaries.
# A dependency of its own that the machine lacks makes the `dlopen` fail, and a
# failed `dlopen` here is one INFO line saying there is no NDI on this machine -
# which is indistinguishable from a machine that simply has no cameras.
for library in "$staging$PRIVATE_LIB"/libndi.so.*; do
  [ -f "$library" ] && carried+=("$OLDPWD/$library")
done

if [ ${#carried[@]} -gt 0 ]; then
  ( cd "$scratch" && dpkg-shlibdeps -O --ignore-missing-info \
      -l"$OLDPWD/$staging$PRIVATE_LIB" -l"$OLDPWD/$staging$PRIVATE_LIB/cef" \
      "${carried[@]}" \
      2>"$scratch/carried.log" ) > "$scratch/carried-depends" || true

  carried_shlibs=$(sed -n 's/^shlibs:Depends=//p' "$scratch/carried-depends" | head -1)
  # `|| true` on the grep, because it answers 1 when it selects nothing — and
  # with `pipefail` that ends this script. It did, with no message: the whole
  # of the first container run's output was "working out the dependencies".
  carried_shlibs=$(printf '%s' "$carried_shlibs" | tr ',' '\n' | sed 's/^ *//' \
    | { grep -vE '^(libav[a-z]+[0-9]*|libsw[a-z]+[0-9]*) ' || true; } \
    | paste -sd ',' - | sed 's/,/, /g')

  # **A Chromium with no dependencies is not a package worth building.**
  # `ovs-console` names only `libcef.so`; everything the browser needs is named
  # by `libcef.so` and by nothing else here. If this came back empty, the
  # libraries it needs are not installed on *this* machine, so `dpkg-shlibdeps`
  # could not resolve them — and the package would install a System screen onto
  # a minimal Debian and leave it unable to load its own library.
  if [ -f "$staging$PRIVATE_LIB/cef/libcef.so" ] && [ -z "$carried_shlibs" ]; then
    echo "" >&2
    echo "this package ships Chromium and dpkg-shlibdeps named nothing for it." >&2
    echo "the libraries libcef.so needs are not installed here, so the package" >&2
    echo "would declare none of them and the System screen could not start." >&2
    echo "" >&2
    if [ -s "$scratch/carried.log" ]; then
      echo "what dpkg-shlibdeps said:" >&2
      sed 's/^/  /' "$scratch/carried.log" >&2
      echo "" >&2
    fi
    echo "install what it names and build again." >&2
    exit 1
  fi

  if [ -n "$carried_shlibs" ]; then
    shlibs="${shlibs:+$shlibs, }$carried_shlibs"
  fi
fi

# Said once each, sorted. The same package can appear twice with different
# minimums, one from each run — `libc6 (>= 2.35)` beside `libc6 (>= 2.39)`.
# That is valid and apt takes the stronger, and collapsing it by hand was tried
# and withdrawn: it is a cosmetic gain, and the shell it takes broke the build
# twice before it worked once.
shlibs=$(printf '%s' "$shlibs" | tr ',' '\n' | sed 's/^ *//' \
  | { grep -v '^$' || true; } | sort -u | paste -sd ',' - | sed 's/,/, /g')

if [ -z "$shlibs" ]; then
  # This could not be reached before: the pipeline above ended the script on
  # the empty input it exists to report on.
  echo "dpkg-shlibdeps found nothing; the package will declare no library dependencies" >&2
  if [ -s "$scratch/binaries.log" ]; then
    echo "what it said:" >&2
    sed 's/^/  /' "$scratch/binaries.log" >&2
  fi
fi

# `adduser` for the system accounts the units run as; `systemd` because the
# services are units.
#
# `libvulkan1` and `polkitd` **cannot be derived** and are the two entries
# written by hand. Neither is linked: the renderer opens Vulkan at runtime by
# name, and polkit is reached over D-Bus by `systemd-logind` on this server's
# behalf. `dpkg-shlibdeps` reads binaries, so it can see neither.
#
# Without `libvulkan1` the package installs happily on a machine with no Vulkan
# and the wall stays black, which is the worst shape a missing dependency can
# take: it looks like a graphics fault.
#
# Without `polkitd` the rule this package installs into
# `/usr/share/polkit-1/rules.d` has nothing to read it, and logind refuses a
# caller with no session outright — so the System screen's Restart and Shut down
# answer "Call to Reboot failed: Access denied". Found on a clean Debian 13 on
# 2026-09-27; the old reference machine had polkitd only because something else
# had dragged it in, which is how this went unnoticed.
#
# `avahi-daemon` is the third, and it is the same trap one level further out.
# `libndi` is linked against `libavahi-client3`, so that *is* derived and was
# present — but the client library is only a way of talking to a daemon over
# D-Bus, and the daemon is not a library. On the show machine on 2026-10-16 the
# package was never installed at all, and NDI discovery was dead in both
# directions: the renderer could not see its own published output, and nothing
# on the network could see it either. Everything else about the machine looked
# perfect, which is why it took an `ldd` of `libndi.so` to find. Three seconds
# after `avahi-daemon` started, the running renderer — unrestarted — listed two
# sources.
#
# A hard dependency rather than a recommendation: unlike a Vulkan driver, which
# one is right is not a fact about the machine. It is the same package
# everywhere, and without it an NDI output is a stream nobody can find.
#
# The rest is what the binaries link against, read out of them.
depends="adduser, systemd, libvulkan1, polkitd, avahi-daemon"

# Debian's FFmpeg must not appear. If it does, something linked against the
# machine's copy and the package would carry a GPL dependency it was built
# specifically to avoid.
if printf '%s' "$shlibs" | grep -qE "libav(codec|format|util)|libswscale"; then
  echo "the binaries depend on Debian's FFmpeg, which is GPL: refusing to package" >&2
  echo "  $shlibs" >&2
  exit 1
fi
[ -n "$shlibs" ] && depends="$depends, $shlibs"
printf 'Depends: %s\n' "$depends" >> "$staging/DEBIAN/control"

# A driver for Vulkan to talk to. Recommended rather than required because
# which one is right is a fact about the machine: `mesa-vulkan-drivers` covers
# Intel and AMD, and an NVIDIA card brings its own with the proprietary driver,
# which is not something a package can pull in.
#
# **Nothing is suggested for the System screen any more.** It used to want
# `cage`, `chromium` and `seatd`, because it was a kiosk compositor and a
# compositor takes a card and a seat. It is a windowless browser now: it paints
# the page into memory and the renderer shows it, so it opens no device and needs
# nothing installed. The Chromium it draws with ships inside this package.
cat >> "$staging/DEBIAN/control" <<'EXTRA'
Recommends: mesa-vulkan-drivers
EXTRA

install -m 0755 deploy/debian/postinst "$staging/DEBIAN/postinst"
install -m 0755 deploy/debian/prerm "$staging/DEBIAN/prerm"
install -m 0755 deploy/debian/postrm "$staging/DEBIAN/postrm"
install -m 0644 deploy/debian/conffiles "$staging/DEBIAN/conffiles" 2>/dev/null || true

echo "building the package"
output="target/package/${package}_${version}_${architecture}.deb"
fakeroot dpkg-deb --build "$staging" "$output" > /dev/null

# The checksum, beside it, under the name the installer looks for. A download
# that arrived wrong is the one failure a package manager cannot catch, and a
# server built from a truncated file fails later and stranger.
( cd "$(dirname "$output")" && sha256sum "$(basename "$output")" > "$(basename "$output").sha256" )

echo
echo "built $output"
echo "       $output.sha256"
dpkg-deb --info "$output" | sed -n 's/^ /  /p' | head -12
echo
echo "install it with:"
echo "  sudo apt install ./$output"
