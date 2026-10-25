use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::path::Path;
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

const MAX_BATCH: usize = 64;
const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_QUEUED_BYTES: usize = 16 * MAX_BODY_BYTES;

pub struct Service {
    readers: Vec<Mutex<Store>>,
    next_reader: AtomicUsize,
    search: SearchReader,
    writes: mpsc::SyncSender<WriteRequest>,
    queued_bytes: Arc<AtomicUsize>,
    metrics: Arc<WriteMetrics>,
    _lock: File,
}

struct WriteRequest {
    operation: String,
    arguments: Value,
    actor: String,
    result: mpsc::SyncSender<AppResult<Value>>,
    _reservation: QueueReservation,
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

struct QueueReservation {
    bytes: usize,
    counter: Arc<AtomicUsize>,
}

impl Drop for QueueReservation {
    fn drop(&mut self) {
        self.counter.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

fn reserve_queue(counter: &Arc<AtomicUsize>, bytes: usize) -> AppResult<QueueReservation> {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current
                .checked_add(bytes)
                .filter(|total| *total <= MAX_QUEUED_BYTES)
        })
        .map_err(|_| {
            AppError::new(
                "overloaded",
                "The queued write payload limit has been reached.",
            )
        })?;

    Ok(QueueReservation {
        bytes,
        counter: Arc::clone(counter),
    })
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
        anyhow::ensure!(
            (Duration::from_millis(1)..=Duration::from_millis(20)).contains(&batch_window),
            "The batch collection window must be between 1 and 20 ms."
        );
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
        let expected_identity = format!("{identity}:search-v2");
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
                let records = store.all_current(after, 1000)?;
                if records.is_empty() {
                    break;
                }

