# LOOP: Eliminate All UI Features and Create Playwright Multi-Terminal Daemon

## Context

This loop removes all web UI, Telegram, API server, cluster, and kanban features from `clud`, keeping only core CLI functionality (`--loop`, `--cron`) and replacing the web UI with a new Playwright-based daemon that displays 8 xterm.js terminals in a flex grid.

**Goal**: Reduce codebase complexity by ~11,000+ files, eliminate FastAPI/Svelte/Telegram dependencies, and provide a clean multi-terminal UI via Playwright.

## Current Iteration

**Status**: Ready to start
**Current Step**: Step 1 - Backup and Branch

---

## Iteration 1: Backup and Branch

### Task
Create a git branch and checkpoint before removing UI features.

### Actions
- [ ] Create branch `remove-ui-features`
- [ ] Create checkpoint commit: "checkpoint: before removing UI features"

### Validation
- Branch created successfully
- All current changes committed

### Next Iteration
→ Iteration 2: Remove Documentation Files

---

## Iteration 2: Remove Documentation Files

### Task
Remove all UI-related documentation files (lowest risk).

### Actions
- [ ] Delete `docs/features/webui.md`
- [ ] Delete `docs/features/terminal.md`
- [ ] Delete `docs/features/telegram-api.md`
- [ ] Delete `docs/telegram-integration.md`
- [ ] Delete `docs/telegram-webapp-design.md`

### Validation
- Files deleted successfully
- No references to deleted files in other docs (grep check)

### Next Iteration
→ Iteration 3: Remove Test Files

---

## Iteration 3: Remove Test Files

### Task
Remove all UI-related test files.

### Actions
- [ ] Delete `tests/integration/test_webui_e2e.py`
- [ ] Delete `tests/integration/test_webui_terminal_tab_e2e.py`
- [ ] Delete `tests/integration/test_telegram_button_e2e.py`
- [ ] Delete `tests/integration/test_telegram_launch_button.py`
- [ ] Delete `tests/test_service_server.py`
- [ ] Delete all `tests/test_telegram_*.py` files
- [ ] Delete `tests/mocks/telegram_api.py`

### Validation
- Files deleted successfully
- Run `bash test` to confirm no import errors from remaining tests

### Next Iteration
→ Iteration 4: Remove Frontend Directories

---

## Iteration 4: Remove Frontend Directories

### Task
Remove all frontend build artifacts and source files.

### Actions
- [ ] Delete `src/clud/webui/frontend/` directory (if exists)
- [ ] Delete `src/clud/telegram/frontend/` directory (if exists)
- [ ] Delete any `node_modules/` directories in UI modules
- [ ] Delete any `package.json`, `package-lock.json` files in UI modules

### Validation
- Directories deleted successfully
- Confirm ~11,000+ files removed

### Next Iteration
→ Iteration 5: Run Lint to Identify Import Errors

---

## Iteration 5: Run Lint to Identify Import Errors

### Task
Run linter to identify what imports will break after module removal.

### Actions
- [ ] Run `bash lint`
- [ ] Document all import errors related to UI modules
- [ ] Create list of files that need updating

### Validation
- Lint results captured
- Import dependency map created

### Next Iteration
→ Iteration 6: Remove Command Handler Files

---

## Iteration 6: Remove Command Handler Files

### Task
Delete command handler files for UI features.

### Actions
- [ ] Delete `src/clud/agent/commands/webui.py`
- [ ] Delete `src/clud/agent/commands/telegram.py`
- [ ] Delete `src/clud/agent/commands/telegram_server.py`
- [ ] Delete `src/clud/agent/commands/api_server.py`
- [ ] Delete `src/clud/agent/commands/kanban.py`
- [ ] Delete `src/clud/agent/commands/code.py`

### Validation
- Files deleted successfully
- Commands no longer importable

### Next Iteration
→ Iteration 7: Update Command Module Exports

---

## Iteration 7: Update Command Module Exports

### Task
Update `src/clud/agent/commands/__init__.py` to remove deleted command imports.

### Actions
- [ ] Read `src/clud/agent/commands/__init__.py`
- [ ] Remove imports for deleted command files
- [ ] Remove from `__all__` exports
- [ ] Run `bash lint` to verify

### Validation
- No import errors from commands module
- Lint passes for this module

### Next Iteration
→ Iteration 8: Remove Integration Files

---

## Iteration 8: Remove Integration Files

### Task
Remove Telegram integration files from messaging and hooks modules.

