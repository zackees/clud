#!/bin/sh
set -eu

distro=$1
mode=$2
tag=$3
version=$4
expected_sha=$5
case "$distro" in
  archlinux)
    pacman -Sy --noconfirm fish shadow curl jq ca-certificates
    shell=/usr/bin/fish
    ;;
  fedora)
    dnf install -y shadow-utils util-linux-user bash coreutils curl jq
    shell=/bin/bash
    ;;
  alpine)
    apk add --no-cache bash shadow coreutils curl jq ca-certificates
    shell=/bin/bash
    ;;
  *) exit 2 ;;
esac

if [ "$mode" = candidate ]; then
  catalog_url="https://github.com/zackees/clud/releases/download/$tag/installer-candidate-manifest.json"
  channel=candidate
  candidate_env="CLUD_INSTALLER_CANDIDATE_TAG=$tag"
elif [ "$mode" = released ]; then
  catalog_url=https://zackees.github.io/clud/install/manifest.json
  channel=latest-stable
  candidate_env=
else
  exit 2
fi
asset_url="https://github.com/zackees/clud/releases/download/$tag/clud-$version-x86_64-unknown-linux-musl"
curl -fLsS --retry 5 --retry-all-errors "$catalog_url" -o /tmp/catalog.json
curl -fLsS --retry 5 --retry-all-errors "$asset_url" -o /tmp/clud-public
test "$(jq -r --arg channel "$channel" '.channels[$channel]' /tmp/catalog.json)" = "$version"
catalog_sha=$(jq -r --arg version "$version" '
  .releases[] | select(.version == $version) | .platforms[] |
  select(.platform.os == "linux" and .platform.arch == "x86_64" and .variant.flavor == "static-musl") |
  .asset.sha256
' /tmp/catalog.json)
test "$catalog_sha" = "$expected_sha"
test "$(sha256sum /tmp/clud-public | cut -d ' ' -f 1)" = "$expected_sha"
test "$(uname -m)" = x86_64
useradd -m -s "$shell" alice
install -m 755 -o alice -g alice /tmp/clud-public /home/alice/clud-public
if [ "$mode" = candidate ]; then
  su - alice -c "env $candidate_env /home/alice/clud-public --installer --install-version $version --yes"
else
  su - alice -c "/home/alice/clud-public --installer --install-version $version --yes"
fi
if [ "$distro" = archlinux ]; then
  selected=$(su - alice -c 'fish -lc "command -v clud; clud --version"')
else
  selected=$(su - alice -c 'bash -lc "command -v clud; clud --version"')
fi
expected_output="/home/alice/.local/bin/clud
clud $version"
test "$selected" = "$expected_output"
installed_sha=$(sha256sum /home/alice/.local/bin/clud | cut -d ' ' -f 1)
test "$installed_sha" = "$expected_sha"
printf 'PUBLIC_DISTRO_EVIDENCE mode=%s tag=%s distro=%s host_arch=x86_64 sha256=%s resolved_path=/home/alice/.local/bin/clud version=%s\n' "$mode" "$tag" "$distro" "$installed_sha" "$version"