                for record in &records {
                    index.upsert(record)?;
                    after = record.id;
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
        let readers = (0..4)
            .map(|_| Store::open_read_only(&database).map(Mutex::new))
            .collect::<AppResult<Vec<_>>>()?;
        let (sender, receiver) = mpsc::sync_channel(256);
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
            next_reader: AtomicUsize::new(0),
            search,
            writes: sender,
            queued_bytes: Arc::new(AtomicUsize::new(0)),
            metrics,
            _lock: lock,
        }))
    }

    pub fn dispatch(&self, operation: &str, arguments: Value, actor: &str) -> AppResult<Value> {
        if is_read_operation(operation) {
            let response = self.read(operation, arguments)?;
            if serde_json::to_vec(&response)?.len() > 4 * MAX_BODY_BYTES {
                return Err(AppError::new(
                    "response_limit",
                    "The response exceeds 4 MiB. Retrieve fewer memories per request.",
                ));
            }

            return Ok(response);
        }

        let encoded_size = serde_json::to_vec(&arguments)?.len();
        if arguments
            .get("memories")
            .and_then(Value::as_array)
            .is_some_and(|memories| memories.len() > MAX_BATCH)
        {
            return Err(AppError::invalid(
                "A service mutation batch may contain at most 64 memories.",
            ));
        }
        if encoded_size > MAX_BODY_BYTES {
            return Err(AppError::invalid(
                "A write request may contain at most 1 MiB of JSON.",
            ));
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let request = WriteRequest {
            operation: operation.into(),
            arguments,
            actor: actor.into(),
            result: sender,
            _reservation: reserve_queue(&self.queued_bytes, encoded_size)?,
            enqueued_at: Instant::now(),
        };
        self.writes.try_send(request).map_err(|error| {
            AppError::new("overloaded", format!("Write queue is unavailable: {error}"))
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
            "queued_payload_bytes": self.queued_bytes.load(Ordering::Relaxed),
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
                } else if error
                    .downcast_ref::<crate::search::SearchBudgetExceeded>()
                    .is_some()
                {
                    "resource_limit"
                } else {
                    "search_failure"
                };

                AppError::new(code, error.to_string())
            })?;
            let lane = self.next_reader.fetch_add(1, Ordering::Relaxed) % self.readers.len();
            let store = self.readers[lane].lock();
            let mut statement = store.connection().prepare_cached("SELECT EXISTS(SELECT 1 FROM memories WHERE key = ?1 AND system_version = ?2 AND system_deleted_at IS NULL)")?;
            let mut valid = Vec::with_capacity(response.hits.len());
            for hit in response.hits {
                if statement.query_row(rusqlite::params![hit.key, hit.version], |row| {
                    row.get::<_, bool>(0)
                })? {
                    valid.push(hit);
                }
            }
            response.hits = valid;

            return Ok(serde_json::to_value(response)?);
        }
        let lane = self.next_reader.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        let store = self.readers[lane].lock();
        match operation {
            "get_memories" => {
                let request: GetMemories = serde_json::from_value(arguments)?;
                if request.keys.is_empty() || request.keys.len() > 64 {
                    return Err(AppError::invalid("Get requires between 1 and 64 keys."));
                }
                let records = store.get_memories(&request)?;

                Ok(json!({ "memories": records }))
            }
            "list_memories" => {
                let request: ListMemories = serde_json::from_value(arguments)?;
                let records = store.list_memories(&request)?;
                let cursor = if records.len() == request.limit {
                    records
                        .last()
                        .map(|record| format!("{}:{}", record.system_updated_at, record.id))
                } else {
                    None
                };

                Ok(json!({"memories": summaries(records)?, "next_cursor": cursor}))
            }
            "list_memory_revisions" => {
                let request: HistoryRequest = serde_json::from_value(arguments)?;
                let records = store.list_revisions(&request)?;
                let cursor = if records.len() == request.limit {
                    records.last().map(|record| record.system_version)
                } else {
                    None
                };

                Ok(json!({"memories": summaries(records)?, "before_version": cursor}))
            }
            "list_kinds" => taxonomy_page(&store, "memory_kinds", arguments),
            "list_tags" => taxonomy_page(&store, "tags", arguments),
            "list_relation_types" => taxonomy_page(&store, "relation_types", arguments),
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
            | "list_kinds"
            | "list_tags"
            | "list_relation_types"
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
        let mut bytes = requests[0].arguments.to_string().len();
        while requests.len() < MAX_BATCH && bytes < MAX_BODY_BYTES {
            let Some(remaining) = batch_window.checked_sub(started.elapsed()) else {
                break;
            };
            match receiver.recv_timeout(remaining) {
                Ok(request) => {
                    bytes += request.arguments.to_string().len();
                    requests.push(request);
                }
                Err(_) => break,
            }
        }
        let mut results = Vec::with_capacity(requests.len());
        for request in &requests {
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
                request.arguments.clone(),
                &request.actor,
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
        .map(|mut record| {
            record.body.clear();
            let mut value = serde_json::to_value(record)?;
            value
                .as_object_mut()
                .expect("Memory record is an object")
                .remove("body");

            Ok(value)
        })
        .collect()
}

fn fault_point(name: &str) {
    #[cfg(feature = "fault-injection")]
    if std::env::var("SEMANTIC_MEMORY_FAILPOINT").ok().as_deref() == Some(name) {
        std::process::exit(86);
    }
    #[cfg(not(feature = "fault-injection"))]
    let _ = name;
}

fn apply_write(
    store: &mut Store,
    operation: &str,
    arguments: Value,
    actor: &str,
) -> AppResult<Value> {
    match operation {
        "create_memories" => {
            let mut batch: Batch<CreateMemory> = serde_json::from_value(arguments)?;
            for memory in &mut batch.memories {
                memory.actor = actor.into();
            }

            Ok(json!({"memories": summaries(store.create_memories(&batch.memories)?)?}))
        }
        "update_memories" => {
            let mut batch: Batch<UpdateMemory> = serde_json::from_value(arguments)?;
            for memory in &mut batch.memories {
                memory.actor = actor.into();
            }

            Ok(json!({"memories": summaries(store.update_memories(&batch.memories)?)?}))
        }
        "update_memory" => {
            let mut input: UpdateMemory = serde_json::from_value(arguments)?;
            input.actor = actor.into();

            Ok(json!({"memories": summaries(store.update_memories(&[input])?)?}))
        }
        "delete_memories" => {
            let batch: Batch<MemoryVersion> = serde_json::from_value(arguments)?;

            Ok(json!({"memories": summaries(store.delete_memories(&batch.memories, actor)?)?}))
        }
        "restore_memories" => {
            let mut batch: Batch<RestoreMemory> = serde_json::from_value(arguments)?;
            for memory in &mut batch.memories {
                memory.actor = actor.into();
            }

            Ok(json!({"memories": summaries(store.restore_memories(&batch.memories)?)?}))
        }
        "purge_memories" => {
            let batch: Batch<MemoryVersion> = serde_json::from_value(arguments)?;

            Ok(json!({"keys": store.purge_memories(&batch.memories)?}))
        }
        _ => {
            let (verb, category) = operation
                .split_once('_')
                .ok_or_else(|| AppError::invalid("Unknown operation."))?;
            let taxonomy = match category {
                "kind" => "kinds",
                "tag" => "tags",
                "relation_type" => "relation_types",
                _ => return Err(AppError::invalid("Unknown operation.")),
            };
            if verb == "delete" {
                let input: KeyInput = serde_json::from_value(arguments)?;
                store.delete_taxonomy(taxonomy, &input.key)?;

                return Ok(json!({"key": input.key, "deleted": true}));
            }
            let input: TaxonomyInput = serde_json::from_value(arguments)?;
            let item = match verb {
                "create" => store.create_taxonomy(taxonomy, &input)?,
                "update" => store.update_taxonomy(taxonomy, &input)?,
                "put" => match store.update_taxonomy(taxonomy, &input) {
                    Ok(item) => item,
                    Err(error) if error.code == "not_found" => {
                        store.create_taxonomy(taxonomy, &input)?
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
    if page.limit == 0 || page.limit > 500 {
        return Err(AppError::invalid(
            "Taxonomy limit must be between 1 and 500.",
        ));
    }
    let mut statement = store.connection().prepare_cached(&format!("SELECT id, key, name, system_created_at, system_updated_at FROM {table} WHERE key > ?1 ORDER BY key LIMIT ?2"))?;
    let mut items = statement
        .query_map(
            rusqlite::params![
                page.cursor.as_deref().unwrap_or(""),
                (page.limit + 1) as i64
            ],
            |row| {
                Ok(crate::models::TaxonomyRecord {
                    id: row.get(0)?,
                    key: row.get(1)?,
                    name: row.get(2)?,
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
            match store.get_memory(key, true) {
                Ok(memory) if memory.system_deleted_at.is_none() => index.upsert(&memory)?,
                Ok(_) => index.remove(key)?,
                Err(error) if error.code == "not_found" => index.remove(key)?,
                Err(error) => return Err(error.into()),
            }
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
    use super::{MAX_QUEUED_BYTES, reserve_queue};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn should_bound_queued_payloads_and_release_cancelled_reservations() {
        let counter = Arc::new(AtomicUsize::new(0));
        let reservation = reserve_queue(&counter, MAX_QUEUED_BYTES).unwrap();
        assert_eq!(reserve_queue(&counter, 1).err().unwrap().code, "overloaded");
        drop(reservation);
        assert_eq!(counter.load(Ordering::Acquire), 0);
        let reservation = reserve_queue(&counter, 1).unwrap();
        drop(reservation);
        assert_eq!(counter.load(Ordering::Acquire), 0);
    }
}