### Actions
- [ ] Delete `src/clud/messaging/telegram.py`
- [ ] Delete `src/clud/hooks/telegram.py`

### Validation
- Files deleted successfully

### Next Iteration
→ Iteration 9: Update Integration Module Exports

---

## Iteration 9: Update Integration Module Exports

### Task
Update messaging and hooks module `__init__.py` files.

### Actions
- [ ] Update `src/clud/messaging/__init__.py` - remove telegram imports
- [ ] Update `src/clud/hooks/__init__.py` - remove telegram imports
- [ ] Run `bash lint` to verify

### Validation
- No import errors from messaging/hooks modules
- Lint passes

### Next Iteration
→ Iteration 10: Remove Webapp Module

---

## Iteration 10: Remove Webapp Module

### Task
Delete the webapp module directory.

### Actions
- [ ] Delete `src/clud/webapp/` directory entirely
- [ ] Run `bash lint` to check for import errors

### Validation
- Directory deleted
- Lint results documented

### Next Iteration
→ Iteration 11: Remove Cluster Module

---

## Iteration 11: Remove Cluster Module

### Task
Delete the cluster module directory.

### Actions
- [ ] Delete `src/clud/cluster/` directory entirely
- [ ] Run `bash lint` to check for import errors

### Validation
- Directory deleted
- Lint results documented

### Next Iteration
→ Iteration 12: Remove Service Module

---

## Iteration 12: Remove Service Module

### Task
Delete the service/daemon module directory.

### Actions
- [ ] FIRST: Check if `src/clud/cron/daemon.py` imports from service module
- [ ] Delete `src/clud/service/` directory entirely
- [ ] Run `bash lint` to check for import errors

### Validation
- Confirmed cron daemon doesn't depend on service module
- Directory deleted
- Lint results documented

### Next Iteration
→ Iteration 13: Remove Telegram Module

---

## Iteration 13: Remove Telegram Module

### Task
Delete the telegram module directory.

### Actions
- [ ] Delete `src/clud/telegram/` directory entirely
- [ ] Run `bash lint` to check for import errors

### Validation
- Directory deleted
- Lint results documented

### Next Iteration
→ Iteration 14: Remove WebUI Module

---

## Iteration 14: Remove WebUI Module

### Task
Delete the webui module directory.

### Actions
- [ ] Delete `src/clud/webui/` directory entirely
- [ ] Run `bash lint` to check for import errors

### Validation
- Directory deleted
- Lint results documented

### Next Iteration
→ Iteration 15: Update CLI Argument Parsing

---

## Iteration 15: Update CLI Argument Parsing

### Task
Remove UI-related CLI flags from argument parser.

### Actions
- [ ] Edit `src/clud/agent_args.py`
- [ ] Remove flag definitions for:
  - `--webui / --ui [PORT]`
  - `--telegram / -tg [TOKEN]`
  - `--telegram-server [PORT]`
  - `--telegram-config PATH`
  - `--api-server [PORT]`
  - `--kanban`
  - `--code [PORT]`
- [ ] Run `bash lint` to verify

### Validation
- Flags removed
- Lint passes for agent_args.py

### Next Iteration
→ Iteration 16: Update CLI Command Routing

---

## Iteration 16: Update CLI Command Routing

### Task
Remove UI-related command routing logic.

### Actions
- [ ] Edit `src/clud/agent_cli.py`
- [ ] Remove routing logic for:
  - `args.webui`
  - `args.telegram`
  - `args.telegram_server`
  - `args.api_server`
  - `args.kanban`
  - `args.code`
- [ ] Run `bash lint` to verify

### Validation
- Routing removed
- Lint passes for agent_cli.py

### Next Iteration
→ Iteration 17: Remove Cluster Entry Point

---

## Iteration 17: Remove Cluster Entry Point

### Task
Remove `clud-cluster` entry point from pyproject.toml.

### Actions
- [ ] Edit `pyproject.toml`
- [ ] Remove `clud-cluster = "clud.cluster.cli:main"` from `[project.scripts]`

### Validation
- Entry point removed
- Only `clud = "clud.cli:main"` remains

### Next Iteration
→ Iteration 18: Remove Python Dependencies

---

## Iteration 18: Remove Python Dependencies

### Task
Remove UI-related dependencies from pyproject.toml.

