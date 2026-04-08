"""Permissive JSON loading helpers for hand-edited config files."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any


def load_json_file_permissive(path: Path) -> Any:
    """Load JSON with support for comments, BOMs, smart quotes, and trailing commas.

    This is intended for user-edited config files. It intentionally does not try
    to recover from arbitrary broken quoting inside JSON strings.
    """
    text = path.read_text(encoding="utf-8-sig")
    normalized = _normalize_quotes(text)
    stripped = _strip_json_comments(normalized)
    cleaned = _remove_trailing_commas(stripped)
    return json.loads(cleaned)


def _normalize_quotes(text: str) -> str:
    return text.replace("\u201c", '"').replace("\u201d", '"').replace("\u2018", "'").replace("\u2019", "'")


def _strip_json_comments(text: str) -> str:
    result: list[str] = []
    in_string = False
    string_quote = ""
    escaped = False
    i = 0
    length = len(text)

    while i < length:
        ch = text[i]
        nxt = text[i + 1] if i + 1 < length else ""

        if in_string:
            result.append(ch)
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == string_quote:
                in_string = False
            i += 1
            continue

        if ch in {'"', "'"}:
            in_string = True
            string_quote = ch
            result.append(ch)
            i += 1
            continue

        if ch == "/" and nxt == "/":
            i += 2
            while i < length and text[i] not in "\r\n":
                i += 1
            continue

        if ch == "/" and nxt == "*":
            i += 2
            while i + 1 < length and not (text[i] == "*" and text[i + 1] == "/"):
                i += 1
            i += 2
            continue

        result.append(ch)
        i += 1

    return "".join(result)


def _remove_trailing_commas(text: str) -> str:
    result: list[str] = []
    in_string = False
    string_quote = ""
    escaped = False
    i = 0
    length = len(text)

    while i < length:
        ch = text[i]

        if in_string:
            result.append(ch)
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == string_quote:
                in_string = False
            i += 1
            continue

        if ch in {'"', "'"}:
            in_string = True
            string_quote = ch
            result.append(ch)
            i += 1
            continue

        if ch == ",":
            j = i + 1
            while j < length and text[j].isspace():
                j += 1
            if j < length and text[j] in "]}":
                i += 1
                continue

        result.append(ch)
        i += 1

    return "".join(result)


__all__ = ["load_json_file_permissive"]
