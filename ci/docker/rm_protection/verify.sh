#!/usr/bin/env bash
# Assert that the cmd-scan hook refuses every `rm -rf $VAR/` shape, inside a
# container.
#
# ## This script never runs a removal
#
# Each case is a JSON hook payload. It reaches the hook as
# `printf '%s' "$payload" | clud-block-bad-cmd` -- stdin, as data. The command
# text inside it is never expanded, evaluated, or passed to a shell, so there
# is nothing for the shell to act on even when the payload literally reads
# `rm -rf /`. The hook parses the JSON and prints a verdict.
#
# The container is defense in depth, not permission. Do not "improve" this by
# executing a case to see what happens; the entire point is that the command
# is rejected before anything runs.
#
# Exit codes: 0 all cases behaved; 1 at least one did not.

set -uo pipefail

BIN="${CLUD_BLOCK_BAD_CMD:-/usr/local/bin/clud-block-bad-cmd}"
CASE_DIR="${CASE_DIR:-/tmp/rm-protection-cases}"
WORK="${WORK_DIR:-/work}"

if [ ! -x "$BIN" ]; then
  echo "FATAL: hook binary not found or not executable: $BIN" >&2
  exit 1
fi

mkdir -p "$WORK"
python3 "$(dirname "$0")/generate_cases.py" --out-dir "$CASE_DIR" --cwd "$WORK"

deny_total=0
deny_failed=0
allow_total=0
allow_failed=0
failures=""

# A refusal is exit code 2 carrying a deny decision. Both are checked: exit 2
# with no decision payload would leave the harness without a reason to show.
while IFS= read -r payload; do
  [ -z "$payload" ] && continue
  deny_total=$((deny_total + 1))
  out="$(printf '%s' "$payload" | "$BIN" 2>/dev/null)"
  rc=$?
  case "$rc:$out" in
    2:*'"deny"'*) ;;
    *)
      deny_failed=$((deny_failed + 1))
      failures="${failures}  NOT REFUSED (rc=${rc}): ${payload}"$'\n'
      ;;
  esac
done < "$CASE_DIR/deny.jsonl"

# The other half of the contract: a guard that refuses everything is an outage.
while IFS= read -r payload; do
  [ -z "$payload" ] && continue
  allow_total=$((allow_total + 1))
  out="$(printf '%s' "$payload" | "$BIN" 2>/dev/null)"
  rc=$?
  if [ "$rc" -eq 2 ]; then
    allow_failed=$((allow_failed + 1))
    failures="${failures}  WRONGLY REFUSED: ${payload}"$'\n'
  fi
done < "$CASE_DIR/allow.jsonl"

echo
echo "hazardous cases refused : $((deny_total - deny_failed))/${deny_total}"
echo "benign cases allowed    : $((allow_total - allow_failed))/${allow_total}"

if [ -n "$failures" ]; then
  echo
  echo "FAILURES:" >&2
  printf '%s' "$failures" >&2
  exit 1
fi

echo
echo "OK: every hazardous rm shape was refused, every benign command survived."
