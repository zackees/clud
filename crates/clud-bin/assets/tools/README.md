# Bundled tools

`src/tools.rs` embeds the scripts here and installs them under
`~/.clud/tools/`. Invoke one with `clud tool run <relative-path> [args]`.

The runner returns the tool's own exit code when the tool finishes. Its
watchdog returns `124` when a time limit fires. A resumable tool gets a JSON
line on stderr with `status: in-progress`; run it again with the same args.
A killable tool gets `status: aborted`. Exit `0` means the tool itself
finished successfully. The watchdog passes its command cap to children as
`CLUD_TOOL_COMMAND_TIMEOUT_SECS`; `github/pr_merge_watch.py` uses that value
to finish its own timeout and cleanup before the wrapper's cap.

Stdout over 32 KB truncates visibly with an artifact path; see
[bounded-output.md](../../../../docs/architecture/bounded-output.md).

`diagnostics/transcript_report.py` reports repeated tool-call bursts,
compactions, context jumps and context errors from one Claude transcript.
It joins clud's launch-context record by hashed session id to report the
effective max-context value and its source
([launch-plan.md](../../../../docs/architecture/launch-plan.md#launch-context-record-1675)).
Its output holds counts and salted fingerprints, never raw content (#1276,
[DD-138](../../../../docs/DESIGN_DECISIONS.md#dd-138-the-first-1276-slice-is-a-read-only-transcript-analyzer-not-a-repeated-call-guard)).

`code/outline.py` lists a file's definitions (Rust, Python, JS/TS, C/C++,
via pinned tree-sitter wheels) and Markdown headings with line ranges, so
`/grind` agents read one range instead of paging through a file (#1339). `code/patch.py` applies
search/replace blocks across files all-or-nothing, each block matching
exactly once in the file's own line endings (an LF block matches a CRLF
working tree and keeps CRLF); in a `/grind` run it refuses files outside the
run facts' task list for the checkout (#1339). `code/ast_grep.py` runs a pinned
ast-grep for structural search and rewrite (#1339).

PR watcher callers select a declared session/background/foreground wait and
required-check or all-check completion scope; see
[watch recovery](../../../../docs/architecture/hook-dispatch.md#watch-recovery-and-continuation-2004).

`github/pr_merge.py` submits an exact GitHub PR head to an enforced merge
queue, or performs a normal squash merge when no queue applies. It requires
`--repo OWNER/REPO` and `--match-head-commit` with the full head SHA; it never
uses admin bypass. A successful enqueue is not a merged PR: follow it with
`github/pr_merge_watch.py`. See [CI merge queue](../../../../docs/architecture/ci.md#merge-queue-2048-decided).

`github/issue_context.py` is the bounded, read-only issue reader (#2229). It
fetches an issue and its comments once over `gh api` GETs, saves the complete
snapshot under `~/.clud/logs/issue-context/<owner>__<repo>/` (never in the
checkout), and prints a byte-bounded view: metadata, a body excerpt, one
excerpt per comment, and explicit omission counts with continuation
instructions. `--comment <id>` prints one comment in full; `--refresh`
re-fetches and reports new/edited comments against the snapshot; `--json`
emits the same summary as JSON. It is the sanctioned path for issue
investigations, in place of `gh issue view --json body,comments` and hand
slicing each comment with `--jq`. See
[bounded-output.md](../../../../docs/architecture/bounded-output.md).
