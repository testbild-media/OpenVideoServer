#!/usr/bin/env bash
# Builds the FFmpeg the desktop client ships, without the GPL parts.
#
#   ./tools/build-ffmpeg.sh                  build into clients/desktop/vendor/ffmpeg
#   ./tools/build-ffmpeg.sh --out DIR        somewhere else
#   ./tools/build-ffmpeg.sh --check DIR      only verify what is already there
#   ./tools/build-ffmpeg.sh --profile NAME   client (default), openh264, server
#
# Run it on Linux. It downloads a pinned FFmpeg release, checks its hash,
# configures it with nothing GPL in it, builds `ffmpeg` and `ffprobe`, records
# exactly what it built, and refuses to leave binaries behind that do not match
# what was asked for.
#
# # Why this exists
#
# The client used to carry `ffmpeg-static` and `@ffprobe-installer/ffprobe`.
# Both are configured `--enable-gpl --enable-version3`, so they are
# GPL-3.0-or-later whatever a package's metadata says — one of them declares
# LGPL-2.1 and ships a GPL binary, which was checked rather than assumed.
# Handing somebody the client therefore meant handing them a GPL-3 program and
# owing them the corresponding source for it.
#
# The owner's decision (owner decision Q18) is to build our own instead: the
# functionality OVS needs is all LGPL, so the obligation can simply not be
# incurred. This is that build.
#
# # What this settles, and what it does not
#
# This is about **copyright**: whose source is in the binary and what that
# obliges you to hand over. It says nothing whatever about **patents**. H.264,
# HEVC and AV1 are patented independently of how their encoders are licensed,
# and "no GPL components" is not "free to distribute anywhere". The codec set a
# public release is configured with is a release and legal decision, to be
# reviewed before any external distribution. Development builds keep
# the broad codec support the product is designed around.
#
# # The server, which used to be the gap here
#
# The server links FFmpeg rather than running it — `ovs-media` and
# `ovs-renderer` reach `libavcodec` through `ffmpeg-next` — and linking is the
# closer relationship of the two, so it was always the larger exposure. On
# Debian that meant a GPL build: Debian's own copyright file says the default
# packages are built with GPL sources.
#
# `--profile server` closes it, and as of the owner's decision on 2026-09-24
# the Debian package ships what it produces rather than depending on Debian's.
# The measurement that decision was made on, taken on the reference machine:
#
#   decoders   all fifteen the product asks about, present in both builds
#   preview    `h264_nvenc` unchanged, 0.64 → 0.63 cores
#              the software fall-back changes encoder entirely, `libx264` →
#              `libopenh264`, and gets *cheaper*: 2.51 → 0.99 cores, for more
#              bitrate at the same target — which is the right trade for a
#              preview, where the processor is scarce and the LAN is not
#   decode     hardware identical; ProRes in software 0.917 → 0.937 cores,
#              a real 2.2% rather than noise — three runs each, ranges apart
#   absent     `hevc_nvenc` and `mjpeg_cuvid`; the server encodes no HEVC, and
#              `media/video.rs` documents its decision not to use `mjpeg_cuvid`
#
# So nothing the server does was lost and its worst case got better.
#
# **`libopenh264` in place of `libx264`** is what made that possible, and
# `ovs-renderer`'s `ENCODERS` already preferred it: hardware first, then
# openh264, then x264. On a build made here the third name is simply never
# found.
#
# # What this still does NOT cover
#
# Patents. See the codec note above: "no GPL components" answers
# a question about copyright and says nothing at all about H.264, HEVC or AV1
# patent licensing, which is a release decision and a legal one.
#
# # The other two profiles
#
# `--profile openh264` is the client build with `libopenh264` added: BSD, so
# still not GPL, and Constrained Baseline only. It exists to be measured
# against `libx264` — `tools/measure-h264-encoders.sh` builds it and uses it —
# and it is not shipped with anything.
#
# `--profile server` is what the server links. Shared libraries rather than
# executables, because `ovs-media` and `ovs-renderer` link `libavcodec` through
# `ffmpeg-next`; `libopenh264` in place of `libx264` for the preview encoder,
# which is the whole reason this profile exists; and the NVIDIA decode and
# encode paths, which the server now depends on — seven layers of HEVC cost
# seven cores on the processor and one on the card.
#
# NVENC and NVDEC do not make a build non-free. FFmpeg dropped that requirement
# in 4.0: the headers are MIT, the driver is loaded at runtime rather than
# linked, and `--enable-nonfree` is only needed for things like `libnpp`, which
# is not here.
#
# # Reproducible means pinned, checked and recorded
#
# A build nobody can repeat is not evidence of anything. So: one version, one
# hash checked before a line of it is read, one configure line in the source
# rather than in somebody's shell history, and a manifest written beside the
# binaries saying what they are. `--check` re-runs the verification alone, which
# is what a release pipeline calls.

