#!/bin/sh
# How the installer picks assets out of a release, against a realistic answer.
#
# Worth doing because every way this goes wrong is silent. A selector that
# matches nothing is indistinguishable from a release that carries nothing:
# both leave the variable empty and the script carries on politely. And an
# asset carries its *uploader's* API url as well as its own, so a naive grep
# for `"url"` finds the wrong one and downloads a description of a person.
#
# The fixture below is shaped like GitHub's real answer, including the uploader
# object and an arm64 package and two checksums to pick wrongly from.
set -eu

architecture=amd64
fail=0

release='{
  "tag_name": "v0.2.0",
  "assets": [
    {
      "url": "https://api.github.com/repos/testbild-media/OpenVideoServer/releases/assets/11",
      "id": 11,
      "name": "OpenVideoServer-0.2.0-setup.exe",
      "label": null,
      "uploader": {
        "login": "someone",
        "id": 99,
        "url": "https://api.github.com/users/someone",
        "html_url": "https://github.com/someone"
      },
      "content_type": "application/octet-stream",
      "size": 123,
      "browser_download_url": "https://github.com/testbild-media/OpenVideoServer/releases/download/v0.2.0/OpenVideoServer-0.2.0-setup.exe"
    },
    {
      "url": "https://api.github.com/repos/testbild-media/OpenVideoServer/releases/assets/12",
      "id": 12,
      "name": "openvideoserver_0.2.0_arm64.deb",
      "uploader": {
        "login": "someone",
        "url": "https://api.github.com/users/someone"
      },
      "browser_download_url": "https://github.com/testbild-media/OpenVideoServer/releases/download/v0.2.0/openvideoserver_0.2.0_arm64.deb"
    },
    {
      "url": "https://api.github.com/repos/testbild-media/OpenVideoServer/releases/assets/13",
      "id": 13,
      "name": "openvideoserver_0.2.0_amd64.deb",
      "uploader": {
        "login": "someone",
        "url": "https://api.github.com/users/someone"
      },
      "browser_download_url": "https://github.com/testbild-media/OpenVideoServer/releases/download/v0.2.0/openvideoserver_0.2.0_amd64.deb"
    },
    {
      "url": "https://api.github.com/repos/testbild-media/OpenVideoServer/releases/assets/14",
      "id": 14,
      "name": "openvideoserver_0.2.0_amd64.deb.sha256",
      "uploader": {
        "login": "someone",
        "url": "https://api.github.com/users/someone"
      },
      "browser_download_url": "https://github.com/testbild-media/OpenVideoServer/releases/download/v0.2.0/openvideoserver_0.2.0_amd64.deb.sha256"
    }
  ]
}'

# The two functions under test, copied from install.sh. Copied rather than
# sourced because that file installs a server the moment it is read.
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

check() {
  if [ "$2" = "$3" ]; then
    echo "  ok: $1"
  else
    echo "  WRONG: $1" >&2
    echo "    wanted $3" >&2
    echo "    got    $2" >&2
    fail=1
  fi
}

PKG='^openvideoserver_.*_'"$architecture"'\\.deb$'
SUM='^openvideoserver_.*_'"$architecture"'\\.deb\\.sha256$'
EXE='\\.exe$'

# **And that the installer uses these, not something like them.** A test with
# its own copy of a pattern proves the copy. The four backslashes are what
# survives `awk -v`, which unescapes its value before the regex sees it: with
# two, the dot matches any character and awk says so in a warning nobody reads.
installer=$(dirname "$0")/../install.sh
for written in \
  '^openvideoserver_.*_${architecture}\\\\.deb\$' \
  '^openvideoserver_.*_${architecture}\\\\.deb\\\\.sha256\$' \
  '\\\\.exe\$'
do
  if grep -qF -- "$written" "$installer"; then
    echo "  ok: install.sh uses $written"
  else
    echo "  WRONG: install.sh does not use $written" >&2
    fail=1
  fi
done

echo "without a token, the public addresses:"
token=""
check "the amd64 package, not the arm64 one" \
  "$(asset_named "$PKG")" \
  "https://github.com/testbild-media/OpenVideoServer/releases/download/v0.2.0/openvideoserver_0.2.0_amd64.deb"
check "its checksum, which is a different asset" \
  "$(asset_named "$SUM")" \
  "https://github.com/testbild-media/OpenVideoServer/releases/download/v0.2.0/openvideoserver_0.2.0_amd64.deb.sha256"
check "the Windows client" \
  "$(asset_named "$EXE")" \
  "https://github.com/testbild-media/OpenVideoServer/releases/download/v0.2.0/OpenVideoServer-0.2.0-setup.exe"

echo "with a token, the API's own addresses:"
token="github_pat_pretend"
# **Not the uploader's.** Every asset here carries one, and it is the trap this
# whole approach exists to avoid.
check "the amd64 package" "$(asset_named "$PKG")" \
  "https://api.github.com/repos/testbild-media/OpenVideoServer/releases/assets/13"
check "its checksum" "$(asset_named "$SUM")" \
  "https://api.github.com/repos/testbild-media/OpenVideoServer/releases/assets/14"
check "the Windows client" "$(asset_named "$EXE")" \
  "https://api.github.com/repos/testbild-media/OpenVideoServer/releases/assets/11"

echo "and the names, because an API address ends in a number:"
check "the package's name" "$(asset_name "$PKG")" "openvideoserver_0.2.0_amd64.deb"
check "the client's name" "$(asset_name "$EXE")" "OpenVideoServer-0.2.0-setup.exe"

echo "a release with no client in it:"
without=$(printf '%s' "$release" | grep -v '\.exe')
release="$without"
check "answers nothing rather than something else" "$(asset_named "$EXE")" ""

[ "$fail" = 0 ] && echo "every selector picks the right asset"
exit "$fail"
