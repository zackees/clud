# PLAN: Eliminate All UI Features and Create Playwright Multi-Terminal Daemon

## Executive Summary

This plan removes all web UI, Telegram, API server, cluster, and kanban features from `clud`, keeping only core CLI functionality (`--loop`, `--cron`) and replacing the web UI with a new Playwright-based daemon that displays 8 xterm.js terminals in a flex grid.

## Current State Analysis

### Features to Remove (17 Major Components)

1. **Web UI** - FastAPI server with Svelte 5 frontend, terminal console, backlog tab
2. **Telegram Bot** - Full bot integration, webhooks, web app, landing page
3. **Telegram Server** - Advanced integration server with WebSocket support
4. **API Server** - REST API for message passing (`--api-server`)
5. **Webapp Module** - Simple HTTP server for landing pages
6. **Service/Daemon** - Local HTTP daemon on port 7565 for agent coordination
7. **Cluster Module** - Distributed agent management control plane
8. **Kanban Board** - vibe-kanban task visualization
9. **Code Server Launcher** - code-server browser integration
10. **Terminal PTY Management** - xterm.js integration in current webui
11. **WebSocket Infrastructure** - Real-time communication layer
12. **Session Management** - Telegram session tracking
13. **Agent Registry** - Agent registration and heartbeat tracking
14. **Service Discovery** - Multi-agent coordination
15. **Database Layer** - SQLAlchemy models for cluster
16. **Authentication** - JWT tokens, password hashing
17. **Frontend Build Systems** - Node.js, npm, Svelte, TypeScript tooling

### Features to Keep (5 Core Components)

1. **--cron Scheduler** - Recurring task automation with daemon
2. **--loop Mode** - Iterative agent execution
3. **--init-loop** - LOOP.md file creation
4. **Core Agent Execution** - Run Claude Code with arguments
5. **Message Hooks** - Event-based integrations (non-Telegram)

---

## Phase 1: Dependency & File Removal

### 1.1 Remove Python Modules

**Delete entire directories:**
```
src/clud/webui/                    # Web UI module (12 Python + frontend)
src/clud/telegram/                 # Telegram module (13 Python + frontend)
src/clud/webapp/                   # Webapp module (2 Python + static)
src/clud/service/                  # Service/daemon module (7 Python)
src/clud/cluster/                  # Cluster module (9 Python + static)
```

**Delete specific files:**
```
src/clud/agent/commands/webui.py
src/clud/agent/commands/telegram.py
src/clud/agent/commands/telegram_server.py
src/clud/agent/commands/api_server.py
src/clud/agent/commands/kanban.py
src/clud/agent/commands/code.py
src/clud/messaging/telegram.py
src/clud/hooks/telegram.py
```

**Update module exports:**
```
src/clud/agent/commands/__init__.py     # Remove deleted command imports
src/clud/messaging/__init__.py          # Remove telegram imports
src/clud/hooks/__init__.py              # Remove telegram imports
```

### 1.2 Remove Dependencies from pyproject.toml

Remove these dependencies (lines 13-41):
```toml
fastapi>=0.115.0
uvicorn[standard]>=0.32.0
websockets>=12.0
sqlalchemy>=2.0.0
aiosqlite>=0.19.0
alembic>=1.12.0
pydantic>=2.5.0
pydantic-settings>=2.1.0
python-jose[cryptography]>=3.3.0
passlib[bcrypt]>=1.7.4
python-multipart>=0.0.6
python-telegram-bot>=20.0
markdown2>=2.4.0
```

Add new dependencies:
```toml
playwright>=1.40.0              # Playwright browser automation
```

### 1.3 Remove CLI Arguments

**File:** `src/clud/agent_args.py`

Remove these argument definitions:
```python
--webui / --ui [PORT]
--telegram / -tg [TOKEN]
--telegram-server [PORT]
--telegram-config PATH
--api-server [PORT]
--kanban
--code [PORT]
```

Keep these arguments:
```python
--cron <subcommand>
--loop [MESSAGE]
--loop-count N
--init-loop
```

### 1.4 Remove Command Routing

**File:** `src/clud/agent_cli.py`

Remove routing logic for:
- `args.webui`
- `args.telegram`
- `args.telegram_server`
- `args.api_server`
- `args.kanban`
- `args.code`

Keep routing for:
- `args.cron`
- `args.loop`
- `args.init_loop`

