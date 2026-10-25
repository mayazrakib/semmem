use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Parser;
use semmem::models::{Basis, CreateMemory, MemoryReference, MetadataPatch, UpdateMemory};
use semmem::service::Service;
use semmem::storage::Store;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Parser)]
struct Arguments {
    #[arg(long = "bench", hide = true)]
    is_bench: bool,
    #[arg(long, default_value_t = 100_000)]
    memories: usize,
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long, default_value_t = 100)]
    iterations: usize,
    #[arg(long, default_value_t = 16)]
    clients: usize,
    #[arg(long, hide = true)]
    startup_probe: bool,
    #[arg(long, default_value_t = 2)]
    batch_window_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct FixtureIdentity {
    version: u32,
    memories: usize,
}

#[derive(Serialize)]
struct Measurement {
    scenario: String,
    samples: usize,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    target_p95_ms: Option<f64>,
    meets_target: Option<bool>,
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();

    if arguments.startup_probe {
        return run_startup_probe(&arguments);
    }

    if arguments.memories == 0
        || arguments.iterations == 0
        || arguments.clients == 0
        || arguments.clients > arguments.memories
    {
        bail!("Memories and iterations must be positive; clients must be between 1 and memories");
    }

    let temporary = tempfile::tempdir()?;
    let directory = arguments.data_dir.as_deref().unwrap_or(temporary.path());
    std::fs::create_dir_all(directory)?;
    eprintln!("Preparing {} benchmark memories", arguments.memories);
    let seed_started = Instant::now();
    let fixture_created = prepare_fixture(directory, arguments.memories)?;
    let seed_seconds = seed_started.elapsed().as_secs_f64();
    let index_existed_before_open = directory.join("search/meta.json").exists();
    let initial_process =
        measure_startup_process(directory, arguments.memories, arguments.batch_window_ms)?;
    let reopened_process =
        measure_startup_process(directory, arguments.memories, arguments.batch_window_ms)?;
    let opening = Instant::now();
    eprintln!("Opening service and publishing the initial search index");
    let service = Service::open_with_batch_window(
        directory,
        Duration::from_millis(arguments.batch_window_ms),
    )?;
    let startup_seconds = opening.elapsed().as_secs_f64();
    let selected = arguments.memories / 2;
    let marker = format!("marker{selected:07}");
    let misspelled = format!("markre{selected:07}");
    let mut measurements = Vec::new();

    for (name, query, fuzzy, prefix, target) in [
        ("warm_exact_rare", marker.as_str(), false, false, 20.0),
        ("warm_exact_common", "persistent", false, false, 20.0),
        ("warm_fuzzy_rare", misspelled.as_str(), true, false, 100.0),
        ("warm_fuzzy_common", "memroy", true, false, 100.0),
        ("warm_phrase", "\"durable memory\"", false, false, 20.0),
        ("warm_prefix", "observ", false, true, 20.0),
    ] {
        eprintln!("Measuring {name}");
        let request = json!({"query": query, "fuzzy": fuzzy, "prefix": prefix, "limit": 20});

        for _ in 0..3 {
            service.dispatch("search_memories", request.clone(), "benchmark")?;
        }

        let mut samples = Vec::with_capacity(arguments.iterations);

        for _ in 0..arguments.iterations {
            let started = Instant::now();
            let response = service.dispatch("search_memories", request.clone(), "benchmark")?;

            if response["hits"]
                .as_array()
                .is_none_or(|hits| hits.is_empty())
            {
                bail!("Benchmark scenario {name} unexpectedly returned no results");
            }

            samples.push(started.elapsed().as_secs_f64() * 1000.0);
        }

        measurements.push(summarize(name, samples, Some(target)));
    }

    let mut isolated_writes = Vec::with_capacity(arguments.iterations);

    for iteration in 0..arguments.iterations {
        isolated_writes.push(run_write(&service, 0, iteration)?);
    }

    measurements.push(summarize(
        "isolated_durable_visible_writes",
        isolated_writes,
        Some(200.0),
    ));