set -euo pipefail
cd "$(dirname "$0")/.."

# The version this product is built and tested against. `ffmpeg-next` 7.1 binds
# the 7.1 ABI, and the test machine's Debian carries 7.1.x, so the client and
# the server read files with the same code.
readonly FFMPEG_VERSION=7.1.1
readonly FFMPEG_SHA256=733984395e0dbbe5c046abda2dc49a5544e7e0e1e2366bba849222ae9e3a03b1

# The NVIDIA codec headers, for `--profile server`. MIT, header-only, and not
# the driver: they are what lets FFmpeg *call* a driver it loads at runtime,
# which is why NVENC and NVDEC do not make a build non-free.
#
# FFmpeg 7.1 wants at least 12.1.14.0. Debian does not package these at all.
#
# The hash is of the tarball this script downloaded on 2026-09-22, recorded so
# that every build afterwards gets the same bytes. Upstream publishes no
# signature or checksum of its own to check it against, and saying so is better
# than implying there is one.
readonly NVCODEC_VERSION=n12.2.72.0
readonly NVCODEC_SHA256=dbeaec433d93b850714760282f1d0992b1254fc3b5a6cb7d76fc1340a1e47563

out=""
check_only=""
profile="client"

while [ $# -gt 0 ]; do
  case "$1" in
    --out) out="${2:?--out wants a directory}"; shift 2 ;;
    --check) check_only=yes; out="${2:?--check wants a directory}"; shift 2 ;;
    --profile) profile="${2:?--profile wants client or openh264}"; shift 2 ;;
    -h|--help) sed -n '2,50p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done

case "$profile" in
  client)
    : "${out:=clients/desktop/vendor/ffmpeg}"
    ;;
  openh264)
    # Not under clients/: this one is never shipped with anything.
    : "${out:=build/ffmpeg-openh264}"
    ;;
  server)
    : "${out:=build/ffmpeg-server}"
    ;;
  *)
    echo "unknown profile $profile: client, openh264 or server" >&2
    exit 2
    ;;
esac

# --- what is refused, and what is required ----------------------------------
#
# The check is on the *binary*, not on this script: what matters is what was
# produced, and a configure line that was edited after the fact would still pass
# a check of the source. `ffmpeg -version` prints the configuration it was
# actually built with.

# Any of these in the configuration means a GPL binary.
readonly FORBIDDEN='--enable-gpl|--enable-version3|--enable-nonfree'

# What an executable is called here.
#
# The `client` profile is the Windows one — `--enable-mediafoundation`,
# `hevc_qsv`, `hevc_amf` — and every path in this file spelled the binaries
# without a suffix, which is what they are called everywhere except the
# platform that profile exists for. So `--check` on a Windows build reported
# both binaries missing, and the build could not have been run for the target
# it was written for. Read from the shell rather than passed in: the same
# script has to do the right thing under MSYS2 and under Debian.
case "${OSTYPE:-$(uname -s)}" in
  msys* | cygwin* | win32 | MINGW* | MSYS* | CYGWIN*) EXE=.exe ;;
  *) EXE= ;;
esac
readonly EXE

