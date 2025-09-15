#!/bin/bash
set -e

# Simple entrypoint that delegates to Python script for complex logic

# Run initial sync and setup
python3 /usr/local/bin/container-sync init

# Check if a custom command was passed
if [ "$1" = "--cmd" ] && [ -n "$2" ]; then
    # Execute the custom command in /workspace as coder user
    echo "Executing custom command: $2"
    exec sudo -u coder bash -c "cd /workspace && $2"
else
    # Start code-server
    exec sudo -u coder bash -c "cd /workspace && code-server --bind-addr=0.0.0.0:8080 --auth=none --disable-telemetry /workspace"
fi