    let active_bytes_before_mixed = active_directory_size(directory)?;
    let mixed_started = Instant::now();
    let barrier = Arc::new(Barrier::new(arguments.clients));
    let mut clients = Vec::with_capacity(arguments.clients);

    for client in 0..arguments.clients {
        let service = service.clone();
        let barrier = barrier.clone();
        let iterations = arguments.iterations;
        let memories = arguments.memories;
        clients.push(thread::spawn(move || {
            run_client(service, barrier, client, iterations, memories)
        }));
    }

    let mut mixed_reads = Vec::new();
    let mut mixed_writes = Vec::new();

    for client in clients {
        let (reads, writes) = client
            .join()
            .map_err(|_| anyhow::anyhow!("Benchmark client panicked"))??;
        mixed_reads.extend(reads);
        mixed_writes.extend(writes);
    }

    let mixed_seconds = mixed_started.elapsed().as_secs_f64();
    measurements.push(summarize("concurrent_reads", mixed_reads, None));
    measurements.push(summarize(
        "concurrent_durable_visible_writes",
        mixed_writes,
        Some(200.0),
    ));
    let disk_bytes = directory_size(directory)?;
    let active_bytes_after_mixed = active_directory_size(directory)?;

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "fixture_version": 1,
            "startup_processes": {
                "initial": initial_process,
                "reopened": reopened_process,
                "os_caches_flushed": false,
                "transport_included": false
            },
            "fixture_created": fixture_created,
            "index_existed_before_open": index_existed_before_open,
            "architecture": std::env::consts::ARCH,
            "operating_system": std::env::consts::OS,
            "logical_cpus": thread::available_parallelism().map(|count| count.get()).ok(),
            "memories": arguments.memories,
            "clients": arguments.clients,
            "batch_window_ms": arguments.batch_window_ms,
            "iterations_per_client": arguments.iterations,
            "fixture_seed_seconds": seed_seconds,
            "service_startup_seconds": startup_seconds,
            "service_open_context": "same process after independent startup probes",
            "mixed_seconds": mixed_seconds,
            "mixed_operations_per_second": (arguments.clients * arguments.iterations) as f64 / mixed_seconds,
            "data_directory_bytes": disk_bytes,
            "active_data_directory_bytes": active_bytes_after_mixed,
            "retained_index_backup_bytes": disk_bytes.saturating_sub(active_bytes_after_mixed),
            "active_bytes_before_mixed": active_bytes_before_mixed,
            "active_bytes_after_mixed": active_bytes_after_mixed,
            "mixed_disk_growth_bytes": i128::from(active_bytes_after_mixed) - i128::from(active_bytes_before_mixed),
            "write_metrics": service.write_metrics(),
            "peak_process_rss_kib": read_peak_rss(),
            "measurements": measurements,
        }))?
    );

    Ok(())
}

fn prepare_fixture(directory: &Path, memories: usize) -> Result<bool> {
    let identity_path = directory.join("benchmark-fixture.json");
    let database_path = directory.join("memory.sqlite3");

    if database_path.exists() {
        let identity: FixtureIdentity = serde_json::from_str(
            &std::fs::read_to_string(&identity_path)
                .context("Existing data directory is not a completed benchmark fixture")?,
        )?;

        if identity.version != 1 || identity.memories != memories {
            bail!("Existing benchmark fixture differs; choose another data directory");
        }

        return Ok(false);
    }

    let mut store = Store::open(database_path)?;
    store.register_author("benchmark", "benchmark")?;

    for first in (0..memories).step_by(64) {
        let batch = (first..(first + 64).min(memories))
            .map(create_fixture_memory)
            .collect::<Vec<_>>();
        store.create_memories(&batch, "benchmark")?;

        if first / 100_000 != (first + batch.len()) / 100_000 {
            eprintln!("Seeded {} memories", first + batch.len());
        }
    }

    for first in (0..memories).step_by(6400) {
        let revisions = (first..(first + 6400).min(memories))
            .step_by(100)
            .map(|index| UpdateMemory {
                key: format!("bench-record-{index:07}"),
                expected_version: 1,
                title: Some(format!("Revised observation {index}")),
                body: None,
                metadata: MetadataPatch::default(),
                r#type: None,
                tags: None,
                references: None,
                author: Some("benchmark".into()),
            })
            .collect::<Vec<_>>();
        store.update_memories(&revisions, "benchmark")?;
    }

    std::fs::write(
        identity_path,
        serde_json::to_vec(&FixtureIdentity {
            version: 1,
            memories,
        })?,
    )?;

    Ok(true)
}

