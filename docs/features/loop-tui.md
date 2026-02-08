# Loop TUI Mode

Terminal user interface for `clud --loop` mode providing an interactive, split-pane experience for managing agent loops.

## Overview

The Loop TUI feature provides a modern terminal user interface for running `clud` in loop mode. It splits the terminal into two areas:
- **90% Output Area**: Streaming display of Claude Code responses and system messages
- **10% Interactive Menu**: Keyboard-navigable menu for loop control

## Usage

Launch the TUI mode with the `--loop-ui` flag:

```bash
clud --loop --loop-ui
```

Or use the shorter form:

```bash
clud --loop --ui
```

## Features

### Split-Pane Layout
- **Output Area**: Real-time streaming of Claude Code agent responses
- **Menu Area**: Bottom-anchored interactive menu with keyboard navigation
- **Responsive Design**: Automatically adapts to terminal size changes
- **Minimum Size Warning**: Alerts when terminal is smaller than 80x20

### Interactive Menu System
- **Main Menu**: Primary navigation options
- **Options Submenu**: Additional controls for loop management
- **Keyboard Navigation**: Full keyboard control without mouse dependency
- **Visual Feedback**: Clear highlighting of selected menu items

### Real-Time Streaming
- **Non-Blocking Output**: Claude Code responses stream in real-time
- **Auto-Scroll**: Output automatically scrolls to show latest content
- **Syntax Highlighting**: Markdown and code formatting preserved
- **Loading Indicators**: Visual feedback for async operations (⏳ loading, ✓ complete)

### Editor Integration
- **UPDATE.md Editing**: Open UPDATE.md in your default editor from within the TUI
- **Auto-Creation**: Creates UPDATE.md if it doesn't exist
- **Terminal Suspension**: Suspends TUI while editor is active
- **Seamless Return**: Returns to TUI after editor closes

### Cross-Platform Support
- **Windows**: Full support in git-bash, PowerShell, and Windows Terminal
- **macOS**: Works in Terminal.app, iTerm2, and other terminals
- **Linux**: Compatible with gnome-terminal, konsole, xterm, and more
- **UTF-8 Encoding**: Proper handling of Unicode characters across platforms

## Keyboard Shortcuts

### Navigation
- `↑` / `↓` - Navigate menu items (vertical)
- `←` / `→` - Navigate menu items (horizontal)
- `Tab` - Move to next menu item
- `Enter` - Select highlighted menu item
- `Esc` - Back to previous menu

### Tips
- Menu navigation wraps around (going left from first item goes to last item)
- Arrow keys and Tab can be used interchangeably
- Esc in main menu exits; Esc in submenu returns to main menu

## Menu Structure

### Main Menu
- **Options** - Opens the options submenu for additional controls
- **Exit** - Quits the clud --loop session

### Options Submenu
- **← Back** - Returns to main menu
- **Edit UPDATE.md** - Opens UPDATE.md in your default editor ($EDITOR or platform default)
- **Halt** - Stops the loop gracefully and exits
- **Help** - Displays help information with all keyboard shortcuts

## System Requirements

### Minimum Requirements
- **Terminal Size**: 80 columns × 20 rows (recommended: 100×30 or larger)
- **UTF-8 Encoding**: Required for proper display of Unicode symbols
- **Python**: 3.10 or later
- **Dependencies**: textual>=0.47.0 (installed automatically)

### Optional Requirements
- **ANSI Color Support**: For enhanced visual appearance
- **$EDITOR Environment Variable**: For custom editor preference
- **Git Bash**: Recommended on Windows for best experience

## Technical Details

