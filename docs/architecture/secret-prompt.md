# Secure credential prompt (#1708)

`clud secret` opens a native masked credential window for an authorized
operation. It supports passwords/tokens and `--multiline` private-key input.
The prompt shows the requested host/purpose (`--label`) and destination.

Choose exactly one sink:

- `--to-file PATH`: the existing private atomic writer creates owner-only
  files (0600 on Unix, protected owner DACL on Windows).
- `--vault deepseek|kimi|openrouter`: the provider's existing native vault
  record, with its stable service/account identifiers.
- `--to-command PROGRAM ARGS...`: send the exact value to command stdin,
  close the pipe, discard both output streams, and report the exit outcome.
  This option owns the rest of the command line, including flags.

For example:

```bash
clud secret --label 'GitLab token for gitlab.example.com' --to-command glab auth login --hostname gitlab.example.com --stdin
clud secret --label 'SSH private key' --multiline --to-command ssh-add -
```

The normal CLI and `clud-secret` alias print only `stored` (exit 0),
`cancelled` (exit 3), or `failed` (exit 1). Invalid options exit 2. Cancellation
leaves sinks untouched. A command sink's output is suppressed even on failure,
because authentication tools may echo input in their diagnostics. Its deadline
covers stdin writing, draining and waiting. Inputs are capped at 1 MiB, reject
NUL, and retain multiline bytes exactly. Empty input cannot be submitted.

`clud-askpass` implements the separate askpass protocol: configure it as
`GIT_ASKPASS`, `SSH_ASKPASS`, or `SUDO_ASKPASS` for the intended operation.
It returns the value through stdout to the requesting tool, as required by
that protocol. It refuses direct agent/tool callers and accepts git/ssh/sudo
ancestors through a bounded shell-wrapper chain. This is an accidental-exposure
guard, not a boundary against a malicious local process impersonating a caller.
Never run askpass directly from a captured agent shell.

The native window uses eframe/egui with password widgets for single and
multiline input, built into the existing multicall executable. UI persistence
is disabled. Secret buffers are zeroized when released, and have no Debug or
serialization surface. No credential value is accepted on the command line.
A missing desktop or unavailable graphics context fails clearly; this command
does not fall back to echoing input in a terminal or asking through chat.

On Linux, the desktop runtime needs libxkbcommon; X11 additionally needs
libxkbcommon-x11. clud checks these before entering winit, whose X11 keyboard
loader otherwise panics on a missing library. A Wayland session does not
require the X11 library, including when XWayland also sets `DISPLAY`.
Linux CI installs the runtime and Xvfb so the native window probe executes.
Windows uses wgpu's Direct3D 12 backend, which can use the system WARP software
adapter. Its FXC shader compiler requires no additional compiler DLL. macOS
and Linux retain the OpenGL renderer.

The automatically discoverable `clud-secret` bundled skill documents sink
selection and status-only verification. Prompting does not itself authorize
logging into an unrelated service. Credentials already pasted into chat should
be rotated.

Debug builds with `CLUD_INTEGRATION_TESTS=1` support a file-backed prompt and
cancellation/timeout fixtures. Release builds ignore these seams. Tests use
fake credentials, check output suppression even when a sink echoes input,
preserve multiline file contents, exercise cancellation and blocked stdin,
and inspect masked widget output without opening a desktop window. A separate
native-window probe renders and delivers a fake multiline value on graphical
runners, using xvfb-run on Linux when available. A second Linux probe runs a
headless Weston compositor with software OpenGL, removes `DISPLAY`, and opens
the window through Wayland. It waits on compositor startup output with a
deadline and always terminates the compositor. Headless skips do not establish
native window coverage.
The native CI setup sets `CLUD_TEST_REQUIRE_SECRET_GUI=1`: there a missing display,
runtime or graphics context fails the probe rather than skipping coverage.
