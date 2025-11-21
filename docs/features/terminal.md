# Terminal Console

The Web UI includes an integrated terminal console that provides direct shell access alongside the chat interface.

## Features

- **Multiple Terminals**: Create multiple terminal sessions with tabbed interface
- **Split-Pane Layout**: Adjustable resize handle between chat and terminal panels
- **Full Shell Access**: Real PTY (pseudo-terminal) with ANSI color support
- **Cross-Platform**: Works on Windows (git-bash/cmd) and Unix (bash/zsh/sh)
- **Responsive Design**: Stacks vertically on mobile devices

## Usage

### Terminal Management

- **New Terminal**: Click the "+" button in the terminal tabs area
- **Switch Terminals**: Click on tab to switch between terminals
- **Close Terminal**: Click "×" on the tab to close a terminal
- **Clear Terminal**: Click the trash icon (🗑️) to clear the active terminal
- **Toggle Panel**: Click the arrow icon (⬇️) to collapse/expand terminal panel
- **Resize Panels**: Drag the vertical resize handle between chat and terminal

### Keyboard Shortcuts

- All standard terminal shortcuts work (Ctrl+C, Ctrl+D, Ctrl+Z, etc.)
- Tab completion, command history (↑/↓), and line editing work as expected
- Copy/paste: Use browser's standard shortcuts (Ctrl+C/V or Cmd+C/V)

## Shell Behavior

- **Windows**: Automatically uses git-bash if available, falls back to cmd.exe
- **Unix/Linux**: Uses user's default shell ($SHELL) or /bin/bash
- **Working Directory**: Terminals start in the selected project directory
- **Environment**: Inherits environment variables from the Web UI server

## Architecture

### Frontend

- **xterm.js** terminal emulator
- **FitAddon** for responsive sizing

### Backend

- **PTY Manager** (`pty_manager.py`) with platform-specific implementations:
  - **Unix**: Native `pty.fork()` with file descriptor I/O
  - **Windows**: `pywinpty` library wrapping Windows ConPTY

### Communication

- **WebSocket endpoint** (`/ws/term`) for real-time I/O streaming
- **Terminal Handler** (`terminal_handler.py`) bridges WebSocket and PTY with async I/O

## Components

- `src/clud/webui/pty_manager.py` - Cross-platform PTY session management
- `src/clud/webui/terminal_handler.py` - WebSocket handler for terminal I/O
- `src/clud/webui/frontend/src/lib/components/Terminal.svelte` - Terminal component (Svelte)
- `tests/test_pty_manager.py` - PTY manager unit tests
- `tests/test_terminal_handler.py` - Terminal handler unit tests

## Security Considerations

- **Localhost Only**: Terminal provides full shell access - only run on trusted localhost
- **No Authentication**: Current implementation has no authentication mechanism
- **Network Deployment**: Requires authentication, resource limits, and security hardening
- **Working Directory Validation**: Terminal starts in validated project directory
- **Environment Inheritance**: Shell inherits all environment variables from server

## Troubleshooting

### Terminal Not Appearing

Check browser console for WebSocket connection errors.

### Commands Not Working

Verify shell is running (check for shell prompt).

### Garbled Output

Ensure terminal is properly sized (resize window to trigger refit).

### Windows Issues

Ensure git-bash is installed at `C:\Program Files\Git\bin\bash.exe`.

### Connection Lost

Terminal will show "[Connection closed]" - create a new terminal tab.

## Related Documentation

- [Web UI](webui.md)
- [Development Setup](../development/setup.md)
- [Architecture](../development/architecture.md)
