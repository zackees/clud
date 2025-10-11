# Implementation Guide: Claude CLI npx Auto-Install

This guide provides step-by-step instructions for implementing the npx fallback feature in clud.

---

## Overview

**Goal**: Automatically use `npx @anthropic-ai/claude-code` when Claude CLI is not installed

**Files to Modify**:
1. `src/clud/agent_foreground.py` - Core implementation
2. `README.md` - User documentation
3. `CLAUDE.md` - Developer documentation
4. `tests/test_yolo.py` - Unit tests

**Estimated Time**: 2-4 hours

---

## Step 1: Modify `agent_foreground.py`

### 1.1 Update `_find_claude_path()` Function

**Location**: Line 251-276

**Current Implementation**:
```python
def _find_claude_path() -> str | None:
    """Find the path to the Claude executable."""
    # ... existing code ...
    return None
```

**New Implementation**:
```python
def _find_claude_path() -> tuple[str | None, str]:
    """
    Find the path to the Claude executable with npx fallback.
    
    Returns:
        tuple[str | None, str]: (path, method) where method is:
            - "installed": Found claude in PATH
            - "npx": Will use npx to run Claude CLI
            - "none": Not found
    """
    # Priority 1: Try to find installed claude in PATH
    if platform.system() == "Windows":
        # On Windows, prefer .cmd and .exe extensions
        claude_path = shutil.which("claude.cmd") or shutil.which("claude.exe")
        if claude_path:
            return claude_path, "installed"

    # Fall back to generic "claude" (for Unix or git bash on Windows)
    claude_path = shutil.which("claude")
    if claude_path:
        return claude_path, "installed"

    # Check common Windows npm global locations
    if platform.system() == "Windows":
        possible_paths = [
            os.path.expanduser("~/AppData/Roaming/npm/claude.cmd"),
            os.path.expanduser("~/AppData/Roaming/npm/claude.exe"),
            "C:/Users/" + os.environ.get("USERNAME", "") + "/AppData/Roaming/npm/claude.cmd",
        ]
        for path in possible_paths:
            if os.path.exists(path):
                return path, "installed"

    # Priority 2: Try npx fallback (if not explicitly disabled)
    if os.environ.get("CLUD_DISABLE_NPX_FALLBACK", "false").lower() != "true":
        npx_path = shutil.which("npx")
        if npx_path:
            return npx_path, "npx"

    # Priority 3: Not found
    return None, "none"
```

### 1.2 Update `_build_claude_command()` Function

**Location**: Line 279-294

**Current Implementation**:
```python
def _build_claude_command(args: Args, claude_path: str) -> list[str]:
    """Build the Claude command with all arguments."""
    cmd = [claude_path, "--dangerously-skip-permissions"]
    # ... rest of implementation ...
    return cmd
```

**New Implementation**:
```python
def _build_claude_command(args: Args, claude_path: str, method: str) -> list[str]:
    """
    Build the Claude command with all arguments.
    
    Args:
        args: Parsed command-line arguments
        claude_path: Path to claude or npx executable
        method: Installation method ("installed" or "npx")
    """
    # Build command based on method
    if method == "npx":
        # Get version from env var or use latest
        version = os.environ.get("CLUD_CLAUDE_VERSION", "")
        package = f"@anthropic-ai/claude-code@{version}" if version else "@anthropic-ai/claude-code"
        cmd = [claude_path, "--yes", package]
    else:
        cmd = [claude_path]
    
    # Add dangerous permissions flag
    cmd.append("--dangerously-skip-permissions")

    # Add continue flag
    if args.continue_flag:
        cmd.append("--continue")

    # Add prompt
    if args.prompt:
        cmd.extend(["-p", args.prompt])

    # Add message
    if args.message:
        cmd.append(args.message)

    # Add remaining claude args
    cmd.extend(args.claude_args)

    return cmd
```

### 1.3 Update `_print_debug_info()` Function

