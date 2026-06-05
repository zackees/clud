-- user_version = 1
-- Canonical schema for the agent-memory store. `{embed_dim}` is interpolated
-- by `memory::schema::migrate` at first open and then frozen for the lifetime
-- of the database file; a subsequent open with a different dim raises
-- `MemoryError::DimMismatch`.
CREATE TABLE memories (
  id              TEXT PRIMARY KEY,                       -- uuidv7
  session_id      TEXT,                                   -- nullable; null = global
  tier            INTEGER NOT NULL DEFAULT 0,             -- 0=Working 1=Episodic 2=Semantic
  content         TEXT NOT NULL,
  created_at_ms       INTEGER NOT NULL,
  updated_at_ms       INTEGER NOT NULL,
  tier_change_at_ms   INTEGER NOT NULL,
  access_count        INTEGER NOT NULL DEFAULT 0,
  last_access_at_ms   INTEGER NOT NULL,
  metadata_json   TEXT
) STRICT;

CREATE INDEX idx_memories_session   ON memories(session_id);
CREATE INDEX idx_memories_tier      ON memories(tier);
CREATE INDEX idx_memories_updated   ON memories(updated_at_ms);

CREATE TABLE sessions (
  id              TEXT PRIMARY KEY,
  started_at_ms   INTEGER NOT NULL,
  ended_at_ms     INTEGER,
  metadata_json   TEXT
) STRICT;

CREATE TABLE memory_relations (
  src_id          TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
  dst_id          TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
  kind            TEXT NOT NULL,
  weight          REAL NOT NULL DEFAULT 1.0,
  created_at_ms   INTEGER NOT NULL,
  PRIMARY KEY (src_id, dst_id, kind)
) STRICT;

CREATE TABLE lessons (
  id              TEXT PRIMARY KEY,
  memory_id       TEXT REFERENCES memories(id) ON DELETE CASCADE,
  summary         TEXT NOT NULL,
  created_at_ms   INTEGER NOT NULL,
  metadata_json   TEXT
) STRICT;

CREATE TABLE actions (
  id              TEXT PRIMARY KEY,
  session_id      TEXT REFERENCES sessions(id) ON DELETE SET NULL,
  kind            TEXT NOT NULL,
  payload_json    TEXT,
  occurred_at_ms  INTEGER NOT NULL
) STRICT;
CREATE INDEX idx_actions_session ON actions(session_id);
CREATE INDEX idx_actions_kind    ON actions(kind);

-- Sidecar table that pins the embedding dim used to build memory_vec.
-- Read on reopen to detect MemoryError::DimMismatch deterministically
-- without parsing the virtual-table DDL.
CREATE TABLE memory_meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT;

CREATE VIRTUAL TABLE memory_vec USING vec0(
  id TEXT PRIMARY KEY,
  embedding FLOAT[{embed_dim}]
);
