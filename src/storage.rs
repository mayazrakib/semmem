use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior, params, params_from_iter,
};

use crate::models::{
    AppError, AppResult, CreateMemory, GetMemories, HistoryRequest, IndexEvent, ListMemories,
    MemoryLink, MemoryMetadata, MemoryRecord, MemoryVersion, RestoreMemory, TaxonomyInput,
    TaxonomyRecord, UpdateMemory,
};

const MEMORY_COLUMNS: &str = "id, key, title, body, kind, actor, system_version, system_created_at, system_updated_at, system_deleted_at, summary, claim_type, scope, observed_at, verified_at, review_after";
const REVISION_SELECT: &str = "SELECT memories.id, memories.key, revisions.title, revisions.body, kinds.key, actors.name, revisions.system_version, memories.system_created_at, revisions.system_updated_at, revisions.system_deleted_at, revisions.summary, revisions.claim_type, revisions.scope, revisions.observed_at, revisions.verified_at, revisions.review_after FROM memory_revisions AS revisions INNER JOIN memories ON memories.id = revisions.memory_id INNER JOIN memory_kinds AS kinds ON kinds.id = revisions.kind_id INNER JOIN actors ON actors.id = revisions.actor_id";

pub struct Store {
    connection: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> AppResult<Self> {
        let mut connection = Connection::open(path)?;
        connection.busy_handler(Some(wait_for_database))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        validate_schema_version(version)?;
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

            if locked_version == 0 {
                transaction.execute_batch(include_str!("../initialize.sql"))?;
                let now = current_time()?;
                transaction.execute("INSERT INTO database_identity (id, key, system_created_at, system_updated_at) VALUES (1, ?1, ?2, ?2)", params![uuid::Uuid::now_v7().to_string(), now])?;
            }

            transaction.commit()?;
        }

        register_text_functions(&connection)?;

        Ok(Self { connection })
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> AppResult<Self> {
        let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_handler(Some(wait_for_database))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;

        if version != 3 {
            return Err(unsupported_schema(version));
        }

        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA temp_store = MEMORY;")?;

        register_text_functions(&connection)?;

        Ok(Self { connection })
    }

    pub fn database_key(&self) -> AppResult<String> {
        self.connection
            .query_row(
                "SELECT key FROM database_identity WHERE id = 1",
                [],
                |row| row.get(0),
            )
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
        compact_records(self.read_memories(request, true)?)
    }