### Architecture
The Loop TUI is built on the [Textual framework](https://textual.textualize.io/), which provides:
- Async architecture for non-blocking streaming
- CSS-like layout system for responsive design
- Rich text rendering with syntax highlighting
- Built-in event handling for keyboard navigation

### Implementation
- **Module**: `src/clud/loop_tui/`
- **Main App**: `src/clud/loop_tui/app.py` - CludLoopTUI class
- **Integration**: `src/clud/loop_tui/integration.py` - CLI integration
- **Worker**: `src/clud/loop_tui/loop_worker.py` - Background loop worker
- **API**: `src/clud/loop_tui/__init__.py` - Public API using lazy-loading proxy pattern

### Testing
Comprehensive test coverage with 74 tests across multiple categories:
- **Unit Tests**: 35 tests for core TUI and worker functionality
- **Snapshot Tests**: 15 tests for visual regression testing
- **E2E Tests**: 24 tests for complete user workflows

Run tests with:
```bash
bash test              # Unit tests only
bash test --full       # Full test suite including E2E
```

## Examples

### Basic Usage
Start a loop with the TUI:
```bash
clud --loop --loop-ui
```

### Custom Editor
Set your preferred editor:
```bash
export EDITOR=vim
clud --loop --loop-ui
```

### Monitoring Loop Progress
The TUI automatically displays:
- ⏳ Loading indicators for async operations
- ✓ Completion confirmations
- Real-time Claude Code output
- Iteration progress and status

## Troubleshooting

### Terminal Too Small Warning
**Issue**: Warning appears that terminal is too small.

**Solution**: Resize your terminal to at least 80×20 (recommended 100×30).

### Unicode Characters Not Displaying
**Issue**: Symbols like ⏳, ✓, ←, ╔ appear as boxes or question marks.

**Solution**: Ensure your terminal supports UTF-8 encoding:
```bash
# Check encoding
echo $LANG

# Should show UTF-8, e.g., en_US.UTF-8
# If not, set it:
export LANG=en_US.UTF-8
```

### Editor Not Opening
**Issue**: "Edit UPDATE.md" doesn't open your preferred editor.

**Solution**: Set the $EDITOR environment variable:
```bash
export EDITOR=nano    # Or vim, emacs, code, etc.
clud --loop --loop-ui
```

### Git Bash Colors Not Working (Windows)
**Issue**: Colors appear incorrect or washed out in git-bash.

**Solution**: Update git-bash to the latest version or try Windows Terminal for better color support.

### TUI Not Responding to Keyboard
**Issue**: Arrow keys or other keys don't navigate the menu.

**Solution**:
1. Ensure you're running in a proper terminal (not inside another TUI app)
2. Check if another process is consuming keyboard input
3. Try using Tab and Enter instead of arrow keys

## Advanced Usage

### Integration with Scripts
The TUI can be integrated into automation scripts:

```bash
#!/bin/bash
# Start loop with TUI in the background
clud --loop --loop-ui &
PID=$!

# Do other work...

# Stop the loop gracefully
kill -TERM $PID
```

### Callbacks and Hooks
The TUI supports callbacks for integration:

```python
from clud.loop_tui import LoopTUI, TUIConfig

def on_halt():
    print("Loop halted by user")

config = TUIConfig(on_halt=on_halt)
LoopTUI.run(config)
```

## Comparison with Plain Loop Mode

| Feature | Plain Loop (`--loop`) | TUI Loop (`--loop --ui`) |
|---------|----------------------|--------------------------|
| Output Display | Plain text to stdout | Rich formatted TUI |
| Interactive Control | Terminal interrupts only | Full menu system |
| Real-time Feedback | Basic | Loading indicators, status |
| Visual Organization | None | Split-pane layout |
| Editor Integration | Manual | Built-in menu option |
| Help System | External docs | Built-in help screen |
| Progress Tracking | Manual | Visual status updates |

## See Also

- [Backlog Tab Feature](backlog.md) - Task visualization from Backlog.md
- [Pipe Mode](pipe-mode.md) - Unix-style I/O piping support
- [Cron Scheduler](cron-scheduler.md) - Automated task scheduling
- [Development Setup](../development/setup.md) - Setting up development environment

## References

- **Textual Framework**: https://textual.textualize.io/
- **TUI Library Research**: `.loop/TUI_LIBRARY_RESEARCH.md`
- **Implementation Plan**: `.loop/LOOP.md`
- **Design Mockup**: `.loop/MOCKUP.md`

---

**Version**: 1.0
**Last Updated**: 2026-02-06
**Status**: Complete and Production-Ready
