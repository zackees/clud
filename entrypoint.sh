#!/bin/bash
set -e

# Simple entrypoint that delegates to Python script for complex logic

# Run initial sync and setup
python3 /usr/local/bin/container-sync init

# Start code-server
exec sudo -u coder bash -c "cd /workspace && code-server --bind-addr 0.0.0.0:8080 --auth none --disable-telemetry /workspace"