### 1.5 Remove Test Files

```
tests/integration/test_webui_e2e.py
tests/integration/test_webui_terminal_tab_e2e.py
tests/integration/test_telegram_button_e2e.py
tests/integration/test_telegram_launch_button.py
tests/test_service_server.py
tests/test_telegram_*.py
tests/mocks/telegram_api.py
```

Keep:
```
tests/test_cron_daemon.py          # Needed for --cron
```

### 1.6 Remove Documentation

**Delete:**
```
docs/features/webui.md
docs/features/terminal.md
docs/features/telegram-api.md
docs/telegram-integration.md
docs/telegram-webapp-design.md
```

**Update:**
```
docs/features/backlog.md           # Remove Web UI references
docs/features/hooks.md             # Remove Telegram references
docs/development/architecture.md   # Remove web module references
CLAUDE.md                          # Remove Telegram, Web UI sections
README.md                          # Update features list
```

### 1.7 Remove Entry Points

**File:** `pyproject.toml`

Remove:
```toml
clud-cluster = "clud.cluster.cli:main"
```

Keep:
```toml
clud = "clud.cli:main"
```

---

## Phase 2: Design New Playwright Multi-Terminal Daemon

### 2.1 Architecture Overview

**Inspired by:** `~/dev/fastled-wasm/src/fastled/playwright/playwright_browser.py`

**New Structure:**
```
src/clud/daemon/
├── __init__.py                    # Lazy-loading proxy pattern
├── playwright_daemon.py           # Main daemon implementation
├── terminal_manager.py            # Manage 8 xterm.js instances
├── html_template.py               # HTML/CSS/JS for 8-terminal grid
├── server.py                      # Simple HTTP server for HTML
└── cli_handler.py                 # CLI command: clud --daemon
```

### 2.2 Key Design Decisions

**1. Playwright Browser Integration**
- Use Playwright to launch Chromium browser
- Load HTML page with 8 xterm.js terminals in flex grid
- Auto-resize browser window to content
- Keep browser alive until user closes it

**2. Terminal Layout**
- 8 xterm.js terminals in a CSS flex grid (2x4 or 4x2)
- Each terminal is a separate PTY process
- All terminals start in user's home directory
- User can launch `clud` from any terminal

**3. Server Architecture**
- Simple HTTP server on random free port (similar to fastled-wasm)
- Serve single HTML page with embedded JavaScript
- WebSocket connections for each terminal's PTY
- No authentication (localhost only)

**4. Process Management**
- Daemon runs as background process
- Playwright browser in separate process
- Each terminal is a separate PTY subprocess
- Clean shutdown on browser close

### 2.3 HTML Template Design

**File:** `src/clud/daemon/html_template.py`

```html
<!DOCTYPE html>
<html>
<head>
    <title>CLUD - Multi-Terminal Daemon</title>
    <link rel="stylesheet" href="https://unpkg.com/xterm@5.3.0/css/xterm.css" />
    <script src="https://unpkg.com/xterm@5.3.0/lib/xterm.js"></script>
    <script src="https://unpkg.com/xterm-addon-fit@0.8.0/lib/xterm-addon-fit.js"></script>
    <script src="https://unpkg.com/xterm-addon-web-links@0.9.0/lib/xterm-addon-web-links.js"></script>
    <style>
        body {
            margin: 0;
            padding: 10px;
            background: #1e1e1e;
            font-family: monospace;
            display: flex;
            flex-direction: column;
            height: 100vh;
        }
        h1 {
            color: #fff;
            font-size: 18px;
            margin: 0 0 10px 0;
        }
        .terminal-grid {
            display: grid;
            grid-template-columns: repeat(2, 1fr);
            grid-template-rows: repeat(4, 1fr);
            gap: 5px;
            flex: 1;
            min-height: 0;
        }
        .terminal-container {
            border: 1px solid #333;
            border-radius: 3px;
            overflow: hidden;
            background: #000;
        }
        .terminal {
            height: 100%;
            padding: 5px;
        }
    </style>
</head>
<body>
    <h1>CLUD Multi-Terminal Daemon - 8 Terminals</h1>
    <div class="terminal-grid">
        <div class="terminal-container"><div id="term0" class="terminal"></div></div>
        <div class="terminal-container"><div id="term1" class="terminal"></div></div>
        <div class="terminal-container"><div id="term2" class="terminal"></div></div>
        <div class="terminal-container"><div id="term3" class="terminal"></div></div>
        <div class="terminal-container"><div id="term4" class="terminal"></div></div>
        <div class="terminal-container"><div id="term5" class="terminal"></div></div>
        <div class="terminal-container"><div id="term6" class="terminal"></div></div>
        <div class="terminal-container"><div id="term7" class="terminal"></div></div>
    </div>
    <script>
        // Initialize 8 terminals with WebSocket connections
        const terminals = [];
        for (let i = 0; i < 8; i++) {
            const term = new Terminal({
                cursorBlink: true,
                fontSize: 14,
                theme: {
                    background: '#000000',
                    foreground: '#ffffff'
                }
            });
            const fitAddon = new FitAddon.FitAddon();
            const webLinksAddon = new WebLinksAddon.WebLinksAddon();
            term.loadAddon(fitAddon);
            term.loadAddon(webLinksAddon);
            term.open(document.getElementById(`term${i}`));
            fitAddon.fit();

            // WebSocket connection to PTY
            const ws = new WebSocket(`ws://localhost:${PORT}/ws/${i}`);
            ws.onmessage = (event) => term.write(event.data);
            term.onData((data) => ws.send(data));

            terminals.push({ term, fitAddon, ws });
        }

        // Auto-resize on window resize
        window.addEventListener('resize', () => {
            terminals.forEach(({ fitAddon }) => fitAddon.fit());
        });
    </script>
