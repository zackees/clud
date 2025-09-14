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
if [ -f "/home/coder/project/install" ]; then
    log "Found install script, executing..."
    cd /home/coder/project
    if [ -x "./install" ]; then
        ./install
    else
        bash ./install
    fi
    log "Install script completed"
elif [ -f "/home/coder/project/bash" ] && [ -x "/home/coder/project/bash" ]; then
    # Check if 'bash install' is a valid command
    cd /home/coder/project
    if ./bash install --help >/dev/null 2>&1 || ./bash install >/dev/null 2>&1; then
        log "Running 'bash install' command..."
        ./bash install
        log "'bash install' completed"
    fi
fi

# Configure code-server
mkdir -p /home/coder/.config/code-server
cat > /home/coder/.config/code-server/config.yaml << YAML_EOF
bind-addr: 0.0.0.0:8080
auth: none
cert: false
YAML_EOF

# Fix permissions
chown -R coder:coder /home/coder/.config

# Start code-server as the coder user
log "Starting code-server on port 8080..."
exec sudo -u coder bash -c "cd /home/coder/project && code-server --bind-addr 0.0.0.0:8080 --auth none --disable-telemetry /home/coder/project"