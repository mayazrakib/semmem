# Database schema

SQLite stores the canonical UTF-8 Markdown bodies and all authoritative fields. Search uses a separate, rebuildable Tantivy index. The schema has no arbitrary metadata column, entity-attribute-value table, or JSON revision snapshot. The sole initializer is [initialize.sql](../initialize.sql), which includes per-revision deletion state and metadata. It creates schema version three for fresh databases. Existing incompatible schemas are rejected without conversion.

## Tables and relationships

| Table | Responsibility |
|---|---|
| `database_identity` | Identifies this database so a search index from another database is not reused |
| `memories` | Stable identity, current revision number, and lifecycle fields |
| `memory_revisions` | Complete title, Markdown body, metadata, kind, actor, operation, and version for each edit |
| `memory_kinds` | Stable kind keys and readable names |
| `tags` | Stable tag keys and readable names |
| `relation_types` | Stable relationship keys and readable names |
| `actors` | Deduplicated authorship labels |
| `memory_revision_tags` | Tags belonging to a particular revision |
| `memory_revision_links` | Typed outgoing links belonging to a particular revision |
| `index_events` | Transactional search-index publication events, including purge tombstones |

Each memory has an integer internal `id` and a unique public `key`. Content belongs to a revision. A deferred composite foreign key ensures that the memory's current `(id, system_version)` identifies an existing revision. The `current_memories` view joins identity, current content, kind, and actor for reads.

Kinds, tags, actors, and relationship names occur in their own tables. Revision associations reference integer keys instead of repeating names. Junction tables have composite primary keys and use `WITHOUT ROWID`. Foreign keys are enabled on service connections. The application uses parameterized SQL, and the migration defines strict tables.

Memory writes create missing kind, tag, and relationship-type rows using the supplied key as their initial name. The default kind is `note`. Explicit taxonomy operations can create entries in advance or change their readable names. Link targets must identify existing memories or memories created in the same batch. Taxonomy deletion rejects entries still referenced by current or historical revisions.

## Revision metadata

The following nullable fields belong to every revision and survive history, deletion, and restoration:

| Field | Meaning |
|---|---|
| `summary` | Manually supplied retrieval summary, returned in full |
| `claim_type` | `explicit user statement`, `verified observation`, or `inference` |
| `scope` | Project or environment label, with exact-match filtering |
| `observed_at` | When the fact was observed or stated |
| `verified_at` | When the fact was last checked |
| `review_after` | When the fact should be reconsidered |

Metadata timestamps are signed integer UTC milliseconds since Unix epoch. Unknown values remain `NULL`; the service does not infer provenance, verification, or expiration. On updates, omission preserves a value, explicit JSON `null` clears it, and a supplied value replaces it. Editing a record does not update `verified_at` automatically.

## Revisions and lifecycle

The trailing `system_*` fields describe internal lifecycle state. Timestamps use UTC Unix milliseconds. `system_version` starts at one and increases for each saved memory revision. `system_operation` records `create`, `update`, `delete`, or `restore`.

Memory content and associations are saved together in one transaction. Revisions retain Markdown, kind, actor, tags, and outgoing relationships through normalized rows. Updates use an expected version, so a concurrent edit produces a conflict instead of silently overwriting content. Restoration appends a new current revision rather than rewinding the version counter.

Soft deletion creates a deletion revision and sets `system_deleted_at`. Ordinary reads, browsing, and search omit deleted memories. Explicit reads can include them. Permanent purge requires the current expected version and removes active or deleted memories. It removes the memory, its revisions, its tag associations, and its outgoing links. It also removes incoming link rows from other memories' current and historical revisions to preserve referential integrity. Those erased historical links cannot be restored. Surviving memories whose current links change receive a new system-authored revision and version, preserving concurrent-edit protection and any existing deletion state. Taxonomy rows and free text in other memories are not rewritten by a purge.

Purge removes logical database records. It does not promise forensic erasure from storage media, old database pages, index segments, logs, or external backups.

## Indexes and publication

The schema indexes active memories by descending update time and ID, revision lookup by memory and version, revision kinds and actors, tag-to-revision membership, and incoming relationships. Primary keys cover identity, public-key, and association lookups. Browse pagination uses the last update timestamp and ID; revision pagination uses the last version. Taxonomy lists use key-based pagination.

Search, browse, history, and memory mutation responses omit full Markdown bodies. Search returns a complete manual summary or a matching paragraph excerpt. `get_memories` defaults to `view: full`, returning exact content and associations; `view: summary` selects compact metadata with a manual summary or the first nonempty paragraph. Pagination is caller controlled, with no application maximum page size. The service imposes no token budget, payload ceiling, query-expansion ceiling, or batch-count ceiling.

SQLite runs in write-ahead logging mode with `synchronous=FULL`, cached prepared statements, a reusable dynamically growing read pool, and an unbounded writer queue. The writer groups pending requests before publishing the search index. Each memory batch has its own transaction.

Index events are written in the same transaction as memory changes. Their textual memory keys deliberately survive memory deletion, allowing search tombstones to be published after a purge. The service advances the search watermark and acknowledges processed events after committing the index. Startup replays pending events; an index with an incompatible database identity or an unreadable index is replaced and rebuilt from current records.

Successful writes are acknowledged after search publication. If SQLite commits but publication fails, the response identifies the committed state and leaves events available for recovery. Search candidates are checked against current SQLite state before they are returned.

Use the CLI and MCP interfaces for mutations so versioning, revisions, and search publication remain coordinated. New databases initialize transactionally at schema version three. Existing incompatible schemas fail explicitly before database settings or content are modified. Search schema generations are tracked independently and can trigger a rebuild without changing canonical memories.

The runtime has no application request or busy-wait deadline. SQLite contention yields and retries; cancellation and authorization validation remain in effect. Machine address space, SQLite representations, and Tantivy-required writer arena configuration remain library constraints. Index maintenance batches process all pending work; their size does not reject caller input. All retrieval uses readable text and ordinary lexical ranking, without embeddings or model inference.
