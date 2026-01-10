#!/bin/bash
set -e

# Check if uv is installed
if ! command -v uv &> /dev/null; then
    echo "Error: uv is not installed. Please install uv first." >&2
    echo "Visit https://docs.astral.sh/uv/getting-started/installation/ for installation instructions." >&2
    exit 1
fi

# Kill any running clud.exe processes to release file locks on Windows
CLUD_PIDS=$(tasklist 2>/dev/null | grep -i "clud.exe" | awk '{print $2}' || true)
if [ -n "$CLUD_PIDS" ]; then
    echo "Killing clud.exe processes to release file locks..."
    taskkill //F //IM clud.exe 2>/dev/null || true
    sleep 1
fi

rm -rf build dist
uv pip install wheel twine
uv build --wheel
uv run twine upload dist/* --verbose

# Restart cron daemon if it was killed
if [ -n "$CLUD_PIDS" ]; then
    echo "Restarting cron daemon..."
    uv run clud --cron start
fi
# echo Pushing git tags…
