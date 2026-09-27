#!/usr/bin/env bash
set -euo pipefail

if command -v pacman >/dev/null 2>&1; then
  pacman -Sy --noconfirm ca-certificates curl
elif command -v dnf >/dev/null 2>&1; then
  dnf install -y ca-certificates curl
else
  echo "Unsupported Linux test image: expected pacman or dnf" >&2
  exit 2
fi

cp clud-installer.exe /tmp/clud-installer.exe
chmod +x /tmp/clud-installer.exe
installer=/tmp/clud-installer.exe
test "$($installer --host-platform)" = "linux x86_64"
version=$($installer --latest-stable)
test -n "$version"
test_home=$(mktemp -d /tmp/clud-installer-home.XXXXXX)
test_tmp=$(mktemp -d /tmp/clud-installer-tmp.XXXXXX)
trap 'rm -rf -- "$test_home" "$test_tmp"' EXIT
for profile in .profile .bash_profile .bash_login .bashrc; do
  printf '%s\n' '# clud-installer managed PATH' > "$test_home/$profile"
done

PATH="$test_home/.local/bin:$PATH" env -u BASH_ENV -u SHELL HOME="$test_home" TMPDIR="$test_tmp" \
  "$installer" --install-version "$version" --yes
resolved=$(env -u BASH_ENV HOME="$test_home" SHELL=/bin/bash \
  bash --login -c 'command -v clud')
test "$resolved" = "$test_home/.local/bin/clud"
actual=$(env -u BASH_ENV HOME="$test_home" SHELL=/bin/bash \
  bash --login -c 'clud --version')
test "$actual" = "clud $version"
interactive_resolved=$(env -u BASH_ENV HOME="$test_home" SHELL=/bin/bash \
  bash -i -c 'command -v clud' 2>/dev/null)
test "$interactive_resolved" = "$test_home/.local/bin/clud"
interactive_actual=$(env -u BASH_ENV HOME="$test_home" SHELL=/bin/bash \
  bash -i -c 'clud --version' 2>/dev/null)
test "$interactive_actual" = "clud $version"
printf 'Linux container acceptance passed: %s -> %s\n' "$resolved" "$actual"