</body>
</html>
```

### 2.4 Playwright Daemon Implementation

**File:** `src/clud/daemon/playwright_daemon.py`

**Key features:**
- Launch Chromium with Playwright
- Similar to `fastled-wasm` pattern:
  - `PlaywrightDaemon` class (like `PlaywrightBrowser`)
  - `run_playwright_daemon()` function (like `run_playwright_browser()`)
  - `PlaywrightDaemonProxy` class for lifecycle management
  - `open_daemon()` function (like `open_with_playwright()`)
- Install Playwright browsers if missing
- Auto-resize browser window to content
- Keep browser alive until user closes it
- Clean shutdown on Ctrl+C or browser close

**Pattern from fastled-wasm:**
```python
# From fastled-wasm/src/fastled/playwright/playwright_browser.py
class PlaywrightBrowser:
    def __init__(self, headless: bool = False):
        self.headless = headless
        self.browser = None
        self.page = None
        self.playwright = None
        self._should_exit = asyncio.Event()

    async def start(self):
        # Launch Chromium
        self.playwright = async_playwright()
        playwright = await self.playwright.start()
        self.browser = await playwright.chromium.launch(headless=self.headless)
        self.page = await self.browser.new_page()

    async def open_url(self, url: str):
        await self.page.goto(url)
        await self.page.wait_for_load_state("networkidle")

    async def wait_for_close(self):
        while not self.browser.is_closed():
            await asyncio.sleep(1)

    async def close(self):
        if self.page:
            await self.page.close()
        if self.browser:
            await self.browser.close()
        if self.playwright:
            await self.playwright.stop()
```

**Adapt for CLUD:**
```python
class PlaywrightDaemon:
    def __init__(self, num_terminals: int = 8):
        self.num_terminals = num_terminals
        self.browser = None
        self.page = None
        self.playwright = None
        self._should_exit = asyncio.Event()
        self.server = None  # HTTP server for HTML
        self.terminal_manager = None  # Manages PTY processes
```

### 2.5 Terminal Manager Implementation

**File:** `src/clud/daemon/terminal_manager.py`

**Responsibilities:**
- Create 8 PTY processes (one per terminal)
- Each PTY starts in user's home directory
- WebSocket handler for each terminal
- Forward stdin/stdout between WebSocket and PTY
- Clean shutdown of all PTY processes

**Key classes:**
```python
class Terminal:
    """Represents a single PTY terminal."""
    def __init__(self, terminal_id: int, cwd: Path):
        self.terminal_id = terminal_id
        self.cwd = cwd
        self.pty_process = None
        self.websocket = None

    def start(self):
        # Create PTY process
        pass

    def stop(self):
        # Kill PTY process
        pass

class TerminalManager:
    """Manages 8 terminal instances."""
    def __init__(self, num_terminals: int = 8):
        self.terminals = []
        for i in range(num_terminals):
            self.terminals.append(Terminal(i, Path.home()))

    def start_all(self):
        for term in self.terminals:
            term.start()

    def stop_all(self):
        for term in self.terminals:
            term.stop()
