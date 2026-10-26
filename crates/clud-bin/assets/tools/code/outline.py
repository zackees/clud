#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "tree-sitter==0.26.0",
#   "tree-sitter-rust==0.24.2",
#   "tree-sitter-python==0.25.0",
#   "tree-sitter-javascript==0.25.0",
#   "tree-sitter-typescript==0.23.2",
#   "tree-sitter-c==0.24.2",
#   "tree-sitter-cpp==0.23.4",
# ]
# ///
# managed-by: clud
"""outline.py — a file's definitions with their line ranges (#1339 item 4).

Usage:
  outline.py [--json] <file>...

/grind agents page through files with offset reads and `sed -n` to find one
function, or read a 70 KB document whole to find one section. This prints
each file's functions, types, `impl` blocks, classes, tests and Markdown
headings with `start-end` line ranges, nested by containment, so a follow-up
read can target exactly the range it needs:

  src/main.rs
    3-9      struct Config
    11-40    impl Config
    12-25      fn load
    43-60    test parses_args

Languages: Rust, Python, JavaScript, TypeScript/TSX, C, C++ (tree-sitter),
and Markdown (ATX headings outside fenced code). Other files are reported as
unsupported.

Run through clud's `tool run` subcommand:
  "$CLUD_EXE" tool run code/outline.py src/main.rs docs/ARCHITECTURE.md

Exit codes:
  0  every file was outlined
  1  usage error
  2  a file was missing, unreadable or unsupported (the others still print)
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from tree_sitter import Language, Node


@dataclass
class Item:
    kind: str
    name: str
    start: int
    end: int
    children: list[Item] = field(default_factory=list)


# Node type -> outline kind, per language.
KINDS: dict[str, dict[str, str]] = {
    "rust": {
        "function_item": "fn",
        "function_signature_item": "fn",
        "struct_item": "struct",
        "enum_item": "enum",
        "union_item": "union",
        "trait_item": "trait",
        "impl_item": "impl",
        "mod_item": "mod",
        "macro_definition": "macro",
        "const_item": "const",
        "static_item": "static",
        "type_item": "type",
    },
    "python": {"function_definition": "def", "class_definition": "class"},
    "javascript": {
        "function_declaration": "function",
        "generator_function_declaration": "function",
        "class_declaration": "class",
        "method_definition": "method",
    },
    "c": {
        "function_definition": "fn",
        "struct_specifier": "struct",
        "enum_specifier": "enum",
        "union_specifier": "union",
    },
}
KINDS["typescript"] = {
    **KINDS["javascript"],
    "interface_declaration": "interface",
    "type_alias_declaration": "type",
    "enum_declaration": "enum",
    "abstract_class_declaration": "class",
    "module": "namespace",
    "internal_module": "namespace",
}
KINDS["cpp"] = {
    **KINDS["c"],
    "class_specifier": "class",
    "namespace_definition": "namespace",
}

SUFFIXES = {
    ".rs": "rust",
    ".py": "python",
    ".pyi": "python",
    ".js": "javascript",
    ".mjs": "javascript",
    ".cjs": "javascript",
    ".jsx": "javascript",
    ".ts": "typescript",
    ".mts": "typescript",
    ".cts": "typescript",
    ".tsx": "tsx",
    ".c": "c",
    ".h": "cpp",
    ".cc": "cpp",
    ".cpp": "cpp",
    ".cxx": "cpp",
    ".hpp": "cpp",
    ".hh": "cpp",
    ".md": "markdown",
    ".markdown": "markdown",
}


def language(name: str) -> Language:
    from tree_sitter import Language
    if name == "rust":
        import tree_sitter_rust as mod

        return Language(mod.language())
    if name == "python":
        import tree_sitter_python as mod

        return Language(mod.language())
    if name == "javascript":
        import tree_sitter_javascript as mod

        return Language(mod.language())
    if name in ("typescript", "tsx"):
        import tree_sitter_typescript as mod

        return Language(mod.language_tsx() if name == "tsx" else mod.language_typescript())
    if name == "c":
        import tree_sitter_c as mod

        return Language(mod.language())
    import tree_sitter_cpp as mod

    return Language(mod.language())


def text(node: Node | None) -> str:
    return node.text.decode("utf-8", errors="replace") if node and node.text else ""


def declarator_name(node: Node | None) -> str:
    """The identifier at the bottom of a C/C++ declarator chain."""
    while node is not None:
        if node.type in ("identifier", "field_identifier", "qualified_identifier", "destructor_name", "operator_name"):
            return text(node)
        node = node.child_by_field_name("declarator")
    return ""


def is_rust_test(node: Node) -> bool:
    prev = node.prev_named_sibling
    while prev is not None and prev.type == "attribute_item":
        if re.fullmatch(r"#\[(\w+::)*test\]", text(prev).replace(" ", "")):
            return True
        prev = prev.prev_named_sibling
    return False


def name_of(node: Node, lang: str) -> str:
    if node.type == "impl_item":
        kind = text(node.child_by_field_name("type"))
        trait = text(node.child_by_field_name("trait"))
        return f"{trait} for {kind}" if trait else kind
    if node.type == "function_definition" and lang in ("c", "cpp"):
        return declarator_name(node.child_by_field_name("declarator"))
    return text(node.child_by_field_name("name"))


def arrow_binding(node: Node) -> str | None:
    """`const f = () => …` / `= function …`: the bound name, else None."""
    if node.type not in ("lexical_declaration", "variable_declaration"):
        return None
    for decl in node.named_children:
        value = decl.child_by_field_name("value")
        if value is not None and value.type in ("arrow_function", "function_expression", "function"):
            return text(decl.child_by_field_name("name"))
    return None


def walk(node: Node, lang: str, kinds: dict[str, str]) -> list[Item]:
    items: list[Item] = []
    for child in node.named_children:
        kind = kinds.get(child.type)
        name = None
        if kind is not None:
            name = name_of(child, lang)
            # A struct/enum reference (`struct foo x;`) has no body: not a definition.
            if lang in ("c", "cpp") and child.type.endswith("_specifier") and child.child_by_field_name("body") is None:
                kind = None
            elif lang == "rust" and kind == "fn" and is_rust_test(child):
                kind = "test"
        elif lang in ("javascript", "typescript", "tsx"):
            name = arrow_binding(child)
            kind = "function" if name else None
        if kind is None:
            items.extend(walk(child, lang, kinds))
            continue
        # Python decorators belong to the definition's range.
        outer = child.parent if child.parent is not None and child.parent.type == "decorated_definition" else child
        item = Item(kind, name or "<anonymous>", outer.start_point[0] + 1, outer.end_point[0] + 1)
        item.children = walk(child, lang, kinds)
        items.append(item)
    return items


FENCE = re.compile(r"^\s{0,3}(`{3,}|~{3,})")
HEADING = re.compile(r"^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$")


def markdown(source: str) -> list[Item]:
    """ATX headings, each spanning to the line before the next heading of the same or higher level."""
    lines = source.splitlines()
    flat: list[tuple[int, int, str]] = []
    fence = ""
    for number, line in enumerate(lines, 1):
        if m := FENCE.match(line):
            mark = m.group(1)
            if not fence:
                fence = mark[0] * len(mark)
            elif mark.startswith(fence):
                fence = ""
            continue
        if not fence and (m := HEADING.match(line)):
            flat.append((len(m.group(1)), number, m.group(2)))
    root: list[Item] = []
    stack: list[tuple[int, Item]] = []
    for i, (level, start, title) in enumerate(flat):
        end = next((s - 1 for lv, s, _ in flat[i + 1 :] if lv <= level), len(lines))
        item = Item(f"h{level}", title, start, end)
        while stack and stack[-1][0] >= level:
            stack.pop()
        (stack[-1][1].children if stack else root).append(item)
        stack.append((level, item))
    return root


def outline(path: Path) -> list[Item]:
    lang = SUFFIXES.get(path.suffix.lower())
    if lang is None:
        raise ValueError(f"unsupported file type: {path.suffix or path.name}")
    source = path.read_bytes()
    if lang == "markdown":
        return markdown(source.decode("utf-8", errors="replace"))
    from tree_sitter import Parser

    tree = Parser(language(lang)).parse(source)
    kinds = KINDS["typescript" if lang == "tsx" else lang]
    return walk(tree.root_node, lang, kinds)


def render(items: list[Item], depth: int = 1) -> list[str]:
    out: list[str] = []
    for item in items:
        span = f"{item.start}-{item.end}"
        out.append(f"{'  ' * depth}{span:<9}{'  ' * (depth - 1)}{item.kind} {item.name}")
        out.extend(render(item.children, depth + 1))
    return out


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(prog="outline.py", description=__doc__.split("\n")[0])
    parser.add_argument("--json", action="store_true", help="print one JSON object per file")
    parser.add_argument("files", nargs="+", type=Path)
    args = parser.parse_args(argv)
    status = 0
    for path in args.files:
        try:
            items = outline(path)
        except (OSError, ValueError) as exc:
            print(f"{path}: {exc}", file=sys.stderr)
            status = 2
            continue
        if args.json:
            print(json.dumps({"path": str(path), "items": [asdict(i) for i in items]}))
        else:
            print(path)
            print("\n".join(render(items)))
    return status


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
