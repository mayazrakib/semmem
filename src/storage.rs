use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior, params, params_from_iter,
};
use serde_json::{Value, json};

use crate::models::{
    AppError, AppResult, CommitRecord, CreateMemory, GetCommits, GetMemories, IndexEvent,
    ListCommits, ListMemories, MemoryMetadata, MemoryRecord, MemoryReference, MemoryToken,
    TaxonomyInput, TaxonomyRecord, UpdateMemory,
};

const MEMORY_COLUMNS: &str = "id, key, title, body, type, system_token, system_created_at, system_updated_at, summary, basis, scope, observed_at, verified_at, review_after, system_created_by, system_updated_by";
const COMMIT_COLUMNS: &str =
    "id, key, summary, operation, actor, changes, system_created_at, system_updated_at";

pub struct Store {
    connection: Connection,
    last_write_commit: Option<i64>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> AppResult<Self> {
        let mut connection = Connection::open(path)?;
        connection.busy_handler(Some(wait_for_database))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        validate_schema_version(version)?;
        if version == 2 {
            validate_schema_shape(&connection)?;
        }
        if version == 0 && connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%')", [], |row| row.get::<_, bool>(0))? {
            return Err(unsupported_schema(version));
        }
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL; PRAGMA temp_store = MEMORY;")?;
        if version == 0 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let locked: i64 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            validate_schema_version(locked)?;
            if locked == 0 {
                transaction.execute_batch(include_str!("../database/initialize.sql"))?;
            } else {
                validate_schema_shape(&transaction)?;
            }
            transaction.commit()?;
        }
        Ok(Self {
            connection,
            last_write_commit: None,
        })
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> AppResult<Self> {
        let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_handler(Some(wait_for_database))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != 2 {
            return Err(unsupported_schema(version));
        }
        validate_schema_shape(&connection)?;
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA temp_store = MEMORY;")?;
        Ok(Self {
            connection,
            last_write_commit: None,
        })
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn database_key(&self) -> AppResult<String> {
        Ok(self
            .connection
            .query_row("SELECT key FROM database WHERE id = 1", [], |row| {
                row.get(0)
            })?)
    }

    pub fn get_memory(&self, key: &str) -> AppResult<MemoryRecord> {
        let transaction = self.connection.unchecked_transaction()?;
        let record = read_memory(&transaction, key)?;
        transaction.commit()?;
        Ok(record)
    }

    pub fn get_memories(&self, request: &GetMemories) -> AppResult<Vec<MemoryRecord>> {
        let transaction = self.connection.unchecked_transaction()?;
        let mut statement = transaction.prepare_cached(&format!("SELECT {MEMORY_COLUMNS} FROM current_memories WHERE key IN (SELECT value FROM json_each(?1))"))?;
        let mut records = statement
            .query_map([serde_json::to_string(&request.keys)?], read_record)?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        hydrate(&transaction, &mut records)?;
        let records = records
            .into_iter()
            .map(|record| (record.key.clone(), record))
            .collect::<BTreeMap<_, _>>();
        let ordered = request
            .keys
            .iter()
            .map(|key| records.get(key).cloned().ok_or_else(|| not_found(key)))
            .collect::<AppResult<Vec<_>>>()?;
        transaction.commit()?;
        Ok(ordered)
    }

    pub fn get_memory_summaries(&self, request: &GetMemories) -> AppResult<Vec<Value>> {
        serialize_records(self.get_memories(request)?)
    }

    pub fn create_memories(
        &mut self,
        inputs: &[CreateMemory],
        actor: &str,
        commit_summary: &str,
    ) -> AppResult<Vec<MemoryRecord>> {
        self.last_write_commit = None;
        validate_write(actor, commit_summary)?;
        validate_batch(inputs.iter().map(|input| input.key.as_str()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = current_time()?;
        let mut changes = Vec::new();
        let mut inserted = BTreeSet::new();
        for input in inputs {
            validate_key(&input.key)?;
            validate_body(&input.body)?;
            if let Some(existing) = transaction
                .query_row(
                    "SELECT id FROM memories WHERE key = ?1",
                    [&input.key],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
            {
                let current = read_memory(&transaction, &input.key)?;
                if current.id != existing || !matches_create(&current, input) {
                    return Err(AppError::new(
                        "already_exists",
                        format!(
                            "Memory '{}' already exists with different content.",
                            input.key
                        ),
                    ));
                }
                continue;
            }
            let type_id = ensure_taxonomy(
                &transaction,
                "memory_types",
                &input.r#type,
                actor,
                now,
                &mut changes,
            )?;
            let metadata = create_metadata(input, now);
            let basis = metadata.basis.map(|value| {
                serde_json::to_value(value)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            });
            transaction.execute("INSERT INTO memories (key, title, body, summary, basis, scope, observed_at, verified_at, review_after, type_id, system_token, system_created_by, system_updated_by, system_created_at, system_updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12, ?13, ?13)", params![input.key, input.title, input.body, metadata.summary, basis, metadata.scope, metadata.observed_at, metadata.verified_at, metadata.review_after, type_id, new_token(), actor, now])?;
            inserted.insert(input.key.clone());
        }
        for input in inputs {
            if !inserted.contains(&input.key) {
                continue;
            }
            let id: i64 = transaction.query_row(
                "SELECT id FROM memories WHERE key = ?1",
                [&input.key],
                |row| row.get(0),
            )?;
            replace_associations(
                &transaction,
                id,
                &input.tags,
                &input.references,
                actor,
                now,
                &mut changes,
            )?;
            add_index_event(&transaction, &input.key, "upsert", actor, now)?;
            changes.push(json!({"entity":"memories","key":input.key,"operation":"create","fields":["title","body","metadata","type","tags","references"],"index_event":"upsert"}));
        }
        let commit_id = if !changes.is_empty() {
            Some(append_commit(
                &transaction,
                actor,
                commit_summary,
                "create_memories",
                changes,
                now,
            )?)
        } else {
            None
        };
        let records = inputs
            .iter()
            .map(|input| read_memory(&transaction, &input.key))
            .collect::<AppResult<Vec<_>>>()?;
        transaction.commit()?;
        self.last_write_commit = commit_id;
        Ok(records)
    }

    pub fn update_memories(
        &mut self,
        inputs: &[UpdateMemory],
        actor: &str,
        commit_summary: &str,
    ) -> AppResult<Vec<MemoryRecord>> {
        self.last_write_commit = None;
        validate_write(actor, commit_summary)?;
        validate_batch(inputs.iter().map(|input| input.key.as_str()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = current_time()?;
        let mut changes = Vec::new();
        let mut records = Vec::new();
        for input in inputs {
            let current = read_memory(&transaction, &input.key)?;
            validate_token(&current, &input.expected_token)?;
            let title = input.title.as_deref().unwrap_or(&current.title);
            let body = input.body.as_deref().unwrap_or(&current.body);
            validate_body(body)?;
            let metadata = input.metadata.apply(&current.metadata);
            let basis = metadata.basis.map(|value| {
                serde_json::to_value(value)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            });
            let memory_type = input.r#type.as_deref().unwrap_or(&current.r#type);
            let tags = input.tags.as_deref().unwrap_or(&current.tags);
            let references = input.references.as_deref().unwrap_or(&current.references);
            if title == current.title
                && body == current.body
                && metadata == current.metadata
                && memory_type == current.r#type
                && same_associations(tags, references, &current)
            {
                records.push(current);
                continue;
            }
            let type_id = ensure_taxonomy(
                &transaction,
                "memory_types",
                memory_type,
                actor,
                now,
                &mut changes,
            )?;
            let changed = transaction.execute("UPDATE memories SET title = ?2, body = ?3, summary = ?4, basis = ?5, scope = ?6, observed_at = ?7, verified_at = ?8, review_after = ?9, type_id = ?10, system_token = ?11, system_updated_by = ?12, system_updated_at = ?13 WHERE id = ?1 AND system_token = ?14", params![current.id, title, body, metadata.summary, basis, metadata.scope, metadata.observed_at, metadata.verified_at, metadata.review_after, type_id, new_token(), actor, now, input.expected_token])?;
            if changed != 1 {
                return Err(conflict(&input.key));
            }
            replace_associations(
                &transaction,
                current.id,
                input.tags.as_deref().unwrap_or(&current.tags),
                input.references.as_deref().unwrap_or(&current.references),
                actor,
                now,
                &mut changes,
            )?;
            add_index_event(&transaction, &input.key, "upsert", actor, now)?;
            let updated = read_memory(&transaction, &input.key)?;
            let before = serde_json::to_value(&current)?;
            let after = serde_json::to_value(&updated)?;
            let fields = after
                .as_object()
                .unwrap()
                .iter()
                .filter(|(key, value)| {
                    !key.starts_with("system_") && before.get(*key) != Some(*value)
                })
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            changes.push(json!({"entity":"memories","key":input.key,"operation":"update","fields":fields,"index_event":"upsert"}));
            records.push(updated);
        }
        let commit_id = if changes.is_empty() {
            None
        } else {
            Some(append_commit(
                &transaction,
                actor,
                commit_summary,
                "update_memories",
                changes,
                now,
            )?)
        };
        transaction.commit()?;
        self.last_write_commit = commit_id;
        Ok(records)
    }

    pub fn delete_memories(
        &mut self,
        inputs: &[MemoryToken],
        actor: &str,
        commit_summary: &str,
    ) -> AppResult<Vec<MemoryRecord>> {
        self.last_write_commit = None;
        validate_write(actor, commit_summary)?;
        validate_batch(inputs.iter().map(|input| input.key.as_str()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = current_time()?;
        let records = inputs
            .iter()
            .map(|input| {
                let record = read_memory(&transaction, &input.key)?;
                validate_token(&record, &input.expected_token)?;
                Ok(record)
            })
            .collect::<AppResult<Vec<_>>>()?;
        let ids = records
            .iter()
            .map(|record| record.id)
            .collect::<BTreeSet<_>>();
        let mut surviving_sources = BTreeSet::new();
        let mut changes = Vec::new();
        for record in &records {
            let mut statement = transaction.prepare_cached(
                "SELECT DISTINCT memory_id FROM memory_references WHERE target_memory_id = ?1",
            )?;
            let sources = statement
                .query_map([record.id], |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            for source in sources {
                if !ids.contains(&source) {
                    surviving_sources.insert(source);
                }
            }
        }
        for record in &records {
            transaction.execute(
                "DELETE FROM memories WHERE id = ?1 AND system_token = ?2",
                params![record.id, record.system_token],
            )?;
            add_index_event(&transaction, &record.key, "delete", actor, now)?;
            changes.push(json!({"entity":"memories","key":record.key,"operation":"delete","associations":"remove outgoing tags and references, and incoming references","index_event":"delete"}));
        }
        for id in surviving_sources {
            transaction.execute("UPDATE memories SET system_token = ?2, system_updated_by = ?3, system_updated_at = ?4 WHERE id = ?1", params![id, new_token(), actor, now])?;
            let key: String =
                transaction.query_row("SELECT key FROM memories WHERE id = ?1", [id], |row| {
                    row.get(0)
                })?;
            add_index_event(&transaction, &key, "upsert", actor, now)?;
            changes.push(json!({"entity":"memories","key":key,"operation":"update","fields":["references"],"reason":"remove references to deleted targets","index_event":"upsert"}));
        }
        let commit_id = Some(append_commit(
            &transaction,
            actor,
            commit_summary,
            "delete_memories",
            changes,
            now,
        )?);
        transaction.commit()?;
        self.last_write_commit = commit_id;
        Ok(records)
    }

    pub fn list_memories(&self, request: &ListMemories) -> AppResult<Vec<MemoryRecord>> {
        if request.limit == 0 {
            return Err(AppError::invalid("Page size must be positive."));
        }
        let transaction = self.connection.unchecked_transaction()?;
        let mut sql = format!("SELECT {MEMORY_COLUMNS} FROM current_memories WHERE 1 = 1");
        let mut values = Vec::<rusqlite::types::Value>::new();
        if let Some(value) = &request.scope {
            sql.push_str(" AND scope = ?");
            values.push(value.clone().into());
        }
        if let Some(value) = request.basis {
            sql.push_str(" AND basis = ?");
            values.push(
                serde_json::to_value(value)?
                    .as_str()
                    .unwrap()
                    .to_owned()
                    .into(),
            );
        }
        if let Some(value) = &request.r#type {
            sql.push_str(" AND type = ?");
            values.push(value.clone().into());
        }
        if !request.tags.is_empty() {
            sql.push_str(" AND NOT EXISTS (SELECT 1 FROM json_each(?) AS requested WHERE NOT EXISTS (SELECT 1 FROM memory_tags AS membership INNER JOIN tags ON tags.id = membership.tag_id WHERE membership.memory_id = current_memories.id AND tags.key = requested.value))");
            values.push(serde_json::to_string(&request.tags)?.into());
        }
        if let Some(value) = &request.referenced_to {
            sql.push_str(" AND EXISTS (SELECT 1 FROM memory_references AS outgoing INNER JOIN memories AS target ON target.id = outgoing.target_memory_id WHERE outgoing.memory_id = current_memories.id AND target.key = ?)");
            values.push(value.clone().into());
        }
        if let Some(value) = &request.referenced_from {
            sql.push_str(" AND EXISTS (SELECT 1 FROM memory_references AS outgoing INNER JOIN memories AS source ON source.id = outgoing.memory_id WHERE outgoing.target_memory_id = current_memories.id AND source.key = ?)");
            values.push(value.clone().into());
        }
        if let Some(cursor) = &request.cursor {
            let (timestamp, id) = parse_cursor(cursor)?;
            sql.push_str(" AND (system_updated_at, id) < (?, ?)");
            values.push(timestamp.into());
            values.push(id.into());
        }
        sql.push_str(" ORDER BY system_updated_at DESC, id DESC LIMIT ?");
        values.push(sqlite_limit(request.limit)?.into());
        let mut statement = transaction.prepare_cached(&sql)?;
        let mut records = statement
            .query_map(params_from_iter(values), read_record)?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        hydrate(&transaction, &mut records)?;
        transaction.commit()?;
        Ok(records)
    }

    pub fn list_memory_summaries(&self, request: &ListMemories) -> AppResult<Vec<Value>> {
        serialize_records(self.list_memories(request)?)
    }

    pub fn current_batch(&self, after_id: i64, limit: usize) -> AppResult<Vec<MemoryRecord>> {
        let transaction = self.connection.unchecked_transaction()?;
        let mut statement = transaction.prepare_cached(&format!(
            "SELECT {MEMORY_COLUMNS} FROM current_memories WHERE id > ?1 ORDER BY id LIMIT ?2"
        ))?;
        let mut records = statement
            .query_map(params![after_id, sqlite_limit(limit)?], read_record)?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        hydrate(&transaction, &mut records)?;
        transaction.commit()?;
        Ok(records)
    }

    pub fn all_memory_keys(&self, after_id: i64, limit: usize) -> AppResult<Vec<(i64, String)>> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT id, key FROM memories WHERE id > ?1 ORDER BY id LIMIT ?2")?;
        Ok(statement
            .query_map(params![after_id, sqlite_limit(limit)?], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn index_events(&self, after: i64, limit: usize) -> AppResult<Vec<IndexEvent>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id, memory_key FROM index_events WHERE id > ?1 ORDER BY id LIMIT ?2",
        )?;
        Ok(statement
            .query_map(params![after, sqlite_limit(limit)?], |row| {
                Ok(IndexEvent {
                    sequence: row.get(0)?,
                    memory_key: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn acknowledge_events(&mut self, watermark: i64) -> AppResult<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed =
            transaction.execute("DELETE FROM index_events WHERE id <= ?1", [watermark])?;
        if changed != 0 {
            append_commit(
                &transaction,
                "system",
                "Acknowledge published search index events.",
                "acknowledge_index_events",
                vec![
                    json!({"entity":"index_events","operation":"delete","through_sequence":watermark,"count":changed}),
                ],
                current_time()?,
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn list_taxonomy(&self, taxonomy: &str) -> AppResult<Vec<TaxonomyRecord>> {
        let table = taxonomy_table(taxonomy)?;
        let mut statement = self.connection.prepare_cached(&format!("SELECT id, key, name, system_created_by, system_updated_by, system_created_at, system_updated_at FROM {table} ORDER BY key"))?;
        Ok(statement
            .query_map([], read_taxonomy)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn create_taxonomy(
        &mut self,
        taxonomy: &str,
        input: &TaxonomyInput,
        actor: &str,
        commit_summary: &str,
    ) -> AppResult<TaxonomyRecord> {
        self.last_write_commit = None;
        validate_write(actor, commit_summary)?;
        validate_key(&input.key)?;
        validate_label(&input.name)?;
        let table = taxonomy_table(taxonomy)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = current_time()?;
        transaction.execute(&format!("INSERT INTO {table} (key, name, system_created_by, system_updated_by, system_created_at, system_updated_at) VALUES (?1, ?2, ?3, ?3, ?4, ?4)"), params![input.key, input.name, actor, now])?;
        let commit_id = Some(append_commit(
            &transaction,
            actor,
            commit_summary,
            "create_taxonomy",
            vec![json!({"entity":table,"key":input.key,"operation":"create","fields":["name"]})],
            now,
        )?);
        let record = read_taxonomy_by_key(&transaction, table, &input.key)?;
        transaction.commit()?;
        self.last_write_commit = commit_id;
        Ok(record)
    }

    pub fn update_taxonomy(
        &mut self,
        taxonomy: &str,
        input: &TaxonomyInput,
        actor: &str,
        commit_summary: &str,
    ) -> AppResult<TaxonomyRecord> {
        self.last_write_commit = None;
        validate_write(actor, commit_summary)?;
        validate_label(&input.name)?;
        let table = taxonomy_table(taxonomy)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = current_time()?;
        let current = read_taxonomy_by_key(&transaction, table, &input.key).map_err(|error| {
            if error.code == "storage_failure" {
                not_found(&input.key)
            } else {
                error
            }
        })?;
        if current.name == input.name {
            transaction.commit()?;
            return Ok(current);
        }
        if transaction.execute(&format!("UPDATE {table} SET name = ?2, system_updated_by = ?3, system_updated_at = ?4 WHERE key = ?1"), params![input.key, input.name, actor, now])? == 0 {
            return Err(not_found(&input.key));
        }
        let commit_id = Some(append_commit(
            &transaction,
            actor,
            commit_summary,
            "update_taxonomy",
            vec![json!({"entity":table,"key":input.key,"operation":"update","fields":["name"]})],
            now,
        )?);
        let record = read_taxonomy_by_key(&transaction, table, &input.key)?;
        transaction.commit()?;
        self.last_write_commit = commit_id;
        Ok(record)
    }

    pub fn delete_taxonomy(
        &mut self,
        taxonomy: &str,
        key: &str,
        actor: &str,
        commit_summary: &str,
    ) -> AppResult<()> {
        self.last_write_commit = None;
        validate_write(actor, commit_summary)?;
        let table = taxonomy_table(taxonomy)?;
        if table == "memory_types" && key == "general" {
            return Err(AppError::new(
                "default_type",
                "The general memory type is required by the database default.",
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction
            .execute(&format!("DELETE FROM {table} WHERE key = ?1"), [key])
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(ref detail, _) if detail.extended_code == 787 => {
                    AppError::new(
                        "referenced_taxonomy",
                        "Taxonomy item is referenced by current memories.",
                    )
                }
                _ => error.into(),
            })?;
        if changed == 0 {
            return Err(not_found(key));
        }
        let commit_id = Some(append_commit(
            &transaction,
            actor,
            commit_summary,
            "delete_taxonomy",
            vec![json!({"entity":table,"key":key,"operation":"delete"})],
            current_time()?,
        )?);
        transaction.commit()?;
        self.last_write_commit = commit_id;
        Ok(())
    }

    pub fn last_commit_id(&self) -> AppResult<Option<i64>> {
        Ok(self.last_write_commit)
    }

    pub fn list_commits(&self, request: &ListCommits) -> AppResult<Vec<CommitRecord>> {
        if request.limit == 0 {
            return Err(AppError::invalid("Commit page size must be positive."));
        }
        let cursor = request
            .cursor
            .as_deref()
            .map(str::parse::<i64>)
            .transpose()
            .map_err(|_| AppError::invalid("Invalid commit cursor."))?;
        let mut statement = self.connection.prepare_cached(&format!("SELECT {COMMIT_COLUMNS} FROM commits WHERE (?1 IS NULL OR id < ?1) ORDER BY id DESC LIMIT ?2"))?;
        Ok(statement
            .query_map(params![cursor, sqlite_limit(request.limit)?], read_commit)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_commits(&self, request: &GetCommits) -> AppResult<Vec<CommitRecord>> {
        let transaction = self.connection.unchecked_transaction()?;
        let mut statement = transaction.prepare_cached(&format!(
            "SELECT {COMMIT_COLUMNS} FROM commits WHERE id IN (SELECT value FROM json_each(?1))"
        ))?;
        let records = statement
            .query_map([serde_json::to_string(&request.ids)?], read_commit)?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let records = records
            .into_iter()
            .map(|record| (record.id, record))
            .collect::<BTreeMap<_, _>>();
        let ordered = request
            .ids
            .iter()
            .map(|id| {
                records.get(id).cloned().ok_or_else(|| {
                    AppError::new("not_found", format!("Commit {id} was not found."))
                })
            })
            .collect::<AppResult<Vec<_>>>()?;
        transaction.commit()?;
        Ok(ordered)
    }
}

fn read_record(row: &Row<'_>) -> rusqlite::Result<MemoryRecord> {
    let basis: Option<String> = row.get(9)?;
    Ok(MemoryRecord {
        id: row.get(0)?,
        key: row.get(1)?,
        title: row.get(2)?,
        body: row.get(3)?,
        r#type: row.get(4)?,
        system_token: row.get(5)?,
        system_created_at: row.get(6)?,
        system_updated_at: row.get(7)?,
        metadata: MemoryMetadata {
            summary: row.get(8)?,
            basis: basis
                .map(|value| serde_json::from_value(json!(value)))
                .transpose()
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        9,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
            scope: row.get(10)?,
            observed_at: row.get(11)?,
            verified_at: row.get(12)?,
            review_after: row.get(13)?,
        },
        system_created_by: row.get(14)?,
        system_updated_by: row.get(15)?,
        tags: Vec::new(),
        references: Vec::new(),
    })
}

pub(crate) fn read_memory(connection: &Connection, key: &str) -> AppResult<MemoryRecord> {
    let mut record = connection
        .query_row(
            &format!("SELECT {MEMORY_COLUMNS} FROM current_memories WHERE key = ?1"),
            [key],
            read_record,
        )
        .optional()?
        .ok_or_else(|| not_found(key))?;
    hydrate(connection, std::slice::from_mut(&mut record))?;
    Ok(record)
}

fn hydrate(connection: &Connection, records: &mut [MemoryRecord]) -> AppResult<()> {
    let ids = serde_json::to_string(&records.iter().map(|record| record.id).collect::<Vec<_>>())?;
    let positions = records
        .iter()
        .enumerate()
        .map(|(position, record)| (record.id, position))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut statement = connection.prepare_cached("SELECT membership.memory_id, tags.key FROM memory_tags AS membership INNER JOIN tags ON tags.id = membership.tag_id WHERE membership.memory_id IN (SELECT value FROM json_each(?1)) ORDER BY membership.memory_id, tags.key")?;
    for entry in statement.query_map([&ids], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })? {
        let (id, key) = entry?;
        records[positions[&id]].tags.push(key);
    }
    let mut statement = connection.prepare_cached("SELECT outgoing.memory_id, target.key, relations.key FROM memory_references AS outgoing INNER JOIN memories AS target ON target.id = outgoing.target_memory_id INNER JOIN reference_types AS relations ON relations.id = outgoing.reference_type_id WHERE outgoing.memory_id IN (SELECT value FROM json_each(?1)) ORDER BY outgoing.memory_id, relations.key, target.key")?;
    for entry in statement.query_map([&ids], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            MemoryReference {
                target_key: row.get(1)?,
                reference_type: row.get(2)?,
            },
        ))
    })? {
        let (id, reference) = entry?;
        records[positions[&id]].references.push(reference);
    }
    Ok(())
}

fn replace_associations(
    connection: &Connection,
    id: i64,
    tags: &[String],
    references: &[MemoryReference],
    actor: &str,
    now: i64,
    changes: &mut Vec<Value>,
) -> AppResult<()> {
    connection.execute("DELETE FROM memory_tags WHERE memory_id = ?1 AND tag_id NOT IN (SELECT tags.id FROM tags INNER JOIN json_each(?2) AS requested ON tags.key = requested.value)", params![id, serde_json::to_string(tags)?])?;
    connection.execute("DELETE FROM memory_references WHERE memory_id = ?1 AND NOT EXISTS (SELECT 1 FROM json_each(?2) AS requested INNER JOIN memories AS target ON target.key = json_extract(requested.value, '$.target_key') INNER JOIN reference_types AS relation ON relation.key = json_extract(requested.value, '$.reference_type') WHERE target.id = memory_references.target_memory_id AND relation.id = memory_references.reference_type_id)", params![id, serde_json::to_string(references)?])?;
    for key in tags.iter().collect::<BTreeSet<_>>() {
        let tag = ensure_taxonomy(connection, "tags", key, actor, now, changes)?;
        connection.execute("INSERT INTO memory_tags (memory_id, tag_id, system_created_by, system_updated_by, system_created_at, system_updated_at) VALUES (?1, ?2, ?3, ?3, ?4, ?4) ON CONFLICT (memory_id, tag_id) DO NOTHING", params![id, tag, actor, now])?;
    }
    let references = references
        .iter()
        .map(|reference| (&reference.target_key, &reference.reference_type))
        .collect::<BTreeSet<_>>();
    for (key, relation) in references {
        let target: i64 = connection
            .query_row("SELECT id FROM memories WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?
            .ok_or_else(|| not_found(key))?;
        let relation =
            ensure_taxonomy(connection, "reference_types", relation, actor, now, changes)?;
        connection.execute("INSERT INTO memory_references (memory_id, target_memory_id, reference_type_id, system_created_by, system_updated_by, system_created_at, system_updated_at) VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?5) ON CONFLICT (memory_id, target_memory_id, reference_type_id) DO NOTHING", params![id, target, relation, actor, now])?;
    }
    Ok(())
}

fn ensure_taxonomy(
    connection: &Connection,
    table: &str,
    key: &str,
    actor: &str,
    now: i64,
    changes: &mut Vec<Value>,
) -> AppResult<i64> {
    validate_key(key)?;
    let changed = connection.execute(&format!("INSERT INTO {table} (key, name, system_created_by, system_updated_by, system_created_at, system_updated_at) VALUES (?1, ?1, ?2, ?2, ?3, ?3) ON CONFLICT (key) DO NOTHING"), params![key, actor, now])?;
    if changed != 0 {
        changes.push(
            json!({"entity":table,"key":key,"operation":"create","reason":"memory association"}),
        );
    }
    Ok(connection.query_row(
        &format!("SELECT id FROM {table} WHERE key = ?1"),
        [key],
        |row| row.get(0),
    )?)
}

fn add_index_event(
    connection: &Connection,
    key: &str,
    operation: &str,
    actor: &str,
    now: i64,
) -> AppResult<()> {
    connection.execute("INSERT INTO index_events (memory_key, operation, system_created_by, system_updated_by, system_created_at, system_updated_at) VALUES (?1, ?2, ?3, ?3, ?4, ?4)", params![key, operation, actor, now])?;
    Ok(())
}

fn append_commit(
    connection: &Connection,
    actor: &str,
    summary: &str,
    operation: &str,
    changes: Vec<Value>,
    now: i64,
) -> AppResult<i64> {
    validate_write(actor, summary)?;
    connection.execute("INSERT INTO commits (key, summary, operation, actor, changes, system_created_at, system_updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)", params![new_token(), summary, operation, actor, serde_json::to_string(&changes)?, now])?;
    Ok(connection.last_insert_rowid())
}

fn read_commit(row: &Row<'_>) -> rusqlite::Result<CommitRecord> {
    let changes: String = row.get(5)?;
    Ok(CommitRecord {
        id: row.get(0)?,
        key: row.get(1)?,
        summary: row.get(2)?,
        operation: row.get(3)?,
        actor: row.get(4)?,
        changes: serde_json::from_str(&changes).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        system_created_at: row.get(6)?,
        system_updated_at: row.get(7)?,
    })
}

fn read_taxonomy(row: &Row<'_>) -> rusqlite::Result<TaxonomyRecord> {
    Ok(TaxonomyRecord {
        id: row.get(0)?,
        key: row.get(1)?,
        name: row.get(2)?,
        system_created_by: row.get(3)?,
        system_updated_by: row.get(4)?,
        system_created_at: row.get(5)?,
        system_updated_at: row.get(6)?,
    })
}

fn read_taxonomy_by_key(
    connection: &Connection,
    table: &str,
    key: &str,
) -> AppResult<TaxonomyRecord> {
    Ok(connection.query_row(&format!("SELECT id, key, name, system_created_by, system_updated_by, system_created_at, system_updated_at FROM {table} WHERE key = ?1"), [key], read_taxonomy)?)
}

fn taxonomy_table(taxonomy: &str) -> AppResult<&'static str> {
    match taxonomy {
        "types" => Ok("memory_types"),
        "tags" => Ok("tags"),
        "reference_types" => Ok("reference_types"),
        _ => Err(AppError::invalid("Unknown taxonomy.")),
    }
}

fn matches_create(record: &MemoryRecord, input: &CreateMemory) -> bool {
    record.title == input.title
        && record.body == input.body
        && record.metadata == create_metadata(input, record.system_created_at)
        && record.r#type == input.r#type
        && record.tags.iter().collect::<BTreeSet<_>>() == input.tags.iter().collect::<BTreeSet<_>>()
        && record
            .references
            .iter()
            .map(|reference| (&reference.target_key, &reference.reference_type))
            .collect::<BTreeSet<_>>()
            == input
                .references
                .iter()
                .map(|reference| (&reference.target_key, &reference.reference_type))
                .collect::<BTreeSet<_>>()
}

fn create_metadata(input: &CreateMemory, now: i64) -> MemoryMetadata {
    input.metadata.apply(&MemoryMetadata {
        observed_at: Some(now),
        verified_at: Some(now),
        ..MemoryMetadata::default()
    })
}

fn validate_write(actor: &str, summary: &str) -> AppResult<()> {
    validate_label(actor)?;
    if summary.trim().is_empty() {
        return Err(AppError::invalid(
            "A nonempty audit commit_summary is required for every change.",
        ));
    }
    Ok(())
}

fn validate_batch<'a>(keys: impl Iterator<Item = &'a str>) -> AppResult<()> {
    let mut seen = BTreeSet::new();
    for key in keys {
        if !seen.insert(key) {
            return Err(AppError::invalid(
                "A batch cannot mutate the same memory more than once.",
            ));
        }
    }
    if seen.is_empty() {
        return Err(AppError::invalid("A batch must contain at least one item."));
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

fn validate_token(record: &MemoryRecord, expected: &str) -> AppResult<()> {
    if record.system_token != expected {
        return Err(conflict(&record.key));
    }
    Ok(())
}

fn conflict(key: &str) -> AppError {
    AppError::new(
        "token_conflict",
        format!("Memory '{key}' changed since it was read. Reread it before retrying."),
    )
}

fn not_found(key: &str) -> AppError {
    AppError::new("not_found", format!("Record '{key}' was not found."))
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

fn new_token() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn wait_for_database(_: i32) -> bool {
    std::thread::yield_now();
    true
}

fn unsupported_schema(version: i64) -> AppError {
    AppError::new(
        "unsupported_schema",
        format!(
            "Database schema version {version} is unsupported. This build requires a fresh current-state database initialized at version 2."
        ),
    )
}

fn validate_schema_version(version: i64) -> AppResult<()> {
    if version != 0 && version != 2 {
        return Err(unsupported_schema(version));
    }
    Ok(())
}

fn sqlite_limit(limit: usize) -> AppResult<i64> {
    i64::try_from(limit)
        .map_err(|_| AppError::invalid("The requested count cannot be represented by SQLite."))
}

fn serialize_records(records: Vec<MemoryRecord>) -> AppResult<Vec<Value>> {
    records
        .into_iter()
        .map(|record| serde_json::to_value(record).map_err(Into::into))
        .collect()
}

fn validate_schema_shape(connection: &Connection) -> AppResult<()> {
    let expected = Connection::open_in_memory()?;
    expected.execute_batch(include_str!("../database/initialize.sql"))?;
    fn read_schema(connection: &Connection) -> rusqlite::Result<Vec<(String, String, String)>> {
        let mut statement = connection.prepare("SELECT type, name, sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name")?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect()
    }
    if read_schema(connection)? != read_schema(&expected)? {
        return Err(unsupported_schema(2));
    }
    Ok(())
}

fn same_associations(
    tags: &[String],
    references: &[MemoryReference],
    current: &MemoryRecord,
) -> bool {
    tags.iter().collect::<BTreeSet<_>>() == current.tags.iter().collect::<BTreeSet<_>>()
        && references
            .iter()
            .map(|reference| (&reference.target_key, &reference.reference_type))
            .collect::<BTreeSet<_>>()
            == current
                .references
                .iter()
                .map(|reference| (&reference.target_key, &reference.reference_type))
                .collect::<BTreeSet<_>>()
}
