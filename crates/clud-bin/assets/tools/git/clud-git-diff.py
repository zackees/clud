#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "pywebview>=5.3",
# ]
# ///
# managed-by: clud
"""Native OS webview diff viewer with file picker + Beyond Compare-style
side-by-side dual-pane diff.

Layout:
    [ file list ] | [ before (old) ] | [ after (new) ]

Click a file in the left panel → its dual-pane diff renders to the
right with synchronized scrolling between the two columns.

Usage:
    uv run --no-project demo/diff_webview.py [LEFT [RIGHT]]

Default range: HEAD~10..HEAD
"""

from __future__ import annotations

import html
import json
import re
import subprocess
import sys
from dataclasses import dataclass, field

import webview


# ---------- diff parsing ----------


@dataclass
class Hunk:
    old_start: int
    new_start: int
    raw_lines: list[tuple[str, str]] = field(default_factory=list)
    # raw_lines: list of (kind, text) where kind ∈ {" ", "-", "+"}.


@dataclass
class FileDiff:
    path: str
    header_lines: list[str] = field(default_factory=list)
    hunks: list[Hunk] = field(default_factory=list)


def get_diff(rev_left: str, rev_right: str) -> str:
    result = subprocess.run(  # noqa: S603, S607
        ["git", "diff", "--no-color", f"{rev_left}..{rev_right}"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0 and not result.stdout:
        return f"git diff failed (exit {result.returncode}):\n{result.stderr}"
    return result.stdout


def parse_diff(diff_text: str) -> list[FileDiff]:
    files: list[FileDiff] = []
    current: FileDiff | None = None
    current_hunk: Hunk | None = None
    file_pattern = re.compile(r"^diff --git a/(.+?) b/(.+?)$")
    hunk_pattern = re.compile(r"^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@")

    for raw in diff_text.splitlines():
        if raw.startswith("diff --git "):
            if current is not None:
                if current_hunk is not None:
                    current.hunks.append(current_hunk)
                    current_hunk = None
                files.append(current)
            m = file_pattern.match(raw)
            path = m.group(2) if m else "?"
            current = FileDiff(path=path, header_lines=[raw])
            current_hunk = None
        elif current is None:
            # Preamble before the first `diff --git` — skip.
            continue
        elif raw.startswith("@@"):
            if current_hunk is not None:
                current.hunks.append(current_hunk)
            m = hunk_pattern.match(raw)
            old_start = int(m.group(1)) if m else 1
            new_start = int(m.group(2)) if m else 1
            current_hunk = Hunk(old_start=old_start, new_start=new_start)
        elif current_hunk is not None:
            if not raw:
                current_hunk.raw_lines.append((" ", ""))
            elif raw[0] in " +-":
                current_hunk.raw_lines.append((raw[0], raw[1:]))
            # "\ No newline at end of file" etc. just gets dropped.
        else:
            # File header lines (index, ---, +++).
            current.header_lines.append(raw)

    if current is not None:
        if current_hunk is not None:
            current.hunks.append(current_hunk)
        files.append(current)
    return files


def hunk_to_side_by_side(
    hunk: Hunk,
) -> tuple[list[dict], list[dict]]:
    """Convert a hunk's unified-diff lines into two parallel column
    arrays (left = old/before, right = new/after) with line numbers
    and per-row kind tags."""
    left: list[dict] = []
    right: list[dict] = []
    pending_l: list[str] = []
    pending_r: list[str] = []
    old_ln = hunk.old_start
    new_ln = hunk.new_start

    def flush() -> None:
        nonlocal old_ln, new_ln
        n = max(len(pending_l), len(pending_r))
        for i in range(n):
            if i < len(pending_l):
                left.append({"ln": old_ln, "kind": "del", "text": pending_l[i]})
                old_ln += 1
            else:
                left.append({"ln": None, "kind": "blank", "text": ""})
            if i < len(pending_r):
                right.append({"ln": new_ln, "kind": "add", "text": pending_r[i]})
                new_ln += 1
            else:
                right.append({"ln": None, "kind": "blank", "text": ""})
        pending_l.clear()
        pending_r.clear()

    for kind, text in hunk.raw_lines:
        if kind == " ":
            flush()
            left.append({"ln": old_ln, "kind": "ctx", "text": text})
            right.append({"ln": new_ln, "kind": "ctx", "text": text})
            old_ln += 1
            new_ln += 1
        elif kind == "-":
            pending_l.append(text)
        elif kind == "+":
            pending_r.append(text)
    flush()
    return left, right


def file_to_payload(file: FileDiff) -> dict:
    sections: list[dict] = []
    for hunk in file.hunks:
        left, right = hunk_to_side_by_side(hunk)
        header = f"@@ -{hunk.old_start} +{hunk.new_start} @@"
        sections.append({"header": header, "left": left, "right": right})
    return {"path": file.path, "sections": sections}


# ---------- rendering ----------


def render_html(rev_left: str, rev_right: str, files: list[FileDiff]) -> str:
    payloads = [file_to_payload(f) for f in files]
    data_json = json.dumps(payloads)
    nav_items = []
    for i, f in enumerate(files):
        escaped = html.escape(f.path)
        nav_items.append(
            f'<a class="nav-item" data-idx="{i}" href="#">{escaped}</a>'
        )
    nav = (
        "\n".join(nav_items)
        if nav_items
        else '<p class="empty">(no files changed)</p>'
    )
    title = f"git diff {html.escape(rev_left)}..{html.escape(rev_right)}"
    file_count = len(files)
    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>{title}</title>
<style>
* {{ box-sizing: border-box; }}
html, body {{ margin: 0; padding: 0; height: 100vh; overflow: hidden; }}
body {{ font-family: ui-monospace, SFMono-Regular, "Cascadia Mono",
       Menlo, Consolas, monospace;
       background: #1e1e1e; color: #d4d4d4;
       display: grid;
       grid-template-columns: 320px 1fr 1fr;
       grid-template-rows: auto 1fr; }}
header {{ grid-column: 1 / -1;
          padding: 0.7rem 1.25rem;
          background: #252526;
          border-bottom: 1px solid #3c3c3c;
          font-size: 0.85rem; color: #ccc; }}
header h1 {{ margin: 0; font-size: 0.92rem; font-weight: 500; color: #cccccc; }}
header .meta {{ font-size: 0.72rem; color: #888; margin-top: 0.15rem; }}
aside {{ grid-row: 2;
         background: #252526;
         border-right: 1px solid #3c3c3c;
         overflow-y: auto; }}
aside h2 {{ position: sticky; top: 0; background: #252526;
            margin: 0; padding: 0.7rem 1rem 0.5rem;
            font-size: 0.72rem; text-transform: uppercase;
            letter-spacing: 0.05em; color: #888;
            border-bottom: 1px solid #3c3c3c; font-weight: 600; }}
.nav-item {{ display: block; padding: 0.4rem 1rem;
             font-size: 0.8rem; color: #cccccc;
             text-decoration: none; border-left: 3px solid transparent;
             white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
             cursor: pointer; }}
.nav-item:hover {{ background: #2a2d2e; }}
.nav-item.active {{ background: #094771; border-left-color: #0e639c; color: #fff; }}
.pane {{ grid-row: 2; overflow: auto; }}
.pane.left  {{ border-right: 1px solid #3c3c3c; }}
.pane.right {{ }}
.pane .pane-title {{ position: sticky; top: 0;
                      padding: 0.55rem 1rem; font-size: 0.72rem;
                      letter-spacing: 0.05em; text-transform: uppercase;
                      background: #1f2226; color: #888;
                      border-bottom: 1px solid #2a2d2e; z-index: 5; }}
.hunk-header {{ padding: 0.4rem 1rem; color: #4ec9b0; background: #2a2d2e;
                font-size: 0.78rem; }}
.row {{ display: flex; font-size: 0.82rem; line-height: 1.45;
        white-space: pre; }}
.row .ln {{ flex: 0 0 4ch; padding: 0 0.5rem; text-align: right;
            color: #555; user-select: none; }}
.row .text {{ flex: 1 1 auto; padding-right: 0.5rem; min-width: 0;
              overflow: hidden; text-overflow: ellipsis; }}
.row.ctx   {{ color: #888; }}
.row.add   {{ background: rgba(101, 153, 63, 0.18); color: #b5cea8; }}
.row.del   {{ background: rgba(204, 78, 78, 0.18); color: #ce9178; }}
.row.blank {{ background: #1a1a1a; color: #444; }}
.empty {{ padding: 2rem 1.5rem; color: #888; }}
</style>
</head>
<body>
<header>
  <h1>{title}</h1>
  <div class="meta">{file_count} file{'s' if file_count != 1 else ''} changed —
                    select one on the left to see its dual-pane diff.
                    Close this window to return to the agent.</div>
</header>
<aside>
  <h2>Files</h2>
  {nav}
</aside>
<div class="pane left"><div class="pane-title">Before ({html.escape(rev_left)})</div>
<div id="leftBody"></div></div>
<div class="pane right"><div class="pane-title">After ({html.escape(rev_right)})</div>
<div id="rightBody"></div></div>
<script>
const PAYLOADS = {data_json};
const leftBody  = document.getElementById('leftBody');
const rightBody = document.getElementById('rightBody');
const items     = document.querySelectorAll('.nav-item');

function renderColumn(target, rows) {{
  const html = rows.map(r => {{
    const ln = r.ln === null ? '' : r.ln;
    const text = r.text
      .replace(/&/g, '&amp;').replace(/</g, '&lt;')
      .replace(/>/g, '&gt;') || '&nbsp;';
    return `<div class="row ${{r.kind}}"><span class="ln">${{ln}}</span><span class="text">${{text}}</span></div>`;
  }}).join('');
  return html;
}}

function showFile(idx) {{
  items.forEach(i => i.classList.toggle('active', Number(i.dataset.idx) === idx));
  const file = PAYLOADS[idx];
  if (!file) {{
    leftBody.innerHTML  = '<p class="empty">(nothing to show)</p>';
    rightBody.innerHTML = '<p class="empty">(nothing to show)</p>';
    return;
  }}
  let leftHtml = '', rightHtml = '';
  for (const section of file.sections) {{
    leftHtml  += `<div class="hunk-header">${{section.header}}</div>` + renderColumn(null, section.left);
    rightHtml += `<div class="hunk-header">${{section.header}}</div>` + renderColumn(null, section.right);
  }}
  leftBody.innerHTML  = leftHtml  || '<p class="empty">(no hunks)</p>';
  rightBody.innerHTML = rightHtml || '<p class="empty">(no hunks)</p>';
}}

items.forEach(item => {{
  item.addEventListener('click', ev => {{
    ev.preventDefault();
    showFile(Number(item.dataset.idx));
  }});
}});

// Synchronized scrolling between the two panes.
const leftPane  = document.querySelector('.pane.left');
const rightPane = document.querySelector('.pane.right');
let syncing = false;
function syncFrom(src, dst) {{
  if (syncing) return;
  syncing = true;
  dst.scrollTop = src.scrollTop;
  requestAnimationFrame(() => {{ syncing = false; }});
}}
leftPane.addEventListener('scroll',  () => syncFrom(leftPane,  rightPane));
rightPane.addEventListener('scroll', () => syncFrom(rightPane, leftPane));

if (PAYLOADS.length > 0) showFile(0);
</script>
</body>
</html>"""


def main() -> int:
    args = sys.argv[1:]
    rev_left = args[0] if len(args) >= 1 else "HEAD~10"
    rev_right = args[1] if len(args) >= 2 else "HEAD"

    diff = get_diff(rev_left, rev_right)
    files = parse_diff(diff)
    page = render_html(rev_left, rev_right, files)

    title = f"git diff {rev_left}..{rev_right}"
    print(
        f"clud diff demo: opening native webview "
        f"({len(files)} file{'s' if len(files) != 1 else ''})…",
        flush=True,
    )
    webview.create_window(title, html=page, width=1600, height=950)
    webview.start()  # blocks until user closes the window
    print("clud diff demo: closed", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