fn create_fixture_memory(index: usize) -> CreateMemory {
    let topics = [
        "astronomy",
        "gardening",
        "software",
        "music",
        "history",
        "cooking",
        "travel",
        "mathematics",
    ];
    let memory_types = ["note", "preference", "decision", "observation"];
    let target_bytes = if index.is_multiple_of(10) { 4096 } else { 512 };
    let mut body = format!(
        "# Observation {index}\n\nmarker{index:07}: This persistent registry records durable memory about {}.\n\n",
        topics[index % topics.len()]
    );

    while body.len() < target_bytes {
        body.push_str("Readable Markdown preserves useful context, source observations, and practical decisions. UTF-8: café, 東京, 🦀.\n");
    }

    let references = if index > 0 && index.is_multiple_of(64) {
        vec![MemoryReference {
            target_key: format!("bench-record-{:07}", index - 1),
            reference_type: "related to".into(),
        }]
    } else {
        vec![]
    };

    CreateMemory {
        key: format!("bench-record-{index:07}"),
        title: format!("{} observation {index}", topics[index % topics.len()]),
        body,
        metadata: MetadataPatch {
            summary: index.is_multiple_of(3).then(|| {
                Some(format!(
                    "Manually supplied {} observation {index}",
                    topics[index % topics.len()]
                ))
            }),
            basis: Some(Some(if index.is_multiple_of(2) {
                Basis::OBSERVED
            } else {
                Basis::STATED
            })),
            scope: Some(Some(format!("project/{}", topics[index % topics.len()]))),
            observed_at: Some(Some(1_700_000_000_000 + index as i64)),
            verified_at: None,
            review_after: index.is_multiple_of(5).then_some(Some(1_800_000_000_000)),
        },
        r#type: memory_types[index % memory_types.len()].into(),
        tags: vec![
            topics[index % topics.len()].into(),
            format!("group-{}", index % 32),
        ],
        references,
        author: "benchmark".into(),
    }
}

fn run_client(
    service: Arc<Service>,
    barrier: Arc<Barrier>,
    client: usize,
    iterations: usize,
    memories: usize,
) -> Result<(Vec<f64>, Vec<f64>)> {
    let mut reads = Vec::new();
    let mut writes = Vec::new();
    barrier.wait();

    for iteration in 0..iterations {
        let selected = (iteration * 7919 + client * 104729) % memories;

        if iteration % 10 == 9 {
            writes.push(run_write(&service, client, iteration)?);
        } else {
            let started = Instant::now();
            let _: Value = if iteration % 10 < 7 {
                service.dispatch("search_memories", json!({"query": format!("marker{selected:07}"), "fuzzy": iteration % 2 == 0, "limit": 20}), "benchmark")?
            } else {
                service.dispatch(
                    "get_memories",
                    json!({"keys": [format!("bench-record-{selected:07}")]}),
                    "benchmark",
                )?
            };
            reads.push(started.elapsed().as_secs_f64() * 1000.0);
        }
    }

    Ok((reads, writes))
}

