CREATE TABLE database_identity (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    key TEXT NOT NULL UNIQUE,

    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE memory_kinds (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE CHECK (length(key) > 0),
    name TEXT NOT NULL CHECK (length(name) > 0),

    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE tags (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE CHECK (length(key) > 0),
    name TEXT NOT NULL CHECK (length(name) > 0),

    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE relation_types (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE CHECK (length(key) > 0),
    name TEXT NOT NULL CHECK (length(name) > 0),

    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE actors (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE CHECK (length(name) > 0),

    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE memories (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE CHECK (length(key) > 0),

    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL,
    system_version INTEGER NOT NULL CHECK (system_version >= 1),
    system_deleted_at INTEGER,

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
    claim_type TEXT CHECK (claim_type IN ('explicit user statement', 'verified observation', 'inference')),
    scope TEXT,
    observed_at INTEGER,
    verified_at INTEGER,
    review_after INTEGER,
    kind_id INTEGER NOT NULL REFERENCES memory_kinds (id),
    actor_id INTEGER NOT NULL REFERENCES actors (id),

    system_version INTEGER NOT NULL CHECK (system_version >= 1),
    system_operation TEXT NOT NULL
        CHECK (system_operation IN ('create', 'update', 'delete', 'restore')),
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL,
    system_deleted_at INTEGER,

    UNIQUE (memory_id, system_version)
) STRICT;

CREATE TABLE memory_revision_tags (
    revision_id INTEGER NOT NULL REFERENCES memory_revisions (id) ON DELETE CASCADE,
    tag_id INTEGER NOT NULL REFERENCES tags (id),

    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL,

    PRIMARY KEY (revision_id, tag_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE memory_revision_links (
    revision_id INTEGER NOT NULL REFERENCES memory_revisions (id) ON DELETE CASCADE,
    target_memory_id INTEGER NOT NULL REFERENCES memories (id),
    relation_type_id INTEGER NOT NULL REFERENCES relation_types (id),

    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL,

    PRIMARY KEY (revision_id, target_memory_id, relation_type_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE index_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    memory_key TEXT NOT NULL,
    memory_version INTEGER NOT NULL,
    operation TEXT NOT NULL CHECK (operation IN ('upsert', 'delete')),

    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE INDEX memories_recent
    ON memories (system_updated_at DESC, id DESC)
    WHERE system_deleted_at IS NULL;

CREATE INDEX memory_revisions_kind
    ON memory_revisions (kind_id, memory_id, system_version);

CREATE INDEX memory_revisions_scope
    ON memory_revisions (scope, memory_id, system_version);

CREATE INDEX memory_revisions_claim_type
    ON memory_revisions (claim_type, memory_id, system_version);

CREATE INDEX memory_revisions_actor ON memory_revisions (actor_id);

CREATE INDEX memory_revision_tags_tag
    ON memory_revision_tags (tag_id, revision_id);

CREATE INDEX memory_revision_links_target
    ON memory_revision_links (target_memory_id, relation_type_id, revision_id);

CREATE INDEX memory_revision_links_relation
    ON memory_revision_links (relation_type_id, revision_id);

CREATE VIEW current_memories AS
SELECT
    memories.id,
    memories.key,
    memory_revisions.id AS revision_id,
    memory_revisions.title,
    memory_revisions.body,
    memory_revisions.summary,
    memory_revisions.claim_type,
    memory_revisions.scope,
    memory_revisions.observed_at,
    memory_revisions.verified_at,
    memory_revisions.review_after,
    memory_kinds.key AS kind,
    actors.name AS actor,
    memories.system_created_at,
    memories.system_updated_at,
    memories.system_version,
    memories.system_deleted_at
FROM memories
INNER JOIN memory_revisions
    ON memory_revisions.memory_id = memories.id
    AND memory_revisions.system_version = memories.system_version
INNER JOIN memory_kinds ON memory_kinds.id = memory_revisions.kind_id
INNER JOIN actors ON actors.id = memory_revisions.actor_id;

PRAGMA user_version = 3;
