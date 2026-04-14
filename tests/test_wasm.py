"""End-to-end unit test for the embedded wasm runtime."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path


def _clud_binary() -> str:
    """Find the clud binary in the venv."""
    venv = Path(sys.executable).parent
    if sys.platform == "win32":
        candidate = venv / "clud.exe"
    else:
        candidate = venv / "clud"
    if candidate.is_file():
        return str(candidate)
    return "clud"


CLUD = _clud_binary()


def _compile_cpp_to_wasm(source: Path, output: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            sys.executable,
            "-m",
            "ziglang",
            "c++",
            "-target",
            "wasm32-freestanding",
            "-O2",
            "-nostdlib",
            "-fno-exceptions",
            "-fno-rtti",
            "-Wl,--no-entry",
            "-Wl,--export=run",
            "-Wl,--export-memory",
            "-Wl,--initial-memory=2097152",
            "-o",
            str(output),
            str(source),
        ],
        capture_output=True,
        text=True,
        timeout=120,
    )


def test_wasm_cpp_hello_world(tmp_path: Path) -> None:
    source = tmp_path / "hello.cpp"
    output = tmp_path / "hello.wasm"
    source.write_text(
        """
extern "C" __attribute__((import_module("host"), import_name("log")))
void host_log(const char* ptr, int len);

extern "C" __attribute__((export_name("run")))
int run() {
    static const char msg[] = "hello from wasm";
    host_log(msg, 15);
    return 0;
}
""".strip(),
        encoding="utf-8",
    )

    compile_result = _compile_cpp_to_wasm(source, output)
    assert compile_result.returncode == 0, compile_result.stderr or compile_result.stdout

    result = subprocess.run(
        [CLUD, "wasm", str(output)],
        capture_output=True,
        text=True,
        timeout=30,
    )

    assert result.returncode == 0, result.stderr
    assert "hello from wasm" in result.stdout