**Location**: Line 297-309

**Add method parameter**:
```python
def _print_debug_info(claude_path: str | None, cmd: list[str], method: str, verbose: bool = False) -> None:
    """Print debug information about Claude execution."""
    if not verbose:
        return

    if claude_path:
        print(f"DEBUG: Found claude via: {method}", file=sys.stderr)
        print(f"DEBUG: Path: {claude_path}", file=sys.stderr)
        print(f"DEBUG: Platform: {platform.system()}", file=sys.stderr)
        if method == "installed":
            print(f"DEBUG: File exists: {os.path.exists(claude_path)}", file=sys.stderr)

    if cmd:
        print(f"DEBUG: Executing command: {cmd}", file=sys.stderr)
```

### 1.4 Update `run()` Function

**Location**: Line 341-432

**Current code** (around line 372):
```python
claude_path = _find_claude_path()
if not claude_path:
    print("Error: Claude Code is not installed or not in PATH", file=sys.stderr)
    print("Install Claude Code from: https://claude.ai/download", file=sys.stderr)
    return 1

# Build command
cmd = _build_claude_command(args, claude_path)
```

**New code**:
```python
# Find Claude executable
claude_path, method = _find_claude_path()
if not claude_path:
    print("Error: Claude Code is not installed or not in PATH", file=sys.stderr)
    print("", file=sys.stderr)
    print("Option 1: Install Node.js (if not installed) and clud will auto-use npx", file=sys.stderr)
    print("Option 2: Install Claude CLI manually:", file=sys.stderr)
    print("  npm install -g @anthropic-ai/claude-code", file=sys.stderr)
    print("  OR visit: https://claude.ai/download", file=sys.stderr)
    return 1

# Show informative message for npx usage (first time or verbose mode)
if method == "npx" and (args.verbose or not _is_npx_cached()):
    print("📦 Running Claude CLI via npx (will cache for future use)...", file=sys.stderr)

# Build command
cmd = _build_claude_command(args, claude_path, method)

# Print debug info
_print_debug_info(claude_path, cmd, method, args.verbose)
```

### 1.5 Add Helper Function for Cache Detection

**Location**: After `_print_error_diagnostics()` function

```python
def _is_npx_cached() -> bool:
    """
    Check if Claude CLI is likely cached by npx.
    This is a heuristic check - npx cache is in ~/.npm/_npx/
    """
    try:
        npx_cache = Path.home() / ".npm" / "_npx"
        if not npx_cache.exists():
            return False
        
        # Check if there are any cached packages
        # (not perfect, but good enough for UX purposes)
        return any(npx_cache.iterdir())
    except Exception:
        # If we can't determine, assume not cached
        return False
```

### 1.6 Update Dry-Run Mode

**Location**: Line 359-369

**Current code**:
```python
if args.dry_run:
    cmd_parts = ["claude", "--dangerously-skip-permissions"]
    # ... rest of dry-run logic
```

**New code**:
```python
if args.dry_run:
    # Detect method for dry-run display
    _, method = _find_claude_path()
    
    if method == "npx":
        version = os.environ.get("CLUD_CLAUDE_VERSION", "")
        package = f"@anthropic-ai/claude-code@{version}" if version else "@anthropic-ai/claude-code"
        cmd_parts = ["npx", "--yes", package, "--dangerously-skip-permissions"]
    else:
        cmd_parts = ["claude", "--dangerously-skip-permissions"]
    
    if args.continue_flag:
        cmd_parts.append("--continue")
    if args.prompt:
        cmd_parts.extend(["-p", args.prompt])
    if args.message:
        cmd_parts.append(args.message)
    cmd_parts.extend(args.claude_args)
    print("Would execute:", " ".join(cmd_parts))
    return 0
```

---

## Step 2: Add Unit Tests

**File**: `tests/test_yolo.py` or create new `tests/test_npx_fallback.py`