# Decoders the ingest pipeline has to be able to open. Short on purpose: this is
# what an operator brings to a show, not everything FFmpeg can read.
#
# `--disable-autodetect` turns off every external library including zlib, and
# the PNG decoder needs zlib: the first real build of this script produced an
# ffmpeg with no `png` in it, configured cleanly, and said nothing. Hence
# `--enable-zlib` below, and hence the check being against this whole list
# rather than against a few of its names.
readonly DECODERS="h264,hevc,av1,vp9,vp8,prores,hap,dnxhd,cfhd,mpeg2video,mpeg1video,mpeg4,vc1,mjpeg,jpeg2000,png,apng,bmp,gif,tiff,webp,rawvideo,ffv1,huffyuv,utvideo,qtrle,theora,wmv3"

# What the client encodes, which is exactly what the server takes: HEVC 8-bit,
# HEVC 10-bit and HAP (as the owner narrowed it, 2026-09-23).
#
# **No `libx265`.** The owner's decision of the same day, and it is two
# decisions in one: libx265 is GPL, which would take the whole binary with it,
# and a software HEVC encode of a show's worth of material is not something
# anybody waits for. HEVC is produced on hardware or it is not produced here.
#
# `hevc_nvenc` and `hevc_mf` are the two backends, and they are backends rather
# than choices an operator makes — `clients/desktop/src/shared/encoder.ts` picks
# between them and never shows either. `hevc_qsv` and `hevc_amf` are built in
# too: they cost nothing to include, they are the next two backends, and a
# machine that has one is a machine that would otherwise fall back to Media
# Foundation.
#
# `hap` needs Snappy, which is the whole reason HAP is worth having: a HAP frame
# is a texture the card samples as it is, and Snappy is the difference between
# that texture being three megabytes on disc and eight. Snappy is BSD.
#
# `prores_ks` stays. Nothing produces ProRes for ingest any more, but the
# encoder is FFmpeg's own, carries no third-party licence, and is what somebody
# asked to hand a file to another system reaches for.
# `libkvazaar` is the one that is always there.
#
# **Hardware where there is any, software always** (owner, 2026-10-13: "Software
# Weg muss immer vorbereitet sein. Es koennte auf low budget Laptops laufen der
# Client", and "Hardware Transcoding waere dann natuerlich immer der gewuenschte
# weg, aber Software Transcoding muss als fallback immer gehen"). Every encoder
# above needs a card, a driver or a Media Foundation transform; a laptop with
# none of them could prepare nothing at all, and since the server takes HEVC and
# HAP and nothing else, that laptop could upload nothing at all.
#
# Kvazaar rather than x265: x265 is GPL-2.0-or-later and cannot go inside a
# client this repository publishes under Apache-2.0, and Kvazaar is
# BSD-3-Clause. It is slower than x265 and far slower than any card, which is
# why `clients/desktop/src/shared/encoder.ts` puts it last in its list and
# reaches for it only when nothing else answered.
encoders="hevc_nvenc,hevc_mf,hevc_qsv,hevc_amf,libkvazaar,hap,prores_ks"
configure_extra="--enable-libsnappy --enable-libkvazaar"
configure_extra="$configure_extra --enable-ffnvcodec --enable-nvenc"
configure_extra="$configure_extra --enable-mediafoundation"
configure_extra="$configure_extra --enable-libvpl --enable-amf"
case "$profile" in
  openh264)
    encoders="$encoders,libopenh264"
    configure_extra="--enable-libopenh264"
    ;;
  server)
    # What the server encodes, and nothing else: H.264 for a preview, on the
    # card where there is one and in software where there is not. No ProRes —
    # that is the client's, and a server that encoded it would be a server
    # writing files, which it does not do.
    encoders="libopenh264,h264_nvenc"
    # Shared libraries, because the server links these rather than running
    # them. The NVIDIA paths for both directions: `*_cuvid` decoders are what
    # `ovs-renderer` opens for H.264, HEVC and AV1, and `h264_nvenc` is the
    # preview encoder it prefers.
    configure_extra="--enable-libopenh264"
    configure_extra="$configure_extra --enable-ffnvcodec --enable-nvenc --enable-nvdec --enable-cuvid"
    ;;
