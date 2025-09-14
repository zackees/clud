# Task: Create Merged Dockerfile for CLUD Development Environment

## 1. Dockerfile Analysis

### CLAUD_DOCKER.dockerfile
**Purpose:** Creates a comprehensive Claude AI development environment with MCP servers and developer tools.

**Key Features:**
- **Base System:** Ubuntu 25.04 with essential dev tools (build-essential, git, python3, curl, wget)
- **Development Tools:**
  - Editors: Vim, Neovim (with LazyVim config), VSCode integration
  - Shell: Bash (will be used instead of Zsh)
  - Modern CLI tools: lazygit (terminal git UI), ripgrep, fd, fzf, bat, eza, zoxide
  - Version control: git, gh (GitHub CLI), delta (better diffs)
- **Language Support:**
  - Go 1.21.5 (for building MCP servers)
  - Node.js v22 via fnm (Fast Node Manager)
  - Python 3 with pip
- **Claude AI Integration:**
  - Claude CLI installation
  - MCP (Model Context Protocol) servers:
    - Unsplash MCP server (built from source)
    - Context7 MCP server (HTTP transport)
    - Playwright MCP server (npm package)
- **User Environment:**
  - Non-root user (claude-user) with sudo privileges
  - GPG agent socket forwarding support
  - Claude config merging from host
  - Workspace directory support with auto-navigation
- **Security & Convenience:**
  - Passwordless sudo for development
  - SSH key forwarding compatibility
  - Dangerous mode support for Claude CLI

### OPEN_VS_CODE_SERVER_DOCKER.dockerfile
**Purpose:** Lightweight VS Code Server (OpenVSCode) web IDE environment.

**Key Features:**
- **Base System:** Ubuntu 25.04 minimal with essentials
- **VS Code Server:**
  - OpenVSCode Server v1.103.1
  - Web-based IDE accessible on port 8743 (will be changed from 3000)
  - No authentication by default (connection token optional for future)
  - Multi-architecture support (x64, arm64, armhf)
- **User Environment:**
  - Non-root user (dev) with sudo privileges
  - Dedicated workspace directory (/workspace)
- **Configuration:**
  - Disabled telemetry and update checks
  - User data persistence in ~/.vscode-server
  - Default workspace folder configuration

## VS Code Server Solution Analysis

### code-server vs OpenVSCode Server Comparison

**code-server (by Coder):**
- Patched fork of VS Code with additional self-hosting features
- Built-in password authentication with rate limiting (Argon2 hashing)
- Supports hosting at sub-paths and custom proxy settings
- Better for individual developers with security needs
- More configuration options and self-contained web views
- Mature Docker-in-Docker support
- Uses Open-VSX marketplace by default

**OpenVSCode Server (by Gitpod):**
- Direct fork staying closely aligned with upstream VS Code
- Minimal changes - only adds what's needed to run in browser
- No built-in authentication (requires manual token setup)
- Daily automated updates from upstream VS Code
- Better for teams wanting vanilla VS Code experience
- Simpler, less opinionated approach
- Backed by major companies (GitLab, VMware, Uber, SAP)

### Decision for CLUD Project

**Using code-server (codercom/code-server)** for the following reasons:
1. **No authentication needed** - Can run without password for local development
2. **Better self-hosting features** - More suitable for individual developer containers
3. **iPad/mobile support** - Excellent documented mobile access methods
4. **Flexible configuration** - Can easily disable auth while keeping option for future
5. **Proxy capabilities** - Built-in proxy for accessing container ports
6. **Extension flexibility** - Better support for custom extension installation
7. **Official Docker image** - Well-maintained `codercom/code-server:latest`

### code-server Installation Method

