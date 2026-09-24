# Kitty-style Windows terminal parity (#1282)

This is the acceptance matrix for [issue #1282](https://github.com/zackees/clud/issues/1282). Its reference is the tracked NixOS [Kitty configuration](https://github.com/zackees/nixos/tree/main/home/kitty), especially `kitty.conf`, `tab_bar.py`, `window_title_bar.py`, `paste.py`, and `keys.py`, plus the `kitty --single-instance` desktop launcher in `system/configuration.nix`. The local NixOS checkout supplied these details; Windows behavior still needs a native graphical test. Treat “implemented in source” as weaker than “verified on Windows.” The requested user-facing Windows entry point is `clud --kitty-term`.

The current clud path is [`--web-term`](web-terminal.md): the CLI launches a Tauri companion; `portable-pty` owns one PTY per tab and xterm.js renders it. `--kitty-term` is not implemented yet; its launch behavior must be tested independently while preserving the existing `--web-term` contract. The companion's Windows `--startup-check` only proves that the executable and manifest load. It does **not** prove a visible window, working ConPTY input/output, or Kitty protocol support. The companion currently has a `+` tab button, tab selection/close confirmation, PTY resize, and process-exit feedback. A newly opened tab starts the default shell (`cmd.exe` on Windows); the first tab runs the forwarded clud command.

## Parity matrix

“Present” means visible in clud source, not yet verified in a native Windows GUI. “Missing” means no equivalent behavior is implemented in the current companion. The implementation may be a WezTerm fork or an owned ConPTY frontend; matching the user action matters more than sharing Kitty's config syntax.

| NixOS Kitty behavior | Current clud companion | Windows acceptance check |
|---|---|---|
| Single launcher opens another OS window in the same terminal process, allowing pane moves across windows | Missing; one companion process/window is launched per clud invocation | Launch twice; confirm both windows work and a pane can move between them without losing its process |
| One PTY per tab; new tabs and OS windows inherit the active pane's cwd | Tabs/PTYs present; new tabs start a shell in the companion's process cwd | Open a tab/window after changing directories; verify the inherited cwd and independent PTY lifetime |
| Horizontal/vertical/automatic splits (`F5`, `F6`, `Alt+Enter`, `Super+Enter`) | Missing | Split in each direction, verify correct placement and inherited cwd |
| Navigate, move, resize, zoom, close, and detach panes; `splits`, `tall`, `fat`, `grid`, `stack` layouts | Missing | Exercise each mapped action with three panes, including detaching to a tab or OS window and moving back |
| Drag pane title bars within/between tabs and OS windows, including tear-out/rejoin | Missing | Drag onto pane edges, title bars, tabs, and the desktop; verify the pane stays live |
| Per-pane title always shows stable OSC 7 shell cwd, Git branch (including worktrees), foreground command, focus/activity/progress | Missing; only tab labels (`clud`/`shell`) and process-exit badges are rendered | Run a long command in one pane while another is active; labels must retain the checkout and branch and update after `cd`/checkout |
| Tab labels use active pane cwd plus command, show when at least two tabs exist; tab index and key/menu hint | Partial tabs; fixed labels, tab bar always visible | Create and close a second tab; verify label content, visibility, and discoverability hint |
| Scrollback 100,000 lines, pager history, fast wheel scrolling, last-command output | Partial scrollback (10,000 xterm.js lines); other actions missing | Produce >10,000 lines, search/view history and last command output, and check wheel behavior |
| Font/theme: JetBrainsMono Nerd Font, Breeze Dark, beam cursor, padding, active borders, truecolor contrast floor | Different hard-coded Cascadia/Consolas theme; Kitty settings absent | Compare a color/font/cursor swatch and unreadable dark-blue link against reference; verify focus cues |
| Keyboard, graphics, hyperlinks and mouse reporting expected by Kitty-aware TUIs | Unverified; xterm.js renderer does not establish Kitty protocol parity | Run protocol probes and representative Claude, Codex, vim/tmux, image and OSC 8 samples on a native Windows GUI |
| `Ctrl+V`/`Super+V`/middle-click/`Shift+Insert` smart paste: save clipboard images as unique files and paste path; text passes through | Missing | Paste text, PNG/JPEG/WebP/GIF and rapid repeated images; verify distinct files/paths and no data loss |
| Paste strips dangerous controls, quotes URLs at prompts, confirms only large payloads; optional preview confirmation | Missing | Paste control bytes and >16 KB text; inspect exact bytes delivered and cancel behavior |
| Copy on selection, `Super+C` copy, right-click disabled, middle-click clipboard paste even under mouse capture | Missing | Select/copy, right/middle click in shell and mouse-aware TUI; verify clipboard and event routing |
| OSC 8 link opens with ordinary click even in a mouse-grabbing app; NixOS uses a desktop-local Brave launcher | Missing; desktop-local Brave placement is NixOS/KWin-specific | Click OSC 8 links in shell and mouse-aware TUI; open on current Windows desktop using its default browser policy |
| Command palette and generated `Super+/` key cheat sheet | Missing | Find and execute every supported action by name; check shortcut list reflects real bindings |
| Window close confirmation, session activity/bell/progress indications, hot config reload | Partial tab-close confirmation and exit badge; remaining behavior unverified/missing | Close a running pane/window and cancel; trigger bell/progress; change config without dropping sessions |

The NixOS clipboard popup's KWin placement script and its shell/Wayland integration are platform-specific mechanisms. Port their observable behavior where applicable; do not transplant those scripts into Windows.

## Reproduction and decision gates

1. On a native Windows desktop, record OS build, clud wheel/version, GPU/renderer, command line, stdout/stderr and screenshots. Check `clud-webterm.exe --startup-check`, `clud --web-term --codex`, and the new `clud --kitty-term --codex` for visible startup, then sustain interactive use (input, resize, tabs, Unicode, clipboard, links, graphics). The startup check is only a loader smoke test.
2. Reproduce the reported Kitty and WezTerm failures independently: native process start, visible window, shell/ConPTY behavior, then representative Kitty graphics and keyboard probes. Capture exact error and lowest failing layer. Do not infer a shared cause from “both broken.”
3. Spike embedding `wezterm-term` against clud's existing PTY/frontend. Check whether it provides the required renderer, keyboard encoding, graphics and UI surfaces, and whether the fork can build and run on Windows. A terminal state engine alone does not supply a GUI. Record a reproducible pass/fail for each requirement before choosing it.
4. Choose the smallest working Windows path: a WezTerm Rust fork when it can deliver the native UI and protocol behavior; otherwise retain ConPTY and implement the missing protocol/UI behavior in a maintained frontend. Add `clud --kitty-term` to CLI parsing, launch dispatch, packaging and recursion protection while preserving the existing `--web-term` contract.
5. Add focused RED→GREEN tests for the chosen path and run the relevant local checks. A native Windows GUI smoke plus interactive parity checks above are release gates; a cross-compiled wheel, `--startup-check`, headless CI or Linux Docker run cannot substitute for them.

Update each matrix status with the test command/evidence as behavior lands. The current source review establishes only the “Present,” “Partial,” and “Missing” implementation observations above.