esac
readonly encoders configure_extra

# The decoders on the card, which are named separately from the software ones
# because they are a different list with the same names in it.
readonly CUVID_DECODERS="h264_cuvid,hevc_cuvid,av1_cuvid,vp9_cuvid,vp8_cuvid,mpeg1_cuvid,mpeg2_cuvid,mpeg4_cuvid,vc1_cuvid"

readonly DEMUXERS="mov,matroska,mpegts,mpegvideo,avi,image2,image2pipe,png_pipe,mjpeg,rawvideo,gif,webp_pipe"
# **`mp4` as well as `mov`**, which is one muxer twice: the two are the same
# code in FFmpeg and the extension decides which name it writes under. Without
# it `ffmpeg out.mp4` answers "Unable to choose an output format", and the
# client names every HEVC file it prepares `.mp4` -- so a build without it is a
# client that cannot prepare anything at all, which is what shipped
# (owner, 2026-10-13, at a venue: a file that would not go in).
readonly MUXERS="mov,mp4,null,rawvideo,image2"
readonly PARSERS="h264,hevc,av1,vp9,vp8,mpegvideo,mpeg4video,vc1,mjpeg,png,dnxhd"
readonly FILTERS="scale,format,null,copy,setpts,fps"
readonly PROTOCOLS="file,pipe"

# --- verifying, which is also what --check does on its own ------------------

