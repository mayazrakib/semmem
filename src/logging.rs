use std::collections::BTreeSet;

use chrono::{DateTime, FixedOffset, SecondsFormat};
use rusqlite::{
    Connection, OptionalExtension, Transaction, TransactionBehavior, params, params_from_iter,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::models::{AppError, AppResult};
use crate::storage::{Store, append_commit, current_time};

const LOG_COLUMNS: &str =
    "id, level, source, message, timestamp, task_id, scope, system_created_by, system_created_at";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    DEBUG,
    INFO,
    WARNING,
    ERROR,
}

impl LogLevel {
    fn as_str(&self) -> &'static str {
        match self {
            Self::DEBUG => "debug",
            Self::INFO => "info",
            Self::WARNING => "warning",
            Self::ERROR => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LogEvent {
    pub id: String,
    pub level: LogLevel,
    pub source: String,
    pub message: String,
    pub timestamp: String,
    pub task_id: Option<String>,
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppendLogs {
    pub summary: String,
    pub events: Vec<LogEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetLogs {
    pub ids: Vec<String>,
}

fn default_limit() -> usize {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListLogs {
    pub task_id: Option<String>,
    pub scope: Option<String>,
    pub source: Option<String>,
    pub level: Option<LogLevel>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRecord {
    #[serde(flatten)]
    pub event: LogEvent,
    pub system_created_by: String,
    pub system_created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogReceipt {
    pub ids: Vec<String>,
    pub inserted: usize,
    pub replayed: usize,
    pub commit_id: Option<i64>,
}

pub fn parse_timestamp(timestamp: &str) -> AppResult<i64> {
    let parsed = DateTime::parse_from_rfc3339(timestamp)
        .map_err(|_| AppError::invalid("Log timestamps must be valid ISO 8601 timestamps."))?;

    if timestamp.len() != 29
        || !timestamp.ends_with("+11:00")
        || parsed.timestamp_subsec_nanos() >= 1_000_000_000
        || parsed.to_rfc3339_opts(SecondsFormat::Millis, false) != timestamp
    {
        return Err(AppError::invalid(
            "Log timestamps require milliseconds and the fixed +11:00 offset.",
        ));
    }

    Ok(parsed.timestamp_millis())
}

fn format_timestamp(timestamp: i64) -> AppResult<String> {
    let parsed = DateTime::from_timestamp_millis(timestamp)
        .ok_or_else(|| AppError::invalid("Stored log timestamp is out of range."))?;
    let offset = FixedOffset::east_opt(11 * 60 * 60).expect("AEDT offset is valid");

    Ok(parsed
        .with_timezone(&offset)
        .to_rfc3339_opts(SecondsFormat::Millis, false))
}

impl Store {
    pub fn append_log_batches(
        &mut self,
        requests: &[(AppResult<AppendLogs>, String)],
    ) -> AppResult<Vec<AppResult<LogReceipt>>> {
        let transaction =
            Transaction::new_unchecked(self.connection(), TransactionBehavior::Immediate)?;
        let mut results = Vec::with_capacity(requests.len());

        for (request, actor) in requests {
            transaction.execute_batch("SAVEPOINT log_request")?;
            let result = match request {
                Ok(request) => append_events(&transaction, request, actor),
                Err(error) => Err(error.clone()),
            };

            if result.is_err() {
                transaction.execute_batch("ROLLBACK TO log_request")?;
            }

            transaction.execute_batch("RELEASE log_request")?;
            results.push(result);
        }

        log_fault("before_log_commit");
        transaction.commit()?;
        log_fault("after_log_commit");

        Ok(results)
    }

    pub fn get_logs(&self, request: &GetLogs) -> AppResult<Vec<LogRecord>> {
        let mut statement = self.connection().prepare_cached(&format!(
            "SELECT {LOG_COLUMNS} FROM log WHERE id IN (SELECT value FROM json_each(?1))"
        ))?;
        let mut records = std::collections::BTreeMap::new();

        for row in statement.query_map([serde_json::to_string(&request.ids)?], read_log)? {
            let record = row?;
            records.insert(record.event.id.clone(), record);
        }

        request
            .ids
            .iter()
            .map(|id| {
                records.get(id).cloned().ok_or_else(|| {
                    AppError::new("not_found", format!("Log event '{id}' was not found."))
                })
            })
            .collect()
    }

    pub fn list_logs(&self, request: &ListLogs) -> AppResult<Value> {
        let limit = request
            .limit
            .checked_add(1)
            .and_then(|value| i64::try_from(value).ok())
            .filter(|_| request.limit > 0)
            .ok_or_else(|| {
                AppError::invalid("Log page size must be positive and fit SQLite's integer range.")
            })?;
        let mut predicates = Vec::new();
        let mut parameters = Vec::<rusqlite::types::Value>::new();

        for (column, value) in [
            ("task_id", request.task_id.as_ref()),
            ("scope", request.scope.as_ref()),
            ("source", request.source.as_ref()),
        ] {
            if let Some(value) = value {
                predicates.push(format!("{column} = ?"));
                parameters.push(value.clone().into());
            }
        }

        if let Some(level) = &request.level {
            predicates.push("level = ?".into());
            parameters.push(level.as_str().to_owned().into());
        }

        let since = request.since.as_deref().map(parse_timestamp).transpose()?;
        let until = request.until.as_deref().map(parse_timestamp).transpose()?;

        if since.zip(until).is_some_and(|(since, until)| since > until) {
            return Err(AppError::invalid("The log time range is reversed."));
        }

        for (predicate, value) in [("timestamp >= ?", since), ("timestamp <= ?", until)] {
            if let Some(value) = value {
                predicates.push(predicate.into());
                parameters.push(value.into());
            }
        }

        if let Some(cursor) = &request.cursor {
            let (timestamp, id): (i64, String) = serde_json::from_str(cursor)
                .map_err(|_| AppError::invalid("Invalid log cursor."))?;
            validate_id(&id)?;
            predicates.push("(timestamp, id) > (?, ?)".into());
            parameters.push(timestamp.into());
            parameters.push(id.into());
        }

        parameters.push(limit.into());
        let filter = if predicates.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", predicates.join(" AND "))
        };
        let mut statement = self.connection().prepare_cached(&format!(
            "SELECT {LOG_COLUMNS} FROM log {filter} ORDER BY timestamp, id LIMIT ?"
        ))?;
        let mut records = statement
            .query_map(params_from_iter(parameters), read_log)?
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = records.len() > request.limit;
        records.truncate(request.limit);
        let cursor = if has_more {
            records
                .last()
                .map(|record| {
                    Ok::<_, AppError>(serde_json::to_string(&(
                        parse_timestamp(&record.event.timestamp)?,
                        &record.event.id,
                    ))?)
                })
                .transpose()?
        } else {
            None
        };

        Ok(json!({"events":records,"next_cursor":cursor}))
    }
}

fn validate_id(id: &str) -> AppResult<()> {
    let parsed =
        uuid::Uuid::parse_str(id).map_err(|_| AppError::invalid("Log event IDs must be UUIDs."))?;

    if parsed.hyphenated().to_string() != id {
        return Err(AppError::invalid(
            "Log event IDs must use canonical lowercase UUID notation.",
        ));
    }

    Ok(())
}

fn validate_line(value: &str) -> AppResult<()> {
    if value.trim().is_empty()
        || value.chars().any(char::is_control)
        || value.contains(['\u{2028}', '\u{2029}'])
    {
        return Err(AppError::invalid(
            "Log sources and messages must be nonblank single-line text without control characters.",
        ));
    }

    Ok(())
}

fn append_events(
    connection: &Connection,
    request: &AppendLogs,
    actor: &str,
) -> AppResult<LogReceipt> {
    if request.summary.trim().is_empty() || actor.trim().is_empty() || request.events.is_empty() {
        return Err(AppError::invalid(
            "Log batches require a nonblank summary, a trusted actor, and at least one event.",
        ));
    }

    let now = current_time()?;
    let mut ids = BTreeSet::new();
    let mut changes = Vec::new();
    let mut statement = connection.prepare_cached("INSERT INTO log (id, level, source, message, timestamp, task_id, scope, system_created_by, system_created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)")?;
    let mut existing = connection.prepare_cached("SELECT level = ?2 AND source = ?3 AND message = ?4 AND timestamp = ?5 AND task_id IS ?6 AND scope IS ?7 AND system_created_by = ?8 FROM log WHERE id = ?1")?;

    for event in &request.events {
        validate_id(&event.id)?;
        validate_line(&event.source)?;
        validate_line(&event.message)?;
        let timestamp = parse_timestamp(&event.timestamp)?;

        if !ids.insert(event.id.clone()) {
            return Err(AppError::invalid(
                "A log batch cannot contain duplicate event IDs.",
            ));
        }

        let matches = existing
            .query_row(
                params![
                    event.id,
                    event.level.as_str(),
                    event.source,
                    event.message,
                    timestamp,
                    event.task_id,
                    event.scope,
                    actor
                ],
                |row| row.get::<_, bool>(0),
            )
            .optional()?;

        match matches {
            Some(true) => {}
            Some(false) => {
                return Err(AppError::new(
                    "conflict",
                    format!(
                        "Log event '{}' already exists with different content or actor.",
                        event.id
                    ),
                ));
            }
            None => {
                statement.execute(params![
                    event.id,
                    event.level.as_str(),
                    event.source,
                    event.message,
                    timestamp,
                    event.task_id,
                    event.scope,
                    actor,
                    now
                ])?;
                changes.push(json!({"entity":"log","key":event.id,"operation":"create"}));
            }
        }
    }

    let inserted = changes.len();
    let commit_id = if changes.is_empty() {
        None
    } else {
        Some(append_commit(
            connection,
            actor,
            &request.summary,
            "append_logs",
            changes,
            now,
        )?)
    };

    Ok(LogReceipt {
        ids: request
            .events
            .iter()
            .map(|event| event.id.clone())
            .collect(),
        inserted,
        replayed: request.events.len() - inserted,
        commit_id,
    })
}

fn read_log(row: &rusqlite::Row<'_>) -> rusqlite::Result<LogRecord> {
    let level: String = row.get(1)?;
    let timestamp = format_timestamp(row.get(4)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })?;

    Ok(LogRecord {
        event: LogEvent {
            id: row.get(0)?,
            level: serde_json::from_value(json!(level)).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
            source: row.get(2)?,
            message: row.get(3)?,
            timestamp,
            task_id: row.get(5)?,
            scope: row.get(6)?,
        },
        system_created_by: row.get(7)?,
        system_created_at: row.get(8)?,
    })
}

fn log_fault(name: &str) {
    #[cfg(feature = "fault-injection")]
    if std::env::var("SEMMEM_FAILPOINT").as_deref() == Ok(name) {
        std::process::exit(86);
    }

    #[cfg(not(feature = "fault-injection"))]
    let _ = name;
}