```python
import os
import subprocess
from unittest import mock

import pytest

from clud.agent_foreground import _build_claude_command, _find_claude_path, _is_npx_cached
from clud.agent_foreground_args import Args


def test_find_claude_path_installed():
    """Test finding installed claude takes priority."""
    with mock.patch("shutil.which") as mock_which:
        mock_which.side_effect = lambda x: "/usr/bin/claude" if x == "claude" else "/usr/bin/npx"
        path, method = _find_claude_path()
        assert path == "/usr/bin/claude"
        assert method == "installed"


def test_find_claude_path_npx_fallback():
    """Test npx fallback when claude not installed."""
    with mock.patch("shutil.which") as mock_which:
        # Only npx is available
        mock_which.side_effect = lambda x: "/usr/bin/npx" if x == "npx" else None
        path, method = _find_claude_path()
        assert path == "/usr/bin/npx"
        assert method == "npx"


def test_find_claude_path_npx_disabled():
    """Test npx fallback can be disabled via env var."""
    with mock.patch("shutil.which") as mock_which, mock.patch.dict(os.environ, {"CLUD_DISABLE_NPX_FALLBACK": "true"}):
        mock_which.side_effect = lambda x: "/usr/bin/npx" if x == "npx" else None
        path, method = _find_claude_path()
        assert path is None
        assert method == "none"


def test_find_claude_path_not_found():
    """Test when neither claude nor npx are available."""
    with mock.patch("shutil.which", return_value=None):
        path, method = _find_claude_path()
        assert path is None
        assert method == "none"


def test_build_command_installed():
    """Test command building with installed claude."""
    args = Args(
        continue_flag=False,
        prompt="test prompt",
        message=None,
        claude_args=[],
        api_key=None,
        api_key_from=None,
        cmd=None,
        dry_run=False,
        verbose=False,
    )
    cmd = _build_claude_command(args, "/usr/bin/claude", "installed")
    
    assert cmd[0] == "/usr/bin/claude"
    assert "--dangerously-skip-permissions" in cmd
    assert "-p" in cmd
    assert "test prompt" in cmd


def test_build_command_npx():
    """Test command building with npx method."""
    args = Args(
        continue_flag=False,
        prompt="test prompt",
        message=None,
        claude_args=[],
        api_key=None,
        api_key_from=None,
        cmd=None,
        dry_run=False,
        verbose=False,
    )
    cmd = _build_claude_command(args, "/usr/bin/npx", "npx")
    
    assert cmd[0] == "/usr/bin/npx"
    assert cmd[1] == "--yes"
    assert cmd[2] == "@anthropic-ai/claude-code"
    assert "--dangerously-skip-permissions" in cmd
    assert "-p" in cmd
    assert "test prompt" in cmd


def test_build_command_npx_with_version():
    """Test command building with npx and version pinning."""
    args = Args(
        continue_flag=False,
        prompt=None,
        message="test",
        claude_args=[],
        api_key=None,
        api_key_from=None,
        cmd=None,
        dry_run=False,
        verbose=False,
    )
    
    with mock.patch.dict(os.environ, {"CLUD_CLAUDE_VERSION": "2.0.14"}):
        cmd = _build_claude_command(args, "/usr/bin/npx", "npx")
    
    assert "@anthropic-ai/claude-code@2.0.14" in cmd


def test_is_npx_cached_true():
    """Test npx cache detection when cache exists."""
    with mock.patch("pathlib.Path.exists", return_value=True), mock.patch("pathlib.Path.iterdir", return_value=["some_cache"]):
        assert _is_npx_cached() is True


def test_is_npx_cached_false():
    """Test npx cache detection when cache doesn't exist."""
    with mock.patch("pathlib.Path.exists", return_value=False):
        assert _is_npx_cached() is False


@pytest.mark.skipif(not shutil.which("npx"), reason="npx not available")
def test_npx_execution_real():
    """Integration test: Run actual npx command."""
    result = subprocess.run(
        ["npx", "--yes", "@anthropic-ai/claude-code", "--version"],
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert result.returncode == 0
    assert "claude" in result.stdout.lower() or "2." in result.stdout
```

