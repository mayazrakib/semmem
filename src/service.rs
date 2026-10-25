use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use fs2::FileExt;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::models::{
    AppError, AppResult, CreateMemory, GetMemories, HistoryRequest, ListMemories, MemoryVersion,
    RestoreMemory, SearchRequest, TaxonomyInput, UpdateMemory,
};
use crate::search::{SearchIndex, SearchReader};
use crate::storage::Store;

pub struct Service {
    readers: ReadPool,
    search: SearchReader,
    writes: mpsc::Sender<WriteRequest>,
    metrics: Arc<WriteMetrics>,
    _lock: File,
}

struct WriteRequest {
    operation: String,
    arguments: Value,
    writer: String,
    result: mpsc::Sender<AppResult<Value>>,
    enqueued_at: Instant,
}

#[derive(Default)]
struct WriteMetrics {
    requests: AtomicU64,
    queue_nanoseconds: AtomicU64,
    maximum_queue_nanoseconds: AtomicU64,
    sqlite_nanoseconds: AtomicU64,
    publication_nanoseconds: AtomicU64,
}

struct ReadPool {
    database: PathBuf,
    idle: Mutex<Vec<Store>>,
    opened: AtomicUsize,
}

impl ReadPool {
    fn open(database: PathBuf) -> AppResult<Self> {
        let connection = Store::open_read_only(&database)?;

        Ok(Self {
            database,
            idle: Mutex::new(vec![connection]),
            opened: AtomicUsize::new(1),
        })
    }

    fn acquire(&self) -> AppResult<ReadConnection<'_>> {
        let available = self.idle.lock().pop();
        let connection = match available {
            Some(connection) => connection,
            None => {
                let connection = Store::open_read_only(&self.database)?;
                self.opened.fetch_add(1, Ordering::Relaxed);

                connection
            }
        };

        Ok(ReadConnection {
            connection: Some(connection),
            pool: self,
        })
    }
}

struct ReadConnection<'a> {
    connection: Option<Store>,
    pool: &'a ReadPool,
}

impl Deref for ReadConnection<'_> {
    type Target = Store;

    fn deref(&self) -> &Self::Target {
        self.connection
            .as_ref()
            .expect("Checked-out connection is present")
    }
}