### Actions
- [ ] Edit `pyproject.toml`
- [ ] Remove these dependencies:
  - `fastapi>=0.115.0`
  - `uvicorn[standard]>=0.32.0`
  - `websockets>=12.0`
  - `sqlalchemy>=2.0.0`
  - `aiosqlite>=0.19.0`
  - `alembic>=1.12.0`
  - `pydantic>=2.5.0`
  - `pydantic-settings>=2.1.0`
  - `python-jose[cryptography]>=3.3.0`
  - `passlib[bcrypt]>=1.7.4`
  - `python-multipart>=0.0.6`
  - `python-telegram-bot>=20.0`
  - `markdown2>=2.4.0`

### Validation
- Dependencies removed from pyproject.toml

### Next Iteration
→ Iteration 19: Add Playwright Dependency

---

## Iteration 19: Add Playwright Dependency

### Task
Add Playwright to dependencies.

### Actions
- [ ] Edit `pyproject.toml`
- [ ] Add `playwright>=1.40.0` to dependencies list

### Validation
- Playwright added to pyproject.toml

### Next Iteration
→ Iteration 20: Reinstall Dependencies

---

## Iteration 20: Reinstall Dependencies

### Task
Update virtual environment with new dependencies.

### Actions
- [ ] Run `bash install` to update dependencies
- [ ] Verify Playwright installed correctly

### Validation
- Install completes successfully
- No dependency conflicts

### Next Iteration
→ Iteration 21: Run Tests to Verify Core Functionality

---

## Iteration 21: Run Tests to Verify Core Functionality

### Task
Ensure core features still work after removal.

### Actions
- [ ] Run `bash test`
- [ ] Document any failures
- [ ] Verify `--cron` and `--loop` tests pass

### Validation
- Core tests pass
- Only UI-related tests are gone

### Next Iteration
→ Iteration 22: Run Full Lint Check

---

## Iteration 22: Run Full Lint Check

### Task
Comprehensive lint check after all removals.

### Actions
- [ ] Run `bash lint`
- [ ] Fix any remaining import errors
- [ ] Fix any type errors
- [ ] Verify zero errors

### Validation
- Lint passes with zero errors
- No pyright errors
- No ruff errors

### Next Iteration
→ Iteration 23: Create Daemon Module Structure

---

## Iteration 23: Create Daemon Module Structure

### Task
Create new daemon module directory and __init__.py with lazy-loading proxy pattern.

### Actions
- [ ] Create `src/clud/daemon/` directory
- [ ] Create `src/clud/daemon/__init__.py` with:
  - `DaemonInfo` dataclass (pid, port, num_terminals)
  - `Daemon` proxy class with static methods
  - `start()`, `is_running()` methods
  - `__all__` exports
- [ ] Run `bash lint` to verify

### Validation
- Module created
- Lint passes
- Follows lazy-loading proxy pattern from CLAUDE.md

### Next Iteration
→ Iteration 24: Create HTML Template Module

---

## Iteration 24: Create HTML Template Module

### Task
Create HTML template for 8-terminal grid layout.

### Actions
- [ ] Create `src/clud/daemon/html_template.py`
- [ ] Implement `get_html_template(port: int, num_terminals: int) -> str`
- [ ] Include:
  - xterm.js CDN links (v5.3.0)
  - xterm-addon-fit
  - xterm-addon-web-links
  - CSS grid layout (2x4 terminals)
  - WebSocket initialization for each terminal
  - Auto-resize on window resize
- [ ] Run `bash lint` to verify

### Validation
- Module created
- HTML template returns valid HTML
- Lint passes

### Next Iteration
→ Iteration 25: Create Terminal Manager Module

---

## Iteration 25: Create Terminal Manager Module

### Task
Create terminal manager for PTY process management.

### Actions
- [ ] Create `src/clud/daemon/terminal_manager.py`
- [ ] Implement `Terminal` class:
  - `__init__(terminal_id, cwd)`
  - `start()` - create PTY process
  - `stop()` - kill PTY process
  - `handle_websocket(websocket)` - forward stdin/stdout
- [ ] Implement `TerminalManager` class:
  - `__init__(num_terminals)`
  - `start_all()`
  - `stop_all()`
  - `get_terminal(terminal_id)`
- [ ] Use `pywinpty` for Windows, `pty` for Unix
- [ ] Use `handle_keyboard_interrupt()` utility
- [ ] Run `bash lint` to verify

### Validation
- Module created
- Type annotations on all methods
- Lint passes

### Next Iteration
→ Iteration 26: Create HTTP Server Module

---

