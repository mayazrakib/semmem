CREATE TABLE database (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    key TEXT NOT NULL UNIQUE,
    system_created_by TEXT NOT NULL CHECK (length(trim(system_created_by)) > 0),
    system_updated_by TEXT NOT NULL CHECK (length(trim(system_updated_by)) > 0),
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE memory_types (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE CHECK (length(key) > 0),
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    system_created_by TEXT NOT NULL,
    system_updated_by TEXT NOT NULL,
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE tags (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE CHECK (length(key) > 0),
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    system_created_by TEXT NOT NULL,
    system_updated_by TEXT NOT NULL,
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE reference_types (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE CHECK (length(key) > 0),
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    system_created_by TEXT NOT NULL,
    system_updated_by TEXT NOT NULL,
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE memories (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE CHECK (length(key) > 0),
    title TEXT NOT NULL,
    body TEXT NOT NULL CHECK (length(trim(body)) > 0),
    summary TEXT,
    basis TEXT CHECK (basis IN ('stated', 'observed', 'inferred', 'assumed')),
    scope TEXT,
    observed_at INTEGER DEFAULT (CAST(unixepoch('subsec') * 1000 AS INTEGER)),
    verified_at INTEGER DEFAULT (CAST(unixepoch('subsec') * 1000 AS INTEGER)),
    review_after INTEGER,
    type_id INTEGER NOT NULL DEFAULT 1 REFERENCES memory_types (id),
    system_token TEXT NOT NULL CHECK (length(system_token) > 0),
    system_created_by TEXT NOT NULL,
    system_updated_by TEXT NOT NULL,
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE memory_tags (
    memory_id INTEGER NOT NULL REFERENCES memories (id) ON DELETE CASCADE,
    tag_id INTEGER NOT NULL REFERENCES tags (id),
    system_created_by TEXT NOT NULL,
    system_updated_by TEXT NOT NULL,
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL,
    PRIMARY KEY (memory_id, tag_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE memory_references (
    memory_id INTEGER NOT NULL REFERENCES memories (id) ON DELETE CASCADE,
    target_memory_id INTEGER NOT NULL REFERENCES memories (id) ON DELETE CASCADE,
    reference_type_id INTEGER NOT NULL REFERENCES reference_types (id),
    system_created_by TEXT NOT NULL,
    system_updated_by TEXT NOT NULL,
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL,
    PRIMARY KEY (memory_id, target_memory_id, reference_type_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE index_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    memory_key TEXT NOT NULL,
    operation TEXT NOT NULL CHECK (operation IN ('upsert', 'delete')),
    system_created_by TEXT NOT NULL,
    system_updated_by TEXT NOT NULL,
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE commits (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    key TEXT NOT NULL UNIQUE,
    summary TEXT NOT NULL CHECK (length(trim(summary)) > 0),
    operation TEXT NOT NULL CHECK (length(trim(operation)) > 0),
    actor TEXT NOT NULL CHECK (length(trim(actor)) > 0),
    changes TEXT NOT NULL CHECK (json_valid(changes) AND json_type(changes) = 'array'),
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TRIGGER commits_no_update BEFORE UPDATE ON commits
BEGIN
    SELECT RAISE(ABORT, 'Audit commits are immutable.');
END;

CREATE TRIGGER commits_no_delete BEFORE DELETE ON commits
BEGIN
    SELECT RAISE(ABORT, 'Audit commits cannot be deleted.');
END;

CREATE INDEX memories_recent ON memories (system_updated_at DESC, id DESC);
CREATE INDEX memories_type ON memories (type_id, id);
CREATE INDEX memories_scope ON memories (scope, id);
CREATE INDEX memories_basis ON memories (basis, id);
CREATE INDEX memory_tags_tag ON memory_tags (tag_id, memory_id);
CREATE INDEX memory_references_target ON memory_references (target_memory_id, reference_type_id, memory_id);
CREATE INDEX memory_references_type ON memory_references (reference_type_id, memory_id);

CREATE VIEW current_memories AS
SELECT memories.id, memories.key, memories.title, memories.body, memories.summary,
    memories.basis, memories.scope, memories.observed_at, memories.verified_at,
    memories.review_after, memory_types.key AS type, memories.system_token,
    memories.system_created_by, memories.system_updated_by,
    memories.system_created_at, memories.system_updated_at
FROM memories
INNER JOIN memory_types ON memory_types.id = memories.type_id;

INSERT INTO memory_types (id, key, name, system_created_by, system_updated_by, system_created_at, system_updated_at)
VALUES (1, 'general', 'general', 'system', 'system', CAST(unixepoch('subsec') * 1000 AS INTEGER), CAST(unixepoch('subsec') * 1000 AS INTEGER));

INSERT INTO database (id, key, system_created_by, system_updated_by, system_created_at, system_updated_at)
VALUES (1, lower(hex(randomblob(16))), 'system', 'system', CAST(unixepoch('subsec') * 1000 AS INTEGER), CAST(unixepoch('subsec') * 1000 AS INTEGER));

INSERT INTO commits (key, summary, operation, actor, changes, system_created_at, system_updated_at)
VALUES (lower(hex(randomblob(16))), 'Initialize the current-state database and default general memory type.', 'initialize', 'system', '[{"entity":"database","operation":"create"},{"entity":"memory_types","key":"general","operation":"create"}]', CAST(unixepoch('subsec') * 1000 AS INTEGER), CAST(unixepoch('subsec') * 1000 AS INTEGER));

PRAGMA user_version = 2;
