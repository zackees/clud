# CLUD Development Environment - Implementation Task

## Executive Summary
Create a unified Docker-based development environment that combines code-server (VS Code in browser) with Claude CLI and modern developer tools, launched via the `clud --ui` command.

## Final Technology Stack Decision

### Primary Components
- **Base Image:** Ubuntu 25.04 (matching CLAUD_DOCKER.dockerfile, not Debian 12 like upstream code-server)
- **IDE:** code-server installed manually on port 8080 (internal), mapped to configurable external port
- **AI Assistant:** Claude CLI with MCP server support
- **Python:** 3.13+ with uv package manager pre-installed
- **Node.js:** via fnm (Fast Node Manager)
- **Shell:** Bash (not Zsh)

### Why code-server over OpenVSCode Server
1. Better self-hosting features for individual developers
2. Excellent iPad/mobile support
3. Built-in proxy for accessing container ports
4. Flexible authentication (can disable for local dev)
5. More mature Docker integration

## Implementation Phases

### Phase 1: Create Merged Dockerfile
Build a single Dockerfile that:
- Starts FROM `ubuntu:25.04` (for consistency with existing CLAUD_DOCKER.dockerfile)
- Installs code-server manually (not using their Docker image)
- Installs Claude CLI and configures with Anthropic API key
- Adds modern developer tools:
  - **Essential:** git, lazygit, ripgrep, fd, fzf, bat
  - **Python:** uv (pre-installed), Python 3.13+
  - **Node:** fnm with Node.js v22
  - **Extras:** eza, zoxide, delta, gh CLI, jq, htop
- Sets up MCP servers (start with essential ones, make configurable):
  - filesystem - File operations
  - git - Repository management
  - fetch - Web content fetching
- Configures auto-run of `./install` script if present on first launch
- Uses `/home/coder/project` as workspace directory

### Phase 2: Update CLUD CLI
Modify `src/clud/cli.py` to:
- Add `--ui` flag to launch code-server
- Add `--port` flag (default: 8743, auto-detect if occupied)
- Add `--api-key` flag for Anthropic API key
- Remove any Docker image fetching logic
- Build image locally if not exists: `docker build -t clud-dev:latest .`
- Launch container with proper mounts and environment:
  ```bash
  docker run -d \
    --name clud-dev \
    -p <port>:8080 \
    -e ANTHROPIC_API_KEY=<key> \
    -e PASSWORD="" \
    -v $(pwd):/home/coder/project \
    -v ~/.config:/home/coder/.config \
    -v ~/.local:/home/coder/.local \
    clud-dev:latest
  ```
- Auto-open browser to `http://localhost:<port>`

### Phase 3: MCP Server Configuration
Implement easy MCP server management:
- Default set: filesystem, git, fetch (always enabled)
- Optional servers via environment variable or config file
- Future: `--mcp-servers` flag to enable specific servers

## Directory Structure
```
clud/
├── Dockerfile              # Merged dockerfile (NEW)
├── src/clud/
│   └── cli.py             # Main CLI entry point (MODIFY)
├── CLAUD_DOCKER.dockerfile # Reference (DELETE after merge)
├── OPEN_VS_CODE_SERVER.dockerfile # Reference (DELETE after merge)
└── TASK.md                # This file (UPDATE as needed)
```

## Success Criteria
1. ✅ `clud --ui` launches code-server in browser
2. ✅ Claude CLI works inside container with API key
3. ✅ Modern dev tools available (lazygit, ripgrep, etc.)
4. ✅ Python development ready with uv
5. ✅ Auto-runs `./install` if present
6. ✅ Port collision handling works
7. ✅ No authentication for local development

## Configuration Examples

### Basic Usage
```bash
clud --ui                    # Launch with defaults
clud --ui --port 9000       # Custom port
clud --ui --api-key sk-...  # With API key
```

### Environment Variables
```bash
export ANTHROPIC_API_KEY=sk-ant-...
export CLUD_PORT=9000
clud --ui  # Uses env vars
```

## Next Steps After This Task
1. Add persistent container management (stop, restart, attach)
2. Implement MCP server plugin system
3. Add VS Code extension pre-installation
4. Create development presets (Python, Node, Full-stack)
5. Add multi-project support with named containers

## Notes
- Container name: `clud-dev` (consider making configurable later)
- Data persistence via volume mounts to host directories
- No authentication by default (PASSWORD="") but keep capability for future
- Focus on developer experience - everything should "just work"
- **IMPORTANT:** code-server upstream uses Debian 12, but we'll use Ubuntu 25.04 for consistency with CLAUD_DOCKER.dockerfile
- code-server will be installed via their install script or .deb package on Ubuntu