# Task: Claude Agent Completion Detection

## Research Findings: PTY vs Subprocess for Terminal Idle Detection

### Problem Statement
We need to implement a feature to detect when a Claude agent has finished executing. The question is whether we need PTY (pseudo-terminal) interface or if a subprocess with streaming input/output would suffice for detecting terminal idle state (defined as 3-second terminal idle).

### Research Results

#### Yes-Claude Project Investigation
After extensive searching, **no GitHub project named "yes-claude" was found** that implements PTY terminal idle detection. The search returned information about Anthropic's official Claude Code tool and related monitoring projects, but not the specific project referenced.

#### PTY vs Subprocess Analysis

**PTY (Pseudo-Terminal) Advantages:**
- **Real-time streaming**: PTY provides true real-time output streaming without buffering issues
- **Terminal behavior emulation**: Programs behave as if running in an interactive terminal (isatty() returns true)
- **Unbuffered output**: Most command-line tools automatically disable output buffering when connected to a PTY
- **Signal handling**: PTY properly handles terminal signals (SIGWINCH, SIGINT, etc.)
- **Single bidirectional channel**: All three standard streams (stdin, stdout, stderr) connect through one PTY

**Subprocess Limitations:**
- **Output buffering**: Programs often buffer output when not connected to a terminal
- **Non-interactive behavior**: Many tools behave differently when stdout is a pipe vs terminal
- **Delayed output**: Real-time streaming requires workarounds (python -u, character-by-character reading)
- **No terminal emulation**: isatty() returns false, affecting program behavior

#### Technical Implementation Considerations

**For Terminal Idle Detection:**
1. **PTY Approach**:
   - Monitor PTY output stream for activity
   - Track last output timestamp
   - Trigger completion when idle for 3+ seconds
   - More reliable for interactive commands

2. **Subprocess Approach**:
   - Monitor subprocess stdout/stderr streams
   - May miss buffered output that appears in bursts
   - Less reliable for commands that behave differently in non-terminal contexts

### Recommendation

**Use PTY for Claude agent completion detection** because:

1. **Accurate terminal behavior**: PTY ensures commands behave as they would in a real terminal
2. **Real-time output detection**: No buffering delays that could affect idle detection timing
3. **Better compatibility**: Works with progress indicators, interactive prompts, and terminal-aware tools
4. **Reliable idle detection**: True real-time output monitoring enables accurate 3-second idle detection

**Implementation approach:**
```python
import pty
import select
import time
import subprocess
import os

def detect_agent_completion(command, idle_timeout=3.0):
    master, slave = pty.openpty()
    process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave)

    last_activity = time.time()
    while process.poll() is None:
        ready, _, _ = select.select([master], [], [], 0.1)
        if ready:
            data = os.read(master, 1024)
            if data:
                last_activity = time.time()
                # Process output data

        if time.time() - last_activity > idle_timeout:
            # Agent appears to be finished (3+ seconds idle)
            break
```

**Conclusion**: PTY is the recommended approach for reliable Claude agent completion detection, despite being slightly more complex than subprocess, due to its superior real-time monitoring capabilities and accurate terminal behavior emulation.

## Windows PTY Implementation Challenges

### The Windows Problem
**Windows is hard** for PTY implementation because:

1. **No Native PTY Support**: Python's standard library `pty` module is Unix-only
   - Documentation states: "Availability: Unix. Pseudo-terminal handling is highly platform dependent"
   - Windows has no equivalent to Unix pseudo-terminals in the standard library

2. **Different Terminal Architecture**: Windows console architecture fundamentally differs from Unix PTY model

### Authoritative Windows PTY Package: **pywinpty**

**pywinpty** is the most authoritative and actively maintained solution for Windows PTY:

- **Latest Version**: 3.0.0 (Released: August 12, 2025)
- **Python Support**: Requires Python >=3.9
- **Dual Backend Support**: ConPTY (native Windows 10+ API) + legacy winpty fallback
- **Active Maintenance**: Regular updates and Windows-specific optimizations

**Installation**: `pip install pywinpty`

### Performance Comparison: ConPTY vs WinPTY

Based on 2025 benchmarks:
- **WinPTY**: Significantly faster performance for terminal operations
- **ConPTY**: Native Windows solution but "quite a bit slower" than WinPTY
- **Trade-off**: WinPTY has better performance but fundamental unfixable bugs; ConPTY is slower but actively developed by Microsoft

### Alternative Options (Not Recommended)

1. **pexpect**: Unix-focused, limited Windows support
2. **ptyprocess**: Low-level foundation, not suitable for direct Windows use
3. **wexpect**: Windows pexpect alternative, but poorly maintained

### Stdlib vs External Package Recommendation

**Use External Package (pywinpty)** because:

1. **No Stdlib Option**: Python's `pty` module doesn't work on Windows
2. **Mature Solution**: pywinpty is battle-tested (used by Jupyter, VS Code, etc.)
3. **Active Development**: Regular updates and Windows 11 compatibility
4. **Cross-Platform Code**: Can conditionally use pywinpty on Windows, stdlib pty on Unix

### Updated Implementation for Windows

```python
import sys
import time

if sys.platform.startswith("win"):
    from winpty import PtyProcess
    def detect_agent_completion_windows(command, idle_timeout=3.0):
        process = PtyProcess.spawn(command)
        last_activity = time.time()

        while process.isalive():
            try:
                data = process.read(timeout=0.1)
                if data:
                    last_activity = time.time()
            except:
                pass

            if time.time() - last_activity > idle_timeout:
                break
else:
    # Unix implementation using stdlib pty
    import pty
    import select
    import subprocess
    import os

    def detect_agent_completion_unix(command, idle_timeout=3.0):
        master, slave = pty.openpty()
        process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave)
        # ... (previous Unix implementation)
```

**Final Recommendation**: Use **pywinpty** for Windows PTY functionality - it's the authoritative, actively maintained solution that handles Windows terminal complexity while providing reliable idle detection capabilities.