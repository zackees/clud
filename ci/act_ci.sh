#!/bin/sh
# Run a Linux job of .github/workflows/ci.yml under act, inside the bosn
# `clud_act` stack (bosn.toml). Usage: act_ci.sh <job-id> | --list
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
trap 'rm -rf "/tmp/$RUN"' EXIT

mkdir -p "$WORK" "$CHECKOUT"
# --no-same-owner: the copy is owned by root, not the host uid, so git doesn't
# refuse it as "dubious ownership".
tar -C "$SRC" --exclude=./target --exclude=./.venv --exclude=./dist \
    --exclude=./.git -cf - . | tar -C "$WORK" --no-same-owner -xf -

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
    act -l -W .github/workflows/ci.yml
    exit
fi
JOB="${1:?usage: act_ci.sh <job-id> | --list}"

# ci.yml checks out `pull_request.head.repo.full_name` at `head.sha`, then
# verifies HEAD == head.sha. The event names this repo and the snapshot commit.
SHA="$(git rev-parse HEAD)"
cat > "/tmp/$RUN/event.json" <<EOF
{"pull_request": {"number": 0, "labels": [],
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

act pull_request -W .github/workflows/ci.yml -j "$JOB" \
    -e "/tmp/$RUN/event.json" \
    --local-repository "actions/checkout@v4=$CHECKOUT" \
    -P "ubuntu-24.04=$IMAGE" \
    --rm \
    --artifact-server-path "/tmp/$RUN/artifacts" \
    --action-cache-path /root/.cache/act