    fn read_memories(
        &self,
        request: &GetMemories,
        is_summary: bool,
    ) -> AppResult<Vec<MemoryRecord>> {
        if request.keys.is_empty() {
            return Err(AppError::invalid("Get requires at least one key."));
        }

        let snapshot = self.connection.unchecked_transaction()?;
        let mut values = vec![rusqlite::types::Value::from(serde_json::to_string(
            &request.keys,
        )?)];
        let columns = memory_columns(is_summary);
        let revision_select = revision_select(is_summary);
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
        if !is_summary {
            read_record_attachments(&snapshot, &mut selected)?;
        }
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

    pub fn create_memories(&mut self, inputs: &[CreateMemory]) -> AppResult<Vec<MemoryRecord>> {
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
            let inserted = transaction.prepare_cached("INSERT INTO memories (key, system_created_at, system_updated_at, system_version) VALUES (?1, ?2, ?2, 1) ON CONFLICT (key) DO NOTHING")?.execute(params![input.key, now])?;

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
                    &input.metadata,
                    &input.kind,
                    &input.tags,
                    &input.links,
                    &input.actor,
                    "create",
                    None,
                    now,
                )?;
                add_index_event(&transaction, &input.key, 1, "upsert", now)?;
            }
        }

        let records = inputs
            .iter()
            .map(|input| read_memory(&transaction, &input.key, false))
            .collect::<AppResult<Vec<_>>>()?;
        transaction.commit()?;

        Ok(records)
    }

    pub fn update_memories(&mut self, inputs: &[UpdateMemory]) -> AppResult<Vec<MemoryRecord>> {
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
            let kind = input.kind.as_deref().unwrap_or(&current.kind);
            let tags = input.tags.as_deref().unwrap_or(&current.tags);
            let links = input.links.as_deref().unwrap_or(&current.links);
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
                kind,
                tags,
                links,
                &input.actor,
                "update",
                None,
                now,
            )?;
            advance_memory(&transaction, &current, version, None, now)?;
            add_index_event(&transaction, &input.key, version, "upsert", now)?;
            records.push(read_memory(&transaction, &input.key, false)?);
        }

        transaction.commit()?;

        Ok(records)
    }

    pub fn delete_memories(
        &mut self,
        inputs: &[MemoryVersion],
        actor: &str,
    ) -> AppResult<Vec<MemoryRecord>> {
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
                &current.kind,
                &current.tags,
                &current.links,
                actor,
                "delete",
                Some(now),
                now,
            )?;
            advance_memory(&transaction, &current, version, Some(now), now)?;
            add_index_event(&transaction, &input.key, version, "delete", now)?;
            records.push(read_memory(&transaction, &input.key, true)?);
        }

        transaction.commit()?;

        Ok(records)
    }

    pub fn restore_memories(&mut self, inputs: &[RestoreMemory]) -> AppResult<Vec<MemoryRecord>> {
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
                &selected.kind,
                &selected.tags,
                &selected.links,
                &input.actor,
                "restore",
                None,
                now,
            )?;
            advance_memory(&transaction, &current, version, None, now)?;
            add_index_event(&transaction, &input.key, version, "upsert", now)?;
            records.push(read_memory(&transaction, &input.key, false)?);
        }

        transaction.commit()?;

        Ok(records)
    }

    pub fn purge_memories(&mut self, inputs: &[MemoryVersion]) -> AppResult<Vec<String>> {
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

        for record in &records {
            let mut statement = transaction.prepare_cached("SELECT DISTINCT source.id, source.key FROM memory_revision_links AS links INNER JOIN memory_revisions AS revisions ON revisions.id = links.revision_id INNER JOIN memories AS source ON source.id = revisions.memory_id AND source.system_version = revisions.system_version WHERE links.target_memory_id = ?1")?;
            let sources = statement
                .query_map([record.id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;

            for (id, key) in sources {
                if !selected_ids.contains(&id) {
                    affected_sources.insert(key);
                }
            }
        }

        for record in &records {
            transaction.execute(
                "DELETE FROM memory_revision_links WHERE target_memory_id = ?1",
                [record.id],
            )?;
            transaction.execute("DELETE FROM memory_revision_links WHERE revision_id IN (SELECT id FROM memory_revisions WHERE memory_id = ?1)", [record.id])?;
        }

        for record in &records {
            transaction.execute("DELETE FROM memories WHERE id = ?1", [record.id])?;
            add_index_event(
                &transaction,
                &record.key,
                record.system_version,
                "delete",
                now,
            )?;
        }

        for key in affected_sources {
            let current = read_memory(&transaction, &key, true)?;
            let version = next_version(current.system_version)?;
            insert_revision(
                &transaction,
                current.id,
                version,
                &current.title,
                &current.body,
                &current.metadata,
                &current.kind,
                &current.tags,
                &current.links,
                "system",
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
            )?;
            let operation = if current.system_deleted_at.is_some() {
                "delete"
            } else {
                "upsert"
            };
            add_index_event(&transaction, &key, version, operation, now)?;
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
        compact_records(self.read_list(request, true)?)
    }

    fn read_list(&self, request: &ListMemories, is_summary: bool) -> AppResult<Vec<MemoryRecord>> {
        if request.limit == 0 {
            return Err(AppError::invalid("Limit must be positive."));
        }

        let snapshot = self.connection.unchecked_transaction()?;
        let columns = memory_columns(is_summary);
        let mut sql = format!("SELECT {columns} FROM current_memories WHERE 1 = 1");
        let mut values: Vec<rusqlite::types::Value> = Vec::new();

        if !request.include_deleted {
            sql.push_str(" AND system_deleted_at IS NULL");
        }

        if let Some(kind) = &request.kind {
            sql.push_str(" AND kind = ?");
            values.push(kind.clone().into());
        }

        if !request.tags.is_empty() {
            sql.push_str(" AND NOT EXISTS (SELECT 1 FROM json_each(?) AS requested WHERE NOT EXISTS (SELECT 1 FROM memory_revision_tags AS membership INNER JOIN tags ON tags.id = membership.tag_id WHERE membership.revision_id = current_memories.revision_id AND tags.key = requested.value))");
            values.push(serde_json::to_string(&request.tags)?.into());
        }

        if let Some(scope) = &request.scope {
            sql.push_str(" AND scope = ?");
            values.push(scope.clone().into());
        }

        if let Some(claim_type) = request.claim_type {
            sql.push_str(" AND claim_type = ?");
            let value = serde_json::to_value(claim_type)?;
            values.push(
                value
                    .as_str()
                    .expect("Claim types serialize as text")
                    .to_owned()
                    .into(),
            );
        }

        if let Some(target) = &request.linked_to {
            sql.push_str(" AND EXISTS (SELECT 1 FROM memory_revision_links AS links INNER JOIN memories AS target ON target.id = links.target_memory_id WHERE links.revision_id = current_memories.revision_id AND target.key = ?)");
            values.push(target.clone().into());
        }

        if let Some(source) = &request.linked_from {
            sql.push_str(" AND EXISTS (SELECT 1 FROM memory_revision_links AS links INNER JOIN memory_revisions AS revisions ON revisions.id = links.revision_id INNER JOIN memories AS source ON source.id = revisions.memory_id AND source.system_version = revisions.system_version WHERE links.target_memory_id = current_memories.id AND source.key = ? AND source.system_deleted_at IS NULL)");
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
        if !is_summary {
            read_record_attachments(&snapshot, &mut records)?;
        }
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
        compact_records(self.read_revisions(request, true)?)
    }

    fn read_revisions(
        &self,
        request: &HistoryRequest,
        is_summary: bool,
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

        let revision_select = revision_select(is_summary);
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
        if !is_summary {
            read_record_attachments(&snapshot, &mut records)?;
        }
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

    pub fn list_taxonomy(&self, taxonomy: &str) -> AppResult<Vec<TaxonomyRecord>> {
        let table = taxonomy_table(taxonomy)?;
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT id, key, name, system_created_at, system_updated_at FROM {table} ORDER BY key"
        ))?;
        let records = statement
            .query_map([], read_taxonomy)?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(records)
    }

    pub fn create_taxonomy(
        &mut self,
        taxonomy: &str,
        input: &TaxonomyInput,
    ) -> AppResult<TaxonomyRecord> {
        let table = taxonomy_table(taxonomy)?;
        validate_key(&input.key)?;
        validate_label(&input.name)?;
        let now = current_time()?;
        let sql = format!(
            "INSERT INTO {table} (key, name, system_created_at, system_updated_at) VALUES (?1, ?2, ?3, ?3) RETURNING id, key, name, system_created_at, system_updated_at"
        );

        self.connection
            .query_row(&sql, params![input.key, input.name, now], read_taxonomy)
            .map_err(Into::into)
    }

    pub fn update_taxonomy(
        &mut self,
        taxonomy: &str,
        input: &TaxonomyInput,
    ) -> AppResult<TaxonomyRecord> {
        let table = taxonomy_table(taxonomy)?;
        validate_label(&input.name)?;
        let sql = format!(
            "UPDATE {table} SET name = ?2, system_updated_at = ?3 WHERE key = ?1 RETURNING id, key, name, system_created_at, system_updated_at"
        );

        self.connection
            .query_row(
                &sql,
                params![input.key, input.name, current_time()?],
                read_taxonomy,
            )
            .optional()?
            .ok_or_else(|| AppError::new("not_found", "Taxonomy item was not found."))
    }

    pub fn delete_taxonomy(&mut self, taxonomy: &str, key: &str) -> AppResult<()> {
        let table = taxonomy_table(taxonomy)?;
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
        kind: row.get(4)?,
        tags: Vec::new(),
        links: Vec::new(),
        actor: row.get(5)?,
        system_version: row.get(6)?,
        system_created_at: row.get(7)?,
        system_updated_at: row.get(8)?,
        system_deleted_at: row.get(9)?,
        metadata: MemoryMetadata {
            summary: row.get(10)?,
            claim_type: row
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

fn read_revision(connection: &Connection, key: &str, version: i64) -> AppResult<MemoryRecord> {
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
    let mut links = connection.prepare_cached("SELECT target.key, relations.key FROM memory_revision_links AS links INNER JOIN memories AS target ON target.id = links.target_memory_id INNER JOIN relation_types AS relations ON relations.id = links.relation_type_id INNER JOIN memory_revisions AS revisions ON revisions.id = links.revision_id WHERE revisions.memory_id = ?1 AND revisions.system_version = ?2 ORDER BY relations.key, target.key")?;
    record.links = links
        .query_map(params![record.id, record.system_version], |row| {
            Ok(MemoryLink {
                target_key: row.get(0)?,
                relation: row.get(1)?,
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

    let links_sql = format!(
        "{requested} SELECT requested.memory_id, requested.system_version, target.key, relations.key FROM requested INNER JOIN memory_revisions AS revisions ON revisions.memory_id = requested.memory_id AND revisions.system_version = requested.system_version INNER JOIN memory_revision_links AS links ON links.revision_id = revisions.id INNER JOIN memories AS target ON target.id = links.target_memory_id INNER JOIN relation_types AS relations ON relations.id = links.relation_type_id ORDER BY requested.memory_id, requested.system_version, relations.key, target.key"
    );
    let mut links = connection.prepare_cached(&links_sql)?;
    let mut rows = links.query([&values])?;

    while let Some(row) = rows.next()? {
        let identity = (row.get::<_, i64>(0)?, row.get::<_, i64>(1)?);
        records[positions[&identity]].links.push(MemoryLink {
            target_key: row.get(2)?,
            relation: row.get(3)?,
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
    kind: &str,
    tags: &[String],
    links: &[MemoryLink],
    actor: &str,
    operation: &str,
    deleted_at: Option<i64>,
    now: i64,
) -> AppResult<()> {
    let kind_id = ensure_taxonomy(connection, "memory_kinds", kind, now)?;
    validate_label(actor)?;
    connection.prepare_cached("INSERT INTO actors (name, system_created_at, system_updated_at) VALUES (?1, ?2, ?2) ON CONFLICT (name) DO NOTHING")?.execute(params![actor, now])?;
    let actor_id: i64 = connection
        .prepare_cached("SELECT id FROM actors WHERE name = ?1")?
        .query_row([actor], |row| row.get(0))?;
    let claim_type = metadata
        .claim_type
        .map(serde_json::to_value)
        .transpose()?
        .and_then(|value| value.as_str().map(str::to_owned));
    connection.prepare_cached("INSERT INTO memory_revisions (memory_id, title, body, kind_id, actor_id, summary, claim_type, scope, observed_at, verified_at, review_after, system_version, system_operation, system_created_at, system_updated_at, system_deleted_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14, ?15)")?.execute(params![memory_id, title, body, kind_id, actor_id, metadata.summary, claim_type, metadata.scope, metadata.observed_at, metadata.verified_at, metadata.review_after, version, operation, now, deleted_at])?;
    let revision_id = connection.last_insert_rowid();

    for tag in tags.iter().collect::<BTreeSet<_>>() {
        let tag_id = ensure_taxonomy(connection, "tags", tag, now)?;
        connection.prepare_cached("INSERT INTO memory_revision_tags (revision_id, tag_id, system_created_at, system_updated_at) VALUES (?1, ?2, ?3, ?3)")?.execute(params![revision_id, tag_id, now])?;
    }

    let mut seen_links = BTreeSet::new();

    for link in links {
        if !seen_links.insert((&link.target_key, &link.relation)) {
            continue;
        }

        let target_id: i64 = connection
            .query_row(
                "SELECT id FROM memories WHERE key = ?1",
                [&link.target_key],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                AppError::new(
                    "not_found",
                    format!("Link target '{}' was not found.", link.target_key),
                )
            })?;
        let relation_id = ensure_taxonomy(connection, "relation_types", &link.relation, now)?;
        connection.prepare_cached("INSERT INTO memory_revision_links (revision_id, target_memory_id, relation_type_id, system_created_at, system_updated_at) VALUES (?1, ?2, ?3, ?4, ?4)")?.execute(params![revision_id, target_id, relation_id, now])?;
    }

    Ok(())
}

fn ensure_taxonomy(connection: &Connection, table: &str, key: &str, now: i64) -> AppResult<i64> {
    validate_key(key)?;
    connection.prepare_cached(&format!("INSERT INTO {table} (key, name, system_created_at, system_updated_at) VALUES (?1, ?1, ?2, ?2) ON CONFLICT (key) DO NOTHING"))?.execute(params![key, now])?;

    connection
        .prepare_cached(&format!("SELECT id FROM {table} WHERE key = ?1"))?
        .query_row([key], |row| row.get(0))
        .map_err(Into::into)
}

fn advance_memory(
    connection: &Connection,
    current: &MemoryRecord,
    version: i64,
    deleted_at: Option<i64>,
    now: i64,
) -> AppResult<()> {
    let changed = connection.prepare_cached("UPDATE memories SET system_version = ?2, system_updated_at = ?3, system_deleted_at = ?4 WHERE id = ?1 AND system_version = ?5")?.execute(params![current.id, version, now, deleted_at, current.system_version])?;

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
) -> AppResult<()> {
    connection.prepare_cached("INSERT INTO index_events (memory_key, memory_version, operation, system_created_at, system_updated_at) VALUES (?1, ?2, ?3, ?4, ?4)")?.execute(params![key, version, operation, now])?;

    Ok(())
}

fn read_taxonomy(row: &Row<'_>) -> rusqlite::Result<TaxonomyRecord> {
    Ok(TaxonomyRecord {
        id: row.get(0)?,
        key: row.get(1)?,
        name: row.get(2)?,
        system_created_at: row.get(3)?,
        system_updated_at: row.get(4)?,
    })
}

fn taxonomy_table(taxonomy: &str) -> AppResult<&'static str> {
    match taxonomy {
        "kind" | "kinds" => Ok("memory_kinds"),
        "tag" | "tags" => Ok("tags"),
        "relation" | "relations" | "relation_types" => Ok("relation_types"),
        _ => Err(AppError::invalid(
            "Taxonomy must be kinds, tags, or relations.",
        )),
    }
}

fn matches_create(record: &MemoryRecord, input: &CreateMemory) -> bool {
    let tags: BTreeSet<&String> = record.tags.iter().collect();
    let requested_tags: BTreeSet<&String> = input.tags.iter().collect();
    let links: BTreeSet<(&String, &String)> = record
        .links
        .iter()
        .map(|link| (&link.target_key, &link.relation))
        .collect();
    let requested_links: BTreeSet<(&String, &String)> = input
        .links
        .iter()
        .map(|link| (&link.target_key, &link.relation))
        .collect();

    record.system_deleted_at.is_none()
        && record.title == input.title
        && record.body == input.body
        && record.metadata == input.metadata
        && record.kind == input.kind
        && record.actor == input.actor
        && tags == requested_tags
        && links == requested_links
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
            "Names and actor labels must not be empty.",
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
            "Database schema version {version} is unsupported. This build requires a fresh database initialized at version 3."
        ),
    )
}

fn validate_schema_version(version: i64) -> AppResult<()> {
    if version != 0 && version != 3 {
        return Err(unsupported_schema(version));
    }

    Ok(())
}

fn sqlite_limit(limit: usize) -> AppResult<i64> {
    i64::try_from(limit)
        .map_err(|_| AppError::invalid("The requested count cannot be represented by SQLite."))
}

fn register_text_functions(connection: &Connection) -> AppResult<()> {
    connection.create_scalar_function(
        "memory_excerpt",
        2,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8
            | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |context| {
            let summary: Option<String> = context.get(1)?;

            if summary
                .as_deref()
                .is_some_and(|summary| !summary.trim().is_empty())
            {
                return Ok(String::new());
            }

            let body = context.get_raw(0).as_str().map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;

            Ok(crate::models::markdown_paragraphs(body)
                .next()
                .unwrap_or("")
                .to_owned())
        },
    )?;

    Ok(())
}

fn excerpt_expression(body: &str, summary: &str) -> String {
    format!("memory_excerpt({body}, {summary})")
}

fn memory_columns(is_summary: bool) -> String {
    if is_summary {
        return MEMORY_COLUMNS.replacen("body", &excerpt_expression("body", "summary"), 1);
    }

    MEMORY_COLUMNS.to_owned()
}

fn revision_select(is_summary: bool) -> String {
    if is_summary {
        return REVISION_SELECT.replacen(
            "revisions.body",
            &excerpt_expression("revisions.body", "revisions.summary"),
            1,
        );
    }

    REVISION_SELECT.to_owned()
}

fn compact_records(records: Vec<MemoryRecord>) -> AppResult<Vec<serde_json::Value>> {
    records
        .into_iter()
        .map(|mut record| {
            let excerpt = std::mem::take(&mut record.body);
            let has_summary = record
                .metadata
                .summary
                .as_ref()
                .is_some_and(|summary| !summary.trim().is_empty());
            let mut value = serde_json::to_value(record)?;
            let fields = value
                .as_object_mut()
                .expect("A memory serializes as an object");
            fields.remove("body");
            fields.remove("tags");
            fields.remove("links");

            if !has_summary {
                fields.insert("excerpt".into(), excerpt.into());
            }

            Ok(value)
        })
        .collect()
}
