CREATE TABLE database (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    key TEXT NOT NULL UNIQUE,
    author INTEGER NOT NULL REFERENCES authors (id),

    system_created_by TEXT NOT NULL CHECK (length(system_created_by) > 0),
    system_updated_by TEXT NOT NULL CHECK (length(system_updated_by) > 0),
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE memory_types (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE CHECK (length(key) > 0),
    name TEXT NOT NULL CHECK (length(name) > 0),
    author INTEGER NOT NULL REFERENCES authors (id),

    system_created_by TEXT NOT NULL CHECK (length(system_created_by) > 0),
    system_updated_by TEXT NOT NULL CHECK (length(system_updated_by) > 0),
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE tags (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE CHECK (length(key) > 0),
    name TEXT NOT NULL CHECK (length(name) > 0),
    author INTEGER NOT NULL REFERENCES authors (id),

    system_created_by TEXT NOT NULL CHECK (length(system_created_by) > 0),
    system_updated_by TEXT NOT NULL CHECK (length(system_updated_by) > 0),
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE reference_types (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE CHECK (length(key) > 0),
    name TEXT NOT NULL CHECK (length(name) > 0),
    author INTEGER NOT NULL REFERENCES authors (id),

    system_created_by TEXT NOT NULL CHECK (length(system_created_by) > 0),
    system_updated_by TEXT NOT NULL CHECK (length(system_updated_by) > 0),
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE authors (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE CHECK (length(name) > 0),
    author INTEGER NOT NULL REFERENCES authors (id),

    system_created_by TEXT NOT NULL CHECK (length(system_created_by) > 0),
    system_updated_by TEXT NOT NULL CHECK (length(system_updated_by) > 0),
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE memories (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE CHECK (length(key) > 0),
    author INTEGER NOT NULL REFERENCES authors (id),

    system_created_by TEXT NOT NULL CHECK (length(system_created_by) > 0),
    system_updated_by TEXT NOT NULL CHECK (length(system_updated_by) > 0),
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL,
    system_version INTEGER NOT NULL CHECK (system_version >= 1),
    system_deleted_at INTEGER,
    system_deleted_by TEXT,

    FOREIGN KEY (id, system_version)
        REFERENCES memory_revisions (memory_id, system_version)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE TABLE memory_revisions (
    id INTEGER PRIMARY KEY,
    memory_id INTEGER NOT NULL REFERENCES memories (id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    body TEXT NOT NULL CHECK (length(body) > 0),
    summary TEXT,
    basis TEXT CHECK (basis IN ('stated', 'observed', 'inferred', 'assumed')),
    scope TEXT,
    observed_at INTEGER DEFAULT (CAST(unixepoch('subsec') * 1000 AS INTEGER)),
    verified_at INTEGER DEFAULT (CAST(unixepoch('subsec') * 1000 AS INTEGER)),
    review_after INTEGER,
    type_id INTEGER NOT NULL DEFAULT 1 REFERENCES memory_types (id),
    author INTEGER NOT NULL REFERENCES authors (id),

    system_version INTEGER NOT NULL CHECK (system_version >= 1),
    system_operation TEXT NOT NULL
        CHECK (system_operation IN ('create', 'update', 'delete', 'restore')),
    system_created_by TEXT NOT NULL CHECK (length(system_created_by) > 0),
    system_updated_by TEXT NOT NULL CHECK (length(system_updated_by) > 0),
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL,
    system_deleted_at INTEGER,
    system_deleted_by TEXT,

    UNIQUE (memory_id, system_version)
) STRICT;

CREATE TABLE memory_revision_tags (
    revision_id INTEGER NOT NULL REFERENCES memory_revisions (id) ON DELETE CASCADE,
    tag_id INTEGER NOT NULL REFERENCES tags (id),
    author INTEGER NOT NULL REFERENCES authors (id),

    system_created_by TEXT NOT NULL CHECK (length(system_created_by) > 0),
    system_updated_by TEXT NOT NULL CHECK (length(system_updated_by) > 0),
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL,

    PRIMARY KEY (revision_id, tag_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE memory_revision_references (
    revision_id INTEGER NOT NULL REFERENCES memory_revisions (id) ON DELETE CASCADE,
    target_memory_id INTEGER NOT NULL REFERENCES memories (id),
    reference_type_id INTEGER NOT NULL REFERENCES reference_types (id),
    author INTEGER NOT NULL REFERENCES authors (id),

    system_created_by TEXT NOT NULL CHECK (length(system_created_by) > 0),
    system_updated_by TEXT NOT NULL CHECK (length(system_updated_by) > 0),
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL,

    PRIMARY KEY (revision_id, target_memory_id, reference_type_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE index_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    memory_key TEXT NOT NULL,
    memory_version INTEGER NOT NULL,
    operation TEXT NOT NULL CHECK (operation IN ('upsert', 'delete')),
    author INTEGER NOT NULL REFERENCES authors (id),

    system_created_by TEXT NOT NULL CHECK (length(system_created_by) > 0),
    system_updated_by TEXT NOT NULL CHECK (length(system_updated_by) > 0),
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE INDEX memories_recent
    ON memories (system_updated_at DESC, id DESC)
    WHERE system_deleted_at IS NULL;

CREATE INDEX memory_revisions_type
    ON memory_revisions (type_id, memory_id, system_version);

CREATE INDEX memory_revisions_scope
    ON memory_revisions (scope, memory_id, system_version);

CREATE INDEX memory_revisions_basis
    ON memory_revisions (basis, memory_id, system_version);

CREATE INDEX memory_revisions_author ON memory_revisions (author);

CREATE INDEX memory_revision_tags_tag
    ON memory_revision_tags (tag_id, revision_id);

CREATE INDEX memory_revision_references_target
    ON memory_revision_references (target_memory_id, reference_type_id, revision_id);

CREATE INDEX memory_revision_references_type
    ON memory_revision_references (reference_type_id, revision_id);

CREATE VIEW current_memories AS
SELECT
    memories.id,
    memories.key,
    memory_revisions.id AS revision_id,
    memory_revisions.title,
    memory_revisions.body,
    memory_revisions.summary,
    memory_revisions.basis,
    memory_revisions.scope,
    memory_revisions.observed_at,
    memory_revisions.verified_at,
    memory_revisions.review_after,
    memory_types.key AS type,
    authors.name AS author,
    memories.system_created_at,
    memories.system_updated_at,
    memories.system_version,
    memories.system_deleted_at,
    memories.system_created_by,
    memories.system_updated_by,
    memories.system_deleted_by
FROM memories
INNER JOIN memory_revisions
    ON memory_revisions.memory_id = memories.id
    AND memory_revisions.system_version = memories.system_version
INNER JOIN memory_types ON memory_types.id = memory_revisions.type_id
INNER JOIN authors ON authors.id = memory_revisions.author;

INSERT INTO authors (id, name, author, system_created_by, system_updated_by, system_created_at, system_updated_at)
VALUES (1, 'system', 1, 'system', 'system', CAST(unixepoch('subsec') * 1000 AS INTEGER), CAST(unixepoch('subsec') * 1000 AS INTEGER));

INSERT INTO memory_types (id, key, name, author, system_created_by, system_updated_by, system_created_at, system_updated_at)
VALUES (1, 'general', 'general', 1, 'system', 'system', CAST(unixepoch('subsec') * 1000 AS INTEGER), CAST(unixepoch('subsec') * 1000 AS INTEGER));

PRAGMA user_version = 1;