```

### 2.6 HTTP Server Implementation

**File:** `src/clud/daemon/server.py`

**Responsibilities:**
- Serve HTML page on random free port
- WebSocket endpoints for each terminal
- Find free port (similar to fastled-wasm)
- Simple HTTP server (no FastAPI needed)

**Use standard library:**
```python
from http.server import HTTPServer, SimpleHTTPRequestHandler
import asyncio
from websockets import serve

class DaemonHTTPServer:
    def __init__(self, num_terminals: int = 8):
        self.port = self.find_free_port()
        self.num_terminals = num_terminals
        self.terminal_manager = TerminalManager(num_terminals)

    def find_free_port(self) -> int:
        # Similar to fastled-wasm pattern
        pass

    async def start(self):
        # Start HTTP server
        # Start WebSocket server
        # Start terminal manager
        pass
```

### 2.7 CLI Handler Implementation

**File:** `src/clud/daemon/cli_handler.py`

**New CLI argument:**
```bash
clud --daemon         # Launch multi-terminal daemon
clud -d               # Short form
```

**Implementation:**
```python
def handle_daemon_command() -> int:
    """Handle clud --daemon command."""
    from clud.daemon import Daemon

    print("Starting CLUD multi-terminal daemon...")
    print("8 terminals will open in Playwright browser")
    print("All terminals start in: " + str(Path.home()))

    # Launch daemon
    daemon = Daemon.start()

    # Wait for user to close browser
    daemon.wait_for_close()

    return 0
```

### 2.8 Lazy-Loading Proxy Pattern

**File:** `src/clud/daemon/__init__.py`

**Follow CLAUDE.md pattern:**
```python
"""Multi-terminal daemon module for clud."""

from dataclasses import dataclass

@dataclass
class DaemonInfo:
    """Information about running daemon."""
    pid: int
    port: int
    num_terminals: int

class Daemon:
    """Proxy class for daemon operations with lazy-loaded implementation."""

    @staticmethod
    def start(num_terminals: int = 8) -> DaemonInfo:
        """Start the multi-terminal daemon."""
        from clud.daemon.playwright_daemon import PlaywrightDaemon

        daemon = PlaywrightDaemon(num_terminals=num_terminals)
        daemon.start()
        return DaemonInfo(
            pid=daemon.pid,
            port=daemon.port,
            num_terminals=num_terminals
        )

    @staticmethod
    def is_running() -> bool:
        """Check if daemon is running."""
        from clud.daemon.playwright_daemon import PlaywrightDaemon

        return PlaywrightDaemon.is_running()

__all__ = [
    "Daemon",
    "DaemonInfo",
]
```

---

## Phase 3: Implementation Plan

### 3.1 Step-by-Step Implementation Order

**Step 1: Backup and Branch**
```bash
git checkout -b remove-ui-features
git add -A
git commit -m "checkpoint: before removing UI features"
```

**Step 2: Remove Dependencies (Least Risky First)**
1. Remove documentation files
2. Remove test files
3. Remove frontend directories (webui/frontend, telegram/frontend)
4. Run `bash lint` to identify import errors

**Step 3: Remove Command Handlers**
1. Delete command files in `src/clud/agent/commands/`
2. Update `__init__.py` to remove imports
3. Run `bash lint` to verify

**Step 4: Remove Modules**
1. Delete `src/clud/webapp/`
2. Delete `src/clud/cluster/`
3. Delete `src/clud/service/`
4. Delete `src/clud/telegram/`
5. Delete `src/clud/webui/`
6. Run `bash lint` after each deletion

**Step 5: Update CLI Argument Parsing**
1. Edit `src/clud/agent_args.py` - remove UI flags
2. Edit `src/clud/agent_cli.py` - remove routing logic
3. Run `bash lint`

**Step 6: Remove Dependencies from pyproject.toml**
1. Remove FastAPI, Uvicorn, WebSockets, etc.
2. Add Playwright
3. Run `bash install` to update dependencies
4. Run `bash test` to ensure core functionality works

**Step 7: Update Documentation**
1. Update CLAUDE.md
2. Update README.md
3. Update architecture docs

**Step 8: Create New Daemon Module**
1. Create `src/clud/daemon/__init__.py` (lazy-loading proxy)
2. Create `src/clud/daemon/html_template.py`
3. Create `src/clud/daemon/terminal_manager.py`
4. Create `src/clud/daemon/server.py`
5. Create `src/clud/daemon/playwright_daemon.py`
6. Create `src/clud/daemon/cli_handler.py`
7. Run `bash lint` after each file

**Step 9: Integrate Daemon Command**
1. Add `--daemon` flag to `agent_args.py`
2. Add routing in `agent_cli.py`
3. Test: `clud --daemon`

**Step 10: Testing & Validation**
1. Test `clud --loop` still works
2. Test `clud --cron` still works
3. Test `clud --daemon` launches browser
4. Test 8 terminals all work independently
5. Test browser close shuts down daemon cleanly

**Step 11: Clean Up**
1. Remove any unused imports
2. Run `bash lint` (MANDATORY)
3. Run `bash test --full`
4. Update version number

**Step 12: Commit & Document**
```bash
git add -A
git commit -m "feat: remove all UI features, add Playwright multi-terminal daemon"
```

---

## Phase 4: Testing Strategy

### 4.1 Unit Tests to Remove
- All webui tests
- All telegram tests
- All API server tests
- All cluster tests
- Service daemon tests

### 4.2 Unit Tests to Keep
- Cron daemon tests (`test_cron_daemon.py`)
- Loop mode tests
- Agent execution tests
- Backlog parser tests (if not UI-dependent)

### 4.3 New Tests to Create

**File:** `tests/test_daemon.py`
```python
import unittest
from clud.daemon import Daemon, DaemonInfo

