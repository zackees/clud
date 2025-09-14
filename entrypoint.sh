#!/bin/bash
set -e

# Function to log messages
log() {
    if [ "${VERBOSE}" = "1" ]; then
        echo "[entrypoint] $1"
    fi
}

# Set up Anthropic API key if provided
if [ -n "${ANTHROPIC_API_KEY}" ]; then
    export ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY}"
    log "Anthropic API key configured"
fi

# Run install script if present in project directory
# Set up Python environment in container-specific location
export CONTAINER_VENV_PATH="/home/coder/.container-venv"

if [ -f "/home/coder/project/install" ]; then
    log "Found install script, executing..."
    cd /home/coder/project
    # Use container-specific venv path if Windows venv exists
    if [ -d ".venv" ] && [ ! -f ".venv/bin/python" ]; then
        log "Windows venv detected, using container-specific venv location..."
        # Create venv in container-specific location
        uv venv --python 3.13 "$CONTAINER_VENV_PATH"
        # Install dependencies using uv
        source "$CONTAINER_VENV_PATH/bin/activate"
        uv pip install -e ".[dev]"
    else
        # Normal install
        if [ -x "./install" ]; then
            ./install
        else
            bash ./install
        fi
    fi
    log "Install script completed"
elif [ -f "/home/coder/project/bash" ] && [ -x "/home/coder/project/bash" ]; then
    # Check if 'bash install' is a valid command
    cd /home/coder/project
    # Use container-specific venv path if Windows venv exists
    if [ -d ".venv" ] && [ ! -f ".venv/bin/python" ]; then
        log "Windows venv detected, using container-specific venv location..."
        # Create venv in container-specific location
        uv venv --python 3.13 "$CONTAINER_VENV_PATH"
        # Install dependencies using uv
        source "$CONTAINER_VENV_PATH/bin/activate"
        uv pip install -e ".[dev]"
    else
        if ./bash install --help >/dev/null 2>&1 || ./bash install >/dev/null 2>&1; then
            log "Running 'bash install' command..."
            ./bash install
            log "'bash install' completed"
        fi
    fi
fi

# Add container venv to PATH if it exists
if [ -d "$CONTAINER_VENV_PATH" ]; then
    export PATH="$CONTAINER_VENV_PATH/bin:$PATH"
    log "Added container venv to PATH"
fi

# Configure code-server
mkdir -p /home/coder/.config/code-server
cat > /home/coder/.config/code-server/config.yaml << YAML_EOF
bind-addr: 0.0.0.0:8080
auth: none
cert: false
YAML_EOF

# Fix permissions (ignore failures for mounted directories)
chown -R coder:coder /home/coder/.config 2>/dev/null || true

# Start code-server as the coder user
log "Starting code-server on port 8080..."
exec sudo -u coder bash -c "cd /home/coder/project && code-server --bind-addr 0.0.0.0:8080 --auth none --disable-telemetry /home/coder/project"