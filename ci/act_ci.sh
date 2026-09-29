#!/bin/sh
# Run a Linux workflow job under act inside the bosn `clud_act` stack.
# Defaults to ci.yml; set ACT_WORKFLOW for another workflow in the same dir.
# Usage: act_ci.sh <job-id> | --list
#
# The checkout is mounted read-only, and a worktree's `.git` is a file that
# points outside the mount. So the tree is copied to a per-run scratch dir and
# given a throwaway one-commit git repo; act reads its sha and ref from that.
set -eu

SRC=/workspace
RUN="act-$(hostname)-$$"
WORK="/tmp/$RUN/src"
CHECKOUT="/tmp/$RUN/checkout"
IMAGE="${ACT_IMAGE:-catthehacker/ubuntu:act-24.04}"
REPO="${ACT_REPO:-zackees/clud}"
WORKFLOW="${ACT_WORKFLOW:-ci.yml}"
EVENT="${ACT_EVENT:-pull_request}"
# Both live in machine-scoped bosn volumes (bosn.toml `clud_act`), so they
# outlive this container: action checkouts, and the Actions cache server that
# backs actions/cache, setup-uv and setup-soldr's caches.
ACTION_CACHE=/root/.cache/act
SERVER_CACHE=/root/.cache/actcache

# Job containers are siblings on the host engine, outside bosn's registry.
# `--rm` removes them on a normal exit; after a crash or interrupt, this trap
# removes the containers labelled with this run and the per-job volumes act
# named after it (the workflow names carry $RUN, see below).
cleanup() {
    ids="$(docker ps -aq --filter "label=clud.act-run=$RUN" 2>/dev/null || true)"
    [ -z "$ids" ] || docker rm -f $ids >/dev/null 2>&1 || true
    vols="$(docker volume ls -q --filter "name=-$RUN-" 2>/dev/null || true)"
    [ -z "$vols" ] || docker volume rm -f $vols >/dev/null 2>&1 || true
    rm -rf "/tmp/$RUN"
}
trap cleanup EXIT

mkdir -p "$WORK" "$CHECKOUT"
# --no-same-owner: the copy is owned by root, not the host uid, so git doesn't
# refuse it as "dubious ownership".
tar -C "$SRC" --exclude=./target --exclude=./.venv --exclude=./dist \
    --exclude=./.git --exclude=./.clud/act-logs -cf - . | tar -C "$WORK" --no-same-owner -xf -

