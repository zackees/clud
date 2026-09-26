#!/usr/bin/env bash
set -euo pipefail

output="${1:-clud-installer.exe}"
env -u BASH_ENV uvx --from 'clang-tool-chain==1.5.8' clang-tool-chain-cosmocpp \
  -std=c++17 -Os -mtiny -s -Wall -Wextra -Werror \
  installer/clud_installer.cpp -o "$output"
python - "$output" <<'PY'
from pathlib import Path
import sys

artifact = Path(sys.argv[1])
assert artifact.read_bytes().startswith(b"MZqFpD"), "not a Cosmopolitan APE/PE"
assert artifact.stat().st_size < 2 * 1024 * 1024, "installer exceeds 2 MiB"
print(f"APE size: {artifact.stat().st_size} bytes")
PY