class TestDaemon(unittest.TestCase):
    def test_daemon_starts(self):
        # Test daemon can start
        pass

    def test_8_terminals_created(self):
        # Test all 8 terminals are created
        pass

    def test_terminals_in_home_directory(self):
        # Test terminals start in home directory
        pass

    def test_daemon_cleanup(self):
        # Test clean shutdown
        pass
```

**File:** `tests/integration/test_daemon_e2e.py`
```python
import unittest
from pathlib import Path

class TestDaemonE2E(unittest.TestCase):
    def test_launch_daemon_browser(self):
        # Test full daemon launch with Playwright
        pass

    def test_terminal_can_run_clud(self):
        # Test running clud from terminal
        pass
```

### 4.4 Manual Testing Checklist

- [ ] `clud --daemon` launches Playwright browser
- [ ] Browser shows 8 terminals in grid layout
- [ ] All 8 terminals are interactive
- [ ] Can type in each terminal independently
- [ ] Can run `clud --loop` from any terminal
- [ ] Can run `clud --cron` commands
- [ ] Closing browser shuts down daemon cleanly
- [ ] No zombie processes left behind
- [ ] Ctrl+C shuts down daemon cleanly
- [ ] Can launch multiple daemons on different ports

---

## Phase 5: Migration & Compatibility

### 5.1 Breaking Changes

**Removed features (no migration path):**
- `clud --webui` - REMOVED
- `clud --ui` - REMOVED
- `clud --telegram` - REMOVED
- `clud --telegram-server` - REMOVED
- `clud --api-server` - REMOVED
- `clud --kanban` - REMOVED
- `clud --code` - REMOVED

**Replacement:**
- Use `clud --daemon` for multi-terminal UI

### 5.2 User Communication

**Update README.md:**
```markdown
## Breaking Changes in v2.0.0

All web UI and integration features have been removed:
- ❌ Web UI (`--webui`, `--ui`)
- ❌ Telegram integration (`--telegram`, `--telegram-server`)
- ❌ API server (`--api-server`)
- ❌ Kanban board (`--kanban`)
- ❌ Code server launcher (`--code`)

**New in v2.0.0:**
- ✅ Multi-terminal daemon (`--daemon`)
  - Playwright-based browser UI
  - 8 xterm.js terminals in flex grid
  - All terminals start in home directory
  - Launch `clud` from any terminal

