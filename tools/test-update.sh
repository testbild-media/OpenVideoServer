#!/bin/sh
# What the update installer does, against stand-ins for the network and apt.
#
#   dash tools/test-update.sh
#
# `deploy/update/ovs-update` runs as root on a show machine and installs a
# package, so every way it could install the wrong thing is tried here: a name
# that is not a version, an older release, a package that does not match its
# checksum, a release with nothing for this machine, an answer whose download
# address points somewhere else, apt failing. `curl`, `apt-get` and
# `dpkg-query` are replaced by small scripts that record what they were asked;
# `dpkg` is the real one, for comparing versions and naming the architecture.
#
# Run under dash in CI, which is the `sh` a Debian unit gets.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
installer="$here/../deploy/update/ovs-update"
shell="${TEST_SHELL:-sh}"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fail=0

architecture=$(dpkg --print-architecture)

mkdir -p "$work/bin"

# **curl**: answers the release from a file, the package with fixed bytes and
# the checksum with their sum (or a wrong one), and writes down every address.
cat > "$work/bin/curl" << 'EOF'
#!/bin/sh
output=""
address=""
while [ $# -gt 0 ]; do
  case "$1" in
    --output) output="$2"; shift 2 ;;
    --header | --max-time) shift 2 ;;
    -*) shift ;;
    *) address="$1"; shift ;;
  esac
done
echo "$address" >> "$FAKE/asked"
case "$address" in
  https://api.github.com/*)
    [ -f "$FAKE/release.json" ] || { echo "curl: (22) The requested URL returned error: 404" >&2; exit 22; }
    cat "$FAKE/release.json" ;;
  *.deb.sha256)
    if [ -f "$FAKE/bad-sum" ]; then
      echo "0000000000000000000000000000000000000000000000000000000000000000  x" > "$output"
    else
      printf 'package bytes' | sha256sum | sed "s/-\$/x/" > "$output"
    fi ;;
  *.deb)
    printf 'package bytes' > "$output" ;;
  *) echo "curl: (6) Could not resolve host" >&2; exit 6 ;;
esac
EOF

# **dpkg-query**: the installed version, from a file apt-get rewrites.
cat > "$work/bin/dpkg-query" << 'EOF'
#!/bin/sh
cat "$FAKE/installed"
EOF

# **apt-get**: writes down what it was asked to install, and then it is.
cat > "$work/bin/apt-get" << 'EOF'
#!/bin/sh
echo "$*" >> "$FAKE/apt"
if [ -f "$FAKE/apt-fails" ]; then
  echo "E: Sub-process /usr/bin/dpkg returned an error code (1)"
  exit 100
fi
for last in "$@"; do :; done
basename "$last" | sed 's/^openvideoserver_\(.*\)_[a-z0-9]*\.deb$/\1/' > "$FAKE/installed"
EOF
chmod +x "$work/bin/curl" "$work/bin/dpkg-query" "$work/bin/apt-get"

# A release shaped like the repository's real answer: compact, an uploader
# object in every asset, an arm64 package to pick wrongly, and the package's
# own address pointing somewhere else — which the installer must not follow.
release_with() {
  version="$1"
  shift
  assets=""
  for name in "$@"; do
    assets="$assets${assets:+,}{\"url\":\"https://api.github.com/repos/testbild-media/OpenVideoServer/releases/assets/1\",\"id\":1,\"name\":\"$name\",\"label\":null,\"uploader\":{\"login\":\"someone\",\"url\":\"https://api.github.com/users/someone\"},\"size\":13,\"browser_download_url\":\"https://elsewhere.example/$name\"}"
  done
  printf '{"tag_name":"v%s","name":"v%s","assets":[%s]}' "$version" "$version" "$assets"
}

full() {
  release_with "$1" \
    "OpenVideoServer-$1-58-setup.exe" \
    "openvideoserver_$1-58_arm64.deb" \
    "openvideoserver_$1-58_$architecture.deb" \
    "openvideoserver_$1-58_$architecture.deb.sha256"
}

# One run of the installer in a clean room. Leaves its exit status in $code.
run() {
  rm -rf "$work/fake" "$work/state" "$work/cache"
  mkdir -p "$work/fake"
  echo "$installed" > "$work/fake/installed"
  [ -z "${answer:-}" ] || printf '%s' "$answer" > "$work/fake/release.json"
  [ -z "${bad_sum:-}" ] || touch "$work/fake/bad-sum"
  [ -z "${apt_fails:-}" ] || touch "$work/fake/apt-fails"
  set +e
  FAKE="$work/fake" PATH="$work/bin:$PATH" \
    OVS_UPDATE_STATE_DIR="$work/state" OVS_UPDATE_CACHE_DIR="$work/cache" \
    "$shell" "$installer" "$@" > "$work/said" 2>&1
  code=$?
  set -e
}

