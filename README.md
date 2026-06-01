# clud

![hero-clud](https://github.com/user-attachments/assets/4009dfee-e703-446d-b073-80d826708a10)

**A fast Rust CLI for Claude Code and Codex that runs in YOLO mode by default — no permission prompts, maximum velocity.**

The name `clud` is simply a shorter, easier-to-type version of `claude`.

| Platform | Build | Lint | Unit Test | Integration Test |
|----------|-------|------|-----------|------------------|
| Linux x86 | [![Build](https://github.com/zackees/clud/actions/workflows/linux-x86-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-x86-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/linux-x86-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-x86-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/linux-x86-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-x86-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/linux-x86-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-x86-integration-test.yml) |
| Linux ARM | [![Build](https://github.com/zackees/clud/actions/workflows/linux-arm-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-arm-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/linux-arm-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-arm-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/linux-arm-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-arm-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/linux-arm-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/linux-arm-integration-test.yml) |
| Windows x86 | [![Build](https://github.com/zackees/clud/actions/workflows/windows-x86-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-x86-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/windows-x86-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-x86-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/windows-x86-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-x86-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/windows-x86-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-x86-integration-test.yml) |
| Windows ARM | [![Build](https://github.com/zackees/clud/actions/workflows/windows-arm-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-arm-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/windows-arm-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-arm-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/windows-arm-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-arm-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/windows-arm-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/windows-arm-integration-test.yml) |
| macOS x86 | [![Build](https://github.com/zackees/clud/actions/workflows/macos-x86-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-x86-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/macos-x86-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-x86-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/macos-x86-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-x86-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/macos-x86-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-x86-integration-test.yml) |
| macOS ARM | [![Build](https://github.com/zackees/clud/actions/workflows/macos-arm-build.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-arm-build.yml) | [![Lint](https://github.com/zackees/clud/actions/workflows/macos-arm-lint.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-arm-lint.yml) | [![Unit Test](https://github.com/zackees/clud/actions/workflows/macos-arm-unit-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-arm-unit-test.yml) | [![Integration Test](https://github.com/zackees/clud/actions/workflows/macos-arm-integration-test.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/macos-arm-integration-test.yml) |

## Installation

**macOS / Linux**

```bash
curl -fsSL https://raw.githubusercontent.com/zackees/clud/main/install.sh | sh
```

**Windows (PowerShell)**

```powershell
irm https://raw.githubusercontent.com/zackees/clud/main/install.ps1 | iex
```

Both scripts install [`uv`](https://docs.astral.sh/uv) if needed, then `uv tool install clud`, and put `clud` on PATH for new shells. Pin a version with `CLUD_VERSION=2.0.14 curl ... | sh` (POSIX) or `$env:CLUD_VERSION = '2.0.14'; irm ... | iex` (PowerShell). Re-run to upgrade.

Already have a Python package manager? Any of these works equivalently:

```bash
uv tool install clud   # recommended — isolated, fast
pipx install clud      # equivalent if you already use pipx
pip install clud       # plain pip; you must ensure the install bin dir is on PATH
```

## Usage

```bash
clud                              # Launch Claude in YOLO mode via subprocess
clud --codex                      # Use Codex as the backend
clud --claude                     # Use Claude as the backend (default)
clud --pty                        # Force PTY launch mode
clud --subprocess                 # Force subprocess launch mode
clud --detach -p "review this PR" # Start a daemon-managed session without attaching
clud --detachable -p "fix CI"     # Ctrl+C asks whether to keep the session in background
clud --transcript session.log -p "debug this" # Tee daemon session output to a file
clud -c                           # Continue the most recent conversation
clud --resume                     # Resume a session
clud --resume abc123              # Resume a specific session by ID or search term
clud -p "refactor the auth layer" # Run with a prompt, exit when done
clud -m "what does this do?"      # Send a one-off message
clud --model opus -p "review PR"  # Choose a model
clud --safe -p "drop the table"   # Disable YOLO mode (keeps permission prompts)
clud --dry-run -p "hello"         # Print what would run without executing
echo "explain this error" | clud  # Pipe mode: read prompt from stdin
clud -- --verbose --debug         # Pass extra flags through to the backend
clud attach                       # List background sessions you can reattach to
clud attach sess-123              # Attach to a specific session
clud list                         # Show background session IDs, PIDs, and cwd
clud wasm guest.wasm              # Run a local wasm module with clud's embedded runtime
```

### Flags

| Flag | Description |
|------|-------------|
| `-p`, `--prompt` | Run with a prompt, exit when complete |
| `-m`, `--message` | Send a one-off message |
| `-c`, `--continue` | Continue the most recent conversation |
| `-r`, `--resume [TERM]` | Resume by session ID or search term |
| `--claude` | Use Claude as the backend |
| `--codex` | Use Codex as the backend |
| `--subprocess` | Force subprocess launch mode |
| `--pty` | Force PTY launch mode |
| `--detach` | Start a daemon-managed session directly in the background |
| `--detachable` | Run attached under the daemon; `Ctrl+C` prompts whether to background or end |
| `--transcript <PATH>` | Tee daemon-managed session output bytes to a transcript file |
| `--model <NAME>` | Set model preference (e.g., haiku, sonnet, opus) |
| `--safe` | Disable YOLO mode (don't inject `--dangerously-skip-permissions`) |
| `--dry-run` | Print what would be executed, then exit |
| `-v`, `--verbose` | Show debug output |
| `-h`, `--help` | Show help |
| `-V`, `--version` | Show version |

Unknown flags are forwarded directly to the backend agent.

`clud` now defaults to subprocess launch mode for Claude and Codex. Use `--pty`
to opt back into PTY while Claude PTY issues are being investigated.

## Codex Support

![codex-supported](https://github.com/user-attachments/assets/de1e23b4-4513-4c92-ba57-3d9dcd1060b6)

The Rust version of `clud` supports Codex directly. Use `--codex` to switch
backends for interactive runs, prompt-driven execution, resume flows, and
detachable sessions.

## Detached Sessions

Use daemon-managed sessions when you want to disconnect and reattach later.

```bash
clud --detachable --codex -p "refactor the parser"
# press Ctrl+C, then press y within 5 seconds to keep it running in background

clud attach
clud attach sess-123
clud list
```

If you press `Ctrl+C` in a `--detachable` session, clud asks `continue session in
the background?` with a 5-second countdown. Press `y` to background it. Press
`Ctrl+C` again, press anything else, or do nothing to end the session instead.

`clud attach` without a session ID lists background sessions. `clud list` shows
the same sessions with their root PID and current working directory.

## Voice Mode (F3 push-to-talk)

`clud` captures microphone input and transcribes it directly into the active
backend prompt using local `whisper.cpp`. Hold `F3`, speak, release `F3`, and
the transcript appears at your cursor without auto-submitting — you can edit
it before pressing Enter. Available on **all six** supported platforms
(Linux x86/ARM, Windows x86/ARM, macOS x86/ARM). On Linux, microphone capture
uses `arecord` on demand so `libasound` is not required for normal startup.

### Enabling it

The minimum is a single env var:

```bash
export CLUD_VOICE=1
clud
```

```powershell
# Windows PowerShell
$env:CLUD_VOICE = "1"
clud
```

On first F3 press, `clud` auto-downloads the Whisper `ggml-small.en.bin`
model (~466 MB) into a per-OS cache directory and verifies it against a
pinned SHA-256. The download runs in the background as soon as voice mode
starts up, so by the time you reach for `F3` it's usually ready.

| Platform | Cache path |
|----------|-----------|
| Linux | `~/.cache/clud/whisper/ggml-small.en.bin` |
| macOS | `~/Library/Caches/clud/whisper/ggml-small.en.bin` |
| Windows | `%LOCALAPPDATA%\clud\whisper\ggml-small.en.bin` |

If you already have a model on disk, point `CLUD_WHISPER_MODEL` at it and
the auto-download is skipped.

### How `F3` behaves on different terminals

| Terminal | Behavior |
|----------|----------|
| Kitty-protocol terminals (kitty, Ghostty, modern iTerm2, WezTerm, Alacritty with kitty mode) | True press-and-hold: recording stops the instant you release `F3`. |
| Everything else (Windows Terminal / ConPTY, older xterm, etc.) | Press `F3` to start; recording auto-stops after 1.5 seconds of silence (VAD) or 30 seconds maximum, whichever comes first. |

Cues are short tones generated programmatically on macOS/Windows — `ding`
on start (~880 Hz, 90 ms), `dong` on stop (~660 Hz, 120 ms). Linux uses a
terminal bell so `clud` does not link audio output libraries at startup. If
the default audio output device is unavailable, `clud` falls back to a
terminal bell.

### Environment variables

| Variable | Default | Purpose |
|----------|---------|---------|
| `CLUD_VOICE` | unset | Enable voice mode (`1`, `true`, `yes`, `on`). Setting `CLUD_WHISPER_MODEL` also implicitly enables it. |
| `CLUD_WHISPER_MODEL` | auto-managed cache path | Override the model location. Trusted as-is — no hash check on user paths. |
| `CLUD_VOICE_LANGUAGE` | inferred (English with `small.en`) | Force a Whisper language code, e.g. `en`, `de`, `fr`. |
| `CLUD_VOICE_TEST_TRANSCRIPT` | unset | Test-only bypass: replaces real transcription with this exact string. Used by the integration test suite. |

### Troubleshooting

- **Nothing happens when I press F3.** Check that `CLUD_VOICE=1` is exported in the same shell. On Linux, install `alsa-utils` so `arecord` is available, then verify a default input device exists (`arecord -l` on Linux, "Sound" preferences on macOS/Windows).
- **"voice mode is enabled but the Whisper model is not yet available"** — the auto-download hasn't finished. Watch stderr for `[clud] voice: download N% (...)` lines, or pre-seed the cache path manually with `curl -L -o <cache-path> https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin`.
- **Recording keeps stopping mid-sentence on non-kitty terminals.** The VAD silence window is 1.5 s — pause less, or switch to a kitty-protocol terminal for true hold-to-record.
- **Transcript is empty / garbage.** Whisper struggles on very short utterances and noisy backgrounds. The `MIN_CAPTURE_MS` floor (150 ms) silently drops sub-150 ms blips; speak for at least half a second.

## `clud loop` — The Ralph Loop

![clud-loop-ralph](https://github.com/user-attachments/assets/b6666429-ead7-419c-831f-db4e17b3840b)

Run the backend in a **ralph loop**: iterate on a task until the agent signals
it's done, or until the iteration count runs out. Fully autonomous — no user
interaction between iterations.

```bash
clud loop "Implement the API endpoints from the spec"
clud loop TASK.md                                  # Read prompt from a file
clud loop https://github.com/org/repo/issues/42    # Fetch & iterate on a GH issue
clud loop --loop-count 10 "fix bugs"               # Custom iteration count
```

### Task input modes

The positional argument is classified in this order:

1. **GH issue / PR URL** — the issue body is fetched via `gh` and cached to
   `<git-root>/.clud/loop/<owner>__<repo>__issue-<n>.md`. Subsequent runs
   reuse the cache; pass `--refresh` to force a re-fetch.
2. **Short form `#42`** — resolves `owner/repo` via `gh repo view`.
3. **Local file path** — read as the prompt.
4. **Literal string** — used as-is.

### Completion signal (DONE / BLOCKED marker files)

`clud loop` injects a short contract into the prompt asking the agent to write
one of two marker files under `<git-root>/.clud/loop/`:

| Marker    | Meaning                                    | Exit code |
|-----------|--------------------------------------------|-----------|
| `DONE`    | Task resolved (one-line summary inside)    | 0         |
| `BLOCKED` | Agent can't proceed (reason inside)        | 3         |
| (neither) | Iteration count exhausted                  | 2         |
| non-zero backend exit | Infra failure                  | propagate |

Stale `DONE` / `BLOCKED` files from a prior run are cleared at start so the
loop can't short-circuit on iteration 1.

Opt out with `--no-done-marker` to restore the old "run N times unless the
backend fails" behavior.

## `clud rebase` — Auto-Rebase

Fetches from origin, rebases the current branch, and resolves conflicts.

```bash
clud rebase
```

## `clud fix` — Auto-Fix

Detects linting and test tools in your repo, runs them, and fixes failures in a loop until everything passes.

```bash
clud fix
```

## `clud up` — Ship It

Runs lint, test, cleanup, then commits.

```bash
clud up
```

## `clud wasm` â€” Embedded Runtime

Loads a local `.wasm` module, wires up a host logging import, and invokes an exported function.

```bash
clud wasm hello.wasm
clud wasm hello.wasm --invoke _start
```

## Development

```bash
bash build                  # Build dev wheel (Rust binary + Python package)
bash lint                   # Lint (cargo fmt + clippy + ruff + banned imports)
bash test                   # Unit tests (Rust + Python)
bash test --integration     # Include integration tests with mock agents
```


## License

Clud Proprietary License. Free use is available for individuals and
organizations under 20 people, with lifetime grandfathering for organizations
that qualified before growing beyond that size. Larger organizations need a
commercial license unless they have a grandfathered or contributor-granted free
license. See [LICENSE](LICENSE) for the full terms.
