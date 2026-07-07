#!/usr/bin/env sh
# One-line installer for clud — the fast Rust CLI for Claude Code and Codex.
#
#   curl -fsSL https://raw.githubusercontent.com/zackees/clud/main/install.sh | sh
#
# What this does:
#   1. Ensures `uv` (https://docs.astral.sh/uv) is installed (idempotent;
#      bootstraps it via Astral's official one-line installer if missing).
#   2. Runs `uv tool install clud[==VERSION]`, which drops `clud` and
#      helper executables such as `clud-block-bad-cmd` into uv's tool bin
#      directory.
#   3. Calls `uv tool update-shell` so the bin directory lands on PATH for
#      future shells.
#   4. Prints the explicit PATH line if your current shell needs reloading.
#
# Environment:
#   CLUD_VERSION   Pin a specific clud version (e.g. CLUD_VERSION=2.0.10).
#                  Unset → install latest.
#   CLUD_NO_PATH   Set to skip `uv tool update-shell` (script still prints
#                  the directory you'd need to add).
#
# Re-running this script upgrades or reinstalls cleanly (`uv tool install
# --reinstall`).
#
# This is the END-USER installer for clud. The repo's `./install` script
# (no extension) is a DEVELOPER helper that installs `soldr` for building
# clud from source — unrelated.

set -eu

CLUD_VERSION="${CLUD_VERSION:-}"

log() { printf '%s\n' "$*" >&2; }
err() { printf 'error: %s\n' "$*" >&2; exit 1; }

require_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        err "required command '$1' is not on PATH"
    fi
}

ensure_uv() {
    if command -v uv >/dev/null 2>&1; then
        return 0
    fi
    log "uv not found — installing via https://astral.sh/uv/install.sh"
    require_cmd curl
    require_cmd sh
    curl -LsSf https://astral.sh/uv/install.sh | sh
    # Astral's installer drops uv into ~/.local/bin (default) or
    # $CARGO_HOME/bin — surface that on PATH for this shell so the
    # subsequent `uv tool install` resolves.
    for d in "$HOME/.local/bin" "$HOME/.cargo/bin"; do
        case ":$PATH:" in
            *":$d:"*) ;;
            *) PATH="$d:$PATH"; export PATH ;;
        esac
    done
    if ! command -v uv >/dev/null 2>&1; then
        err "uv install did not put uv on PATH — open a new shell and re-run, or install uv manually from https://docs.astral.sh/uv"
    fi
}

install_clud() {
    spec="clud"
    if [ -n "$CLUD_VERSION" ]; then
        spec="clud==$CLUD_VERSION"
    fi
    log "installing $spec via uv tool"
    # --force replaces any prior install cleanly so re-running this
    # script upgrades or repairs without manual `uv tool uninstall`.
    uv tool install --force "$spec"
}

verify_clud_install() {
    tool_bin=$(uv tool dir --bin 2>/dev/null || true)
    if [ -z "$tool_bin" ]; then
        err "could not determine uv tool bin dir after install"
    fi
    clud_bin="$tool_bin/clud"
    guard_bin="$tool_bin/clud-block-bad-cmd"
    if [ ! -x "$clud_bin" ]; then
        err "installed clud is missing or not executable at $clud_bin"
    fi
    if [ ! -x "$guard_bin" ]; then
        err "installed clud is missing native helper at $guard_bin; try re-running this installer"
    fi

    deny_command="bad"
    deny_command="$deny_command cmd"
    deny_payload="{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"$deny_command\"}}"
    set +e
    deny_out=$(printf '%s' "$deny_payload" | "$guard_bin" 2>/dev/null)
    deny_rc=$?
    set -e
    if [ "$deny_rc" -ne 2 ] \
        || ! printf '%s' "$deny_out" | grep -q 'permissionDecision' \
        || ! printf '%s' "$deny_out" | grep -q 'deny'; then
        err "native clud-block-bad-cmd deny smoke failed (exit $deny_rc)"
    fi

    allow_payload='{"tool_name":"Bash","tool_input":{"command":"echo ok"}}'
    set +e
    printf '%s' "$allow_payload" | "$guard_bin" >/dev/null 2>&1
    allow_rc=$?
    set -e
    if [ "$allow_rc" -ne 0 ]; then
        err "native clud-block-bad-cmd allow smoke failed (exit $allow_rc)"
    fi
}

setup_path() {
    if [ -n "${CLUD_NO_PATH:-}" ]; then
        log "CLUD_NO_PATH set — skipping shell PATH update"
    else
        # `uv tool update-shell` edits the user's shell profile to add
        # uv's tool bin dir to PATH for future shells. It is idempotent.
        if ! uv tool update-shell >/dev/null 2>&1; then
            log "warning: 'uv tool update-shell' failed; you may need to add uv's tool bin dir to PATH manually"
        fi
    fi
    # Resolve the bin dir uv used so we can tell the user the exact path.
    tool_bin=""
    if command -v uv >/dev/null 2>&1; then
        tool_bin=$(uv tool dir --bin 2>/dev/null || true)
    fi
    if [ -n "$tool_bin" ]; then
        case ":$PATH:" in
            *":$tool_bin:"*)
                log "clud is on PATH — try 'clud --version'"
                ;;
            *)
                log ""
                log "clud installed at: $tool_bin/clud"
                log "Add this directory to your current shell with:"
                log "  export PATH=\"$tool_bin:\$PATH\""
                log "Future shells should pick it up automatically (uv tool update-shell)."
                ;;
        esac
    fi
}

main() {
    ensure_uv
    install_clud
    verify_clud_install
    setup_path
}

main "$@"
