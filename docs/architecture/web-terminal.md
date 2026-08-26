# Web terminal

`clud --web-term` starts the desktop `clud-webterm` companion instead of
running the backend in the invoking console. The companion creates a PTY per
tab and runs the original `clud` command inside it, so normal launch planning,
backend selection, and terminal behavior remain owned by the CLI.

`clud --set-web-term` persists `web_term.enabled` in the global settings file.
With that preference enabled, a bare `clud` opens the companion; commands and
subcommands continue to run in the invoking terminal. `CLUD_WEBTERM=1` is set
inside each companion tab, which prevents the inner `clud` process from opening
another companion.

The web terminal is a desktop-wheel companion: Windows and macOS wheels bundle
it adjacent to `clud`; Linux and headless installs report that the companion is
unavailable instead of silently changing command execution. The companion is
added after the main wheel build and its wheel `RECORD` is regenerated. The
Linux-to-Windows cross-build writes and links a Common Controls v6 manifest
without relying on a host resource compiler; this is required before Windows
can resolve Tauri's `comctl32!TaskDialogIndirect` import (#1033).
