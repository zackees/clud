# CLUD Development Environment
# Optimized build: Static base image with Claude CLI and code-server
# Project is mounted, not installed, for faster rebuilds
FROM ubuntu:24.04

# Build arguments
ARG USERNAME=coder
ARG USER_UID=1001
ARG USER_GID=1001
ARG CODE_SERVER_VERSION=4.96.2

# Environment variables
ENV DEBIAN_FRONTEND=noninteractive \
    LANG=en_US.UTF-8 \
    LC_ALL=en_US.UTF-8 \
    SHELL=/bin/bash \
    PATH=/home/${USERNAME}/.local/bin:/usr/local/bin:$PATH

# ============================================================================
# Install system packages and development tools (STATIC - cacheable)
# ============================================================================

RUN apt-get update && apt-get install -y \
    # Core tools
    build-essential \
    ca-certificates \
    curl \
    wget \
    git \
    gnupg \
    unzip \
    zip \
    sudo \
    locales \
    openssh-client \
    # Essential CLI tools
    fzf \
    ripgrep \
    fd-find \
    jq \
    htop \
    vim \
    dos2unix \
    rsync \
    && rm -rf /var/lib/apt/lists/*

# Configure locale
RUN echo "en_US.UTF-8 UTF-8" > /etc/locale.gen && \
    locale-gen && \
    update-ca-certificates

# ============================================================================
# OPTIONAL TOOLS (commented out for faster builds)
# IMPORTANT: Do NOT remove these comments - they contain useful tools that can be enabled when needed
# ============================================================================

# Install lazygit (uncomment for git UI)
RUN LAZYGIT_VERSION=$(curl -s "https://api.github.com/repos/jesseduffield/lazygit/releases/latest" | jq -r '.tag_name' | sed 's/v//') && \
    curl -Lo lazygit.tar.gz "https://github.com/jesseduffield/lazygit/releases/latest/download/lazygit_${LAZYGIT_VERSION}_Linux_x86_64.tar.gz" && \
    tar xf lazygit.tar.gz lazygit && \
    install lazygit /usr/local/bin && \
    rm -f lazygit.tar.gz lazygit

# Install Go (needed for some MCP servers)
RUN ARCH=$(dpkg --print-architecture) && \
    if [ "$ARCH" = "amd64" ]; then GOARCH="amd64"; else GOARCH="arm64"; fi && \
    wget -O go.tar.gz "https://go.dev/dl/go1.23.4.linux-${GOARCH}.tar.gz" && \
    tar -C /usr/local -xzf go.tar.gz && \
    rm go.tar.gz

ENV CGO_ENABLED=0 \
    PATH="/usr/local/go/bin:$PATH"

# ============================================================================
# Create user and setup permissions (STATIC - cacheable)
# ============================================================================

# Create non-root user with passwordless sudo
RUN groupadd --gid ${USER_GID} ${USERNAME} && \
    useradd --uid ${USER_UID} --gid ${USER_GID} -m ${USERNAME} -s /bin/bash && \
    echo "${USERNAME} ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/${USERNAME} && \
    chmod 0440 /etc/sudoers.d/${USERNAME} && \
    mkdir -p /home/${USERNAME}/project /workspace /host /var/log && \
    chown -R ${USERNAME}:${USERNAME} /home/${USERNAME} && \
    chown ${USERNAME}:${USERNAME} /workspace /host && \
    chmod 755 /workspace /host /var/log && \
    touch /var/log/clud-sync.log && \
    chown ${USERNAME}:${USERNAME} /var/log/clud-sync.log

# ============================================================================
# Install code-server (STATIC - cacheable)
# ============================================================================

# Install code-server using their install script
RUN curl -fsSL https://code-server.dev/install.sh | sh -s -- --version=${CODE_SERVER_VERSION} && \
    mkdir -p /home/${USERNAME}/.config/code-server && \
    chown -R ${USERNAME}:${USERNAME} /home/${USERNAME}/.config

# ============================================================================
# Install Python 3.13 via deadsnakes PPA (STATIC - cacheable)
# ============================================================================

USER root

# Install Python 3.13 from deadsnakes PPA (much faster than building from source)
RUN apt-get update && apt-get install -y software-properties-common && \
    add-apt-repository -y ppa:deadsnakes/ppa && \
    apt-get update && \
    apt-get install -y \
        python3.13 \
        python3.13-venv \
        python3.13-dev \
    && rm -rf /var/lib/apt/lists/*

# Create symlinks for python and python3
RUN ln -sf /usr/bin/python3.13 /usr/local/bin/python3 && \
    ln -sf /usr/bin/python3.13 /usr/local/bin/python

# Install pip for Python 3.13
RUN python3.13 -m ensurepip --upgrade

# Install uv system-wide
RUN curl -LsSf https://astral.sh/uv/install.sh | sh && \
    mv /root/.local/bin/uv /usr/local/bin/uv && \
    chmod +x /usr/local/bin/uv

# ============================================================================
# Install Claude CLI (STATIC - cacheable)
# ============================================================================

USER root
WORKDIR /root

# Install Claude CLI for root user
RUN curl -fsSL https://claude.ai/install.sh | bash

# ============================================================================
# OPTIONAL: Node.js and MCP Servers (commented out for faster builds)
# IMPORTANT: Do NOT remove these comments - they enable Node.js ecosystem and MCP servers
# ============================================================================

# Install fnm (Fast Node Manager) and Node.js 22
RUN curl -fsSL https://fnm.vercel.app/install | bash
ENV PATH="/root/.local/share/fnm:$PATH"
RUN bash -c 'source /root/.bashrc && fnm install 22 && fnm default 22'

# Install MCP servers via npm (requires Node.js above)
RUN bash -c 'source /root/.bashrc && eval "$(fnm env)" && npm install -g @modelcontextprotocol/server-filesystem'

# Setup default MCP server configurations
RUN mkdir -p /home/${USERNAME}/.config/claude && \
    cat > /home/${USERNAME}/.config/claude/mcp_config.json << 'EOF'
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/home/coder/project"]
    }
  }
}
EOF

# ============================================================================
# Setup shell environment and aliases (STATIC - cacheable)
# ============================================================================

# Create optimized bashrc with clud alias for root user
# NOTE: Currently using root user's bashrc. The coder user might be removed in the future.
RUN cat >> /root/.bashrc << 'EOF'

# IMPORTANT: Do NOT remove these comments - they contain environment setup for optional tools

# FNM setup (enabled - Node.js/fnm is installed above)
export PATH="/home/coder/.local/share/fnm:$PATH"
eval "$(fnm env --use-on-cd)"

# PATH setup
export PATH="/root/.local/bin:$PATH"

# Aliases
alias ll="ls -la"
alias la="ls -A"
alias l="ls -CF"
alias ..="cd .."
alias ...="cd ../.."
alias lg="lazygit"

# CLUD alias - the main purpose of this container
alias clud='claude code --dangerously-skip-permissions'

# Container sync commands (using container-sync script)
alias sync='python3 /usr/local/bin/container-sync sync'
alias sync-preview='python3 /usr/local/bin/container-sync sync-preview'

# Show sync logs
alias sync-logs='tail -f /var/log/clud-sync.log 2>/dev/null || echo "No sync logs available"'

# Better history
export HISTSIZE=10000
export HISTFILESIZE=20000
export HISTCONTROL=ignoreboth
shopt -s histappend

# Editor
export EDITOR=vim

# Prompt with color
PS1='\[\033[01;32m\]\u@niteris-clud\[\033[00m\]:\[\033[01;34m\]\w\[\033[00m\]\$ '

# Auto-cd to workspace directory if it exists
if [ -d "/workspace" ]; then
    cd /workspace
fi

# Welcome message
echo "┌─ CLUD Development Environment ─────────────────────────────────────┐"
echo "│ Working Directory: /workspace (synced from /host)                  │"
echo "│ Type 'clud' to start Claude with dangerous permissions enabled     │"
echo "│                                                                     │"
echo "│ Sync Commands:                                                      │"
echo "│   sync        - Save workspace changes back to host                │"
echo "│   sync-status - Preview what would be synced (dry-run)             │"
echo "│                                                                     │"
echo "│ Note: Your project files are isolated in /workspace until you run  │"
echo "│       'sync' to save changes back to the host filesystem           │"
echo "└────────────────────────────────────────────────────────────────────┘"
echo ""
EOF

# Fix line endings for Windows compatibility
RUN dos2unix /root/.bashrc

# ============================================================================
# Setup entrypoint (LIGHTWEIGHT - fast to change)
# ============================================================================

USER root

# Copy Python sync scripts
COPY src/clud/container_sync.py /usr/local/bin/container-sync
COPY src/clud/bg.py /usr/local/bin/bg.py
RUN chmod +x /usr/local/bin/container-sync /usr/local/bin/bg.py

# Create Claude commands directory
RUN mkdir -p /root/.claude/commands && \
    chmod -R 755 /root/.claude/commands

# Copy only the plugin files if they exist
COPY docker/plugins/claude/commands/*.md /root/.claude/commands/ 2>/dev/null || echo "No default plugins found - plugins will be installed at runtime"

# Copy and set up entrypoint script
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# Expose code-server port
EXPOSE 8080

# Set working directory
WORKDIR /workspace

# Set entrypoint and default command
ENTRYPOINT ["/entrypoint.sh"]