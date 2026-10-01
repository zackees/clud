"""Only `safe-mktemp` may write creation-ledger rows (#1667, DD-136).

Scans `crates/clud-bin/src` in the checkout (present in every lane) for every
ledger insert entry point and fails when a file outside its allowlist names
one. The agent-reachable writer is `safe_mktemp.rs` alone; the other entries
are the definitions, the daemon plumbing that carries the op, and tests that
drive the registry directly.
"""

from __future__ import annotations

import re
from pathlib import Path

SRC = Path(__file__).resolve().parents[1] / "crates" / "clud-bin" / "src"

ALLOWED: dict[str, frozenset[str]] = {
    # The client call: its definition, re-export, and the one caller.
    "gc_client_insert_created": frozenset({"daemon/client.rs", "daemon/mod.rs", "safe_mktemp.rs"}),
    # The wire op: defined, sent by the client fn, handled by the daemon.
    "InsertCreated": frozenset(
        {"daemon/types.rs", "daemon/client.rs", "daemon/gc_service.rs", "daemon/server.rs"}
    ),
    # The registry write: defined, called by the daemon worker, and tests.
    "insert_created(": frozenset(
        {"gc/registry.rs", "daemon/gc_service.rs", "gc_tests.rs", "safe_mktemp_tests.rs"}
    ),
}


def violations(src: Path) -> list[str]:
    """`<needle> in <file>` for every use outside the allowlist."""
    out = []
    for path in sorted(src.rglob("*.rs")):
        rel = path.relative_to(src).as_posix()
        text = path.read_text(encoding="utf-8", errors="replace")
        for needle, allowed in ALLOWED.items():
            if re.search(rf"\b{re.escape(needle)}", text) and rel not in allowed:
                out.append(f"{needle} in {rel}")
    return out


def test_only_safe_mktemp_writes_ledger_rows() -> None:
    assert SRC.is_dir(), SRC
    assert (SRC / "safe_mktemp.rs").is_file()
    assert violations(SRC) == []
    # Every allowlisted needle is really present, so the scan is live.
    text = (SRC / "safe_mktemp.rs").read_text(encoding="utf-8")
    assert "gc_client_insert_created(" in text


def test_scanner_flags_a_second_caller(tmp_path: Path) -> None:
    (tmp_path / "daemon").mkdir()
    (tmp_path / "daemon" / "client.rs").write_text("pub fn gc_client_insert_created() {}\n")
    (tmp_path / "safe_mktemp.rs").write_text("gc_client_insert_created();\n")
    assert violations(tmp_path) == []
    (tmp_path / "rm_tool.rs").write_text("crate::daemon::gc_client_insert_created();\n")
    (tmp_path / "hook.rs").write_text("GcOp::InsertCreated { entry }\n")
    (tmp_path / "x.rs").write_text("registry.insert_created(&row)\n")
    assert violations(tmp_path) == [
        "InsertCreated in hook.rs",
        "gc_client_insert_created in rm_tool.rs",
        "insert_created( in x.rs",
    ]