verify() {
  local where="$1" failures=0

  # A shared build's own libraries, not the machine's. Without this the binary
  # runs against whatever `libavcodec.so` the loader finds first — which on a
  # Debian machine is Debian's, and every check below then passes or fails on
  # the wrong build. It happened: the first server build reported "no
  # libopenh264 encoder" while listing every encoder Debian ships.
  local run=("$where/ffmpeg$EXE")
  if [ -d "$where/lib" ]; then
    run=(env "LD_LIBRARY_PATH=$where/lib" "$where/ffmpeg$EXE")
  fi

  # A server build is libraries. The binaries are built too, because they are
  # how the configuration is read back and how somebody diagnoses a clip, but
  # what the server links is what has to be there.
  if [ "$profile" = server ]; then
    for library in libavcodec libavformat libavutil libswscale; do
      if ! ls "$where/lib/$library.so."* > /dev/null 2>&1; then
        echo "  MISSING $library.so" >&2
        failures=$((failures + 1))
      fi
    done
    if [ ! -f "$where/lib/pkgconfig/libavcodec.pc" ]; then
      echo "  MISSING the pkg-config files a build needs to find these" >&2
      failures=$((failures + 1))
    fi
  fi

  for binary in "ffmpeg$EXE" "ffprobe$EXE"; do
    if [ ! -x "$where/$binary" ]; then
      echo "  MISSING $binary" >&2
      failures=$((failures + 1))
      continue
    fi

    local reported
    if [ -d "$where/lib" ]; then
      reported=$(env "LD_LIBRARY_PATH=$where/lib" "$where/$binary" -hide_banner -version 2>/dev/null | tr ' ' '\n')
    else
      reported=$("$where/$binary" -hide_banner -version 2>/dev/null | tr ' ' '\n')
    fi

    local version
    version=$(printf '%s\n' "$reported" | sed -n "s/^n\{0,1\}\($FFMPEG_VERSION\)\$/\1/p" | head -1)
    if [ -z "$version" ]; then
      echo "  $binary is not $FFMPEG_VERSION" >&2
      failures=$((failures + 1))
    fi

    local gpl
    gpl=$(printf '%s\n' "$reported" | grep -E "^($FORBIDDEN)\$" || true)
    if [ -n "$gpl" ]; then
      echo "  $binary was built with $(printf '%s' "$gpl" | tr '\n' ' ')" >&2
      failures=$((failures + 1))
    fi
  done

  # **Nothing it cannot find on a machine that has only the installer.**
  #
  # A binary that needs a DLL beside it works on every machine it was built on
  # and fails on every machine it is installed on, which is a fault no care at
  # the build end can catch. It failed exactly that way once (owner, 2026-10-10)
  # and the process said nothing at all, because a process that will not start
  # has no stderr to say it with.
  #
  # The rule is a name test rather than a list of Windows' own DLLs: a system
  # DLL is `KERNEL32.dll` or `ole32.dll`, and the things that go missing are
  # `libstdc++-6.dll`, `libwinpthread-1.dll`, `zlib1.dll` — ports, carrying the
  # names ports have.
  if [ -n "$EXE" ] && ! command -v objdump > /dev/null; then
    # Said rather than skipped. The thing this catches cannot be seen on the
    # machine that builds it, so a check that quietly did not run would leave
    # exactly the impression that it had.
    echo "  no objdump, so what these binaries need could not be checked" >&2
    echo "    (mingw-w64-x86_64-toolchain carries it; install it and build again)" >&2
    failures=$((failures + 1))
  elif [ -n "$EXE" ]; then
    for binary in "ffmpeg$EXE" "ffprobe$EXE"; do
      [ -x "$where/$binary" ] || continue
      local carried
      carried=$(objdump -p "$where/$binary" 2>/dev/null \
        | sed -n 's/^[[:space:]]*DLL Name:[[:space:]]*//p' \
        | grep -iE '^(lib|zlib|msys-|avcodec|avformat|avutil|swscale|swresample)' \
        || true)
      if [ -n "$carried" ]; then
        echo "  $binary needs libraries it does not ship:" >&2
        printf '    %s\n' $carried >&2
        echo "    (build with --extra-ldflags=-static)" >&2
        failures=$((failures + 1))
      fi
    done
  fi

  # The licence material the LGPL requires to travel with the binaries, and
  # the BSD notices of what is linked into them.
  for file in COPYING.LGPLv2.1 CONFIGURATION.txt THIRD-PARTY.txt; do
    if [ ! -s "$where/$file" ]; then
      echo "  MISSING $file" >&2
      failures=$((failures + 1))
    fi
  done

  # And that the decoders an operator's files need are really in there — all of
  # them, rather than a sample. A build that configured cleanly and dropped a
  # codec would pass every check above and fail at the first import, which is
  # exactly what happened the first time this script was run for real.
  if [ -x "$where/ffmpeg$EXE" ]; then
    local available
    available=$("${run[@]}" -hide_banner -decoders 2>/dev/null | awk '{print $2}')
    local wanted
    for wanted in $(printf '%s' "$DECODERS" | tr ',' ' '); do
      if ! printf '%s\n' "$available" | grep -qx "$wanted"; then
        echo "  no $wanted decoder" >&2
        failures=$((failures + 1))
      fi
    done
  fi

  # And that it can write the containers the client asks it for.
  #
  # The decoders are checked above and the encoders below, and between them sat
  # the gap this is here for: a build with every encoder and no `mp4` muxer
  # passes both and cannot write a single file the client asks for. It shipped
  # that way, and the first thing anybody saw was an import that would not
  # start.
  if [ -x "$where/ffmpeg$EXE" ]; then
    local writes
    writes=$("${run[@]}" -hide_banner -muxers 2>/dev/null | awk '{print $2}')
    local container
    for container in $(printf '%s' "$MUXERS" | tr ',' ' '); do
      if ! printf '%s\n' "$writes" | grep -qx "$container"; then
        echo "  no $container muxer" >&2
        failures=$((failures + 1))
      fi
    done
  fi

  # The client's own encoders, which are the whole reason this profile exists.
  #
  # Checked against the binary rather than against the configure line, because
  # the two disagree more often than anybody expects: a missing Snappy leaves
  # `hap` out with nothing said, and a toolchain without the Media Foundation
  # headers drops `hevc_mf` the same way. A client that shipped without them
  # would refuse every file an operator dropped on it and say it could not
  # prepare them, which is a true sentence about the wrong problem.
  #
  # `hevc_qsv` and `hevc_amf` are not checked: they need vendor SDKs that a
  # build machine may not have, they are spare backends rather than the path,
  # and a build without them is a client that works.
  if [ "$profile" = client ] && [ -x "$where/ffmpeg$EXE" ]; then
    local produced
    produced=$("${run[@]}" -hide_banner -encoders 2>/dev/null | awk '{print $2}')
    local needed
    # `libkvazaar` is in this list for the same reason `hap` is: it is a path
    # the client depends on rather than a spare. A build that quietly lost it
    # is a build that cannot prepare anything on a machine with no hardware
    # encoder, which is the machine it exists for.
    for needed in hevc_nvenc hevc_mf hap libkvazaar; do
      if ! printf '%s\n' "$produced" | grep -qx "$needed"; then
        echo "  no $needed encoder" >&2
        failures=$((failures + 1))
      fi
    done
  fi

  # The profile's own encoder, for the same reason: a build that quietly lost
  # it would be measured as though openh264 were unavailable rather than
  # unbuilt.
  if [ "$profile" != client ] && [ -x "$where/ffmpeg$EXE" ]; then
    if ! "${run[@]}" -hide_banner -encoders 2>/dev/null |
      awk '{print $2}' | grep -qx libopenh264; then
      echo "  no libopenh264 encoder" >&2
      failures=$((failures + 1))
    fi
  fi

  # And the card's paths, which are the other half of why a server build
  # exists. A server that fell back to software decoding because a configure
  # flag went missing would run, and would run out of cores at four layers.
  if [ "$profile" = server ] && [ -x "$where/ffmpeg$EXE" ]; then
    local available
    available=$("${run[@]}" -hide_banner -decoders 2>/dev/null | awk '{print $2}')
    local wanted
    for wanted in $(printf '%s' "$CUVID_DECODERS" | tr ',' ' '); do
      if ! printf '%s\n' "$available" | grep -qx "$wanted"; then
        echo "  no $wanted decoder" >&2
        failures=$((failures + 1))
      fi
    done
    if ! "${run[@]}" -hide_banner -encoders 2>/dev/null |
      awk '{print $2}' | grep -qx h264_nvenc; then
      echo "  no h264_nvenc encoder" >&2
      failures=$((failures + 1))
    fi
  fi

  if [ "$failures" -gt 0 ]; then
    echo "$where: $failures problem(s)" >&2
    return 1
  fi
  echo "$where: FFmpeg $FFMPEG_VERSION ($profile), no GPL components, licence material present"
}

