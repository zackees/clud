#!/bin/sh
set -eu

test -f /mount-probe/parent/mounted/CLAUDE.md
mkdir -p /mount-probe/shim /mount-probe/handoff /mount-probe/home
cp /build/target/debug/clud-shim /mount-probe/shim/rm
ln -s /bin/busybox /mount-probe/handoff/rm
export HOME=/mount-probe/home
# The floor is session-only (#1546): stamp the session like clud does.
CLUD_SHIM_ABI=$(/build/target/debug/clud-shim --registry | sed -n 's/.*"abi":"\([^"]*\)".*/\1/p')
export CLUD_SHIM_ABI
export CLUD_RM_SHIM_DIR=/mount-probe/shim
export PATH=/mount-probe/shim:/mount-probe/handoff:/usr/bin:/bin

touch /mount-probe/leaf
/mount-probe/shim/rm -f /mount-probe/leaf
test ! -e /mount-probe/leaf

set +e
/mount-probe/shim/rm -rf /mount-probe/parent
parent_code=$?
set -e
test "$parent_code" -eq 2
test -f /mount-probe/parent/mounted/CLAUDE.md

mkdir -p /mount-probe/recording
cp /workspace/ci/docker/rm_protection/recording_stub.sh /mount-probe/recording/rm
chmod +x /mount-probe/recording/rm
export PATH=/mount-probe/shim:/mount-probe/recording:/usr/bin:/bin
set +e
/mount-probe/shim/rm -rf /mount-probe/parent/mounted
direct_code=$?
set -e
test "$direct_code" -eq 2
test ! -e /mount-probe/stub-called
test -f /mount-probe/parent/mounted/CLAUDE.md
printf 'BusyBox handoff and bind-mount floor passed\n'