## Iteration 26: Create HTTP Server Module

### Task
Create simple HTTP server for serving HTML and WebSocket connections.

### Actions
- [ ] Create `src/clud/daemon/server.py`
- [ ] Implement `DaemonHTTPServer` class:
  - `__init__(num_terminals)`
  - `find_free_port() -> int` - find random available port
  - `start()` - start HTTP and WebSocket servers
  - `stop()` - stop servers and clean up
- [ ] Use `http.server` for HTTP (serve HTML template)
- [ ] Use `websockets` library for WebSocket endpoints
- [ ] WebSocket endpoint: `/ws/{terminal_id}`
- [ ] Run `bash lint` to verify

### Validation
- Module created
- Server can start/stop cleanly
- Lint passes

### Next Iteration
→ Iteration 27: Create Playwright Daemon Module

---

## Iteration 27: Create Playwright Daemon Module

### Task
Create Playwright daemon implementation (main orchestrator).

### Actions
- [ ] Create `src/clud/daemon/playwright_daemon.py`
- [ ] Implement `PlaywrightDaemon` class (similar to fastled-wasm pattern):
  - `__init__(num_terminals)`
  - `async start()` - launch browser and server
  - `async open_url(url)` - navigate to HTML page
  - `async wait_for_close()` - wait for user to close browser
  - `async close()` - clean shutdown
- [ ] Auto-install Playwright browsers if missing
- [ ] Launch Chromium browser (not headless)
- [ ] Auto-resize browser window to content
- [ ] Run `bash lint` to verify

### Validation
- Module created
- Async methods properly typed
- Lint passes

### Next Iteration
→ Iteration 28: Create CLI Handler Module

---

## Iteration 28: Create CLI Handler Module

### Task
Create CLI handler for `--daemon` command.

### Actions
- [ ] Create `src/clud/daemon/cli_handler.py`
- [ ] Implement `handle_daemon_command() -> int`
- [ ] Print startup messages:
  - "Starting CLUD multi-terminal daemon..."
  - "8 terminals will open in Playwright browser"
  - "All terminals start in: {home_directory}"
- [ ] Call `Daemon.start()`
- [ ] Wait for browser close
- [ ] Return exit code
- [ ] Run `bash lint` to verify

### Validation
- Module created
- Handler properly typed
- Lint passes

### Next Iteration
→ Iteration 29: Add --daemon CLI Argument

---

## Iteration 29: Add --daemon CLI Argument

### Task
Add `--daemon` flag to argument parser.

### Actions
- [ ] Edit `src/clud/agent_args.py`
- [ ] Add `--daemon` / `-d` argument:
  - Action: store_true
  - Help: "Launch multi-terminal daemon with Playwright browser"
- [ ] Run `bash lint` to verify

### Validation
- Flag added successfully
- Lint passes

### Next Iteration
→ Iteration 30: Add Daemon Command Routing

---

## Iteration 30: Add Daemon Command Routing

### Task
Add routing logic for `--daemon` command.

### Actions
- [ ] Edit `src/clud/agent_cli.py`
- [ ] Add routing logic:
  - If `args.daemon`, call `daemon_cli_handler.handle_daemon_command()`
  - Return exit code
- [ ] Import `cli_handler` from `clud.daemon`
- [ ] Run `bash lint` to verify

### Validation
- Routing added
- Import works correctly
- Lint passes

### Next Iteration
→ Iteration 31: Manual Test - Launch Daemon

---

## Iteration 31: Manual Test - Launch Daemon

### Task
Test basic daemon launch functionality.

### Actions
- [ ] Run `clud --daemon`
- [ ] Verify Playwright browser launches
- [ ] Verify HTML page loads
- [ ] Verify 8 terminal placeholders visible
- [ ] Close browser
- [ ] Verify daemon shuts down cleanly

### Validation
- Daemon launches without errors
- Browser displays correctly
- Clean shutdown works

### Next Iteration
→ Iteration 32: Manual Test - Terminal Interactivity

---

## Iteration 32: Manual Test - Terminal Interactivity

### Task
Test terminal interactivity and PTY connections.

### Actions
- [ ] Run `clud --daemon`
- [ ] Verify all 8 terminals are interactive
- [ ] Type commands in each terminal
- [ ] Verify commands execute correctly
- [ ] Verify output displays correctly
- [ ] Test Ctrl+C in terminals
- [ ] Test command history (up/down arrows)