if [ -n "$check_only" ]; then
  verify "$out"
  exit $?
fi

# --- building ---------------------------------------------------------------

for needed in curl tar make gcc pkg-config; do
  command -v "$needed" >/dev/null || { echo "$needed is not installed" >&2; exit 1; }
done

# Without an assembler FFmpeg builds and decodes several times slower, which is
# not something to ship and not something to measure against either. Refused
# rather than quietly worked around.
if ! command -v nasm >/dev/null && ! command -v yasm >/dev/null; then
  echo "nasm (or yasm) is not installed, and a build without it is several" >&2
  echo "times slower to decode. Install one, or pass --disable-x86asm by hand" >&2
  echo "if you genuinely want a slow build." >&2
  exit 1
fi

# Static executables for the client, shared libraries for the server. A client
# ships one file that runs anywhere; a server links these into two processes and
# wants one copy of each.
#
# **`--enable-static` is not the whole of "runs anywhere".** It makes FFmpeg's
# own libraries static and leaves everything else a DLL: the MinGW runtime, and
# every MSYS2 package the configure line enables. Snappy is C++, so
# `libstdc++-6.dll` on its own would be enough to stop it. Those are on the
# PATH where it is built and nowhere on a machine that only has the installer,
# and Windows then refuses to start the process with nothing on stderr — which
# is what the owner met on 2026-10-10, "Command failed" and not one word about
# why, on every file he dropped.
#
# `-static` to the linker is what actually makes it one file. Only for the
# client: the server's libraries are meant to be shared, by all three processes
# that link them.
if [ "$profile" = server ]; then
  sharing="--enable-shared --disable-static"
  decoders="$DECODERS,$CUVID_DECODERS"
