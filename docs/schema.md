# Database Schema

SQLite stores canonical current Markdown bodies and metadata. Tantivy provides a rebuildable full-text index. [database/initialize.sql](../database/initialize.sql) is the sole initializer for schema v4. Existing incompatible databases are rejected intact and require an explicitly authorized reset rather than migration.

## Current State

| Table | Responsibility |
|---|---|
| `database` | Database identity for search-index ownership. |
| `memories` | Current content, metadata, opaque concurrency token, and system timestamps. |
| `memory_types` | Stable type keys and readable names. |
| `tags` | Stable tag keys and readable names. |
| `reference_types` | Stable reference type keys and readable names. |
| `memory_tags` | Current memory tags. |
| `memory_references` | Current typed memory references. |
| `index_events` | Transactional search publication events. |
| `commits` | Append-only mutation audit entries. |
| `commit_changes` | Ordered relational entity changes belonging to a commit. |
| `commit_change_fields` | Ordered field names belonging to an entity change. |
| `log` | Immutable Work events, event time, task context, and trusted ingestion metadata. |

The `current_memories` view exposes current memory records. There are no author fields, authors table, revisions, deleted states, snapshots, or version counters. References use `target_key` and `reference_type`. The default memory type is `general`. Strict normalized tables retain current content and associations. Referenced taxonomy entries cannot be deleted.

## Metadata and Concurrency

| Field | Meaning |
|---|---|
| `summary` | Optional memory content summary, returned intact. |
| `basis` | `stated`, `observed`, `inferred`, or `assumed`. |
| `scope` | Project or environment label. |
| `observed_at` | Observation timestamp; defaults to current time on creation. |
| `verified_at` | Verification timestamp; defaults to current time on creation. |
| `review_after` | Optional review timestamp. |

Dates use signed integer UTC milliseconds since Unix epoch. Explicit null remains valid for nullable metadata. Updates preserve omitted values; explicit null clears them. Ordinary edits do not refresh verification time.

Each memory has an opaque `system_token` that changes on mutation. Existing-record writes require the last-read token as `expected_token`. Tokens detect conflicts and prevent stale updates or deletions. Tokens do not identify historical records. Writes atomically update current content, associations, audit entries, and search publication events. No-op writes create no audit entry.

Deletion permanently removes the memory and incoming and outgoing references. Surviving memories whose current references change receive fresh concurrency tokens. There is no restoration or separate purge operation. Deletion does not rewrite free text or promise forensic erasure from storage media or external backups.

## Mutation Audit

Every mutation envelope requires a nonempty `commit_summary`; the CLI equivalent is `--message`. This audit message is distinct from optional memory `summary`. Missing or blank messages reject the entire mutation before durable changes. Failed or rolled-back writes create no commit.

Each `commits` row includes a monotonic integer `id`, unique stable `key`, required `summary`, `operation`, trusted `actor`, and system creation and update timestamps. Ordered changes live in `commit_changes`, and ordered changed-field names live in `commit_change_fields`. Audit reads reconstruct the existing JSON `changes` response; no JSON change collections are persisted. Change descriptions identify affected entities and fields without storing previous bodies, record snapshots, or concurrency-token history. Database triggers reject audit-row updates and deletion in all three audit tables. Initialization seeds the database identity, default type, and an initialization audit entry. Audit reads use `list_commits` and `get_commits` or CLI `commits` and `commit`.

## Retrieval and Publication

Search, browsing, reads, and mutation responses return complete current bodies and stored summaries. The accepted `summary` view is a compatibility alias for full records. There are no generated excerpts. Historical search, timestamp snapshots, revision reads, and deleted-state queries are unavailable. Pagination and batch sizes are caller controlled; the service adds no token budgets, storage quotas, retention limits, or arbitrary request ceilings.

SQLite uses WAL with `synchronous=FULL`. A single writer coordinates transactions with a reusable read pool. Transactional index events identify changed memory keys after updates and deletion. Successful writes are acknowledged after index publication. Startup replays pending events or rebuilds an unusable index from current canonical records. SQLite validates indexed search candidates before returning results.

Publication failures report committed state and affected keys. Callers inspect current records before retrying. Index generation is independent of the database schema identifier. Necessary library representation limits, transaction coordination, and index-maintenance resources do not establish application-level data or request quotas.

## Work Events

`log` stores UUID `id`, `level`, `source`, `message`, event `timestamp`, nullable `task_id` and `scope`, and trusted `system_created_by` and `system_created_at`. The event timestamp is stored as integer UTC milliseconds and exchanged as millisecond ISO 8601 at fixed `+11:00`. The ingestion timestamp remains UTC milliseconds. Source and message are nonblank single-line text. Levels are `debug`, `info`, `warning`, and `error`. IDs use canonical lowercase hyphenated UUID notation. Database triggers prohibit event updates and deletion.

`append_logs` accepts a required nonblank caller-written `summary` and an `events` array. Each request is atomic. Identical event UUIDs and payloads from the same actor replay without inserting rows; conflicting content or actors reject that request. Duplicate UUIDs within one request are rejected. Mixed new and replayed events create one audit commit covering only new rows; pure replays create none. Receipts contain `ids`, `inserted`, `replayed`, and nullable `commit_id`, without echoing messages.

Adjacent logging requests share a durable SQLite transaction through the single writer. Per-request savepoints isolate validation failures. Responses follow the shared commit. Logging bypasses Tantivy and search publication. Reads use the existing reusable connection pool. There are no event mutation tokens, full-text search, migrations, automatic resets, or filesystem log imports.

`get_logs` returns `events` in requested UUID order and fails if any requested ID is absent. `list_logs` returns `events` and `next_cursor`, ordered by `(timestamp, id)` ascending. Optional exact filters are `task_id`, `scope`, `source`, and `level`; inclusive `since` and `until` use the event timestamp format. The default page size is 20, and callers can choose any positive SQLite-representable size. Follow cursors with unchanged filters. Concurrent inserts after the cursor are eligible for later pages; events inserted before it require a new browse. Browsing is not a frozen snapshot.

## Normalization

The persisted schema is in third normal form under its domain dependencies. Entity IDs and unique stable keys determine entity attributes. Junction keys determine relationship audit metadata. A commit and change ordinal determine one change; the full field key determines one ordered field name. Event UUIDs determine complete immutable work events. Actor labels, scope strings, fixed enums, and opaque Markdown content carry no separately stored dependent attributes and need no lookup tables.

Typed change columns retain event-time audit descriptions even after referenced entities are deleted, so entity keys are intentionally not foreign keys to live records. The `has_fields` flag preserves the distinction between omitted fields and an explicit empty field list in the existing API. Views and the rebuildable Tantivy copy are projections rather than additional canonical facts. Immutable child facts inherit lifecycle metadata through their parent commit; log rows have no update lifecycle.
