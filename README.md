# clud

![hero-clud](https://github.com/user-attachments/assets/4009dfee-e703-446d-b073-80d826708a10)

**A luxury agentic experience with Claude, Codex and Deepseek. By running them all on Claude**

  * Fixes Windows Performance Problem with Windows git/bash zombie process
  * transltate codex and deepseek into claude terminal

The name `clud` is simply a shorter, easier-to-type version of `claude`.

## Built in support for running codex on the claude harness:

`clud --codex --harness claude`

## A /goal tuned for one task: solve it and push and merge the PR

`clud do github.com/zackess/isssu/123`

## Grind down your bug list starting with the easy one

`clud grind`

[![CI](https://github.com/zackees/clud/actions/workflows/ci.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/ci.yml)
[![Auto Release](https://github.com/zackees/clud/actions/workflows/auto-release.yml/badge.svg)](https://github.com/zackees/clud/actions/workflows/auto-release.yml)

CI builds each of the six target triples once on Linux and executes the result
on native Linux/Windows/macOS runners — see
[docs/architecture/ci.md](docs/architecture/ci.md).

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
clud --deepseek                   # Use DeepSeek through the Claude harness
clud --kimi                       # Use Kimi through the Claude harness
clud --openrouter                 # Use Claude through OpenRouter
clud --pty                        # Force PTY launch mode
clud --subprocess                 # Force subprocess launch mode
clud --pty --graphics=sixel       # Force a Sixel header above PTY output
clud --demo-gfx-sixel             # Render the README hero image as a Sixel demo
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
| `--deepseek` | Use DeepSeek's Anthropic-compatible API through Claude Code |
| `--kimi` | Use Kimi's Anthropic-compatible API through Claude Code |
| `--openrouter` | Use OpenRouter's Anthropic endpoint through Claude Code |
| `--subprocess` | Force subprocess launch mode |
| `--pty` | Force PTY launch mode |
| `--graphics <auto\|off\|sixel>` | Control PTY graphics headers. `auto` only enables Sixel from a live terminal probe |
| `--graphics-image <PATH>` | Render a custom image as the PTY graphics header when Sixel is enabled |
| `--demo-gfx-sixel` | Render the README hero image as a standalone Sixel demo and exit |
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

### OpenRouter through Claude Code

OpenRouter uses its own API key; a DeepSeek or Anthropic key cannot authenticate
there. Store it in the native credential vault, then launch:

```bash
clud auth login openrouter
clud --openrouter
```

Clud connects Claude Code directly to `https://openrouter.ai/api`, explicitly
clears inherited Anthropic API-key auth, and enables gateway model discovery.
The reviewed default is OpenRouter's Claude Sonnet alias; Fable, Opus, Sonnet,
Haiku, and subagent roles use OpenRouter's documented Anthropic aliases. OpenRouter
only guarantees Claude Code compatibility through Anthropic's first-party
provider, so arbitrary non-Claude models are best-effort. If Claude Code has a
cached Anthropic login and reports authentication or model-not-found errors,
run `/logout` once inside Claude Code, exit, and relaunch `clud --openrouter`.
Use `clud --claude` to return to native Claude routing.

`clud` now defaults to subprocess launch mode for Claude and Codex. Use `--pty`
to opt back into PTY while Claude PTY issues are being investigated.

## PTY Graphics Headers

PTY sessions can reserve a small header area above the backend terminal and
draw a Sixel image there. `--graphics=auto` is conservative: it only enables
the header when `running-process` reports Sixel as supported from a live probe
of the current terminal. Missing metadata, non-TTY attaches, blocked terminals,
and host-name-only hints stay text-only. Use `--graphics=off` to disable the
feature or `--graphics=sixel` to force it.

By default clud renders the bundled README hero image. Pass
`--graphics-image <PATH>` to use a PNG or JPEG instead. Direct PTY launches
reserve rows before the backend starts. Daemon-managed sessions decide at attach
time, because the
detached worker does not know which terminal will attach later; reattach and
resize paths redraw the header where the terminal reports support.

## Codex Support

![codex-supported](https://github.com/user-attachments/assets/de1e23b4-4513-4c92-ba57-3d9dcd1060b6)

The Rust version of `clud` supports Codex directly. Use `--codex` to switch
backends for interactive runs, prompt-driven execution, resume flows, and
detachable sessions.

### Codex through Claude Code (experimental)

To use a Codex model with Claude Code's harness features, opt in explicitly:

```bash
clud --codex --harness claude
clud --codex --harness claude --model terra@high
```

Provider and harness are separate choices. `--harness default` restores the
provider's native harness for one launch. An explicit session/global choice is
offered interactively; global choices are stored in `~/.clud/settings.json`.
When a saved non-default harness is used on a TTY, clud prints a green
`[clud] Harness override: Claude (global setting)` notice. CLI flags always
override saved settings.

Use a platform API key by setting `OPENAI_API_KEY` in the launch environment.
ChatGPT subscription login is experimental and compatibility-sensitive:

```bash
clud codex-auth login --acknowledge-experimental
clud codex-auth status
clud codex-auth logout
```

The subscription record is clud-owned and never falls back silently to an API
key. `logout` removes only clud's record, not Codex CLI credentials.

#### Cross-route troubleshooting

- **Claude executable missing:** install Claude Code and ensure `claude` is on
  PATH. Use `clud --codex --harness default` while fixing the installation.
- **Unsupported pair:** only Codex provider through Claude is supported. Claude
  provider through Codex is rejected before launch.
- **Login expired:** run `clud codex-auth login --acknowledge-experimental`;
  do not expect an existing subscription record to fall back to an API key.
- **Callback ports occupied:** free 1455 or 1457, then retry login. The command
  uses 1455 first and 1457 only as its fallback.
- **Bridge start or upstream failure:** run `clud --dry-run --codex --harness
  claude` to inspect the resolved target. Check proxy/firewall rules permit the
  configured OpenAI endpoint; upstream 4xx errors usually require a credential,
  model, or request change, while transient 5xx/429 failures are retried only
  before output begins.
- **Disable/rollback:** pass `--harness default` or reset the stored harness in
  `clud settings`. Native `clud`, `clud --claude`, and `clud --codex` launches
  are unaffected.

Compatibility evidence, security boundaries, and the no-sidecar design live in
[the Codex-via-Claude architecture document](docs/architecture/codex-via-claude.md).

### Codex Hook Warnings

On `clud --codex` launches, clud runs a lightweight hook-health check before
starting Codex. The check compares Claude Code and Codex `PreToolUse` hook
coverage and inspects Codex hook trust state. These warnings are informational;
normal launch continues unless the backend itself fails.

If Codex has `PreToolUse` hooks but Claude Code does not, clud prints:

```text
[clud] warning: Codex PreToolUse hooks exist, but Claude PreToolUse hooks are missing or inactive. Run `clud --fix-hooks`.
```

Run `clud --dry-run --fix-hooks` to see the planned repair actions. Run
`clud --fix-hooks` only when you want clud to add deterministic Codex trust
entries or ask the selected backend to translate a missing hook between Claude
Code and Codex.

Codex hook matchers may use `*` as a catch-all. When equivalent Claude Code
hooks use the same command across several tool matchers, the Codex repair plan
prefers one catch-all hook instead of repeated per-tool prompts. That reduces
duplicate Codex hook review/trust approvals while keeping per-tool hooks when
commands differ.

On Windows, Codex hook commands that call a `.cmd` or `.bat` wrapper need
explicit exit-code propagation. If the command does not include
`$LASTEXITCODE`, clud warns:

```text
[clud] warning: Codex hook command in C:\Users\you\.codex\hooks.json uses a Windows batch wrapper without explicit `$LASTEXITCODE` propagation; a blocking hook may fail open.
```

Fix the hook by invoking a native executable directly, or by making the
PowerShell hook command end with `exit $LASTEXITCODE` after the batch wrapper.
Without that, a hook intended to block a tool call can return success to Codex
after the wrapper fails.

## `clud-cmd-scan` — Command Guard

Agents sometimes reach for the wrong command — bare `cargo` when your repo
needs `soldr cargo`, or `playwright` when you have a faster test script.
`clud-cmd-scan` catches those *before* they run. It's a hook that reads every
shell command the agent is about to execute, and for the ones you've banned it
blocks the command and tells the agent what to run instead.

clud ships with a few built-in rules (bare `cargo`/`rustc` → `soldr cargo`,
whole-disk `find /`). You add your own in `.clud/settings.json`.

### Turn it on

The guard runs as a Claude Code hook. Add it once to `~/.claude/settings.json`
(covers every repo) or to a single repo's `.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [{ "type": "command", "command": "clud-cmd-scan", "timeout": 30 }]
      }
    ]
  }
}
```

If you ever see `clud-cmd-scan: command not found`, the guard isn't installed
and is doing nothing — run `uv tool install --force clud`. (If you have an old
config that calls `block-bad-cmd`, clud quietly upgrades it to `clud-cmd-scan`
the next time it launches.)

### Write your first rule

A rule says: *when the agent runs command X, block it and suggest Y.* Put it in
`bad_commands`:

```json
{
  "bad_commands": [
    {
      "id": "no-raw-playwright",
      "match": "playwright",
      "replacement": "npm run test:integration",
      "reason": "use the blessed pipeline; raw playwright is slower.",
      "allow_override": true
    }
  ]
}
```

Now when the agent tries to run `playwright`, it gets stopped:

```text
$ playwright test
use the blessed pipeline; raw playwright is slower. Use `npm run test:integration` instead.
Blocked `playwright` by rule `no-raw-playwright`
from `/repo/.clud/settings.json#/bad_commands/0`.
```

The block message always names the exact rule and file that fired, so you're
never left guessing why something was denied.

**One thing to know about `match`:** it only looks at the *program being run* —
the first word of the command — not the whole line. So a rule for `playwright`
stops `playwright test`, but leaves `rg playwright` and `grep playwright .`
alone, because there you're *searching for* the word, not running the program.
By default `match` is a glob (`*`, `?`), matching the whole program name; add
`"match_mode": "regex"` if you need a regex.

You don't have to worry about the agent sneaking a command past the rule with
`&&`, pipes, `bash -c '...'`, `$(...)`, `eval`, and so on — the scanner unwraps
all of those and checks inside. Words merely quoted in an `echo` are left alone.

### Where rules live

You can define rules in three files. They **add together** — a rule in any of
them applies:

| File | Use it for |
| --- | --- |
| `~/.clud/settings.json` | rules you want on your own machine, everywhere |
| `<repo>/.clud/settings.json` | rules for the whole team — commit it |
| `<repo>/.clud/settings.local.json` | your personal rules for one repo — gitignored |

If two rules share an `id`, the more specific file wins (repo over user).

### Block only the *dangerous* form of a command

Sometimes `git push` is fine but `git push --force` isn't. Add an `arguments`
block to match on the flags:

```json
{
  "bad_commands": [
    {
      "id": "no-force-push",
      "match": "git",
      "arguments": {
        "ordered": ["push"],
        "any": ["--force", "-f"],
        "none": ["--force-with-lease"]
      },
      "replacement": "git push --force-with-lease",
      "reason": "unconditional force pushes can overwrite remote work."
    }
  ]
}
```

This blocks `git push --force` but lets `git push --force-with-lease` and
`git status` through. The building blocks: `any` (at least one is present),
`all` (all present), `none` (none present), and `ordered` (these appear in this
order, gaps allowed). Everything you list must hold at once. See DD-017 for the
rest (`prefix`, `contiguous`, short-flag bundles like `-rf`, and looking past
wrappers like `sudo`).

### Block a *pipe* between two commands

Some risks are about two commands together — like piping a download straight
into a shell. Those go in `bad_pipelines`:

```json
{
  "bad_pipelines": [
    {
      "id": "no-download-to-shell",
      "stages": [
        { "match": "curl" },
        { "match": "^(?:ba)?sh$", "match_mode": "regex" }
      ],
      "replacement": "download the script, inspect it, then run it",
      "reason": "piping downloaded content into a shell hides executed code."
    }
  ]
}
```

This trips on `curl ... | sh` (and `| bash`). A `|` inside a quoted string
doesn't count as a real pipe.

### Let the agent through, just this once

If a rule sets `"allow_override": true`, you can wave a single command past it
by setting an environment variable — with a reason:

```bash
CLUD_BAD_CMD_OVERRIDE="no-raw-playwright:debugging a trace-viewer bug"
```

The `id:reason` form is required; an override with no reason is ignored and the
command stays blocked. It has to be a real environment variable — you can't
just type it in front of the command.

### What it is (and isn't)

This is a guardrail for a *cooperative* agent, not a security wall. If it can't
parse a command it lets it through (fails open), and it makes no attempt to
stop someone deliberately hiding a command — via a shell variable, base64, a
throwaway script, etc. Its whole job is to stop an agent from *accidentally*
grabbing the wrong tool.

For the complete field reference, see **DD-016** and **DD-017** in
[`docs/DESIGN_DECISIONS.md`](docs/DESIGN_DECISIONS.md).

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

### Daemon idle lifetime

The daemon starts on demand and, by default, exits after 15 minutes with no active work. This
releases its GC database and background resources; the next normal daemon-backed command starts
it again transparently. Active foreground clients, detached/repeat sessions, dashboard or top
polling, RPC connections, and maintenance prevent this shutdown. Configure
`daemon.idle_timeout_secs` in `~/.clud/settings.json` to another positive number of seconds, or
set it to `0` to disable idle retirement.

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

### In-chat `/loop` for Codex models

Codex does not ship a `/loop` command. clud used to fill that gap with a bundled
`clud-loop` skill; it is **retired** as of the `--harness claude` cross-route.
Run Codex models under the Claude harness and you get the harness's own `/loop`:

```bash
clud --codex --harness claude
```

Retiring it also fixes a mis-fire: the skill's triggers keyed on the word
"Codex", so a Codex model driving the *Claude* harness would pick the polyfill
over the harness's native `/loop`. clud purges the installed copy from both
`~/.claude/skills/` and `~/.codex/skills/` on next launch, leaving any copy you
edited yourself in place.

For a plain `clud --codex` session with no cross-route, the external runner is
still there:

```bash
clud --codex loop .clud/loop/LOOP.md
clud --codex loop --repeat 30m --loop-count 1 --no-done .clud/loop/LOOP.md
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

## `clud do <url>` — Implement to a Merged PR

Launches the agent with the `/goal` implementation contract after substituting
the supplied URL into the prompt.

```bash
clud do https://github.com/zackees/clud/issues/866
clud do --dry-run https://github.com/zackees/clud/issues/866
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
organizations under 6 people, with lifetime grandfathering for organizations
that qualified before growing beyond that size. Larger organizations need a
commercial license unless they have a grandfathered or contributor-granted free
license. See [LICENSE](LICENSE) for the full terms.
