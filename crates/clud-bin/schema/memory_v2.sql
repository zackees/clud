-- user_version = 2
-- Identity + scoping migration (#267). Forward-only.
--
-- Adds repo-level scoping primitives alongside the existing session_id
-- column. Scope is composed by `memory::identity::scope_key`:
--   - `repo://<normalized-origin-url>` when the working tree has an origin remote.
--   - `dir://<canonical-common-dir>` fallback when origin is missing.
--   - With a `#branch=<name>` suffix when the working tree opted into
--     branch isolation via the `<common_dir>/.clud/memory-branch-isolate` marker.
--
-- `branch_name` records the current branch name (`None` on detached HEAD)
-- as provenance metadata; it is NOT a partition key by default — cross-branch
-- memory continuity is the common case (see DD-014).
--
-- `is_orphan` is a boolean flag (0/1) recording whether the branch was
-- detected as an orphan branch (no merge-base against the default branch).
-- It is provenance only; orphans share scope with main by default.
--
-- Backfill rule: existing rows on a v1 database keep `scope_key = NULL`,
-- which behaves like "global" (matches existing `session_id = NULL`
-- semantics — null filters match every row). Callers that need
-- repo-scoped behavior post-migration must re-save the affected memories
-- with an explicit scope.

ALTER TABLE memories ADD COLUMN scope_key   TEXT;
ALTER TABLE memories ADD COLUMN branch_name TEXT;
ALTER TABLE memories ADD COLUMN is_orphan   INTEGER NOT NULL DEFAULT 0;

CREATE INDEX idx_memories_scope ON memories(scope_key);