**Docker Base Image:** `codercom/code-server:latest`
- Supports amd64 and arm64 architectures
- Runs on port 8080 internally (we'll map to 8743)
- User runs as `coder` with configurable UID/GID
- Home directory at `/home/coder`
- Project mounted at `/home/coder/project`

**Key Environment Variables:**
- `DOCKER_USER` - Set to match host user
- `PASSWORD` - Optional password (we'll disable)
- `PORT` - Internal port (default 8080)

## 2. Feature Importance Ranking for CLUD Project

### Critical (Must Have):
1. **VS Code Server (OpenVSCode)** - Primary UI interface for development
2. **Base Ubuntu 25.04 system** - Consistent environment
3. **Non-root user with sudo** - Security and flexibility
4. **Workspace directory management** - Project organization
5. **Essential dev tools** (git, curl, wget, build-essential) - Basic functionality
6. **uv** - Fast Python package installer and virtual environment manager (pre-installed)
7. **Auto-run ./install script** - Run if exists on first launch for repo setup

### High Priority:
8. **Claude CLI** - Core AI assistant integration
9. **Node.js environment (fnm)** - JavaScript/TypeScript development
10. **Python 3.13+ with pip** - Python development support
11. **Bash shell** - Standard shell environment
12. **MCP servers** - Extended Claude capabilities
13. **lazygit** - Terminal UI for git commands with visual interface
14. **ripgrep (rg)** - Fast recursive grep with smart defaults
15. **fd** - Fast and user-friendly alternative to find

### Medium Priority:
16. **Neovim with LazyVim** - Alternative editor option
17. **fzf** - Fuzzy finder for files and command history
18. **GitHub CLI (gh)** - GitHub integration
19. **Go language support** - For building tools
20. **GPG agent forwarding** - Secure operations
21. **eza** - Modern replacement for ls with colors and icons
22. **zoxide** - Smarter cd command that learns your habits
23. **bat** - Cat clone with syntax highlighting
24. **delta** - Better git diffs with syntax highlighting

### Nice to Have:
25. **htop/btop** - Interactive process viewers
26. **tree** - Directory tree visualization
27. **jq** - JSON processor
28. **tldr** - Simplified man pages
29. **ncdu** - Disk usage analyzer
30. **duf** - Better df alternative
31. **Colorful bash prompt** - Visual enhancement
32. **Tokyo Night theme** - Consistent theming across tools

## 3. Merge Strategy

The merged Dockerfile will:
1. Use `codercom/code-server:latest` as base image instead of Ubuntu
2. Install code-server as the primary UI component (already in base)
3. Include Claude CLI and essential MCP servers
4. Setup a unified user environment with proper permissions and bash shell
5. Configure both code-server and terminal access with bash
6. Install modern developer tools on top of code-server base
7. Optimize layers for build efficiency and size

## 4. Implementation Plan

### Phase 1: Dockerfile Creation
- Combine essential features from both files
- Optimize build stages for caching
- Ensure VS Code Server and Claude CLI coexist properly
- Test multi-architecture support
- Install modern developer tools (lazygit, ripgrep, fd, bat, eza, etc.)
- Pre-install uv for Python package management

### Phase 2: CLUD CLI Updates
- Remove Docker image fetching logic
- Implement local Docker build with caching
- Add `--ui` flag handler for VS Code Server launch
- Add `--port` flag for custom port selection
- Add `--api-key` flag to pass Anthropic API key
- Implement port availability checking and auto-selection
- Configure dynamic port forwarding without authentication
- Pass environment variables (ANTHROPIC_API_KEY) to container
- Handle container lifecycle (start, stop, exec)
- Auto-run `./install` script if exists on first container launch

### Phase 3: Client-Server Connection
- Use configurable port for VS Code Server (default 8743)
- Auto-detect available port if default is occupied
- No authentication required (keep token capability for future if needed)
- Open browser automatically to VS Code interface at selected port
- Provide terminal access through VS Code or direct exec
- Ensure all modern CLI tools are accessible in terminal

## 5. Technical Specifications

### Docker Image:
- **Name:** clud-dev
- **Tag:** latest (auto-built if missing)
- **Base:** `codercom/code-server:latest`
- **Exposed Ports:** 8080 internal (mapped to configurable external port)
- **Volumes:**
  - `/home/coder/project` - Project files (workspace)
  - `/home/coder/.local` - User local data
  - `/home/coder/.config` - Configuration including Claude settings
  - SSH/GPG sockets (optional)

### CLUD CLI Changes:
- **New Flags:**
  - `--ui` - Launch code-server interface
  - `--port <port>` - Specify port for code-server (default: 8743, auto-detect if occupied)
  - `--api-key <key>` - Pass Anthropic API key to container
- **Build Command:** `docker build -t clud-dev:latest .`
- **Run Command:**
  ```
  docker run -d -p <port>:8080 \
    -e ANTHROPIC_API_KEY=<key> \
    -e PASSWORD="" \
    -v $(pwd):/home/coder/project \
    -v ~/.config:/home/coder/.config \
    -v ~/.local:/home/coder/.local \
    clud-dev:latest
  ```
- **Connection:** Auto-open `http://localhost:<port>` (no auth with PASSWORD="")
- **Port Selection:** Client will find available port if default is occupied

### Environment Variables:
- `ANTHROPIC_API_KEY` - Anthropic API key passed from client
- `PASSWORD` - Empty string to disable authentication
- `DOCKER_USER` - Set to match host user
- `WORKSPACE_PATH` - `/home/coder/project` (fixed in code-server)
- `CLAUDE_DANGEROUS_MODE` - Enable Claude dangerous mode