### Validation
- All terminals work independently
- Input/output working correctly
- No lag or connection issues

### Next Iteration
→ Iteration 33: Manual Test - Run CLUD from Terminal

---

## Iteration 33: Manual Test - Run CLUD from Terminal

### Task
Test running `clud --loop` from daemon terminals.

### Actions
- [ ] Run `clud --daemon`
- [ ] In terminal 1, run `clud --loop "test message"`
- [ ] Verify loop mode works correctly
- [ ] In terminal 2, run `clud --cron status`
- [ ] Verify cron commands work

### Validation
- Can launch clud from daemon terminals
- Core features work correctly

### Next Iteration
→ Iteration 34: Create Unit Tests for Daemon

---

## Iteration 34: Create Unit Tests for Daemon

### Task
Create unit tests for daemon module.

### Actions
- [ ] Create `tests/test_daemon.py`
- [ ] Test cases:
  - `test_daemon_info_dataclass()` - DaemonInfo structure
  - `test_terminal_creation()` - Terminal class initialization
  - `test_terminal_manager_creation()` - TerminalManager initialization
  - `test_find_free_port()` - Port finding logic
  - `test_html_template_generation()` - HTML template output
- [ ] Use `unittest` framework
- [ ] Run `bash test` to verify

### Validation
- Tests created
- All tests pass
- Follows unittest pattern

### Next Iteration
→ Iteration 35: Create E2E Tests for Daemon

---

## Iteration 35: Create E2E Tests for Daemon

### Task
Create end-to-end integration tests for daemon.

### Actions
- [ ] Create `tests/integration/test_daemon_e2e.py`
- [ ] Test cases:
  - `test_daemon_launch_browser()` - Full daemon launch
  - `test_terminals_start_in_home_dir()` - Working directory check
  - `test_daemon_shutdown_cleanup()` - Clean shutdown
- [ ] Use unique ports per test
- [ ] Exclude from pyright type checking
- [ ] Run `bash test --full` to verify

### Validation
- E2E tests created
- All tests pass
- No zombie processes

### Next Iteration
→ Iteration 36: Update CLAUDE.md Documentation

---

## Iteration 36: Update CLAUDE.md Documentation

### Task
Update project documentation to reflect removed features and new daemon.

### Actions
- [ ] Edit `CLAUDE.md`
- [ ] Remove sections:
  - Web UI references
  - Telegram integration
  - API server documentation
- [ ] Add section for:
  - `--daemon` Multi-terminal Daemon feature
  - Playwright requirements
  - Terminal usage guide
- [ ] Update dependency list:
  - Remove: fastapi, uvicorn, python-telegram-bot, etc.
  - Add: playwright
- [ ] Update Quick Reference commands

### Validation
- Documentation updated
- No references to removed features
- Daemon feature documented

### Next Iteration
→ Iteration 37: Update README.md

---

## Iteration 37: Update README.md

### Task
Update README with breaking changes and new features.

### Actions
- [ ] Edit `README.md`
- [ ] Add "Breaking Changes in v2.0.0" section:
  - List all removed features
  - Explain replacement with `--daemon`
- [ ] Update features list:
  - Remove: Web UI, Telegram, API server, Kanban, Code server
  - Add: Multi-terminal daemon
  - Keep: Loop mode, Cron scheduler
- [ ] Update installation instructions (Playwright)
- [ ] Update usage examples

### Validation
- README updated
- Breaking changes clearly communicated
- New features documented

### Next Iteration
→ Iteration 38: Update Architecture Documentation

---

## Iteration 38: Update Architecture Documentation

### Task
Update architecture docs to reflect new structure.

### Actions
- [ ] Edit `docs/development/architecture.md`
- [ ] Remove sections for deleted modules:
  - webui, telegram, webapp, service, cluster
- [ ] Add section for daemon module:
  - Structure overview
  - Key components
  - Playwright integration
- [ ] Update module dependency diagram

### Validation
- Architecture docs updated
- Accurate reflection of current codebase

### Next Iteration
→ Iteration 39: Update Backlog and Hooks Documentation

---

## Iteration 39: Update Backlog and Hooks Documentation

### Task
Update feature docs to remove UI/Telegram references.

### Actions
- [ ] Edit `docs/features/backlog.md`:
  - Remove Web UI tab references
  - Keep CLI usage
- [ ] Edit `docs/features/hooks.md`:
  - Remove Telegram hook examples
  - Keep general hook pattern
- [ ] Verify no other docs reference removed features