---

## Step 3: Update Documentation

### 3.1 Update `README.md`

**Section: Installation**

Add after the `pip install clud` section:

```markdown
### Claude CLI Installation (Automatic!)

If you have **Node.js/npm** installed, `clud` will automatically run Claude CLI via `npx` - no manual installation needed!

```bash
# If you have Node.js, just run:
pip install clud
clud  # Works immediately!
```

**Manual Installation (Optional)**

If you prefer a globally installed Claude CLI:

```bash
# Option 1: Via npm
npm install -g @anthropic-ai/claude-code

# Option 2: Via official installer
# Visit: https://claude.ai/download
```

**Disabling npx Fallback**

If you prefer to require manual Claude CLI installation:

```bash
export CLUD_DISABLE_NPX_FALLBACK=true
```
```

**New Section: Environment Variables**

Add to configuration section:

```markdown
### Environment Variables

#### Claude CLI Configuration
- `CLUD_DISABLE_NPX_FALLBACK` - Disable automatic npx fallback (default: `false`)
- `CLUD_CLAUDE_VERSION` - Pin specific Claude CLI version (e.g., `2.0.14`)
- `CLUD_NPX_VERBOSE` - Show verbose npx operations (default: `false`)

Example:
```bash
# Pin to specific version
export CLUD_CLAUDE_VERSION=2.0.14

# Disable npx fallback
export CLUD_DISABLE_NPX_FALLBACK=true
```
```

### 3.2 Update `CLAUDE.md`

**Add to Architecture section**:

```markdown
### Claude CLI Detection

`clud` uses a tiered approach to find and execute Claude CLI:

1. **Installed Claude CLI** - Searches PATH for `claude`, `claude.cmd`, or `claude.exe`
2. **npx Fallback** - If Claude CLI not found, uses `npx @anthropic-ai/claude-code`
3. **Error** - If neither available, provides helpful installation instructions

The npx fallback can be disabled via `CLUD_DISABLE_NPX_FALLBACK=true` environment variable.

**Performance Notes:**
- First npx run: ~3-5 seconds (downloads and caches package)
- Subsequent runs: ~1-2 seconds (uses cached package)
- Installed Claude CLI: Instant (no overhead)
```

---

## Step 4: Manual Testing

### 4.1 Test on Clean System

```bash
# 1. Ensure Claude CLI is NOT installed
which claude  # Should return nothing

# 2. Ensure npm/npx IS installed
which npx  # Should return path

# 3. Test clud with npx fallback
clud --dry-run -p "test"
# Expected: "Would execute: npx --yes @anthropic-ai/claude-code ..."

# 4. Test actual execution (requires API key)
clud -p "echo hello world"
# Expected: 📦 message, then Claude CLI runs
```

### 4.2 Test with Installed Claude CLI

```bash
# 1. Install Claude CLI
npm install -g @anthropic-ai/claude-code

# 2. Test clud
clud --dry-run -p "test"
# Expected: "Would execute: claude --dangerously-skip-permissions ..."

# 3. Verify installed version takes priority
clud --verbose -p "test" 2>&1 | grep "Found claude"
# Expected: "DEBUG: Found claude via: installed"
```

### 4.3 Test Opt-Out

```bash
# 1. Remove installed Claude CLI
npm uninstall -g @anthropic-ai/claude-code

# 2. Disable npx fallback
export CLUD_DISABLE_NPX_FALLBACK=true

# 3. Test clud
clud --dry-run -p "test"
# Expected: Error message about Claude CLI not found
```

### 4.4 Test Version Pinning