else
  sharing="--disable-shared --enable-static"
  sharing="$sharing --extra-ldflags=-static"
  # **The C++ runtime, named rather than left to luck.** `libvpl.a` carries
  # C++ objects as of libvpl 2.17 (MSYS2 moved to it on 2026-10-10, mid
  # evening), and FFmpeg links its configure check for libvpl with the C
  # compiler -- so a library that was installed, with its `vpl.pc` right
  # there, came back as "libvpl >= 2.6 not found" while `config.log` showed
  # `__cxa_begin_catch` and `__gxx_personality_seh0` undefined. FFmpeg names
  # `-lstdc++` itself for snappy, which is why that half has always worked,
  # and does not for libvpl.
  #
  # Nothing new ends up in the binary: snappy is C++ and already brings
  # libstdc++ in, which the comment above this says in as many words. And
  # libstdc++ carries the GCC Runtime Library Exception, which exists so that
  # a program under any licence may link it.
  sharing="$sharing --extra-libs=-lstdc++"
  decoders="$DECODERS"
fi
readonly sharing decoders

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Where the build installs itself. For the client that is a staging directory
# and two files are picked out of it; for the server it is the final place,
# because the `.pc` files a linker reads have the prefix baked into them and a
# build installed to a temporary directory and then copied points at a
# directory that no longer exists.
mkdir -p "$out"
if [ "$profile" = server ]; then
  prefix=$(cd "$out" && pwd)
else
  prefix="$work/staged"
fi
readonly prefix

tarball="$work/ffmpeg-$FFMPEG_VERSION.tar.xz"
echo "fetching FFmpeg $FFMPEG_VERSION"
curl -fsSL -o "$tarball" "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz"

echo "$FFMPEG_SHA256  $tarball" | sha256sum -c - >/dev/null || {
  echo "the tarball does not match the hash this script pins" >&2
  echo "expected $FFMPEG_SHA256" >&2
  echo "got      $(sha256sum "$tarball" | cut -d' ' -f1)" >&2
  exit 1
}

tar xf "$tarball" -C "$work"
source_dir="$work/ffmpeg-$FFMPEG_VERSION"

# The NVIDIA headers, into the same staging prefix the build will look in.
if [ "$profile" = server ]; then
  headers="$work/nv-codec-headers-$NVCODEC_VERSION.tar.gz"
  echo "fetching nv-codec-headers $NVCODEC_VERSION"
  curl -fsSL -o "$headers" \
    "https://github.com/FFmpeg/nv-codec-headers/archive/refs/tags/$NVCODEC_VERSION.tar.gz"

  echo "$NVCODEC_SHA256  $headers" | sha256sum -c - > /dev/null || {
    echo "the nv-codec-headers tarball does not match the hash this script pins" >&2
    echo "expected $NVCODEC_SHA256" >&2
    echo "got      $(sha256sum "$headers" | cut -d' ' -f1)" >&2
    exit 1
  }

  tar xf "$headers" -C "$work"
  make -C "$work/nv-codec-headers-$NVCODEC_VERSION" \
    PREFIX="$work/staged" install > "$work/nvcodec.log" 2>&1 || {
      echo "installing nv-codec-headers failed; the tail of its log:" >&2
      tail -20 "$work/nvcodec.log" >&2
      exit 1
    }
  export PKG_CONFIG_PATH="$work/staged/lib/pkgconfig:${PKG_CONFIG_PATH:-}"
fi

