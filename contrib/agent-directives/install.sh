#!/usr/bin/env bash
set -euo pipefail

template="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)/INSTRUCTIONS.md"
shared_dir="${XDG_CONFIG_HOME:-$HOME/.config}/agent-directives"
shared_file="$shared_dir/INSTRUCTIONS.md"
codex_file="${CODEX_HOME:-$HOME/.codex}/AGENTS.md"
claude_file="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/CLAUDE.md"

check_target() {
    local target="$1"
    if [[ ( -e "$target" || -L "$target" ) && ! "$target" -ef "$shared_file" ]]; then
        printf 'Refusing to replace existing file: %s\n' "$target" >&2
        exit 1
    fi
}

check_target "$codex_file"
check_target "$claude_file"

mkdir -p -- "$shared_dir"
if [[ ! -e "$shared_file" ]]; then
    cp -- "$template" "$shared_file"
fi

for target in "$codex_file" "$claude_file"; do
    mkdir -p -- "$(dirname -- "$target")"
    if [[ ! -e "$target" && ! -L "$target" ]]; then
        ln -s -- "$shared_file" "$target"
    fi
done