stage() {
  sed -n 's/.*"stage":"\([^"]*\)".*/\1/p' "$work/state/status.json" 2> /dev/null
}

check() {
  if [ "$2" = "$3" ]; then
    echo "  ok: $1"
  else
    echo "  WRONG: $1" >&2
    echo "    wanted $3" >&2
    echo "    got    $2" >&2
    sed 's/^/    | /' "$work/said" >&2
    fail=1
  fi
}

echo "a newer release is installed, from this repository:"
installed="0.2.45-57" answer=$(full 0.2.46) bad_sum="" apt_fails=""
run 0.2.46
check "it succeeds" "$code" 0
check "and says so" "$(stage)" "installed"
check "apt installed this machine's package" \
  "$(sed 's/.* //' "$work/fake/apt")" "$work/cache/openvideoserver_0.2.46-58_$architecture.deb"
check "downloaded from the repository, not where the answer pointed" \
  "$(grep -c 'elsewhere' "$work/fake/asked")" 0
check "from this repository's own release" \
  "$(sed -n 2p "$work/fake/asked")" \
  "https://github.com/testbild-media/OpenVideoServer/releases/download/v0.2.46/openvideoserver_0.2.46-58_$architecture.deb"
check "and leaves no package behind" "$(ls "$work/cache" | wc -l | tr -d ' ')" 0

echo "a name that is not a version:"
installed="0.2.45-57" answer=$(full 0.2.46)
run "0.2.46; reboot"
check "is refused" "$(stage)" "refused"
check "before anything is fetched" "$(cat "$work/fake/asked" 2> /dev/null | wc -l | tr -d ' ')" 0
run "../0.2.46"
check "nor a path" "$(stage)" "refused"

echo "the same version, or an older one:"
installed="0.2.46-58" answer=$(full 0.2.46)
run 0.2.46
check "the same is refused" "$(stage)" "refused"
installed="0.2.47-60"
run 0.2.46
check "an older one too" "$(stage)" "refused"
check "and apt is never asked" "$([ -f "$work/fake/apt" ] && echo asked || echo not)" "not"

echo "the same version again, by hand:"
installed="0.2.46-58" answer=$(full 0.2.46)
run --reinstall 0.2.46
check "is installed again" "$(stage)" "installed"
check "with --reinstall" "$(grep -c -- '--reinstall' "$work/fake/apt")" 1

echo "a package that does not match its checksum:"
installed="0.2.45-57" answer=$(full 0.2.46) bad_sum="yes"
run 0.2.46
check "fails" "$(stage)" "failed"
check "without installing it" "$([ -f "$work/fake/apt" ] && echo asked || echo not)" "not"
check "and does not keep it" "$(ls "$work/cache" | wc -l | tr -d ' ')" 0
bad_sum=""

echo "a release with nothing for this machine:"
answer=$(release_with 0.2.46 "openvideoserver_0.2.46-58_other.deb" "openvideoserver_0.2.46-58_other.deb.sha256")
run 0.2.46
check "fails" "$(stage)" "failed"
answer=$(release_with 0.2.46 "openvideoserver_0.2.46-58_$architecture.deb")
run 0.2.46
check "nor one whose checksum is missing" "$(stage)" "failed"

echo "no such release, or no network:"
answer=""
run 0.2.46
check "fails" "$(stage)" "failed"

echo "apt cannot install it:"
answer=$(full 0.2.46) apt_fails="yes"
run 0.2.46
check "fails" "$(stage)" "failed"
check "with apt's own last word" \
  "$(sed -n 's/.*"detail":"\([^"]*\)".*/\1/p' "$work/state/status.json")" \
  "apt-get could not install it: E: Sub-process /usr/bin/dpkg returned an error code (1)"
apt_fails=""

echo "what the server reads:"
installed="0.2.45-57" answer=$(full 0.2.46)
run 0.2.46
check "is one line of JSON with the four fields" \
  "$(sed 's/"at":"[^"]*"/"at":""/' "$work/state/status.json")" \
  "{\"version\":\"0.2.46\",\"stage\":\"installed\",\"detail\":\"openvideoserver 0.2.46-58\",\"at\":\"\"}"

[ "$fail" = 0 ] && echo "the installer installs only what it should"
exit "$fail"
