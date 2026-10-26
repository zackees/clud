"""Unit coverage for the bundled `code/outline.py` tool (#1339 item 4)."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

TOOL_DIR = Path(__file__).resolve().parents[1] / "crates" / "clud-bin" / "assets" / "tools" / "code"


def _load():
    if str(TOOL_DIR) not in sys.path:
        sys.path.insert(0, str(TOOL_DIR))
    import outline

    return outline


outline = _load()

RUST = """\
use std::fmt;

/// A config.
pub struct Config {
    name: String,
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn displays_name() {
        assert!(true);
    }
}
"""

PYTHON = """\
import os


@decorated
def top(x):
    return x


class Thing:
    def method(self):
        pass
"""

MARKDOWN = """\
# Title

intro

## One

```sh
# not a heading
```

### One.a

## Two
"""


def _flat(items, depth=0):
    for item in items:
        yield depth, item.kind, item.name, item.start, item.end
        yield from _flat(item.children, depth + 1)


def _outline(tmp_path: Path, name: str, source: str):
    path = tmp_path / name
    path.write_text(source, encoding="utf-8")
    return list(_flat(outline.outline(path)))


def test_markdown_headings_span_to_the_next_peer(tmp_path: Path) -> None:
    assert _outline(tmp_path, "doc.md", MARKDOWN) == [
        (0, "h1", "Title", 1, 13),
        (1, "h2", "One", 5, 12),
        (2, "h3", "One.a", 11, 12),
        (1, "h2", "Two", 13, 13),
    ]


def test_rust_items_nest_and_tests_are_marked(tmp_path: Path) -> None:
    pytest.importorskip("tree_sitter_rust")
    assert _outline(tmp_path, "lib.rs", RUST) == [
        (0, "struct", "Config", 4, 6),
        (0, "impl", "fmt::Display for Config", 8, 12),
        (1, "fn", "fmt", 9, 11),
        (0, "mod", "tests", 15, 20),
        (1, "test", "displays_name", 17, 19),
    ]


def test_python_ranges_include_decorators(tmp_path: Path) -> None:
    pytest.importorskip("tree_sitter_python")
    assert _outline(tmp_path, "mod.py", PYTHON) == [
        (0, "def", "top", 4, 6),
        (0, "class", "Thing", 9, 11),
        (1, "def", "method", 10, 11),
    ]


def test_unsupported_files_fail_without_stopping_the_rest(tmp_path: Path, capsys) -> None:
    good = tmp_path / "doc.md"
    good.write_text("# Only\n", encoding="utf-8")
    bad = tmp_path / "Cargo.toml"
    bad.write_text("[package]\n", encoding="utf-8")
    assert outline.main([str(bad), str(good)]) == 2
    captured = capsys.readouterr()
    assert "unsupported file type: .toml" in captured.err
    assert "h1 Only" in captured.out
