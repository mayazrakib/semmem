ALTER TABLE memory_revisions ADD COLUMN system_deleted_at INTEGER;

UPDATE memory_revisions
SET system_deleted_at = system_created_at
WHERE system_operation = 'delete';

PRAGMA user_version = 2;
