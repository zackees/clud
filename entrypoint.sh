#!/bin/bash
set -e

# Simple entrypoint that delegates to Python script for complex logic

echo "[ENTRYPOINT] Starting container entrypoint with args: $@"
echo "[ENTRYPOINT] Current working directory: $(pwd)"
echo "[ENTRYPOINT] Running as user: $(whoami)"

# Assert that required directories exist (guaranteed by Dockerfile)
echo "[ENTRYPOINT] Verifying container directory structure..."
for dir in /workspace /host; do
    if [ ! -d "$dir" ]; then
        echo "ERROR: Required directory $dir does not exist!" >&2
        echo "This indicates a problem with the Docker image build." >&2
        exit 1
    fi
    echo "✓ $dir exists"
done

# Run initial sync and setup
echo "[ENTRYPOINT] Running container-sync init..."
python3 /usr/local/bin/container-sync init
echo "[ENTRYPOINT] Container-sync init completed"

# Check if a custom command was passed
if [ "$1" = "--cmd" ] && [ -n "$2" ]; then
    # Execute the custom command in /workspace
    echo "[ENTRYPOINT] Executing custom command: $2"
    exec bash -l -c "cd /workspace && $2"
else
    # Start code-server
    echo "[ENTRYPOINT] Starting code-server..."
    echo "[ENTRYPOINT] About to exec: code-server --bind-addr=0.0.0.0:8080 --auth=none --disable-telemetry /workspace"
    exec bash -l -c "cd /workspace && code-server --bind-addr=0.0.0.0:8080 --auth=none --disable-telemetry /workspace"
fi