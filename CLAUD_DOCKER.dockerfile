## This is the original docker file in run-claude
##



# vim: set ft=dockerfile:
# ============================================================================
# Stage 1: Base tools and development environment
# ============================================================================
FROM ubuntu:25.04 AS base-tools

# Install system dependencies including zsh and tools
RUN apt-get update && apt-get install -y \
    build-essential \
    ca-certificates \
    curl \
    wget \
    git \
    python3 \
    unzip \
    python3-pip \
    sudo \
    fzf \
    zsh \
    gh \
    vim \
    neovim \
    htop \
    jq \
    tree \
    ripgrep \
    fd-find \
    gpg \
    git-delta

# Clean up apt cache
RUN rm -rf /var/lib/apt/lists/*

# Install Go
RUN ARCH=$(dpkg --print-architecture) && \
    if [ "$ARCH" = "amd64" ]; then GOARCH="amd64"; else GOARCH="arm64"; fi && \
    wget -O go.tar.gz "https://go.dev/dl/go1.21.5.linux-${GOARCH}.tar.gz" \
    && tar -C /usr/local -xzf go.tar.gz \
    && rm go.tar.gz

ENV PATH=/usr/local/go/bin:$PATH
ENV CGO_ENABLED=0

# Create user
ARG USERNAME=claude-user
RUN useradd -m -s /bin/zsh ${USERNAME} \
    && echo ${USERNAME} ALL=\(root\) NOPASSWD:ALL > /etc/sudoers.d/${USERNAME} \
    && chmod 0440 /etc/sudoers.d/${USERNAME}

# Build and install Unsplash MCP server
WORKDIR /tmp
RUN git config --global url."https://github.com/".insteadOf git@github.com: \
    && git clone https://github.com/douglarek/unsplash-mcp-server.git \
    && cd unsplash-mcp-server \
    && go build -o /usr/local/bin/unsplash-mcp-server ./cmd/server \
    && git config --global --unset url."https://github.com/".insteadOf

# ============================================================================
# Stage 2: User environment setup (zsh, fnm, node)
# ============================================================================
FROM base-tools AS user-env

# Switch to user and setup zsh with oh-my-zsh
USER $USERNAME
WORKDIR /home/$USERNAME

RUN sh -c "$(curl -fsSL https://raw.githubusercontent.com/ohmyzsh/ohmyzsh/master/tools/install.sh)" "" --unattended \
    && git clone https://github.com/zsh-users/zsh-autosuggestions ${ZSH_CUSTOM:-~/.oh-my-zsh/custom}/plugins/zsh-autosuggestions \
    && git clone https://github.com/zsh-users/zsh-syntax-highlighting.git ${ZSH_CUSTOM:-~/.oh-my-zsh/custom}/plugins/zsh-syntax-highlighting

# Setup fnm for user
RUN curl -o- https://fnm.vercel.app/install | bash
ENV PATH="/home/$USERNAME/.local/share/fnm:$PATH"
SHELL ["/bin/bash", "-c"]
RUN eval "$(fnm env)" && fnm install 22 && fnm default 22 && fnm use 22

# Install LazyVim
RUN git clone https://github.com/LazyVim/starter ~/.config/nvim \
    && rm -rf ~/.config/nvim/.git
RUN nvim --headless "+Lazy! sync" +qa

# ============================================================================
# Stage 3: Claude and MCP servers
# ============================================================================
FROM user-env AS claude-mcp

# Install Claude CLI
RUN eval "$(fnm env)" && curl -fsSL https://claude.ai/install.sh | bash
ENV PATH=/home/$USERNAME/.local/bin:$PATH

# Install Playwright MCP via npm
RUN eval "$(fnm env)" && npm install -g @playwright/mcp@latest

# Setup MCP servers using claude mcp add
RUN eval "$(fnm env)" && claude mcp add unsplash \
    --scope user \
    /usr/local/bin/unsplash-mcp-server

RUN eval "$(fnm env)" && claude mcp add context7 \
    --scope user \
    --transport http \
    https://mcp.context7.com/mcp

RUN eval "$(fnm env)" && claude mcp add playwright \
    --scope user \
    npx @playwright/mcp@latest

# ============================================================================
# Stage 4: Final runtime image
# ============================================================================
FROM claude-mcp AS final

# Create entrypoint script that handles workspace directory change (as root)
USER root
RUN cat > /entrypoint.sh << 'EOF'
#!/bin/sh
# Merge Claude config from host file if available
if [ -f "$HOME/.claude.host.json" ]; then
    CONFIG_KEYS="oauthAccount hasSeenTasksHint userID hasCompletedOnboarding lastOnboardingVersion subscriptionNoticeCount hasAvailableSubscription s1mAccessCache"
    # Build jq expression for extraction
    JQ_EXPR=""
    for key in $CONFIG_KEYS; do
        if [ -n "$JQ_EXPR" ]; then
            JQ_EXPR="$JQ_EXPR, ";
        fi
        JQ_EXPR="$JQ_EXPR\"$key\": .$key";
    done
    # Extract config data and add bypass permissions
    HOST_CONFIG=$(jq -c "{$JQ_EXPR, \"bypassPermissionsModeAccepted\": true}" "$HOME/.claude.host.json" 2>/dev/null || echo "")
    if [ -n "$HOST_CONFIG" ] && [ "$HOST_CONFIG" != "null" ] && [ "$HOST_CONFIG" != "{}" ]; then
        if [ -f "$HOME/.claude.json" ]; then
            # Merge with existing container file
            jq ". * $HOST_CONFIG" "$HOME/.claude.json" > "$HOME/.claude.json.tmp" && mv "$HOME/.claude.json.tmp" "$HOME/.claude.json"
        else
            # Create new container file with host config
            echo "$HOST_CONFIG" | jq . > "$HOME/.claude.json"
        fi
        if [ "$RUN_CLAUDE_VERBOSE" = "1" ]; then echo "Claude config merged from host file"; fi
    else
        if [ "$RUN_CLADE_VERBOSE" = "1" ]; then echo "No valid config found in host file"; fi
    fi
else
    if [ "$RUN_CLADE_VERBOSE" = "1" ]; then echo "No host Claude config file mounted"; fi
fi

# Link GPG agent socket if forwarded
if [ -S "/gpg-agent-extra" ]; then
    # Detect expected socket location dynamically
    EXPECTED_SOCKET=$(gpgconf --list-dirs agent-socket 2>/dev/null)
    if [ -n "$EXPECTED_SOCKET" ]; then
        # Create directory structure for expected socket location
        mkdir -p "$(dirname "$EXPECTED_SOCKET")"
        chmod 700 "$(dirname "$EXPECTED_SOCKET")"
        # Link forwarded socket to expected location
        ln -sf /gpg-agent-extra "$EXPECTED_SOCKET"
        if [ "$RUN_CLADE_VERBOSE" = "1" ]; then echo "GPG agent socket linked at $EXPECTED_SOCKET"; fi
    else
        # Fallback to traditional ~/.gnupg location
        mkdir -p ~/.gnupg; chmod 700 ~/.gnupg
        ln -sf /gpg-agent-extra ~/.gnupg/S.gpg-agent
        if [ "$RUN_CLADE_VERBOSE" = "1" ]; then echo "GPG agent socket linked at ~/.gnupg/S.gpg-agent (fallback)"; fi
    fi
fi

# Change to workspace directory if provided
if [ -n "$WORKSPACE_PATH" ] && [ -d "$WORKSPACE_PATH" ]; then
    cd "$WORKSPACE_PATH"
fi

exec "$@"
EOF

RUN chmod +x /entrypoint.sh

# Create claude-exec wrapper script for proper environment setup in docker exec
RUN cat > /usr/local/bin/claude-exec << 'EOF'
#!/bin/zsh
# Change to workspace directory if available, fallback to home
if [[ -n "$WORKSPACE_PATH" && -d "$WORKSPACE_PATH" ]]; then cd "$WORKSPACE_PATH"; else cd ~; fi
# Execute the requested command or start interactive zsh
if [[ $# -gt 0 ]]; then
    # Source zsh environment files for command execution
    [[ -f ~/.zshenv ]] && source ~/.zshenv
    [[ -f ~/.zshrc ]] && source ~/.zshrc
    exec "$@"
else
    # Let zsh handle its own sourcing for interactive shells
    exec /bin/zsh
fi
EOF

RUN chmod +x /usr/local/bin/claude-exec

# Set working directory for user sessions
USER $USERNAME
WORKDIR /home/$USERNAME

# Configure zsh with theme, plugins, and aliases
RUN cat > ~/.zshrc << 'EOF'
export ZSH="$HOME/.oh-my-zsh"
ZSH_THEME="robbyrussell"
plugins=(git zsh-autosuggestions zsh-syntax-highlighting)
source $ZSH/oh-my-zsh.sh

# Colorful prompt prefix
export PS1="%F{red}[%F{yellow}r%F{green}u%F{cyan}n%F{blue}-%F{magenta}c%F{red}l%F{yellow}a%F{green}u%F{cyan}d%F{blue}e%F{magenta}]%f $PS1"

# History configuration
HISTFILE=~/.zsh_history
HISTSIZE=50000
SAVEHIST=50000

# Node version manager eval
eval "$(fnm env --use-on-cd --shell zsh)"

# Claude aliases - conditional based on dangerous mode
if [ "$CLAUDE_DANGEROUS_MODE" = "1" ] || [ "$ANTHROPIC_DANGEROUS_MODE" = "1" ]; then
    alias claude="claude --dangerously-skip-permissions"
fi
alias claude-safe="command claude"

# General aliases
alias ll="ls -la"
alias vim="nvim"
alias vi="nvim"

# Git SSH configuration
export GIT_SSH_COMMAND="ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR"
EOF

ENTRYPOINT ["/entrypoint.sh"]
CMD ["/bin/zsh"]