**Kept features:**
- ✅ Loop mode (`--loop`)
- ✅ Cron scheduler (`--cron`)
- ✅ Core agent execution
```

### 5.3 Version Bump

**Update pyproject.toml:**
```toml
version = "2.0.0"  # Major version bump for breaking changes
```

---

## Phase 6: File Inventory Summary

### Files to DELETE (43+ Python files)

**Modules:**
```
src/clud/webui/                    # 12 files + frontend (5500+ files)
src/clud/telegram/                 # 13 files + frontend (5500+ files)
src/clud/webapp/                   # 2 files + static HTML
src/clud/service/                  # 7 files
src/clud/cluster/                  # 9 files + static
```

**Commands:**
```
src/clud/agent/commands/webui.py
src/clud/agent/commands/telegram.py
src/clud/agent/commands/telegram_server.py
src/clud/agent/commands/api_server.py
src/clud/agent/commands/kanban.py
src/clud/agent/commands/code.py
```

**Integrations:**
```
src/clud/messaging/telegram.py
src/clud/hooks/telegram.py
```

**Tests:**
```
tests/integration/test_webui_e2e.py
tests/integration/test_webui_terminal_tab_e2e.py
tests/integration/test_telegram_button_e2e.py
tests/integration/test_telegram_launch_button.py
tests/test_service_server.py
tests/test_telegram_*.py
tests/mocks/telegram_api.py
```

**Documentation:**
```
docs/features/webui.md
docs/features/terminal.md
docs/features/telegram-api.md
docs/telegram-integration.md
docs/telegram-webapp-design.md
```

### Files to CREATE (6 new files)

```
src/clud/daemon/__init__.py
src/clud/daemon/html_template.py
src/clud/daemon/terminal_manager.py
src/clud/daemon/server.py
src/clud/daemon/playwright_daemon.py
src/clud/daemon/cli_handler.py
tests/test_daemon.py
tests/integration/test_daemon_e2e.py
```

### Files to UPDATE (8 files)

```
src/clud/agent_args.py             # Remove UI flags, add --daemon
src/clud/agent_cli.py              # Remove UI routing, add daemon routing
src/clud/agent/commands/__init__.py  # Remove deleted imports
src/clud/messaging/__init__.py     # Remove telegram imports
src/clud/hooks/__init__.py         # Remove telegram imports
pyproject.toml                     # Remove deps, add playwright, bump version
CLAUDE.md                          # Update documentation
README.md                          # Update features list
```

---

## Phase 7: Risk Assessment & Mitigation

### 7.1 High Risk Areas

**Risk 1: Breaking Existing Users**
- **Impact:** Users relying on removed features will break
- **Mitigation:**
  - Major version bump (2.0.0)
  - Clear migration guide in README
  - Deprecation warnings before removal (if time permits)

**Risk 2: Dependency Conflicts**
- **Impact:** Removing pydantic/FastAPI may break other code
- **Mitigation:**
  - Run `bash lint` after each module removal
  - Check all imports with `rg "from pydantic"` before removing
  - Test core features after dependency removal

**Risk 3: Cron Daemon Dependency on Service Module**
- **Impact:** Cron daemon may depend on removed service code
- **Mitigation:**
  - Carefully review `src/clud/cron/daemon.py` before deleting service
  - Ensure cron daemon is standalone
  - Test `clud --cron` after service removal

**Risk 4: Playwright Not Installing**
- **Impact:** Users can't use new daemon feature
- **Mitigation:**
  - Auto-install playwright browsers on first `--daemon` launch
  - Fallback error message with manual install instructions
  - Document playwright requirements in README

### 7.2 Medium Risk Areas

**Risk 5: PTY Management Complexity**
- **Impact:** Terminal PTY processes may not work correctly
- **Mitigation:**
  - Use existing `pywinpty` dependency (already in project)
  - Reference removed `webui/terminal_handler.py` for PTY patterns
  - Test on Windows, Linux, macOS

**Risk 6: WebSocket Connections**
- **Impact:** Terminal input/output may be laggy or broken
- **Mitigation:**
  - Use lightweight WebSocket library (avoid FastAPI overhead)
  - Test with high-throughput commands (e.g., `ls -R /`)
  - Buffer management for stdin/stdout

### 7.3 Low Risk Areas

**Risk 7: HTML/CSS/JS Bugs**
- **Impact:** Terminal grid layout may not render correctly
- **Mitigation:**
  - Use mature xterm.js library (well-tested)
  - Simple CSS grid layout (minimal complexity)
  - Test on different screen sizes

---

## Phase 8: Success Criteria

### 8.1 Functional Requirements

- [ ] All UI features removed (17 components)
- [ ] `clud --daemon` command implemented
- [ ] Playwright browser launches successfully
- [ ] 8 xterm.js terminals render in grid
- [ ] All terminals interactive and independent
- [ ] Can run `clud --loop` from any terminal
- [ ] Can run `clud --cron` commands
- [ ] `bash lint` passes with zero errors
- [ ] `bash test` passes all unit tests
- [ ] `bash test --full` passes E2E tests

### 8.2 Performance Requirements

- [ ] Daemon starts in < 5 seconds
- [ ] Browser launches in < 3 seconds
- [ ] Terminal input lag < 50ms
- [ ] Memory usage < 500MB (8 terminals + browser)
- [ ] Clean shutdown in < 2 seconds

### 8.3 Code Quality Requirements

- [ ] No pyright type errors
- [ ] No ruff linting errors
- [ ] All new code follows lazy-loading proxy pattern
- [ ] All functions have type annotations
- [ ] All PTY processes use `handle_keyboard_interrupt()`
- [ ] Exception handling with context logging
- [ ] Unit test coverage > 80% for new code

---

## Phase 9: Timeline Estimate

### Conservative Estimates (Single Developer)

**Phase 1: Dependency & File Removal**
- 1.1 Remove Python modules: 2 hours
- 1.2 Remove dependencies: 30 minutes
- 1.3 Remove CLI arguments: 1 hour
- 1.4 Remove command routing: 1 hour
- 1.5 Remove test files: 30 minutes
- 1.6 Remove documentation: 30 minutes
- 1.7 Remove entry points: 15 minutes
- **Subtotal: ~6 hours**

**Phase 2: Design New Daemon**
- Already complete (this document)
- **Subtotal: 0 hours**

**Phase 3: Implementation**
- 3.1 Steps 1-7: Removal work (~6 hours, matches Phase 1)
- 3.1 Step 8: Create daemon module (~8 hours)
  - HTML template: 1 hour
  - Terminal manager: 3 hours
  - HTTP/WebSocket server: 2 hours
  - Playwright daemon: 2 hours
- 3.1 Step 9: Integration (~1 hour)
- 3.1 Step 10: Testing & validation (~3 hours)
- 3.1 Step 11: Clean up (~1 hour)
- 3.1 Step 12: Commit & document (~30 minutes)
- **Subtotal: ~19.5 hours**

**Phase 4: Testing**
- Create new tests: 2 hours
- Manual testing: 2 hours
- Bug fixes: 2 hours
- **Subtotal: ~6 hours**

**Phase 5-9: Documentation & Polish**
- Migration guide: 1 hour
- README updates: 1 hour
- Risk mitigation: 1 hour
- **Subtotal: ~3 hours**

**Total: ~35 hours (1 week full-time, or 2-3 weeks part-time)**

---

## Phase 10: Future Enhancements (Post-Launch)

### 10.1 Terminal Improvements
- [ ] Configurable number of terminals (not hardcoded to 8)
- [ ] Terminal tabs instead of grid layout
- [ ] Terminal split panes (like tmux)
- [ ] Persistent terminal sessions (survive daemon restart)

### 10.2 UI Improvements
- [ ] Dark/light theme toggle
- [ ] Configurable terminal colors
- [ ] Zoom in/out for terminals
- [ ] Fullscreen mode for single terminal

### 10.3 Productivity Features
- [ ] Save/restore terminal layouts
- [ ] Named terminal sessions
- [ ] Broadcast input to multiple terminals
- [ ] Terminal history search

### 10.4 Integration Features
- [ ] Remote terminal access (secure tunnel)
- [ ] Share terminal session (read-only viewer)
- [ ] Record terminal sessions (asciinema-like)

---

## Conclusion

This plan completely removes all web UI, Telegram, API server, cluster, and kanban features from `clud`, reducing the codebase by ~11,000+ files (mostly node_modules). The new Playwright-based daemon provides a clean, simple multi-terminal UI that is purpose-built for launching multiple `clud` instances.

**Key Benefits:**
1. **Faster development** - No more maintaining FastAPI, Svelte, Telegram bot
2. **Simpler codebase** - 43 fewer Python files, zero frontend complexity
3. **Better performance** - No web server overhead, direct PTY connections
4. **Modern UI** - Playwright + xterm.js is more maintainable than custom Svelte
5. **Focus on core value** - `--loop` and `--cron` are the real features

**Next Steps:**
1. Get user approval for this plan
2. Create git branch `remove-ui-features`
3. Execute Phase 3 step-by-step
4. Run `bash lint` after every change (MANDATORY)
5. Test thoroughly before merging
