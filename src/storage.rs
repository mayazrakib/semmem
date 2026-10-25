use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior, params, params_from_iter,
};

use crate::models::{
    AppError, AppResult, AuthorRecord, CreateMemory, GetMemories, HistoryRequest, IndexEvent,
    ListMemories, MemoryMetadata, MemoryRecord, MemoryReference, MemoryVersion, RestoreMemory,
    TaxonomyInput, TaxonomyRecord, UpdateMemory,
};

const MEMORY_COLUMNS: &str = "id, key, title, body, type, author, system_version, system_created_at, system_updated_at, system_deleted_at, summary, basis, scope, observed_at, verified_at, review_after, system_created_by, system_updated_by, system_deleted_by";
const REVISION_SELECT: &str = "SELECT memories.id, memories.key, revisions.title, revisions.body, types.key, authors.name, revisions.system_version, memories.system_created_at, revisions.system_updated_at, revisions.system_deleted_at, revisions.summary, revisions.basis, revisions.scope, revisions.observed_at, revisions.verified_at, revisions.review_after, memories.system_created_by, revisions.system_updated_by, revisions.system_deleted_by FROM memory_revisions AS revisions INNER JOIN memories ON memories.id = revisions.memory_id INNER JOIN memory_types AS types ON types.id = revisions.type_id INNER JOIN authors ON authors.id = revisions.author";

pub struct Store {
    connection: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> AppResult<Self> {
        let mut connection = Connection::open(path)?;
        connection.busy_handler(Some(wait_for_database))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        validate_schema_version(version)?;
        if version == 1 {
            validate_schema_shape(&connection)?;
        }
        if version == 0 && connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%')", [], |row| row.get::<_, bool>(0))? {
            return Err(unsupported_schema(version));
        }
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL; PRAGMA temp_store = MEMORY;")?;

        if version == 0 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let locked_version: i64 =
                transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            validate_schema_version(locked_version)?;
            if locked_version == 1 {
                validate_schema_shape(&transaction)?;
            }

            if locked_version == 0 {
                transaction.execute_batch(include_str!("../initialize.sql"))?;
                let now = current_time()?;
                transaction.execute("INSERT INTO database (id, key, author, system_created_by, system_updated_by, system_created_at, system_updated_at) VALUES (1, ?1, 1, 'system', 'system', ?2, ?2)", params![uuid::Uuid::now_v7().to_string(), now])?;
            }

