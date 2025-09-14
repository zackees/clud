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

# Configure code-server
mkdir -p /home/coder/.config/code-server
cat > /home/coder/.config/code-server/config.yaml << 'YAML_EOF'
bind-addr: 0.0.0.0:8080
auth: none
cert: false
YAML_EOF

# Fix permissions (ignore failures for mounted directories)
chown -R coder:coder /home/coder/.config 2>/dev/null || true

# Start code-server as the coder user
log "Starting code-server on port 8080..."
exec sudo -u coder bash -c "cd /workspace && code-server --bind-addr 0.0.0.0:8080 --auth none --disable-telemetry /workspace"