fn run_write(service: &Service, client: usize, iteration: usize) -> Result<f64> {
    let key = format!("bench-record-{client:07}");
    let current = service.dispatch("get_memories", json!({"keys": [&key]}), "benchmark")?;
    let version = current["memories"][0]["system_version"]
        .as_i64()
        .context("Missing current version")?;
    let mut body = create_fixture_memory(client).body;
    let publication_marker = format!("publicationclient{client}iteration{iteration}");
    body.push_str(&format!("\n\n{publication_marker}\n"));
    let started = Instant::now();
    service.dispatch(
        "update_memory",
        json!({"key": key, "expected_version": version, "body": body}),
        "benchmark",
    )?;
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    let published = service.dispatch(
        "search_memories",
        json!({"query": publication_marker, "fuzzy": false}),
        "benchmark",
    )?;

    if !published["hits"]
        .as_array()
        .is_some_and(|hits| hits.iter().any(|hit| hit["key"] == key))
    {
        bail!("Successful write was not immediately searchable");
    }

    Ok(elapsed)
}

fn summarize(name: &str, mut samples: Vec<f64>, target: Option<f64>) -> Measurement {
    samples.sort_by(f64::total_cmp);
    let percentile = |fraction: f64| {
        samples
            .get(((samples.len() as f64 * fraction).ceil() as usize).saturating_sub(1))
            .copied()
            .unwrap_or(0.0)
    };
    let p95_ms = percentile(0.95);

    Measurement {
        scenario: name.into(),
        samples: samples.len(),
        p50_ms: percentile(0.50),
        p95_ms,
        p99_ms: percentile(0.99),
        target_p95_ms: target,
        meets_target: target
            .filter(|_| !samples.is_empty())
            .map(|target| p95_ms <= target),
    }
}

fn directory_size(directory: &Path) -> Result<u64> {
    let mut bytes = 0;

    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        bytes += if path.is_dir() {
            directory_size(&path)?
        } else {
            path.metadata()?.len()
        };
    }

    Ok(bytes)
}

fn read_peak_rss() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmHWM:")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
}

fn active_directory_size(directory: &Path) -> Result<u64> {
    let mut bytes = 0;

    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if name.starts_with("search.replaced.") || name.starts_with("search.damaged.") {
            continue;
        }

        bytes += if entry.path().is_dir() {
            directory_size(&entry.path())?
        } else {
            entry.metadata()?.len()
        };
    }

    Ok(bytes)
}

fn measure_startup_process(
    directory: &Path,
    memories: usize,
    batch_window_ms: u64,
) -> Result<Value> {
    let started = Instant::now();
    let output = Command::new(std::env::current_exe()?)
        .arg("--startup-probe")
        .arg("--data-dir")
        .arg(directory)
        .arg("--memories")
        .arg(memories.to_string())
        .arg("--batch-window-ms")
        .arg(batch_window_ms.to_string())
        .output()?;

    if !output.status.success() {
        bail!(
            "Startup probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let mut report: Value = serde_json::from_slice(&output.stdout)?;
    report["launch_to_exit_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);

    Ok(report)
}

fn run_startup_probe(arguments: &Arguments) -> Result<()> {
    let directory = arguments
        .data_dir
        .as_deref()
        .context("Startup probe requires --data-dir")?;
    let index_existed = directory.join("search/meta.json").exists();
    let started = Instant::now();
    let service = Service::open_with_batch_window(
        directory,
        Duration::from_millis(arguments.batch_window_ms),
    )?;
    let initialize_ms = started.elapsed().as_secs_f64() * 1000.0;
    let first_request = Instant::now();
    let response = service.dispatch(
        "search_memories",
        json!({"query": format!("marker{:07}", arguments.memories / 2), "fuzzy": false}),
        "benchmark",
    )?;
    let first_request_ms = first_request.elapsed().as_secs_f64() * 1000.0;

    if response["hits"].as_array().is_none_or(Vec::is_empty) {
        bail!("Startup probe query returned no records");
    }

    println!(
        "{}",
        serde_json::to_string(&json!({
            "index_existed_before_open": index_existed,
            "service_initialize_ms": initialize_ms,
            "first_request_ms": first_request_ms,
            "initialize_and_first_request_ms": started.elapsed().as_secs_f64() * 1000.0,
            "peak_process_rss_kib": read_peak_rss(),
            "process_reused": false
        }))?
    );

    Ok(())
}