echo "configuring"
(
  cd "$source_dir"
  # Everything off, then exactly what is wanted back on. The alternative —
  # starting from everything and disabling — is a list that grows silently with
  # every FFmpeg release.
  ./configure \
    --prefix="$prefix" \
    --disable-everything \
    --disable-gpl \
    --disable-nonfree \
    --disable-version3 \
    --disable-doc \
    --disable-network \
    --disable-autodetect \
    --enable-zlib \
    $sharing \
    --enable-small \
    --enable-decoder="$decoders" \
    --enable-encoder="$encoders" \
    --enable-demuxer="$DEMUXERS" \
    --enable-muxer="$MUXERS" \
    --enable-parser="$PARSERS" \
    --enable-filter="$FILTERS" \
    --enable-protocol="$PROTOCOLS" \
    $configure_extra \
    > "$work/configure.log" 2>&1 || {
      echo "configure failed; the tail of its log:" >&2
      tail -30 "$work/configure.log" >&2
      # **And configure's own log**, which is the only file that says *why* a
      # check failed: the compiler line it ran and what came back. Without it
      # a library that is installed, with its `.pc` file right there, reads as
      # "not found" and there is nothing to go on. Printed rather than kept,
      # because the work directory is a `mktemp -d` the exit trap removes.
      if [ -f ffbuild/config.log ]; then
        echo "--- the tail of ffbuild/config.log ---" >&2
        tail -40 ffbuild/config.log >&2
      fi
      exit 1
    }

  echo "building"
  make -j"$(nproc)" > "$work/make.log" 2>&1 || {
    echo "the build failed; the tail of its log:" >&2
    tail -30 "$work/make.log" >&2
    exit 1
  }
)

mkdir -p "$out"
install -m 0755 "$source_dir/ffmpeg$EXE" "$out/ffmpeg$EXE"
install -m 0755 "$source_dir/ffprobe$EXE" "$out/ffprobe$EXE"

# A server build is libraries, and the headers and pkg-config files that let
# something link them. FFmpeg installs them itself rather than this script
# picking files out: the set of `.so` symlinks a linker expects is FFmpeg's
# business.
if [ "$profile" = server ]; then
  echo "installing the libraries"
  make -C "$source_dir" install > "$work/install.log" 2>&1 || {
    echo "installing failed; the tail of its log:" >&2
    tail -20 "$work/install.log" >&2
    exit 1
  }
fi

# The licence material, which has to travel with the binaries rather than be
# somewhere a reader is expected to look.
install -m 0644 "$source_dir/COPYING.LGPLv2.1" "$out/COPYING.LGPLv2.1"
[ -f "$source_dir/LICENSE.md" ] && install -m 0644 "$source_dir/LICENSE.md" "$out/LICENSE.md"

# What was built, written down beside it. A release that cannot say exactly what
# its FFmpeg is has not met the obligation it is trying to meet.
{
  echo "FFmpeg $FFMPEG_VERSION"
  echo "source https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz"
  echo "sha256 $FFMPEG_SHA256"
  echo "built  $(date -u +%Y-%m-%dT%H:%M:%SZ) by tools/build-ffmpeg.sh --profile $profile"
  echo
  echo "configuration as the binary reports it:"
  if [ -d "$out/lib" ]; then
    env "LD_LIBRARY_PATH=$out/lib" "$out/ffmpeg$EXE" -hide_banner -version 2>/dev/null |
      sed -n '/^configuration:/,$p' | head -1
  else
    "$out/ffmpeg$EXE" -hide_banner -version 2>/dev/null | sed -n '/^configuration:/,$p' | head -1
  fi
} > "$out/CONFIGURATION.txt"

# The notices BSD-3-Clause asks to travel with a binary distribution.
#
# Snappy has been linked in since September and its notice never shipped; this
# is that gap and Kvazaar's together. Taken from the packages themselves rather
# than written out here, so what travels is the notice the library travels
# with. A library whose package carries no licence file is named all the same:
# an empty line in this file is a question somebody can answer, and a missing
# name is not.
{
  echo "Third-party components linked into these binaries."
  echo "FFmpeg itself is LGPL-2.1; its text is beside this file."
  for library in snappy kvazaar; do
    echo
    echo "--- $library ---"
    found=""
    for licence in /mingw64/share/licenses/"$library"/* /usr/share/doc/"$library"/copyright; do
      if [ -s "$licence" ]; then
        cat "$licence"
        found=yes
        break
      fi
    done
    [ -n "$found" ] || echo "(no licence file in the package this was built against)"
  done
} > "$out/THIRD-PARTY.txt"

echo
verify "$out"
