"""Refresh the ``model_contexts`` section of server-settings.json from
OpenRouter's public model datasheet (#1258).

Scheduled by ``.github/workflows/refresh-model-contexts.yml``; run by hand
with ``python -m ci.refresh_model_contexts``. The script rewrites only its
own section and exits non-zero when the datasheet cannot be fetched or
parsed, so a scheduled run goes red instead of silently publishing an empty
or stale map.

Python here is stdlib-only (``urllib``): this repository bans ``subprocess``.
"""

from __future__ import annotations

import json
import sys
import urllib.request
from pathlib import Path
from typing import Any

DATASHEET_URL = "https://openrouter.ai/api/v1/models"
ASSET_PATH = (
    Path(__file__).resolve().parent.parent / "crates/clud-bin/assets/server-settings.json"
)
SECTION_KEY = "model_contexts"

# Bounds and charset mirror `ModelContexts::validate` in
# server_settings/sections.rs; change both together. The acceptance-relevant
# invariant is that everything this producer publishes also passes the Rust
# guard tests, so a mismatch turns CI red rather than shipping a section
# that silently falls back to the built-in copy.
MIN_CONTEXT_TOKENS = 1_000
MAX_CONTEXT_TOKENS = 10_000_000
MAX_MODEL_ID_LEN = 128
MODEL_ID_EXTRA_BYTES = frozenset("._-/:~")


class RefreshError(RuntimeError):
    """A datasheet row or response that must not be published."""


def fetch_rows(timeout: float = 30.0) -> list[dict[str, Any]]:
    with urllib.request.urlopen(DATASHEET_URL, timeout=timeout) as response:
        document = json.load(response)
    if not isinstance(document, dict):
        raise RefreshError("datasheet root is not an object")
    rows = document.get("data")
    if not isinstance(rows, list) or not rows:
        raise RefreshError("datasheet has no `data` rows")
    return rows


def _validate_model_id(model_id: Any, index: int) -> str:
    if not isinstance(model_id, str) or not model_id:
        raise RefreshError(f"row {index} has no non-empty string `id`")
    if len(model_id) > MAX_MODEL_ID_LEN:
        raise RefreshError(f"row {index}: id longer than {MAX_MODEL_ID_LEN} bytes: {model_id!r}")
    if not all(
        char.isascii() and (char.isalnum() or char in MODEL_ID_EXTRA_BYTES) for char in model_id
    ):
        raise RefreshError(f"{model_id}: id contains characters outside the wire-ID charset")
    return model_id


def normalize(rows: list[Any]) -> dict[str, int]:
    """``{wire id: context_length}`` sorted by wire id (deterministic diffs).

    A malformed row fails the whole run: skipping it would silently shrink
    the published map, which is exactly the failure this producer exists to
    prevent.
    """
    windows: dict[str, int] = {}
    for index, row in enumerate(rows):
        if not isinstance(row, dict):
            raise RefreshError(f"row {index} is not an object")
        model_id = _validate_model_id(row.get("id"), index)
        context = row.get("context_length")
        if not isinstance(context, int) or isinstance(context, bool):
            raise RefreshError(
                f"{model_id}: `context_length` is missing or not an integer: {context!r}"
            )
        if not MIN_CONTEXT_TOKENS <= context <= MAX_CONTEXT_TOKENS:
            raise RefreshError(
                f"{model_id}: `context_length` {context} outside "
                f"{MIN_CONTEXT_TOKENS}..={MAX_CONTEXT_TOKENS}"
            )
        if model_id in windows:
            raise RefreshError(f"{model_id}: duplicate id in the datasheet")
        windows[model_id] = context
    if not windows:
        raise RefreshError("datasheet produced no context windows")
    return dict(sorted(windows.items()))


def rewrite(document_text: str, windows: dict[str, int]) -> str:
    """Return ``document_text`` with only ``sections.model_contexts`` replaced.

    JSON round-tripping preserves the order of every other key, so untouched
    sections come back byte-for-byte identical (pinned by a test).
    """
    document = json.loads(document_text)
    if not isinstance(document, dict):
        raise RefreshError("server-settings root is not an object")
    sections = document.get("sections")
    if not isinstance(sections, dict):
        raise RefreshError("server-settings has no `sections` object")
    sections[SECTION_KEY] = dict(sorted(windows.items()))
    return json.dumps(document, indent=2) + "\n"


def main(argv: list[str] | None = None) -> int:
    del argv  # no flags today; kept so tests can call main() explicitly
    try:
        windows = normalize(fetch_rows())
        original = ASSET_PATH.read_text(encoding="utf-8")
        updated = rewrite(original, windows)
    except RefreshError as error:
        print(f"refresh_model_contexts: {error}", file=sys.stderr)
        return 1
    except (OSError, ValueError) as error:
        print(f"refresh_model_contexts: fetch or parse failed: {error}", file=sys.stderr)
        return 1
    if updated == original:
        print(f"model_contexts already fresh ({len(windows)} models)")
        return 0
    ASSET_PATH.write_text(updated, encoding="utf-8")
    print(f"model_contexts refreshed ({len(windows)} models)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
