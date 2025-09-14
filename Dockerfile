# CLUD Development Environment
# Combines code-server (VS Code in browser) with Claude CLI and modern developer tools
FROM ubuntu:25.04

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
    PATH=/home/${USERNAME}/.local/bin:/usr/local/go/bin:$PATH

# ============================================================================
# Stage 1: System dependencies and tools
# ============================================================================

# Install system packages and development tools
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
    # Python
    python3 \
    python3-pip \
    python3-venv \
    # Essential CLI tools
    fzf \
    ripgrep \
    fd-find \
    jq \
    htop \
    vim \
    && rm -rf /var/lib/apt/lists/*

# Configure locale
RUN echo "en_US.UTF-8 UTF-8" > /etc/locale.gen && \
    locale-gen && \
    update-ca-certificates

# # Install lazygit (commented out for faster build)
# RUN LAZYGIT_VERSION=$(curl -s "https://api.github.com/repos/jesseduffield/lazygit/releases/latest" | jq -r '.tag_name' | sed 's/v//') && \
#     curl -Lo lazygit.tar.gz "https://github.com/jesseduffield/lazygit/releases/latest/download/lazygit_${LAZYGIT_VERSION}_Linux_x86_64.tar.gz" && \
#     tar xf lazygit.tar.gz lazygit && \
#     install lazygit /usr/local/bin && \
#     rm -f lazygit.tar.gz lazygit

# # Install Go (needed for some MCP servers) (commented out for faster build)
# RUN ARCH=$(dpkg --print-architecture) && \
#     if [ "$ARCH" = "amd64" ]; then GOARCH="amd64"; else GOARCH="arm64"; fi && \
#     wget -O go.tar.gz "https://go.dev/dl/go1.23.4.linux-${GOARCH}.tar.gz" && \
#     tar -C /usr/local -xzf go.tar.gz && \
#     rm go.tar.gz

# ENV CGO_ENABLED=0

# ============================================================================
# Stage 2: Create user and setup permissions
# ============================================================================

# Create non-root user with passwordless sudo
RUN groupadd --gid ${USER_GID} ${USERNAME} && \
    useradd --uid ${USER_UID} --gid ${USER_GID} -m ${USERNAME} -s /bin/bash && \
    echo "${USERNAME} ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/${USERNAME} && \
    chmod 0440 /etc/sudoers.d/${USERNAME} && \
    mkdir -p /home/${USERNAME}/project && \
    chown -R ${USERNAME}:${USERNAME} /home/${USERNAME}

# ============================================================================
# Stage 3: Install code-server
# ============================================================================

# Install code-server using their install script
RUN curl -fsSL https://code-server.dev/install.sh | sh -s -- --version=${CODE_SERVER_VERSION} && \
    mkdir -p /home/${USERNAME}/.config/code-server && \
    chown -R ${USERNAME}:${USERNAME} /home/${USERNAME}/.config

# ============================================================================
# Stage 4: User environment setup (fnm, node, Python tools)
# ============================================================================

# Switch to user for remaining setup
USER ${USERNAME}
WORKDIR /home/${USERNAME}

# TODO: Install fnm (Fast Node Manager) and Node.js 22 (commented out for faster initial build)
# RUN curl -fsSL https://fnm.vercel.app/install | bash
# ENV PATH="/home/${USERNAME}/.local/share/fnm:$PATH"
# RUN bash -c 'eval "$(fnm env)" && fnm install 22 && fnm default 22'

# Install uv (Python package manager) system-wide
USER root
RUN curl -LsSf https://astral.sh/uv/install.sh | sh && \
    mv /root/.local/bin/uv /usr/local/bin/uv && \
    chmod +x /usr/local/bin/uv

USER ${USERNAME}

# ============================================================================
# Stage 5: Install Claude CLI and MCP servers
# ============================================================================

# Install Claude CLI
SHELL ["/bin/bash", "-c"]

# Install Claude CLI
RUN curl -fsSL https://claude.ai/install.sh | bash

# Install MCP servers via npm
# RUN export PATH="/home/${USERNAME}/.local/share/fnm:$PATH" && \
#     eval "$(fnm env)" && \
#     npm install -g \
#         @modelcontextprotocol/server-filesystem \
#         @modelcontextprotocol/server-git \
#         @modelcontextprotocol/server-fetch

# Setup default MCP server configurations
# RUN mkdir -p /home/${USERNAME}/.config/claude && \
#     cat > /home/${USERNAME}/.config/claude/mcp_config.json << 'EOF'
# {
#   "mcpServers": {
#     "filesystem": {
#       "command": "npx",
#       "args": ["-y", "@modelcontextprotocol/server-filesystem", "/home/coder/project"]
#     },
#     "git": {
#       "command": "npx",
#       "args": ["-y", "@modelcontextprotocol/server-git", "--repository", "/home/coder/project"]
#     },
#     "fetch": {
#       "command": "npx",
#       "args": ["-y", "@modelcontextprotocol/server-fetch"]
#     }
#   }
# }
# EOF

# ============================================================================
# Stage 6: Setup entrypoint and configuration
# ============================================================================

# Switch back to root for entrypoint setup
USER root

# Copy and set up entrypoint script
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# ============================================================================
# Stage 7: Final configuration
# ============================================================================

# Create bashrc for better shell experience
USER ${USERNAME}
RUN cat >> /home/${USERNAME}/.bashrc << 'EOF'

# TODO: FNM setup (commented out for faster initial build)
# export PATH="/home/coder/.local/share/fnm:$PATH"
# eval "$(fnm env --use-on-cd)"

# UV setup
export PATH="/home/coder/.local/bin:$PATH"

# Claude CLI setup
export PATH="/home/coder/.local/bin:$PATH"

# Aliases
alias ll="ls -la"
alias la="ls -A"
alias l="ls -CF"
alias ..="cd .."
alias ...="cd ../.."
alias lg="lazygit"

# Better history
export HISTSIZE=10000
export HISTFILESIZE=20000
export HISTCONTROL=ignoreboth
shopt -s histappend

# Editor
export EDITOR=vim

# Prompt with color
PS1='\[\033[01;32m\]\u@clud-dev\[\033[00m\]:\[\033[01;34m\]\w\[\033[00m\]\$ '

# Auto-cd to project directory if it exists
if [ -d "/home/coder/project" ]; then
    cd /home/coder/project
fi
EOF

# Expose code-server port
EXPOSE 8080

# Set working directory
WORKDIR /home/${USERNAME}/project

# Set entrypoint and default command
ENTRYPOINT ["/entrypoint.sh"]