# act names job containers from the workflow and job names only
# (act-<workflow>-<job>-<hash>), so two concurrent runs of ci.yml on one host,
# from any checkout or session, force-remove each other's containers
# ("No such container" mid-step). A per-run workflow name keeps them apart,
# for ci.yml and for the reusable workflows it calls.
for wf in "$WORK"/.github/workflows/*.yml; do
    sed -i "1,/^name:/s/^name: \(.*\)/name: \1 $RUN/" "$wf"
done

git -C "$WORK" init -q -b act-local
git -C "$WORK" -c user.name=act -c user.email=act@localhost add -A
git -C "$WORK" -c user.name=act -c user.email=act@localhost commit -q -m "act snapshot"

cd "$WORK"
if [ "${1:-}" = "--list" ]; then
    act -l -W ".github/workflows/$WORKFLOW"
    exit
fi
JOB="${1:?usage: act_ci.sh <job-id> | --list}"

# ci.yml checks out `pull_request.head.repo.full_name` at `head.sha`, then
# verifies HEAD == head.sha. The event names this repo and the snapshot commit.
SHA="$(git rev-parse HEAD)"
LABELS='[]'
if [ "${ACT_CI_FULL:-0}" = 1 ]; then
    LABELS='[{"name":"ci-full"}]'
fi
cat > "/tmp/$RUN/event.json" <<EOF
{"pull_request": {"number": 0, "labels": $LABELS,
  "head": {"sha": "$SHA", "ref": "act-local", "repo": {"full_name": "$REPO"}},
  "base": {"ref": "main", "repo": {"full_name": "$REPO"}}},
 "repository": {"full_name": "$REPO", "default_branch": "main"}}
EOF

# act copies the local tree into the job only when it can skip
# `actions/checkout`, which requires the step's `ref` to equal github.ref.
# ci.yml pins `ref` to the head SHA, so the real action would run and fetch
# from GitHub. Instead checkout is swapped for a local action that carries the
# snapshot (with its .git) and unpacks it into the job's workspace.
tar -C "$WORK" -cf "$CHECKOUT/src.tar" .
cat > "$CHECKOUT/action.yml" <<'EOF'
name: act snapshot checkout
description: act_ci.sh stand-in for actions/checkout; unpacks the local snapshot.
inputs:
  repository: {required: false}
  ref: {required: false}
  fetch-depth: {required: false}
  token: {required: false}
  path: {required: false}
  submodules: {required: false}
  persist-credentials: {required: false}
  sparse-checkout: {required: false}
  lfs: {required: false}
runs:
  using: composite
  steps:
    - run: |
        tar -xf "$GITHUB_ACTION_PATH/src.tar" -C "$GITHUB_WORKSPACE"
        git config --global --add safe.directory "$GITHUB_WORKSPACE"
        git -C "$GITHUB_WORKSPACE" rev-parse HEAD
      shell: bash
EOF

# GitHub API reads go through bosn's read-only proxy: the act tasks in bosn.toml
# declare `github_api = "proxy"`, so bosn sets GITHUB_API_URL to a per-run
# loopback URL whose host side adds the credential and refuses writes. act's job
# containers use host networking and reach it directly; the token never enters
# a container. act needs the value spelled out (a bare `--env NAME` passes "").
set --
if [ -n "${GITHUB_API_URL:-}" ]; then
    set -- --env "GITHUB_API_URL=$GITHUB_API_URL"
fi
if [ -n "${GITHUB_TOKEN:-}" ]; then
    set -- "$@" -s GITHUB_TOKEN
fi
if [ "${ACT_PUBLIC_X64:-0}" = 1 ]; then
    TAG="${ACT_RELEASE_TAG:-$(sed -n 's/^version = "\([0-9][^"]*\)"/\1/p' "$SRC/pyproject.toml" | head -n 1)}"
    [ -n "$TAG" ] || { echo "No release tag in pyproject.toml" >&2; exit 1; }
    set -- "$@" --input "release_tag=$TAG" --input mode=candidate \
        --env PYTEST_ADDOPTS=-s \
        --matrix target:x86_64-unknown-linux-musl
fi

# --cache-server-path: act's default is ~/.cache/actcache, but pinning it to
# the volume makes the reuse explicit. Without a persistent path every run
# restored nothing: a cold venv, and 0 zccache hits in the Rust build.
# --pull=false: reuse the local runner image (a missing one is still pulled).
# --init: reap detached daemon children so strict shutdown tests see exited PIDs.
run_act() {
    # Act treats the synthetic PR payload as a pull_request event even when
    # workflow_call is requested. Use its generated call event for that lane.
    if [ "$EVENT" = pull_request ]; then
        set -- -e "/tmp/$RUN/event.json" "$@"
    fi
    act "$EVENT" -W ".github/workflows/$WORKFLOW" -j "$JOB" \
        --local-repository "actions/checkout@v4=$CHECKOUT" \
        -P "ubuntu-24.04=$IMAGE" \
        --pull=false \
        --rm \
        --container-options "--init --label clud.act-run=$RUN" \
        --artifact-server-path "/tmp/$RUN/artifacts" \
        --action-cache-path "$ACTION_CACHE" \
        --cache-server-path "$SERVER_CACHE" "$@"
}

# Live, durable logs (#1548). bosn buffers a task's output until it exits and
# removes the container on a timeout or Ctrl-C, so act's output also goes to a
# file on the host: the `clud_act` stack binds `.clud/act-logs` at /act-logs.
# The paths are printed first, so `tail -f` works while the job runs, and the
# file survives a timeout, Ctrl-C or container removal. Without the mount the
# log falls back to /tmp and does not survive the container.
LOG_DIR="${ACT_LOG_DIR:-/act-logs}"
if ! mkdir -p "$LOG_DIR" 2>/dev/null || [ ! -w "$LOG_DIR" ]; then
    LOG_DIR=/tmp/act-logs
    mkdir -p "$LOG_DIR"
    echo "act_ci: /act-logs is not mounted; logging to $LOG_DIR (lost with the container)" >&2
fi
# Logs are user-only: act masks secrets, but a log is still not for others.
chmod 700 "$LOG_DIR" 2>/dev/null || true
umask 077
LOG="$LOG_DIR/$RUN-$JOB.log"
: >"$LOG"
# The container runs as root; hand the log to the host user who owns the
# directory, so `tail -f` on the host can read it.
OWNER="$(stat -c %u:%g "$LOG_DIR" 2>/dev/null || true)"
[ -z "$OWNER" ] || chown "$OWNER" "$LOG" 2>/dev/null || true
echo "act_ci: log   $LOG" >&2
# ACT_JSON=1 asks act for one JSON object per line (act --json replaces the
# text stream, so the .log then holds JSON, teed to a .jsonl of its own).
if [ "${ACT_JSON:-0}" = 1 ]; then
    JSONL="$LOG_DIR/$RUN-$JOB.jsonl"
    : >"$JSONL"
    [ -z "$OWNER" ] || chown "$OWNER" "$JSONL" 2>/dev/null || true
    echo "act_ci: jsonl $JSONL" >&2
    set -- --json "$@"
else
    # One prefix per job so parallel jobs do not interleave unreadably.
    set -- --log-prefix-job-id "$@"
fi

# Follow the log so bosn's stream shows output as it appears. This is a tee
# in POSIX sh, which has no pipefail or PIPESTATUS: act's own status is kept.
tail -n +1 -f "$LOG" &
TAIL_PID=$!
status=0
run_act "$@" >>"$LOG" 2>&1 || status=$?
sleep 1
kill "$TAIL_PID" 2>/dev/null || true
wait "$TAIL_PID" 2>/dev/null || true
[ -z "${JSONL:-}" ] || cp "$LOG" "$JSONL"
[ "$status" -eq 0 ] || exit "$status"

if [ "${ACT_PUBLIC_X64:-0}" = 1 ]; then
    # Act may exit successfully after a skipped job; require the actual pytest
    # result and the host's emitted evidence for this exact release and arch.
    grep -F 'Job succeeded' "$LOG" >/dev/null
    grep -E '(^|[^0-9])1 passed([,[:space:]]|$)' "$LOG" >/dev/null
    grep -F 'PUBLIC_EVIDENCE ' "$LOG" \
        | grep -F "\"tag\": \"$TAG\"" \
        | grep -F '"arch": "x86_64"' >/dev/null
fi
