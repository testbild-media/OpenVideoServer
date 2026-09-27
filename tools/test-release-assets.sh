#!/bin/sh
# The two greps the installer picks assets with, against a release that looks
# like the one the workflow publishes.
#
# Worth doing because a grep that matches nothing is indistinguishable from a
# release that carries nothing: both leave the variable empty and the script
# carries on politely.
set -eu

architecture=amd64

release='{
  "tag_name": "v0.2.0",
  "assets": [
    {
      "name": "OpenVideoServer-0.2.0-setup.exe",
      "browser_download_url": "https://github.com/testbild-media/OpenVideoServer/releases/download/v0.2.0/OpenVideoServer-0.2.0-setup.exe"
    },
    {
      "name": "openvideoserver_0.2.0_amd64.deb",
      "browser_download_url": "https://github.com/testbild-media/OpenVideoServer/releases/download/v0.2.0/openvideoserver_0.2.0_amd64.deb"
    },
    {
      "name": "openvideoserver_0.2.0_amd64.deb.sha256",
      "browser_download_url": "https://github.com/testbild-media/OpenVideoServer/releases/download/v0.2.0/openvideoserver_0.2.0_amd64.deb.sha256"
    },
    {
      "name": "openvideoserver_0.2.0_arm64.deb",
      "browser_download_url": "https://github.com/testbild-media/OpenVideoServer/releases/download/v0.2.0/openvideoserver_0.2.0_arm64.deb"
    }
  ]
}'

asset=$(printf '%s' "$release" \
  | grep -o "\"browser_download_url\": *\"[^\"]*openvideoserver_[^\"]*_${architecture}\.deb\"" \
  | head -1 | sed 's/.*"\(https[^"]*\)"/\1/' || true)

client_asset=$(printf '%s' "$release" \
  | grep -o "\"browser_download_url\": *\"[^\"]*\.exe\"" \
  | head -1 | sed 's/.*"\(https[^"]*\)"/\1/' || true)

echo "package: ${asset:-(none)}"
echo "client:  ${client_asset:-(none)}"

fail=0
case "$asset" in
  *openvideoserver_0.2.0_amd64.deb) ;;
  *) echo "WRONG: the package should be the amd64 .deb, not the arm64 one or the checksum" >&2; fail=1 ;;
esac
case "$client_asset" in
  *OpenVideoServer-0.2.0-setup.exe) ;;
  *) echo "WRONG: the client should be the installer" >&2; fail=1 ;;
esac

# And a release with no client in it, which is every release before today.
without=$(printf '%s' "$release" | grep -v '\.exe')
none=$(printf '%s' "$without" \
  | grep -o "\"browser_download_url\": *\"[^\"]*\.exe\"" \
  | head -1 | sed 's/.*"\(https[^"]*\)"/\1/' || true)
if [ -n "$none" ]; then
  echo "WRONG: found a client in a release that has none: $none" >&2
  fail=1
else
  echo "no client in a release without one: correct"
fi

[ "$fail" = 0 ] && echo "both greps pick the right asset"
exit "$fail"
