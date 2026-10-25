# Database Schema

SQLite stores canonical Markdown bodies and authoritative fields. Tantivy provides a rebuildable full-text index. [initialize.sql](../initialize.sql) is the sole initializer for the new schema v1. Existing incompatible schemas, including earlier schemas carrying version one, are rejected without migration or database changes.

## Tables and Attribution

| Table | Responsibility |
|---|---|
| `database` | Database identity for search-index ownership |
| `memories` | Stable memory identity, current revision, and lifecycle fields |
| `memory_revisions` | Complete content and metadata for each revision |
| `memory_types` | Stable type keys and readable names |
| `tags` | Stable tag keys and readable names |
| `reference_types` | Stable reference type keys and readable names |
| `authors` | Registered content authors |
| `memory_revision_tags` | Per-revision tags |
| `memory_revision_references` | Per-revision typed references |
| `index_events` | Transactional search publication events |

Every application table has mandatory author attribution through a non-null `author` foreign key to `authors`. Public records expose author names. An internal system author bootstraps database and housekeeping records and is excluded from registered-author choices. References use `target_key` and `reference_type`. The default memory type is `general`.

Content author and trusted writer identity are separate. System attribution uses `system_created_by`, `system_updated_by`, and `system_deleted_by`. Clients cannot set system audit fields. Creation requires a nonblank author; new names register automatically. Updates preserve authorship unless explicitly changed. Deletion and restoration preserve content authorship. Registration is idempotent, and deregistration rejects authors referenced by saved rows or history.

A deferred composite foreign key ensures that each memory's current `(id, system_version)` identifies an existing revision. Strict normalized tables retain complete content, tags, and references. Missing taxonomy keys are created from memory writes; explicit taxonomy operations can set readable names. Referenced taxonomy entries cannot be deleted.

## Revision Metadata

| Field | Meaning |
|---|---|
| `summary` | Stored summary, returned intact |
| `basis` | `stated`, `observed`, `inferred`, or `assumed` |
| `scope` | Project or environment label |
| `observed_at` | Observation timestamp; defaults to current time on creation |
| `verified_at` | Verification timestamp; defaults to current time on creation |
| `review_after` | Optional review timestamp |

Timestamps use signed integer UTC milliseconds since Unix epoch. Explicit null remains valid for nullable metadata. On creation, omitted observation and verification timestamps receive defaults. Updates preserve omitted values; explicit null clears them. Ordinary edits do not automatically refresh verification time. Identical creation retries remain idempotent despite timestamp defaults.

## Revisions and Lifecycle

`system_version` starts at one and increases for every saved revision. `system_operation` identifies creation, update, deletion, or restoration. Writes save content and associations atomically and require the expected version for existing memories. Restoration appends a revision instead of rewinding the version counter.

Soft deletion creates a deletion revision. Current reads and searches exclude deleted memories unless explicitly requested. Purge removes the memory, every revision, and incoming and outgoing reference rows, including historical references. Surviving memories whose current references change receive a new guarded revision while preserving content authorship and deletion state. Purge does not rewrite free text or promise forensic erasure from storage media or external backups.

## Retrieval and Historical Search

Search, browsing, history, reads, and mutation responses return complete bodies and stored summaries. The accepted `summary` view is a compatibility alias for full records. There are no generated excerpts. Search hits always include `body`, `system_created_at`, `system_updated_at`, revision identity, score, author, and metadata.

Current live search is the default. `history: true` searches all retained revisions. `as_of` selects the revision effective at a UTC Unix millisecond timestamp; it cannot be combined with history mode. Revisions become effective inclusively at their update timestamp and cease to be effective when the next revision begins. If revisions share a timestamp, the highest applicable version wins. `include_deleted` applies to the selected revision's state, so pre-deletion history remains searchable by default.

Filters evaluate the selected revision's metadata. Exact and fuzzy candidates are ranked together before limiting results. Scores descend globally, with key ascending and version descending as deterministic tie breaks. Pagination and batch sizes are caller controlled; the service introduces no token budget or arbitrary payload ceiling.

## Index Publication and Recovery

SQLite uses WAL with `synchronous=FULL`. A single writer coordinates transactions with a reusable read pool. Transactional index events identify changed memory keys even after purge. Publication refreshes every retained revision of an affected memory and removes all indexed versions after purge. Successful writes are acknowledged after search publication.

The index stores revision identity, effective intervals, current/deleted state, full bodies, and metadata. Startup rebuilds from all canonical revisions when the database identity or search generation changes or the index is unusable. Pending events are replayed after crashes. Search candidates are validated against SQLite according to the requested temporal mode before returning results.

Publication failures report committed state and affected versions. Callers inspect current records before retrying. Index generation is independent of SQLite schema version. Library representation and machine resource limits remain; index maintenance does not impose application-level request ceilings.