```bash
# 1. Pin to specific version
export CLUD_CLAUDE_VERSION=2.0.14

# 2. Test dry-run
clud --dry-run -p "test"
# Expected: "Would execute: npx --yes @anthropic-ai/claude-code@2.0.14 ..."
```

---

## Step 5: Run Tests

```bash
# Run unit tests
bash test

# Run specific npx tests
uv run pytest tests/test_npx_fallback.py -v

# Run integration tests
uv run pytest tests/ -v
```

---

## Step 6: Update CI/CD (Optional)

If you want to test npx fallback in CI, update `.github/workflows/linux-test.yml`:

```yaml
- name: Test clud with npx fallback
  run: |
    # Uninstall Claude CLI to test npx fallback
    npm uninstall -g @anthropic-ai/claude-code || true
    
    # Test dry-run (should use npx)
    output=$(clud --dry-run -p "test" 2>&1)
    if ! echo "$output" | grep -q "npx.*@anthropic-ai/claude-code"; then
      echo "Error: npx fallback not working"
      exit 1
    fi
    echo "✓ npx fallback working correctly"
```

---

## Rollback Plan

If issues arise:

1. **Quick Fix**: Add environment variable to disable by default
   ```python
   if os.environ.get("CLUD_ENABLE_NPX_FALLBACK", "false").lower() == "true":
   ```

2. **Full Rollback**: Revert `agent_foreground.py` changes
   ```bash
   git revert <commit-hash>
   ```

---

## Success Metrics

Track these metrics after deployment:

1. **GitHub Issues**: Monitor for npx-related issues
2. **User Feedback**: Collect feedback on first-run experience
3. **Error Rates**: Track "Claude CLI not found" errors (should decrease)
4. **Support Tickets**: Monitor installation-related support requests

---

## Common Issues & Solutions

### Issue: "npx not found"

**Solution**: User needs Node.js installed
```bash
# macOS
brew install node

# Ubuntu/Debian
sudo apt-get install nodejs npm

# Windows
# Download from: https://nodejs.org/
```

### Issue: "Network error downloading package"

**Solution**: Check network/firewall, or disable npx fallback
```bash
export CLUD_DISABLE_NPX_FALLBACK=true
```

### Issue: "Wrong Claude CLI version"

**Solution**: Clear npx cache or pin version
```bash
# Clear cache
rm -rf ~/.npm/_npx/

# Pin version
export CLUD_CLAUDE_VERSION=2.0.14
```

---

## Checklist

- [ ] Modify `_find_claude_path()` to return tuple
- [ ] Update `_build_claude_command()` to handle npx
- [ ] Add `_is_npx_cached()` helper function
- [ ] Update `_print_debug_info()` to show method
- [ ] Update `run()` function to use new signatures
- [ ] Update dry-run mode to show npx command
- [ ] Add unit tests for npx detection
- [ ] Add unit tests for command building
- [ ] Add integration test for npx execution
- [ ] Update README.md installation section
- [ ] Update README.md environment variables section
- [ ] Update CLAUDE.md architecture section
- [ ] Manual test on clean system
- [ ] Manual test with installed Claude CLI
- [ ] Manual test with opt-out
- [ ] Manual test with version pinning
- [ ] Run full test suite
- [ ] Update CHANGELOG.md

---

## Timeline

- **Day 1, Hours 1-2**: Code implementation
- **Day 1, Hours 2-3**: Unit tests
- **Day 1, Hour 3**: Documentation
- **Day 1, Hour 4**: Manual testing & verification

**Total**: ~4 hours for complete implementation

---

## Questions?

If you encounter issues during implementation, refer to:
- Full feasibility report: `FEASIBILITY_REPORT_CLAUDE_CLI_AUTO_INSTALL.md`
- Summary: `FEASIBILITY_SUMMARY.md`
- This guide: `IMPLEMENTATION_GUIDE_NPX_FALLBACK.md`

Happy coding! 🚀
