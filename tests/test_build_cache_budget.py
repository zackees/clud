"""The Linux x64 build cache must stay saveable.

setup-soldr skips the build-cache save when the store exceeds its payload cap
(6 GiB before compression) and the job stays green. zccache's default budget is
5% of the disk (40-200 GiB), so on a CI runner the store grows until it crosses
the cap and the cache freezes (clud, 2026-09-28, 6.19 GB). A budget below the
cap makes the daemon evict least-recently-used entries instead.
"""

from __future__ import annotations

import re
from pathlib import Path

WORKFLOWS = Path(__file__).resolve().parent.parent / ".github" / "workflows"
BUILD_TARGET = WORKFLOWS / "_build-target.yml"
#: setup-soldr's default `cache-payload-max-bytes` (nothing in this repo overrides it).
PAYLOAD_CAP = 6 * 1024**3


def _budget_line() -> str:
    match = re.search(
        r"^      ZCCACHE_CACHE_SIZE_BYTES: (.+)$", BUILD_TARGET.read_text(encoding="utf-8"), re.M
    )
    assert match, "_build-target.yml sets no ZCCACHE_CACHE_SIZE_BYTES"
    return match.group(1)


def test_linux_x64_compile_jobs_get_a_store_budget_below_the_payload_cap() -> None:
    line = _budget_line()
    assert "inputs.compile" in line
    assert "inputs.target == 'x86_64-unknown-linux-gnu'" in line
    budget = int(re.findall(r"'(\d+)'", line)[0])
    # Eviction stops at 70-80% of the budget, so the saved store sits under the cap
    # with room for one build's worth of growth.
    assert budget < PAYLOAD_CAP
    assert budget * 1.5 < PAYLOAD_CAP


def test_the_budget_is_empty_everywhere_else() -> None:
    """An empty value is ignored by soldr, so other targets and the clippy job are unchanged."""
    assert _budget_line().rstrip().endswith("|| '' }}")


def test_the_store_size_is_reported_before_the_post_step_decides() -> None:
    text = BUILD_TARGET.read_text(encoding="utf-8")
    assert "- name: Report build-cache store size" in text
    after = text.split("- name: Report build-cache store size\n", 1)[1]
    step = after.split("\n      - name:", 1)[0]
    assert "always()" in step
    assert "ZCCACHE_CACHE_DIR" in step
    assert "du -sb" in step
