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
    system_created_at INTEGER NOT NULL,
    system_updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE commit_changes (
    commit_id INTEGER NOT NULL REFERENCES commits (id),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    entity TEXT NOT NULL CHECK (length(trim(entity)) > 0),
    key TEXT,
    operation TEXT NOT NULL CHECK (length(trim(operation)) > 0),
    reason TEXT,
    associations TEXT,
    index_event TEXT CHECK (index_event IN ('upsert', 'delete')),
    through_sequence INTEGER CHECK (through_sequence >= 0),
    count INTEGER CHECK (count >= 0),
    has_fields INTEGER NOT NULL CHECK (has_fields IN (0, 1)),
    PRIMARY KEY (commit_id, ordinal)
) STRICT, WITHOUT ROWID;

CREATE TABLE commit_change_fields (
    commit_id INTEGER NOT NULL,
    change_ordinal INTEGER NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    PRIMARY KEY (commit_id, change_ordinal, ordinal),
    UNIQUE (commit_id, change_ordinal, name),
    FOREIGN KEY (commit_id, change_ordinal) REFERENCES commit_changes (commit_id, ordinal)
) STRICT, WITHOUT ROWID;

CREATE TABLE log (
    id TEXT PRIMARY KEY CHECK (length(id) = 36 AND id GLOB '????????-????-????-????-????????????' AND length(replace(id, '-', '')) = 32 AND replace(id, '-', '') NOT GLOB '*[^0-9a-f]*'),
    level TEXT NOT NULL CHECK (level IN ('debug', 'info', 'warning', 'error')),
    source TEXT NOT NULL CHECK (length(trim(source)) > 0 AND instr(source, char(10)) = 0 AND instr(source, char(13)) = 0),
    message TEXT NOT NULL CHECK (length(trim(message)) > 0 AND instr(message, char(10)) = 0 AND instr(message, char(13)) = 0),
    timestamp INTEGER NOT NULL,
    task_id TEXT,
    scope TEXT,
    system_created_by TEXT NOT NULL CHECK (length(trim(system_created_by)) > 0),
    system_created_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE INDEX log_chronological ON log (timestamp, id);
CREATE INDEX log_task ON log (task_id, timestamp, id);
CREATE INDEX log_scope ON log (scope, timestamp, id);
CREATE INDEX log_source ON log (source, timestamp, id);
CREATE INDEX log_level ON log (level, timestamp, id);

CREATE TRIGGER commits_no_update BEFORE UPDATE ON commits
BEGIN
    SELECT RAISE(ABORT, 'Audit commits are immutable.');
END;

CREATE TRIGGER commits_no_delete BEFORE DELETE ON commits
BEGIN
    SELECT RAISE(ABORT, 'Audit commits cannot be deleted.');
END;

CREATE TRIGGER commit_changes_no_update BEFORE UPDATE ON commit_changes
BEGIN
    SELECT RAISE(ABORT, 'Audit records are immutable.');
END;

CREATE TRIGGER commit_changes_no_delete BEFORE DELETE ON commit_changes
BEGIN
    SELECT RAISE(ABORT, 'Audit records cannot be deleted.');
END;

CREATE TRIGGER commit_change_fields_no_update BEFORE UPDATE ON commit_change_fields
BEGIN
    SELECT RAISE(ABORT, 'Audit records are immutable.');
END;

CREATE TRIGGER commit_change_fields_no_delete BEFORE DELETE ON commit_change_fields
BEGIN
    SELECT RAISE(ABORT, 'Audit records cannot be deleted.');
END;

CREATE TRIGGER log_no_update BEFORE UPDATE ON log
BEGIN
    SELECT RAISE(ABORT, 'Audit records are immutable.');
END;

CREATE TRIGGER log_no_delete BEFORE DELETE ON log
BEGIN
    SELECT RAISE(ABORT, 'Audit records cannot be deleted.');
END;

CREATE TRIGGER commits_no_replace BEFORE INSERT ON commits
WHEN EXISTS (SELECT 1 FROM commits WHERE id = NEW.id OR key = NEW.key)
BEGIN
    SELECT RAISE(ABORT, 'Audit records cannot be replaced.');
END;

CREATE TRIGGER commit_changes_no_replace BEFORE INSERT ON commit_changes
WHEN EXISTS (SELECT 1 FROM commit_changes WHERE commit_id = NEW.commit_id AND ordinal = NEW.ordinal)
BEGIN
    SELECT RAISE(ABORT, 'Audit records cannot be replaced.');
END;

CREATE TRIGGER commit_change_fields_no_replace BEFORE INSERT ON commit_change_fields
WHEN EXISTS (SELECT 1 FROM commit_change_fields WHERE commit_id = NEW.commit_id AND change_ordinal = NEW.change_ordinal AND (ordinal = NEW.ordinal OR name = NEW.name))
BEGIN
    SELECT RAISE(ABORT, 'Audit records cannot be replaced.');
END;

CREATE TRIGGER log_no_replace BEFORE INSERT ON log
WHEN EXISTS (SELECT 1 FROM log WHERE id = NEW.id)
BEGIN
    SELECT RAISE(ABORT, 'Audit records cannot be replaced.');
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

INSERT INTO commits (key, summary, operation, actor, system_created_at, system_updated_at)
VALUES (lower(hex(randomblob(16))), 'Initialize the current-state database and default general memory type.', 'initialize', 'system', CAST(unixepoch('subsec') * 1000 AS INTEGER), CAST(unixepoch('subsec') * 1000 AS INTEGER));

INSERT INTO commit_changes (commit_id, ordinal, entity, key, operation, has_fields)
VALUES (1, 0, 'database', NULL, 'create', 0), (1, 1, 'memory_types', 'general', 'create', 0);

PRAGMA user_version = 4;
