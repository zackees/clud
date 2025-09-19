#!/bin/bash
set -e

# Check if uv is installed
if ! command -v uv &> /dev/null; then
    echo "Error: uv is not installed. Please install uv first." >&2
    echo "Visit https://docs.astral.sh/uv/getting-started/installation/ for installation instructions." >&2
    exit 1
fi

rm -rf build dist
uv pip install wheel twine
uv build --wheel
uv run twine upload dist/* --verbose
# echo Pushing git tags…