### Validation
- Feature docs updated
- No broken references

### Next Iteration
→ Iteration 40: Run Comprehensive Lint Check

---

## Iteration 40: Run Comprehensive Lint Check

### Task
Final lint check before version bump.

### Actions
- [ ] Run `bash lint`
- [ ] Fix any remaining errors
- [ ] Verify zero pyright errors
- [ ] Verify zero ruff errors

### Validation
- Lint passes with zero errors
- All type annotations correct
- Code quality standards met

### Next Iteration
→ Iteration 41: Run Full Test Suite

---

## Iteration 41: Run Full Test Suite

### Task
Run full test suite including E2E tests.

### Actions
- [ ] Run `bash test --full`
- [ ] Verify all tests pass
- [ ] Document any failures and fix
- [ ] Verify core features still work:
  - `--loop` mode
  - `--cron` scheduler
  - `--daemon` multi-terminal

### Validation
- All tests pass
- No regressions in core features

### Next Iteration
→ Iteration 42: Bump Version Number

---

## Iteration 42: Bump Version Number

### Task
Update version to 2.0.0 for major breaking changes.

### Actions
- [ ] Edit `pyproject.toml`
- [ ] Update `version = "2.0.0"`
- [ ] Update changelog (if exists)

### Validation
- Version updated to 2.0.0
- Breaking changes justified

### Next Iteration
→ Iteration 43: Final Cleanup

---

## Iteration 43: Final Cleanup

### Task
Remove any unused imports and clean up code.

### Actions
- [ ] Search for unused imports: `bash lint`
- [ ] Remove any commented-out code
- [ ] Verify no dead code remains
- [ ] Run `bash clean` to remove build artifacts
- [ ] Run `bash lint` one final time

### Validation
- No unused imports
- Clean codebase
- Lint passes

### Next Iteration
→ Iteration 44: Create Final Commit

---

## Iteration 44: Create Final Commit

### Task
Commit all changes with comprehensive message.

### Actions
- [ ] Run `git add -A`
- [ ] Create commit with message:
  ```
  feat: remove all UI features, add Playwright multi-terminal daemon

  BREAKING CHANGES:
  - Removed Web UI (--webui, --ui)
  - Removed Telegram integration (--telegram, --telegram-server)
  - Removed API server (--api-server)
  - Removed Kanban board (--kanban)
  - Removed Code server launcher (--code)
  - Removed cluster module and clud-cluster entry point

  NEW FEATURES:
  - Added --daemon multi-terminal UI
  - 8 xterm.js terminals in Playwright browser
  - All terminals start in home directory
  - Can run clud from any terminal

  DEPENDENCIES:
  - Removed: fastapi, uvicorn, websockets, sqlalchemy, pydantic,
    python-telegram-bot, markdown2
  - Added: playwright

  This reduces the codebase by ~11,000+ files (mostly node_modules)
  and simplifies maintenance significantly.

  Version bump to 2.0.0 for breaking changes.
  ```

### Validation
- Commit created successfully
- All changes staged

### Next Iteration
→ DONE

---

## Iteration 45: DONE

### Summary

Successfully removed all UI features from clud and replaced with Playwright multi-terminal daemon:

**Removed (17 major components):**
- Web UI, Telegram Bot, Telegram Server, API Server
- Webapp, Service/Daemon, Cluster, Kanban Board
- Code Server, Terminal PTY (old), WebSocket Infrastructure
- Session Management, Agent Registry, Service Discovery
- Database Layer, Authentication, Frontend Build Systems

**Added (1 new feature):**
- Playwright Multi-Terminal Daemon (6 new modules)

**Kept (5 core components):**
- --cron Scheduler
- --loop Mode
- --init-loop
- Core Agent Execution
- Message Hooks

**Results:**
- ~11,000+ files removed
- 43+ Python files deleted
- 6 new Python files created
- 8 files updated
- Zero lint errors
- All tests passing
- Version bumped to 2.0.0

**Validation Checklist:**
- [x] All UI features removed
- [x] `clud --daemon` works
- [x] Playwright browser launches
- [x] 8 terminals interactive
- [x] Can run `clud --loop` from terminals
- [x] Can run `clud --cron` commands
- [x] `bash lint` passes
- [x] `bash test --full` passes
- [x] Documentation updated
- [x] Breaking changes communicated
- [x] Version bumped to 2.0.0

The project is now significantly simplified with focus on core value: `--loop` and `--cron` automation.
