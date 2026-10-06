"""#1839: a forwarded `--` must not turn the watcher's flags into positionals.

`clud tool run github/pr_merge_watch.py 1835 -- --no-cancel` hands the tool
`["1835", "--", "--no-cancel"]`. argparse read `--no-cancel` as a second
positional and exited 64, which in a background wait silently burned a CI run.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "github" / "pr_merge_watch.py"


@pytest.fixture
def watcher():
    name = "clud_test_pr_merge_watch_1839"
    spec = importlib.util.spec_from_file_location(name, SCRIPT)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    try:
        yield module
    finally:
        sys.modules.pop(name, None)


@pytest.mark.parametrize(
    "argv",
    [
        ["1835", "--no-cancel"],
        ["--no-cancel", "1835"],
        ["1835", "--", "--no-cancel"],
        ["--", "--no-cancel", "1835"],
    ],
)
def test_every_spelling_parses_the_same(watcher, argv) -> None:
    ns = watcher.parse_args(argv)
    assert ns.pr == "1835"
    assert ns.no_cancel is True


def test_an_unknown_flag_is_still_a_usage_error(watcher) -> None:
    with pytest.raises(SystemExit) as exit_info:
        watcher.parse_args(["1835", "--", "--not-a-flag"])
    assert exit_info.value.code == watcher.EXIT_USAGE