impl Drop for ReadConnection<'_> {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            self.pool.idle.lock().push(connection);
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Batch<T> {
    memories: Vec<T>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyInput {
    key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorInput {
    author: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaxonomyPage {
    #[serde(default = "taxonomy_limit")]
    limit: usize,
    cursor: Option<String>,
}

fn taxonomy_limit() -> usize {
    100
}

impl Service {
    pub fn open(directory: impl AsRef<Path>) -> anyhow::Result<Arc<Self>> {
        Self::open_with_batch_window(directory, Duration::from_millis(2))
    }

    pub fn open_with_batch_window(
        directory: impl AsRef<Path>,
        batch_window: Duration,
    ) -> anyhow::Result<Arc<Self>> {
        let directory = directory.as_ref();
        std::fs::create_dir_all(directory)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("service.lock"))?;
        lock.try_lock_exclusive()
            .map_err(|_| anyhow::anyhow!("Another server owns this data directory."))?;
        let database = directory.join("memory.sqlite3");
        let mut store = Store::open(&database)?;
        let identity = store.database_key()?;
        let index_path = directory.join("search");
        let marker = directory.join("search.identity");
        let expected_identity = format!("{identity}:search-v1-history");
        let matches = index_path.join("meta.json").is_file()
            && std::fs::read_to_string(&marker).ok().as_deref() == Some(expected_identity.as_str());

        if index_path.exists() && !matches {
            std::fs::rename(
                &index_path,
                directory.join(format!("search.replaced.{}", uuid::Uuid::now_v7())),
            )?;
        }

        let (mut index, rebuild) = match SearchIndex::open(&index_path) {
            Ok(index) => (index, !matches),
            Err(error) => {
                tracing::warn!(%error, "Rebuilding unreadable search index");
                if index_path.exists() {
                    std::fs::rename(
                        &index_path,
                        directory.join(format!("search.damaged.{}", uuid::Uuid::now_v7())),
                    )?;
                }

                (SearchIndex::open(&index_path)?, true)
            }
        };

        if rebuild {
            let mut after = 0;
            loop {
                let keys = store.all_memory_keys(after, 1000)?;
                if keys.is_empty() {
                    break;
                }

                for (id, key) in keys {
                    index.replace_revisions(&key, &store.revisions_for_key(&key)?)?;
                    after = id;
                }
            }

            let watermark: i64 = store.connection().query_row(
                "SELECT COALESCE(MAX(id), 0) FROM index_events",
                [],
                |row| row.get(0),
            )?;
            index.commit(watermark)?;
            store.acknowledge_events(watermark)?;
            std::fs::write(marker, expected_identity)?;
        }

        publish_events(&mut store, &mut index)?;
        let search = index.reader();
        let readers = ReadPool::open(database)?;
        let (sender, receiver) = mpsc::channel();
        let metrics = Arc::new(WriteMetrics::default());
        let writer_metrics = Arc::clone(&metrics);
        let worker_lock = lock.try_clone()?;
        thread::Builder::new()
            .name("memory-writer".into())
            .spawn(move || {
                let _ownership = worker_lock;
                run_writer(store, index, receiver, writer_metrics, batch_window);
            })?;

        Ok(Arc::new(Self {
            readers,
            search,
            writes: sender,
            metrics,
            _lock: lock,
        }))
    }

    pub fn dispatch(&self, operation: &str, arguments: Value, writer: &str) -> AppResult<Value> {
        if is_read_operation(operation) {
            return self.read(operation, arguments);
        }

        let (sender, receiver) = mpsc::channel();
        let request = WriteRequest {
            operation: operation.into(),
            arguments,
            writer: writer.into(),
            result: sender,
            enqueued_at: Instant::now(),
        };
        self.writes.send(request).map_err(|_| {
            AppError::new(
                "internal_failure",
                "The write worker stopped before accepting the request.",
            )
        })?;

        receiver.recv().map_err(|_| {
            AppError::new(
                "internal_failure",
                "The write worker stopped; inspect current versions before retrying.",
            )
        })?
    }

    pub fn write_metrics(&self) -> Value {
        let requests = self.metrics.requests.load(Ordering::Relaxed);
        json!({
            "requests": requests,
            "queue_wait_mean_ms": self.metrics.queue_nanoseconds.load(Ordering::Relaxed) as f64 / requests.max(1) as f64 / 1_000_000.0,
            "queue_wait_max_ms": self.metrics.maximum_queue_nanoseconds.load(Ordering::Relaxed) as f64 / 1_000_000.0,
            "sqlite_transaction_total_ms": self.metrics.sqlite_nanoseconds.load(Ordering::Relaxed) as f64 / 1_000_000.0,
            "search_publication_total_ms": self.metrics.publication_nanoseconds.load(Ordering::Relaxed) as f64 / 1_000_000.0,
            "read_connections_opened": self.readers.opened.load(Ordering::Relaxed),
            "read_connections_idle": self.readers.idle.lock().len(),
        })
    }

    fn read(&self, operation: &str, arguments: Value) -> AppResult<Value> {
        if operation == "search_memories" {
            let request: SearchRequest = serde_json::from_value(arguments)?;
            let mut response = self.search.search(&request).map_err(|error| {
                let code = if error
                    .downcast_ref::<crate::search::InvalidSearchQuery>()
                    .is_some()
                {
                    "invalid_input"
                } else {
                    "search_failure"
                };

                AppError::new(code, error.to_string())
            })?;
            let store = self.readers.acquire()?;
            let snapshot = store.connection().unchecked_transaction()?;
            let mut valid = Vec::with_capacity(response.hits.len());
            for hit in response.hits {
                let selected = if request.history {
                    snapshot.query_row(
                        "SELECT EXISTS(SELECT 1 FROM memory_revisions r JOIN memories m ON m.id = r.memory_id WHERE m.key = ?1 AND r.system_version = ?2 AND (?3 OR r.system_deleted_at IS NULL))",
                        rusqlite::params![hit.key, hit.version, request.include_deleted],
                        |row| row.get::<_, bool>(0),
                    )?
                } else if let Some(as_of) = request.as_of {
                    snapshot.query_row(
                        "SELECT EXISTS(SELECT 1 FROM memory_revisions r JOIN memories m ON m.id = r.memory_id WHERE m.key = ?1 AND r.system_version = ?2 AND r.system_updated_at <= ?3 AND (?4 OR r.system_deleted_at IS NULL) AND NOT EXISTS(SELECT 1 FROM memory_revisions later WHERE later.memory_id = r.memory_id AND later.system_updated_at <= ?3 AND later.system_version > r.system_version))",
                        rusqlite::params![hit.key, hit.version, as_of, request.include_deleted],
                        |row| row.get::<_, bool>(0),
                    )?
                } else {
                    snapshot.query_row(
                        "SELECT EXISTS(SELECT 1 FROM memories WHERE key = ?1 AND system_version = ?2 AND (?3 OR system_deleted_at IS NULL))",
                        rusqlite::params![hit.key, hit.version, request.include_deleted],
                        |row| row.get::<_, bool>(0),
                    )?
                };

                if selected {
                    let record = crate::storage::read_revision(&snapshot, &hit.key, hit.version)?;
                    valid.push(crate::models::SearchHit {
                        key: record.key,
                        title: record.title,
                        body: record.body,
                        version: record.system_version,
                        score: hit.score,
                        metadata: record.metadata,
                        r#type: record.r#type,
                        author: record.author,
                        tags: record.tags,
                        references: record.references,
                        system_created_at: record.system_created_at,
                        system_updated_at: record.system_updated_at,
                        system_deleted_at: record.system_deleted_at,
                        system_created_by: record.system_created_by,
                        system_updated_by: record.system_updated_by,
                        system_deleted_by: record.system_deleted_by,
                    });
                }
            }
            snapshot.commit()?;
            response.hits = valid;

            return Ok(serde_json::to_value(response)?);
        }
        let store = self.readers.acquire()?;
        match operation {
            "get_memories" => {
                let request: GetMemories = serde_json::from_value(arguments)?;
                let records = store.get_memories(&request)?;

                Ok(json!({ "memories": records }))
            }
            "list_memories" => {
                let request: ListMemories = serde_json::from_value(arguments)?;
                let records = store.list_memory_summaries(&request)?;
                let cursor = if records.len() == request.limit {
                    records
                        .last()
                        .map(|record| format!("{}:{}", record["system_updated_at"], record["id"]))
                } else {
                    None
                };

                Ok(json!({"memories": records, "next_cursor": cursor}))
            }
            "list_memory_revisions" => {
                let request: HistoryRequest = serde_json::from_value(arguments)?;
                let records = store.list_revision_summaries(&request)?;
                let cursor = if records.len() == request.limit {
                    records
                        .last()
                        .map(|record| record["system_version"].clone())
                } else {
                    None
                };

                Ok(json!({"memories": records, "before_version": cursor}))
            }
            "list_types" => taxonomy_page(&store, "memory_types", arguments),
            "list_tags" => taxonomy_page(&store, "tags", arguments),
            "list_reference_types" => taxonomy_page(&store, "reference_types", arguments),
            "list_authors" => {
                let _: EmptyInput = serde_json::from_value(arguments)?;
                Ok(json!({"items": store.list_authors()?}))
            }
            _ => Err(AppError::invalid("Unknown read operation.")),
        }
    }
}

pub fn is_read_operation(operation: &str) -> bool {
    matches!(
        operation,
        "search_memories"
            | "get_memories"
            | "list_memories"
            | "list_memory_revisions"
            | "list_types"
            | "list_tags"
            | "list_reference_types"
            | "list_authors"
    )
}

fn run_writer(
    mut store: Store,
    mut index: SearchIndex,
    receiver: mpsc::Receiver<WriteRequest>,
    metrics: Arc<WriteMetrics>,
    batch_window: Duration,
) {
    while let Ok(first) = receiver.recv() {
        let started = Instant::now();
        let mut requests = vec![first];
        while let Some(remaining) = batch_window.checked_sub(started.elapsed()) {
            match receiver.recv_timeout(remaining) {
                Ok(request) => {
                    requests.push(request);
                }
                Err(_) => break,
            }
        }
        let mut results = Vec::with_capacity(requests.len());
        for request in &mut requests {
            let queued = request
                .enqueued_at
                .elapsed()
                .as_nanos()
                .min(u64::MAX as u128) as u64;
            metrics.requests.fetch_add(1, Ordering::Relaxed);
            metrics
                .queue_nanoseconds
                .fetch_add(queued, Ordering::Relaxed);
            metrics
                .maximum_queue_nanoseconds
                .fetch_max(queued, Ordering::Relaxed);
            let writing = Instant::now();
            results.push(apply_write(
                &mut store,
                &request.operation,
                std::mem::take(&mut request.arguments),
                &request.writer,
            ));
            metrics.sqlite_nanoseconds.fetch_add(
                writing.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                Ordering::Relaxed,
            );
        }
        fault_point("after_sqlite_commit");
        let publishing = Instant::now();
        let publication = publish_events(&mut store, &mut index);
        metrics.publication_nanoseconds.fetch_add(
            publishing.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
        fault_point("before_acknowledgment");
        for (request, result) in requests.into_iter().zip(results) {
            let response = match (result, &publication) {
                (Ok(result), Ok(())) => Ok(result),
                (Ok(result), Err(error)) => {
                    let mut failure = AppError::new(
                        "publication_incomplete",
                        format!(
                            "SQLite committed the write, but search publication failed: {error}. Inspect affected versions before retrying."
                        ),
                    );
                    failure.committed = true;
                    if let Some(memories) = result.get("memories").and_then(Value::as_array) {
                        failure.affected = memories
                            .iter()
                            .filter_map(|memory| {
                                Some(crate::models::MemoryReceipt {
                                    key: memory.get("key")?.as_str()?.into(),
                                    version: memory.get("system_version").and_then(Value::as_i64),
                                })
                            })
                            .collect();
                    } else if let Some(keys) = result.get("keys").and_then(Value::as_array) {
                        failure.affected = keys
                            .iter()
                            .filter_map(|key| {
                                key.as_str().map(|key| crate::models::MemoryReceipt {
                                    key: key.into(),
                                    version: None,
                                })
                            })
                            .collect();
                    }

                    Err(failure)
                }
                (Err(error), _) => Err(error),
            };
            let _ = request.result.send(response);
        }
    }
}

fn summaries(records: Vec<crate::models::MemoryRecord>) -> AppResult<Vec<Value>> {
    records
        .into_iter()
        .map(|record| serde_json::to_value(record).map_err(Into::into))
        .collect()
}

fn fault_point(name: &str) {
    #[cfg(feature = "fault-injection")]
    if std::env::var("SEMMEM_FAILPOINT").ok().as_deref() == Some(name) {
        std::process::exit(86);
    }
    #[cfg(not(feature = "fault-injection"))]
    let _ = name;
}

fn apply_write(
    store: &mut Store,
    operation: &str,
    arguments: Value,
    writer: &str,
) -> AppResult<Value> {
    match operation {
        "create_memories" => {
            let batch: Batch<CreateMemory> = serde_json::from_value(arguments)?;

            Ok(json!({"memories": summaries(store.create_memories(&batch.memories, writer)?)?}))
        }
        "update_memories" => {
            let batch: Batch<UpdateMemory> = serde_json::from_value(arguments)?;

            Ok(json!({"memories": summaries(store.update_memories(&batch.memories, writer)?)?}))
        }
        "update_memory" => {
            let input: UpdateMemory = serde_json::from_value(arguments)?;

            Ok(json!({"memories": summaries(store.update_memories(&[input], writer)?)?}))
        }
        "delete_memories" => {
            let batch: Batch<MemoryVersion> = serde_json::from_value(arguments)?;

            Ok(json!({"memories": summaries(store.delete_memories(&batch.memories, writer)?)?}))
        }
        "restore_memories" => {
            let batch: Batch<RestoreMemory> = serde_json::from_value(arguments)?;

            Ok(json!({"memories": summaries(store.restore_memories(&batch.memories, writer)?)?}))
        }
        "purge_memories" => {
            let batch: Batch<MemoryVersion> = serde_json::from_value(arguments)?;

            Ok(json!({"keys": store.purge_memories(&batch.memories, writer)?}))
        }
        "register_author" => {
            let input: AuthorInput = serde_json::from_value(arguments)?;

            Ok(json!({"author": store.register_author(&input.author, writer)?}))
        }
        "deregister_author" => {
            let input: AuthorInput = serde_json::from_value(arguments)?;
            store.deregister_author(&input.author, writer)?;

            Ok(json!({"author": input.author, "deleted": true}))
        }
        _ => {
            let (verb, category) = operation
                .split_once('_')
                .ok_or_else(|| AppError::invalid("Unknown operation."))?;
            let taxonomy = match category {
                "type" => "types",
                "tag" => "tags",
                "reference_type" => "reference_types",
                _ => return Err(AppError::invalid("Unknown operation.")),
            };
            if verb == "delete" {
                let input: KeyInput = serde_json::from_value(arguments)?;
                store.delete_taxonomy(taxonomy, &input.key, writer)?;

                return Ok(json!({"key": input.key, "deleted": true}));
            }
            let input: TaxonomyInput = serde_json::from_value(arguments)?;
            let item = match verb {
                "create" => store.create_taxonomy(taxonomy, &input, writer)?,
                "update" => store.update_taxonomy(taxonomy, &input, writer)?,
                "put" => match store.update_taxonomy(taxonomy, &input, writer) {
                    Ok(item) => item,
                    Err(error) if error.code == "not_found" => {
                        store.create_taxonomy(taxonomy, &input, writer)?
                    }
                    Err(error) => return Err(error),
                },
                _ => return Err(AppError::invalid("Unknown operation.")),
            };

            Ok(json!({"item": item}))
        }
    }
}

fn taxonomy_page(store: &Store, table: &str, arguments: Value) -> AppResult<Value> {
    let page: TaxonomyPage = serde_json::from_value(arguments)?;
    if page.limit == 0 {
        return Err(AppError::invalid("Taxonomy page size must be positive."));
    }
    let requested = page
        .limit
        .checked_add(1)
        .and_then(|limit| i64::try_from(limit).ok())
        .ok_or_else(|| AppError::invalid("Taxonomy page size exceeds SQLite's integer range."))?;
    let mut statement = store.connection().prepare_cached(&format!("SELECT t.id, t.key, t.name, t.system_created_at, t.system_updated_at, a.name, t.system_created_by, t.system_updated_by FROM {table} t JOIN authors a ON a.id = t.author WHERE t.key > ?1 ORDER BY t.key LIMIT ?2"))?;
    let mut items = statement
        .query_map(
            rusqlite::params![page.cursor.as_deref().unwrap_or(""), requested],
            |row| {
                Ok(crate::models::TaxonomyRecord {
                    id: row.get(0)?,
                    key: row.get(1)?,
                    name: row.get(2)?,
                    author: row.get(5)?,
                    system_created_by: row.get(6)?,
                    system_updated_by: row.get(7)?,
                    system_created_at: row.get(3)?,
                    system_updated_at: row.get(4)?,
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let more = items.len() > page.limit;
    items.truncate(page.limit);
    let cursor = if more {
        items.last().map(|item| item.key.clone())
    } else {
        None
    };

    Ok(json!({"items": items, "next_cursor": cursor}))
}

fn publish_events(store: &mut Store, index: &mut SearchIndex) -> anyhow::Result<()> {
    let mut watermark = index.watermark()?;
    loop {
        let events = store.index_events(watermark, 1000)?;
        if events.is_empty() {
            break;
        }
        let keys: BTreeSet<_> = events
            .iter()
            .map(|event| event.memory_key.as_str())
            .collect();
        for key in keys {
            index.replace_revisions(key, &store.revisions_for_key(key)?)?;
        }
        watermark = events.last().expect("Nonempty event batch").sequence;
        index.commit(watermark)?;
    }
    if let Err(error) = store.acknowledge_events(watermark) {
        tracing::warn!(%error, watermark, "Search published; deferring index-event cleanup");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ReadPool;
    use crate::storage::Store;
    use std::sync::atomic::Ordering;

    #[test]
    fn grows_read_pool_on_demand_and_reuses_returned_connections() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("pool.sqlite3");
        let _store = Store::open(&database).unwrap();
        let pool = ReadPool::open(database).unwrap();
        let held = (0..8).map(|_| pool.acquire().unwrap()).collect::<Vec<_>>();
        assert_eq!(pool.opened.load(Ordering::Relaxed), 8);
        assert!(pool.idle.lock().is_empty());
        drop(held);
        assert_eq!(pool.idle.lock().len(), 8);
        let again = pool.acquire().unwrap();
        assert_eq!(pool.opened.load(Ordering::Relaxed), 8);
        assert!(!again.database_key().unwrap().is_empty());
    }
}