            transaction.commit()?;
        }

        Ok(Self { connection })
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> AppResult<Self> {
        let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_handler(Some(wait_for_database))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;

        if version != 1 {
            return Err(unsupported_schema(version));
        }

        validate_schema_shape(&connection)?;
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA temp_store = MEMORY;")?;

        Ok(Self { connection })
    }

    pub fn database_key(&self) -> AppResult<String> {
        self.connection
            .query_row("SELECT key FROM database WHERE id = 1", [], |row| {
                row.get(0)
            })
            .map_err(Into::into)
    }

    pub fn get_memory(&self, key: &str, include_deleted: bool) -> AppResult<MemoryRecord> {
        let snapshot = self.connection.unchecked_transaction()?;
        let record = read_memory(&snapshot, key, include_deleted)?;
        snapshot.commit()?;

        Ok(record)
    }

    pub fn get_revision(&self, key: &str, version: i64) -> AppResult<MemoryRecord> {
        let snapshot = self.connection.unchecked_transaction()?;
        let record = read_revision(&snapshot, key, version)?;
        snapshot.commit()?;

        Ok(record)
    }

    pub fn get_memories(&self, request: &GetMemories) -> AppResult<Vec<MemoryRecord>> {
        self.read_memories(request, false)
    }

    pub fn get_memory_summaries(&self, request: &GetMemories) -> AppResult<Vec<serde_json::Value>> {
        compact_records(self.read_memories(request, false)?)
    }

    fn read_memories(
        &self,
        request: &GetMemories,
        _is_summary: bool,
    ) -> AppResult<Vec<MemoryRecord>> {
        if request.keys.is_empty() {
            return Err(AppError::invalid("Get requires at least one key."));
        }

        let snapshot = self.connection.unchecked_transaction()?;
        let mut values = vec![rusqlite::types::Value::from(serde_json::to_string(
            &request.keys,
        )?)];
        let columns = MEMORY_COLUMNS;
        let revision_select = REVISION_SELECT;
        let sql = if let Some(version) = request.version {
            values.push(version.into());

            format!(
                "{revision_select} WHERE memories.key IN (SELECT value FROM json_each(?)) AND revisions.system_version = ?"
            )
        } else {
            values.push(i64::from(request.include_deleted).into());

            format!(
                "SELECT {columns} FROM current_memories WHERE key IN (SELECT value FROM json_each(?)) AND (? OR system_deleted_at IS NULL)"
            )
        };
        let mut statement = snapshot.prepare_cached(&sql)?;
        let mut selected = statement
            .query_map(params_from_iter(values), read_record)?
            .collect::<Result<Vec<_>, _>>()?;
        read_record_attachments(&snapshot, &mut selected)?;
        let by_key: BTreeMap<&str, &MemoryRecord> = selected
            .iter()
            .map(|record| (record.key.as_str(), record))
            .collect();
        let records = request
            .keys
            .iter()
            .map(|key| {
                by_key
                    .get(key.as_str())
                    .map(|record| (*record).clone())
                    .ok_or_else(|| {
                        AppError::new(
                            "not_found",
                            format!("Memory '{key}' or its requested revision was not found."),
                        )
                    })
            })
            .collect::<AppResult<Vec<_>>>()?;
        drop(statement);
        snapshot.commit()?;

        Ok(records)
    }

    pub fn create_memories(
        &mut self,
        inputs: &[CreateMemory],
        writer: &str,
    ) -> AppResult<Vec<MemoryRecord>> {
        validate_label(writer)?;
        validate_batch(inputs.len())?;
        validate_unique_keys(inputs.iter().map(|input| input.key.as_str()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = current_time()?;
        let mut identities = Vec::with_capacity(inputs.len());

        for input in inputs {
            validate_key(&input.key)?;
            validate_body(&input.body)?;
            let author = ensure_author(&transaction, &input.author, writer, now)?;
            let inserted = transaction.prepare_cached("INSERT INTO memories (key, author, system_created_by, system_updated_by, system_created_at, system_updated_at, system_version) VALUES (?1, ?3, ?4, ?4, ?2, ?2, 1) ON CONFLICT (key) DO NOTHING")?.execute(params![input.key, now, author, writer])?;

            if inserted == 0 {
                let existing = read_memory(&transaction, &input.key, true)?;

                if !matches_create(&existing, input) {
                    return Err(AppError::new(
                        "already_exists",
                        format!(
                            "Memory '{}' already exists with different content.",
                            input.key
                        ),
                    ));
                }

                identities.push(None);

                continue;
            }

            identities.push(Some(transaction.last_insert_rowid()));
        }

        for (input, id) in inputs.iter().zip(identities) {
            if let Some(id) = id {
                insert_revision(
                    &transaction,
                    id,
                    1,
                    &input.title,
                    &input.body,
                    &create_metadata(input, now),
                    &input.r#type,
                    &input.tags,
                    &input.references,
                    &input.author,
                    writer,
                    "create",
                    None,
                    now,
                )?;
                add_index_event(&transaction, &input.key, 1, "upsert", now, writer)?;
            }
        }

        let records = inputs
            .iter()
            .map(|input| read_memory(&transaction, &input.key, false))
            .collect::<AppResult<Vec<_>>>()?;
        transaction.commit()?;

        Ok(records)
    }

    pub fn update_memories(
        &mut self,
        inputs: &[UpdateMemory],
        writer: &str,
    ) -> AppResult<Vec<MemoryRecord>> {
        validate_label(writer)?;
        validate_batch(inputs.len())?;
        validate_unique_keys(inputs.iter().map(|input| input.key.as_str()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = current_time()?;
        let mut records = Vec::with_capacity(inputs.len());

        for input in inputs {
            let current = read_memory(&transaction, &input.key, false)?;
            validate_version(&current, input.expected_version)?;
            let title = input.title.as_deref().unwrap_or(&current.title);
            let body = input.body.as_deref().unwrap_or(&current.body);
            let memory_type = input.r#type.as_deref().unwrap_or(&current.r#type);
            let tags = input.tags.as_deref().unwrap_or(&current.tags);
            let references = input.references.as_deref().unwrap_or(&current.references);
            validate_body(body)?;
            let version = next_version(current.system_version)?;
            let metadata = input.metadata.apply(&current.metadata);
            insert_revision(
                &transaction,
                current.id,
                version,
                title,
                body,
                &metadata,
                memory_type,
                tags,
                references,
                input.author.as_deref().unwrap_or(&current.author),
                writer,
                "update",
                None,
                now,
            )?;
            advance_memory(&transaction, &current, version, None, now, writer)?;
            add_index_event(&transaction, &input.key, version, "upsert", now, writer)?;
            records.push(read_memory(&transaction, &input.key, false)?);
        }

        transaction.commit()?;

        Ok(records)
    }

    pub fn delete_memories(
        &mut self,
        inputs: &[MemoryVersion],
        writer: &str,
    ) -> AppResult<Vec<MemoryRecord>> {
        validate_label(writer)?;
        validate_batch(inputs.len())?;
        validate_unique_keys(inputs.iter().map(|input| input.key.as_str()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = current_time()?;
        let mut records = Vec::with_capacity(inputs.len());

        for input in inputs {
            let current = read_memory(&transaction, &input.key, false)?;
            validate_version(&current, input.expected_version)?;
            let version = next_version(current.system_version)?;
            insert_revision(
                &transaction,
                current.id,
                version,
                &current.title,
                &current.body,
                &current.metadata,
                &current.r#type,
                &current.tags,
                &current.references,
                &current.author,
                writer,
                "delete",
                Some(now),
                now,
            )?;
            advance_memory(&transaction, &current, version, Some(now), now, writer)?;
            add_index_event(&transaction, &input.key, version, "delete", now, writer)?;
            records.push(read_memory(&transaction, &input.key, true)?);
        }

        transaction.commit()?;

        Ok(records)
    }

    pub fn restore_memories(
        &mut self,
        inputs: &[RestoreMemory],
        writer: &str,
    ) -> AppResult<Vec<MemoryRecord>> {
        validate_label(writer)?;
        validate_batch(inputs.len())?;
        validate_unique_keys(inputs.iter().map(|input| input.key.as_str()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = current_time()?;
        let mut records = Vec::with_capacity(inputs.len());

        for input in inputs {
            let current = read_memory(&transaction, &input.key, true)?;
            validate_version(&current, input.expected_version)?;
            let selected = match input.revision {
                Some(version) => read_revision(&transaction, &input.key, version)?,
                None if current.system_deleted_at.is_some() => current.clone(),
                None => {
                    return Err(AppError::invalid(
                        "Specify a revision when restoring an active memory.",
                    ));
                }
            };
            let version = next_version(current.system_version)?;
            insert_revision(
                &transaction,
                current.id,
                version,
                &selected.title,
                &selected.body,
                &selected.metadata,
                &selected.r#type,
                &selected.tags,
                &selected.references,
                &selected.author,
                writer,
                "restore",
                None,
                now,
            )?;
            advance_memory(&transaction, &current, version, None, now, writer)?;
            add_index_event(&transaction, &input.key, version, "upsert", now, writer)?;
            records.push(read_memory(&transaction, &input.key, false)?);
        }

        transaction.commit()?;

        Ok(records)
    }

    pub fn purge_memories(
        &mut self,
        inputs: &[MemoryVersion],
        writer: &str,
    ) -> AppResult<Vec<String>> {
        validate_label(writer)?;
        validate_batch(inputs.len())?;
        validate_unique_keys(inputs.iter().map(|input| input.key.as_str()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = current_time()?;
        let records = inputs
            .iter()
            .map(|input| {
                let record = read_memory(&transaction, &input.key, true)?;
                validate_version(&record, input.expected_version)?;

                Ok(record)
            })
            .collect::<AppResult<Vec<_>>>()?;
        let selected_ids: BTreeSet<i64> = records.iter().map(|record| record.id).collect();
        let mut affected_sources = BTreeSet::new();
        let mut affected_history = BTreeSet::new();

        for record in &records {
            let mut statement = transaction.prepare_cached("SELECT DISTINCT source.id, source.key, source.system_version = revisions.system_version FROM memory_revision_references AS outgoing INNER JOIN memory_revisions AS revisions ON revisions.id = outgoing.revision_id INNER JOIN memories AS source ON source.id = revisions.memory_id WHERE outgoing.target_memory_id = ?1")?;
            let sources = statement
                .query_map([record.id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;

            for (id, key, is_current) in sources {
                if !selected_ids.contains(&id) {
                    affected_history.insert(key.clone());

                    if is_current {
                        affected_sources.insert(key);
                    }
                }
            }
        }

        for record in &records {
            transaction.execute(
                "DELETE FROM memory_revision_references WHERE target_memory_id = ?1",
                [record.id],
            )?;
            transaction.execute("DELETE FROM memory_revision_references WHERE revision_id IN (SELECT id FROM memory_revisions WHERE memory_id = ?1)", [record.id])?;
        }

        for record in &records {
            transaction.execute("DELETE FROM memories WHERE id = ?1", [record.id])?;
            add_index_event(
                &transaction,
                &record.key,
                record.system_version,
                "delete",
                now,
                writer,
            )?;
        }

        for key in &affected_sources {
            let current = read_memory(&transaction, key, true)?;
            let version = next_version(current.system_version)?;
            insert_revision(
                &transaction,
                current.id,
                version,
                &current.title,
                &current.body,
                &current.metadata,
                &current.r#type,
                &current.tags,
                &current.references,
                &current.author,
                writer,
                "update",
                current.system_deleted_at,
                now,
            )?;
            advance_memory(
                &transaction,
                &current,
                version,
                current.system_deleted_at,
                now,
                writer,
            )?;
            let operation = if current.system_deleted_at.is_some() {
                "delete"
            } else {
                "upsert"
            };
            add_index_event(&transaction, key, version, operation, now, writer)?;
        }

        for key in affected_history.difference(&affected_sources) {
            let current = read_memory(&transaction, key, true)?;
            let operation = if current.system_deleted_at.is_some() {
                "delete"
            } else {
                "upsert"
            };
            add_index_event(
                &transaction,
                key,
                current.system_version,
                operation,
                now,
                writer,
            )?;
        }

        transaction.commit()?;

        Ok(records.into_iter().map(|record| record.key).collect())
    }

    pub fn list_memories(&self, request: &ListMemories) -> AppResult<Vec<MemoryRecord>> {
        self.read_list(request, false)
    }

    pub fn list_memory_summaries(
        &self,
        request: &ListMemories,
    ) -> AppResult<Vec<serde_json::Value>> {
        compact_records(self.read_list(request, false)?)
    }

    fn read_list(&self, request: &ListMemories, _is_summary: bool) -> AppResult<Vec<MemoryRecord>> {
        if request.limit == 0 {
            return Err(AppError::invalid("Limit must be positive."));
        }

        let snapshot = self.connection.unchecked_transaction()?;
        let columns = MEMORY_COLUMNS;
        let mut sql = format!("SELECT {columns} FROM current_memories WHERE 1 = 1");
        let mut values: Vec<rusqlite::types::Value> = Vec::new();

        if !request.include_deleted {
            sql.push_str(" AND system_deleted_at IS NULL");
        }

        if let Some(memory_type) = &request.r#type {
            sql.push_str(" AND type = ?");
            values.push(memory_type.clone().into());
        }

        if !request.tags.is_empty() {
            sql.push_str(" AND NOT EXISTS (SELECT 1 FROM json_each(?) AS requested WHERE NOT EXISTS (SELECT 1 FROM memory_revision_tags AS membership INNER JOIN tags ON tags.id = membership.tag_id WHERE membership.revision_id = current_memories.revision_id AND tags.key = requested.value))");
            values.push(serde_json::to_string(&request.tags)?.into());
        }

        if let Some(scope) = &request.scope {
            sql.push_str(" AND scope = ?");
            values.push(scope.clone().into());
        }

        if let Some(basis) = request.basis {
            sql.push_str(" AND basis = ?");
            let value = serde_json::to_value(basis)?;
            values.push(
                value
                    .as_str()
                    .expect("Basis values serialize as text")
                    .to_owned()
                    .into(),
            );
        }

        if let Some(target) = &request.referenced_to {
            sql.push_str(" AND EXISTS (SELECT 1 FROM memory_revision_references AS outgoing INNER JOIN memories AS target ON target.id = outgoing.target_memory_id WHERE outgoing.revision_id = current_memories.revision_id AND target.key = ?)");
            values.push(target.clone().into());
        }

        if let Some(source) = &request.referenced_from {
            sql.push_str(" AND EXISTS (SELECT 1 FROM memory_revision_references AS outgoing INNER JOIN memory_revisions AS revisions ON revisions.id = outgoing.revision_id INNER JOIN memories AS source ON source.id = revisions.memory_id AND source.system_version = revisions.system_version WHERE outgoing.target_memory_id = current_memories.id AND source.key = ? AND source.system_deleted_at IS NULL)");
            values.push(source.clone().into());
        }

        if let Some(cursor) = &request.cursor {
            let (timestamp, id) = parse_cursor(cursor)?;
            sql.push_str(" AND (system_updated_at, id) < (?, ?)");
            values.push(timestamp.into());
            values.push(id.into());
        }

        sql.push_str(" ORDER BY system_updated_at DESC, id DESC LIMIT ?");
        values.push((sqlite_limit(request.limit)?).into());
        let mut statement = snapshot.prepare_cached(&sql)?;
        let mut records = statement
            .query_map(params_from_iter(values), read_record)?
            .collect::<Result<Vec<_>, _>>()?;
        read_record_attachments(&snapshot, &mut records)?;
        drop(statement);
        snapshot.commit()?;

        Ok(records)
    }

    pub fn list_revisions(&self, request: &HistoryRequest) -> AppResult<Vec<MemoryRecord>> {
        self.read_revisions(request, false)
    }

    pub fn list_revision_summaries(
        &self,
        request: &HistoryRequest,
    ) -> AppResult<Vec<serde_json::Value>> {
        compact_records(self.read_revisions(request, false)?)
    }

    fn read_revisions(
        &self,
        request: &HistoryRequest,
        _is_summary: bool,
    ) -> AppResult<Vec<MemoryRecord>> {
        if request.limit == 0 {
            return Err(AppError::invalid("Limit must be positive."));
        }

        let snapshot = self.connection.unchecked_transaction()?;
        let exists: bool = snapshot
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM memories WHERE key = ?1)")?
            .query_row([&request.key], |row| row.get(0))?;

        if !exists {
            return Err(AppError::new(
                "not_found",
                format!("Memory '{}' was not found.", request.key),
            ));
        }

        let revision_select = REVISION_SELECT;
        let sql = format!(
            "{revision_select} WHERE memories.key = ?1 AND (?2 IS NULL OR revisions.system_version < ?2) ORDER BY revisions.system_version DESC LIMIT ?3"
        );
        let mut statement = snapshot.prepare_cached(&sql)?;
        let mut records = statement
            .query_map(
                params![
                    request.key,
                    request.before_version,
                    sqlite_limit(request.limit)?
                ],
                read_record,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        read_record_attachments(&snapshot, &mut records)?;
        drop(statement);
        snapshot.commit()?;

        Ok(records)
    }

    pub fn all_current(&self, after_id: i64, limit: usize) -> AppResult<Vec<MemoryRecord>> {
        let snapshot = self.connection.unchecked_transaction()?;
        let sql = format!(
            "SELECT {MEMORY_COLUMNS} FROM current_memories WHERE id > ?1 AND system_deleted_at IS NULL ORDER BY id LIMIT ?2"
        );
        let mut statement = snapshot.prepare_cached(&sql)?;
        let mut records = statement
            .query_map(params![after_id, sqlite_limit(limit)?], read_record)?
            .collect::<Result<Vec<_>, _>>()?;
        read_record_attachments(&snapshot, &mut records)?;
        drop(statement);
        snapshot.commit()?;

        Ok(records)
    }

    pub fn all_memory_keys(&self, after_id: i64, limit: usize) -> AppResult<Vec<(i64, String)>> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT id, key FROM memories WHERE id > ?1 ORDER BY id LIMIT ?2")?;
        let keys = statement
            .query_map(params![after_id, sqlite_limit(limit)?], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(keys)
    }

    pub fn revisions_for_key(&self, key: &str) -> AppResult<Vec<MemoryRecord>> {
        let snapshot = self.connection.unchecked_transaction()?;
        let mut statement = snapshot.prepare_cached(&format!(
            "{REVISION_SELECT} WHERE memories.key = ?1 ORDER BY revisions.system_version"
        ))?;
        let mut records = statement
            .query_map([key], read_record)?
            .collect::<Result<Vec<_>, _>>()?;
        read_record_attachments(&snapshot, &mut records)?;
        drop(statement);
        snapshot.commit()?;

        Ok(records)
    }

    pub fn index_events(&self, after: i64, limit: usize) -> AppResult<Vec<IndexEvent>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id, memory_key FROM index_events WHERE id > ?1 ORDER BY id LIMIT ?2",
        )?;
        let events = statement
            .query_map(params![after, sqlite_limit(limit)?], |row| {
                Ok(IndexEvent {
                    sequence: row.get(0)?,
                    memory_key: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(events)
    }

    pub fn acknowledge_events(&mut self, watermark: i64) -> AppResult<()> {
        self.connection
            .execute("DELETE FROM index_events WHERE id <= ?1", [watermark])?;

        Ok(())
    }

    pub fn list_authors(&self) -> AppResult<Vec<AuthorRecord>> {
        let mut statement = self.connection.prepare_cached("SELECT name, system_created_at, system_updated_at, system_created_by, system_updated_by FROM authors WHERE id != 1 ORDER BY name")?;
        let authors = statement
            .query_map([], read_author)?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(authors)
    }

    pub fn register_author(&mut self, author: &str, writer: &str) -> AppResult<AuthorRecord> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_author(&transaction, author, writer, current_time()?)?;
        let record = transaction.query_row("SELECT name, system_created_at, system_updated_at, system_created_by, system_updated_by FROM authors WHERE name = ?1", [author], read_author)?;
        transaction.commit()?;

        Ok(record)
    }

    pub fn deregister_author(&mut self, author: &str, writer: &str) -> AppResult<()> {
        validate_author(author)?;
        validate_label(writer)?;
        let changed = self
            .connection
            .execute("DELETE FROM authors WHERE name = ?1", [author])
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(ref detail, _) if detail.extended_code == 787 => {
                    AppError::new(
                        "referenced_author",
                        "Author is referenced by canonical records.",
                    )
                }
                _ => error.into(),
            })?;

        if changed == 0 {
            return Err(AppError::new("not_found", "Author was not found."));
        }

        Ok(())
    }

    pub fn list_taxonomy(&self, taxonomy: &str) -> AppResult<Vec<TaxonomyRecord>> {
        let table = taxonomy_table(taxonomy)?;
        let mut statement = self.connection.prepare_cached(&format!("SELECT items.id, items.key, items.name, items.system_created_at, items.system_updated_at, authors.name, items.system_created_by, items.system_updated_by FROM {table} AS items INNER JOIN authors ON authors.id = items.author ORDER BY items.key"))?;
        let records = statement
            .query_map([], read_taxonomy)?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(records)
    }

    pub fn create_taxonomy(
        &mut self,
        taxonomy: &str,
        input: &TaxonomyInput,
        writer: &str,
    ) -> AppResult<TaxonomyRecord> {
        let table = taxonomy_table(taxonomy)?;
        validate_key(&input.key)?;
        validate_label(&input.name)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = current_time()?;
        let author = ensure_author(&transaction, &input.author, writer, now)?;
        transaction.execute(&format!("INSERT INTO {table} (key, name, author, system_created_by, system_updated_by, system_created_at, system_updated_at) VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?5)"), params![input.key, input.name, author, writer, now])?;
        let record = read_taxonomy_by_key(&transaction, table, &input.key)?;
        transaction.commit()?;

        Ok(record)
    }

    pub fn update_taxonomy(
        &mut self,
        taxonomy: &str,
        input: &TaxonomyInput,
        writer: &str,
    ) -> AppResult<TaxonomyRecord> {
        let table = taxonomy_table(taxonomy)?;
        validate_label(&input.name)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = current_time()?;
        let author = ensure_author(&transaction, &input.author, writer, now)?;
        let changed = transaction.execute(&format!("UPDATE {table} SET name = ?2, author = ?3, system_updated_by = ?4, system_updated_at = ?5 WHERE key = ?1"), params![input.key, input.name, author, writer, now])?;

        if changed == 0 {
            return Err(AppError::new("not_found", "Taxonomy item was not found."));
        }

        let record = read_taxonomy_by_key(&transaction, table, &input.key)?;
        transaction.commit()?;

        Ok(record)
    }

    pub fn delete_taxonomy(&mut self, taxonomy: &str, key: &str, writer: &str) -> AppResult<()> {
        let table = taxonomy_table(taxonomy)?;
        validate_label(writer)?;
        if table == "memory_types" && key == "general" {
            return Err(AppError::new(
                "default_type",
                "The general memory type is required by the database default.",
            ));
        }

        let changed = self
            .connection
            .execute(&format!("DELETE FROM {table} WHERE key = ?1"), [key])
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(ref detail, _) if detail.extended_code == 787 => {
                    AppError::new(
                        "referenced_taxonomy",
                        "Taxonomy item is referenced by memory history.",
                    )
                }
                _ => error.into(),
            })?;

        if changed == 0 {
            return Err(AppError::new("not_found", "Taxonomy item was not found."));
        }

        Ok(())
    }
}

fn read_record(row: &Row<'_>) -> rusqlite::Result<MemoryRecord> {
    Ok(MemoryRecord {
        id: row.get(0)?,
        key: row.get(1)?,
        title: row.get(2)?,
        body: row.get(3)?,
        r#type: row.get(4)?,
        tags: Vec::new(),
        references: Vec::new(),
        author: row.get(5)?,
        system_created_by: row.get(16)?,
        system_updated_by: row.get(17)?,
        system_deleted_by: row.get(18)?,
        system_version: row.get(6)?,
        system_created_at: row.get(7)?,
        system_updated_at: row.get(8)?,
        system_deleted_at: row.get(9)?,
        metadata: MemoryMetadata {
            summary: row.get(10)?,
            basis: row
                .get::<_, Option<String>>(11)?
                .map(|value| serde_json::from_value(serde_json::Value::String(value)))
                .transpose()
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        11,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
            scope: row.get(12)?,
            observed_at: row.get(13)?,
            verified_at: row.get(14)?,
            review_after: row.get(15)?,
        },
    })
}

fn read_memory(
    connection: &Connection,
    key: &str,
    include_deleted: bool,
) -> AppResult<MemoryRecord> {
    let sql = format!(
        "SELECT {MEMORY_COLUMNS} FROM current_memories WHERE key = ?1 AND (?2 OR system_deleted_at IS NULL)"
    );
    let mut record = connection
        .prepare_cached(&sql)?
        .query_row(params![key, include_deleted], read_record)
        .optional()?
        .ok_or_else(|| AppError::new("not_found", format!("Memory '{key}' was not found.")))?;
    read_attachments(connection, &mut record)?;

    Ok(record)
}

pub(crate) fn read_revision(
    connection: &Connection,
    key: &str,
    version: i64,
) -> AppResult<MemoryRecord> {
    let sql =
        format!("{REVISION_SELECT} WHERE memories.key = ?1 AND revisions.system_version = ?2");
    let mut record = connection
        .prepare_cached(&sql)?
        .query_row(params![key, version], read_record)
        .optional()?
        .ok_or_else(|| {
            AppError::new(
                "not_found",
                format!("Revision {version} of memory '{key}' was not found."),
            )
        })?;
    read_attachments(connection, &mut record)?;

    Ok(record)
}

fn read_attachments(connection: &Connection, record: &mut MemoryRecord) -> AppResult<()> {
    let mut tags = connection.prepare_cached("SELECT tags.key FROM memory_revision_tags AS membership INNER JOIN tags ON tags.id = membership.tag_id INNER JOIN memory_revisions AS revisions ON revisions.id = membership.revision_id WHERE revisions.memory_id = ?1 AND revisions.system_version = ?2 ORDER BY tags.key")?;
    record.tags = tags
        .query_map(params![record.id, record.system_version], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut references = connection.prepare_cached("SELECT target.key, relations.key FROM memory_revision_references AS outgoing INNER JOIN memories AS target ON target.id = outgoing.target_memory_id INNER JOIN reference_types AS relations ON relations.id = outgoing.reference_type_id INNER JOIN memory_revisions AS revisions ON revisions.id = outgoing.revision_id WHERE revisions.memory_id = ?1 AND revisions.system_version = ?2 ORDER BY relations.key, target.key")?;
    record.references = references
        .query_map(params![record.id, record.system_version], |row| {
            Ok(MemoryReference {
                target_key: row.get(0)?,
                reference_type: row.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(())
}

fn read_record_attachments(connection: &Connection, records: &mut [MemoryRecord]) -> AppResult<()> {
    if records.is_empty() {
        return Ok(());
    }

    let positions: BTreeMap<(i64, i64), usize> = records
        .iter()
        .enumerate()
        .map(|(index, record)| ((record.id, record.system_version), index))
        .collect();
    let identities = records
        .iter()
        .map(|record| [record.id, record.system_version])
        .collect::<Vec<_>>();
    let values = serde_json::to_string(&identities)?;
    let requested = "WITH requested (memory_id, system_version) AS (SELECT json_extract(value, '$[0]'), json_extract(value, '$[1]') FROM json_each(?1)) ";
    let tags_sql = format!(
        "{requested} SELECT requested.memory_id, requested.system_version, tags.key FROM requested INNER JOIN memory_revisions AS revisions ON revisions.memory_id = requested.memory_id AND revisions.system_version = requested.system_version INNER JOIN memory_revision_tags AS membership ON membership.revision_id = revisions.id INNER JOIN tags ON tags.id = membership.tag_id ORDER BY requested.memory_id, requested.system_version, tags.key"
    );
    let mut tags = connection.prepare_cached(&tags_sql)?;
    let mut rows = tags.query([&values])?;

    while let Some(row) = rows.next()? {
        let identity = (row.get::<_, i64>(0)?, row.get::<_, i64>(1)?);
        records[positions[&identity]].tags.push(row.get(2)?);
    }

    let references_sql = format!(
        "{requested} SELECT requested.memory_id, requested.system_version, target.key, relations.key FROM requested INNER JOIN memory_revisions AS revisions ON revisions.memory_id = requested.memory_id AND revisions.system_version = requested.system_version INNER JOIN memory_revision_references AS outgoing ON outgoing.revision_id = revisions.id INNER JOIN memories AS target ON target.id = outgoing.target_memory_id INNER JOIN reference_types AS relations ON relations.id = outgoing.reference_type_id ORDER BY requested.memory_id, requested.system_version, relations.key, target.key"
    );
    let mut references = connection.prepare_cached(&references_sql)?;
    let mut rows = references.query([&values])?;

    while let Some(row) = rows.next()? {
        let identity = (row.get::<_, i64>(0)?, row.get::<_, i64>(1)?);
        records[positions[&identity]]
            .references
            .push(MemoryReference {
                target_key: row.get(2)?,
                reference_type: row.get(3)?,
            });
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_revision(
    connection: &Connection,
    memory_id: i64,
    version: i64,
    title: &str,
    body: &str,
    metadata: &MemoryMetadata,
    memory_type: &str,
    tags: &[String],
    references: &[MemoryReference],
    author: &str,
    writer: &str,
    operation: &str,
    deleted_at: Option<i64>,
    now: i64,
) -> AppResult<()> {
    let author_id = ensure_author(connection, author, writer, now)?;
    let type_id = ensure_taxonomy(
        connection,
        "memory_types",
        memory_type,
        author_id,
        writer,
        now,
    )?;
    let basis = metadata
        .basis
        .map(serde_json::to_value)
        .transpose()?
        .and_then(|value| value.as_str().map(str::to_owned));
    let deleted_by: Option<String> = if deleted_at.is_none() {
        None
    } else if operation == "delete" {
        Some(writer.into())
    } else {
        connection.query_row(
            "SELECT system_deleted_by FROM memories WHERE id = ?1",
            [memory_id],
            |row| row.get(0),
        )?
    };
    connection.prepare_cached("INSERT INTO memory_revisions (memory_id, title, body, type_id, author, summary, basis, scope, observed_at, verified_at, review_after, system_version, system_operation, system_created_at, system_updated_at, system_deleted_at, system_created_by, system_updated_by, system_deleted_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14, ?15, ?16, ?16, ?17)")?.execute(params![memory_id, title, body, type_id, author_id, metadata.summary, basis, metadata.scope, metadata.observed_at, metadata.verified_at, metadata.review_after, version, operation, now, deleted_at, writer, deleted_by])?;
    let revision_id = connection.last_insert_rowid();

    for tag in tags.iter().collect::<BTreeSet<_>>() {
        let tag_id = ensure_taxonomy(connection, "tags", tag, author_id, writer, now)?;
        connection.prepare_cached("INSERT INTO memory_revision_tags (revision_id, tag_id, author, system_created_by, system_updated_by, system_created_at, system_updated_at) VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?5)")?.execute(params![revision_id, tag_id, author_id, writer, now])?;
    }

    let mut seen_references = BTreeSet::new();

    for reference in references {
        if !seen_references.insert((&reference.target_key, &reference.reference_type)) {
            continue;
        }

        let target_id: i64 = connection
            .query_row(
                "SELECT id FROM memories WHERE key = ?1",
                [&reference.target_key],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                AppError::new(
                    "not_found",
                    format!("Reference target '{}' was not found.", reference.target_key),
                )
            })?;
        let reference_type_id = ensure_taxonomy(
            connection,
            "reference_types",
            &reference.reference_type,
            author_id,
            writer,
            now,
        )?;
        connection.prepare_cached("INSERT INTO memory_revision_references (revision_id, target_memory_id, reference_type_id, author, system_created_by, system_updated_by, system_created_at, system_updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?6)")?.execute(params![revision_id, target_id, reference_type_id, author_id, writer, now])?;
    }

    Ok(())
}

fn ensure_taxonomy(
    connection: &Connection,
    table: &str,
    key: &str,
    author: i64,
    writer: &str,
    now: i64,
) -> AppResult<i64> {
    validate_key(key)?;
    connection.prepare_cached(&format!("INSERT INTO {table} (key, name, author, system_created_by, system_updated_by, system_created_at, system_updated_at) VALUES (?1, ?1, ?2, ?3, ?3, ?4, ?4) ON CONFLICT (key) DO NOTHING"))?.execute(params![key, author, writer, now])?;

    connection
        .prepare_cached(&format!("SELECT id FROM {table} WHERE key = ?1"))?
        .query_row([key], |row| row.get(0))
        .map_err(Into::into)
}

fn ensure_author(connection: &Connection, author: &str, writer: &str, now: i64) -> AppResult<i64> {
    validate_author(author)?;
    validate_label(writer)?;
    connection.prepare_cached("INSERT INTO authors (name, author, system_created_by, system_updated_by, system_created_at, system_updated_at) VALUES (?1, 1, ?2, ?2, ?3, ?3) ON CONFLICT (name) DO NOTHING")?.execute(params![author, writer, now])?;

    connection
        .prepare_cached("SELECT id FROM authors WHERE name = ?1")?
        .query_row([author], |row| row.get(0))
        .map_err(Into::into)
}

fn validate_author(author: &str) -> AppResult<()> {
    validate_label(author)?;

    if author == "system" {
        return Err(AppError::invalid(
            "The system author is reserved for internal records.",
        ));
    }

    Ok(())
}

fn create_metadata(input: &CreateMemory, now: i64) -> MemoryMetadata {
    input.metadata.apply(&MemoryMetadata {
        observed_at: Some(now),
        verified_at: Some(now),
        ..Default::default()
    })
}

fn advance_memory(
    connection: &Connection,
    current: &MemoryRecord,
    version: i64,
    deleted_at: Option<i64>,
    now: i64,
    writer: &str,
) -> AppResult<()> {
    let deleted_by = if deleted_at.is_none() {
        None
    } else {
        current.system_deleted_by.as_deref().or(Some(writer))
    };
    let changed = connection.prepare_cached("UPDATE memories SET system_version = ?2, system_updated_at = ?3, system_deleted_at = ?4, system_updated_by = ?6, system_deleted_by = ?7, author = (SELECT author FROM memory_revisions WHERE memory_id = ?1 AND system_version = ?2) WHERE id = ?1 AND system_version = ?5")?.execute(params![current.id, version, now, deleted_at, current.system_version, writer, deleted_by])?;

    if changed != 1 {
        return Err(AppError::new(
            "version_conflict",
            "Memory changed while saving.",
        ));
    }

    Ok(())
}

fn add_index_event(
    connection: &Connection,
    key: &str,
    version: i64,
    operation: &str,
    now: i64,
    writer: &str,
) -> AppResult<()> {
    connection.prepare_cached("INSERT INTO index_events (memory_key, memory_version, operation, author, system_created_by, system_updated_by, system_created_at, system_updated_at) VALUES (?1, ?2, ?3, COALESCE((SELECT author FROM memories WHERE key = ?1), 1), ?5, ?5, ?4, ?4)")?.execute(params![key, version, operation, now, writer])?;

    Ok(())
}

fn read_taxonomy(row: &Row<'_>) -> rusqlite::Result<TaxonomyRecord> {
    Ok(TaxonomyRecord {
        id: row.get(0)?,
        key: row.get(1)?,
        name: row.get(2)?,
        system_created_at: row.get(3)?,
        system_updated_at: row.get(4)?,
        author: row.get(5)?,
        system_created_by: row.get(6)?,
        system_updated_by: row.get(7)?,
    })
}

fn taxonomy_table(taxonomy: &str) -> AppResult<&'static str> {
    match taxonomy {
        "type" | "types" | "memory_types" => Ok("memory_types"),
        "tag" | "tags" => Ok("tags"),
        "reference_type" | "reference_types" => Ok("reference_types"),
        _ => Err(AppError::invalid(
            "Taxonomy must be types, tags, or reference_types.",
        )),
    }
}

fn matches_create(record: &MemoryRecord, input: &CreateMemory) -> bool {
    let tags: BTreeSet<&String> = record.tags.iter().collect();
    let requested_tags: BTreeSet<&String> = input.tags.iter().collect();
    let references: BTreeSet<(&String, &String)> = record
        .references
        .iter()
        .map(|link| (&link.target_key, &link.reference_type))
        .collect();
    let requested_references: BTreeSet<(&String, &String)> = input
        .references
        .iter()
        .map(|link| (&link.target_key, &link.reference_type))
        .collect();

    record.system_deleted_at.is_none()
        && record.title == input.title
        && record.body == input.body
        && record.metadata == create_metadata(input, record.system_created_at)
        && record.r#type == input.r#type
        && record.author == input.author
        && tags == requested_tags
        && references == requested_references
}

fn validate_batch(length: usize) -> AppResult<()> {
    if length == 0 {
        return Err(AppError::invalid("A batch must contain at least one item."));
    }

    Ok(())
}

fn validate_unique_keys<'a>(keys: impl Iterator<Item = &'a str>) -> AppResult<()> {
    let mut seen = BTreeSet::new();

    for key in keys {
        if !seen.insert(key) {
            return Err(AppError::invalid(
                "A batch cannot mutate the same memory more than once.",
            ));
        }
    }

    Ok(())
}

fn validate_key(key: &str) -> AppResult<()> {
    if key.is_empty() || key.trim() != key || key.chars().any(char::is_control) {
        return Err(AppError::invalid(
            "Keys must be nonempty, trimmed text without control characters.",
        ));
    }

    Ok(())
}

fn validate_label(label: &str) -> AppResult<()> {
    if label.trim().is_empty() {
        return Err(AppError::invalid(
            "Names and author labels must not be empty.",
        ));
    }

    Ok(())
}

fn validate_body(body: &str) -> AppResult<()> {
    if body.trim().is_empty() {
        return Err(AppError::invalid("Memory body must not be empty."));
    }

    Ok(())
}

fn validate_version(memory: &MemoryRecord, expected: i64) -> AppResult<()> {
    if memory.system_version != expected {
        return Err(AppError::new(
            "version_conflict",
            format!(
                "Memory '{}' is at version {}, expected {expected}.",
                memory.key, memory.system_version
            ),
        ));
    }

    Ok(())
}

fn parse_cursor(cursor: &str) -> AppResult<(i64, i64)> {
    let (timestamp, id) = cursor
        .split_once(':')
        .ok_or_else(|| AppError::invalid("Invalid list cursor."))?;

    Ok((
        timestamp
            .parse()
            .map_err(|_| AppError::invalid("Invalid list cursor."))?,
        id.parse()
            .map_err(|_| AppError::invalid("Invalid list cursor."))?,
    ))
}

fn current_time() -> AppResult<i64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| AppError::new("clock_failure", error.to_string()))?;

    i64::try_from(elapsed.as_millis())
        .map_err(|_| AppError::new("clock_failure", "System time is out of range."))
}

fn next_version(version: i64) -> AppResult<i64> {
    version
        .checked_add(1)
        .ok_or_else(|| AppError::new("version_exhausted", "Memory version limit was reached."))
}

fn wait_for_database(_: i32) -> bool {
    std::thread::yield_now();

    true
}

fn unsupported_schema(version: i64) -> AppError {
    AppError::new(
        "unsupported_schema",
        format!(
            "Database schema version {version} is unsupported. This build requires a fresh database initialized at version 1."
        ),
    )
}

fn validate_schema_version(version: i64) -> AppResult<()> {
    if version != 0 && version != 1 {
        return Err(unsupported_schema(version));
    }

    Ok(())
}

fn sqlite_limit(limit: usize) -> AppResult<i64> {
    i64::try_from(limit)
        .map_err(|_| AppError::invalid("The requested count cannot be represented by SQLite."))
}

fn compact_records(records: Vec<MemoryRecord>) -> AppResult<Vec<serde_json::Value>> {
    records
        .into_iter()
        .map(|record| serde_json::to_value(record).map_err(Into::into))
        .collect()
}

fn read_author(row: &Row<'_>) -> rusqlite::Result<AuthorRecord> {
    Ok(AuthorRecord {
        author: row.get(0)?,
        system_created_at: row.get(1)?,
        system_updated_at: row.get(2)?,
        system_created_by: row.get(3)?,
        system_updated_by: row.get(4)?,
    })
}

fn read_taxonomy_by_key(
    connection: &Connection,
    table: &str,
    key: &str,
) -> AppResult<TaxonomyRecord> {
    connection.query_row(&format!("SELECT items.id, items.key, items.name, items.system_created_at, items.system_updated_at, authors.name, items.system_created_by, items.system_updated_by FROM {table} AS items INNER JOIN authors ON authors.id = items.author WHERE items.key = ?1"), [key], read_taxonomy).map_err(Into::into)
}

fn validate_schema_shape(connection: &Connection) -> AppResult<()> {
    let expected = Connection::open_in_memory()?;
    expected.execute_batch(include_str!("../initialize.sql"))?;

    fn read_schema(connection: &Connection) -> rusqlite::Result<Vec<(String, String, String)>> {
        let mut statement = connection.prepare("SELECT type, name, sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name")?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect()
    }

    if read_schema(connection)? != read_schema(&expected)? {
        return Err(unsupported_schema(1));
    }

    Ok(())
}
