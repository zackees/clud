---
name: clud-python-lint-deadcode
description: Find unused Python code (production-focused, symbols reachable only from tests count as dead) via the bundled vulture-backed tool.
triggers:
  - When the user asks to find dead code, unused code, or what can be deleted in a Python project
  - When the user mentions vulture, dead code analysis, or code cleanup audits
  - When the user invokes "/clud-python-lint-deadcode"
---
<!-- managed-by: clud -->

# /clud-python-lint-deadcode

Find dead Python code in a project — symbols (functions, classes, methods, imports, variables) that no production caller exercises. The skill's bundled tool runs `vulture` with the rule that **symbols reachable only from tests count as production-dead**: the test exists to verify behavior, but if no production caller invokes that behavior, the production code itself is unused.

Three hard rules:

1. **The tool only reports; it never deletes.** The agent (or human) decides what to remove based on confidence + understanding of the codebase.
2. **Test-reachability does not save a symbol.** A function only called from `tests/` is still flagged. This is intentional — see "Why" below.
3. **RED -> GREEN around any removal.** Before deleting a flagged symbol, the agent must add or identify a test/regression that confirms the symbol's behavior is genuinely unused (or that the existing tests still pass after removal). Confidence scores are guidance, not proof.

## Invocation

```
clud tool run python/lint_deadcode.py [<path>...] [--min-confidence N] [--exclude PATH]... [--json]
```

Defaults: scans the current directory, `--min-confidence 60`. The tool always emits structured JSON to stdout.

Output shape:

```json
{
  "v": 1,
  "files_scanned": 42,
  "deadcode": [
    {
      "file": "src/foo.py",
      "name": "old_helper",
      "line": 42,
      "type": "function",
      "confidence": 60,
      "size": 7,
      "reachable_from_tests": false
    }
  ]
}
```

Exit code: `0` (no dead code), `1` (at least one production-dead symbol), `2` (tool error).

## Workflow

1. **Run the tool** in the project root: `clud tool run python/lint_deadcode.py`.
2. **Sort findings by confidence**. `confidence ≥ 80` is high-signal (vulture is very sure). 60–79 needs more thought.
3. **Investigate before deleting**. For each candidate:
   - Read the symbol's surrounding context — is it part of a public API, plugin entry point, dynamic-dispatch target, or `__all__` export?
   - Check whether it's reachable via reflection, decorators, `getattr`, entry points (`pyproject.toml`), or import-time side effects vulture cannot see statically.
   - If genuinely unused: delete it.
4. **Run tests after each batch of removals**. RED -> GREEN: the test suite that was green before must stay green after. If a test was the only caller, the test should also go.
5. **Re-run the tool**. Removing dead code can reveal more dead code (the chained-dead-code case). Single-pass for V1; iterative `--converge` is future work — see "Convergence" below.

## Why test-reachable counts as dead

A function `compute_legacy_format()` with one caller — `test_compute_legacy_format()` — is production-dead even though the test passes. The test verifies that `compute_legacy_format` behaves correctly, but no production code path calls it. The behavior is exercised in tests only because the tests exist; nothing real depends on it. Delete both.

This rule sometimes surfaces *intentional* test-only helpers (test fixtures used as utility). The tool reports them; the human/agent reads them and skips obvious cases.

## Convergence

`vulture`'s single-pass mode reports symbols with no static reference. Removing those symbols can reveal *their* callees as newly-unused — a chained-dead-code case. V1 of this tool ships single-pass; the `--converge` flag is reserved for the iterative fixed-point loop.

Workaround until `--converge` lands: run the tool, fix the high-confidence findings, re-run. Repeat until empty.

## Failure modes to avoid

- **Deleting public API.** A symbol vulture flags may be your library's public surface that lives at the import root and is only "called" by your users (whose code vulture cannot see). Check `__all__`, `pyproject.toml` entry points, and module-level imports before deleting.
- **Deleting dynamic-dispatch targets.** Decorator registries, factory dictionaries, plugin systems — anything that resolves at runtime via string lookup is invisible to vulture.
- **Deleting code referenced by docstrings or comments.** Vulture sees code only. A test referenced by `pytest --collect-only` only via name discovery may be invisible.
- **Skipping the test-suite check after removals.** RED -> GREEN is mandatory; don't trust confidence alone.
- **Treating low confidence (< 60) as actionable without manual review.** vulture's `--min-confidence` exists to filter the noise; respect it.

## When not to use this

- Pre-1.0 codebases where the public API is intentionally over-provisioned for future use.
- Codebases with extensive use of dynamic dispatch, reflection, or runtime-loaded plugins.
- Codebases where the agent doesn't have access to the test suite (test-reachability classification is broken without it).

## Related

- `crates/clud-bin/assets/tools/python/lint_deadcode.py` — the tool source.
- `crates/clud-bin/src/tools.rs` — `BUNDLED_TOOLS` registry entry with `kill_semantics: Killable`, `command_timeout: 60m`, `progress_timeout: 2m`.
- Vulture's docs: https://github.com/jendrikseipp/vulture for `--ignore-names`, `--exclude`, allowlist patterns, etc.
