use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use anyhow::{Result, ensure};
use clap::Parser;
use rusqlite::Connection;
use semmem::models::CreateMemory;
use semmem::service::Service;
use semmem::storage::Store;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Parser)]
struct Arguments {
    #[arg(long = "bench", hide = true)]
    is_bench: bool,
    #[arg(long, default_value_t = 10_000)]
    memories: usize,
    #[arg(long, default_value_t = 100)]
    iterations: usize,
    #[arg(long, default_value_t = 3)]
    repetitions: usize,
}

fn measure(operation: impl FnOnce() -> Result<Value>) -> Result<(f64, Value)> {
    let started = Instant::now();
    let response = operation()?;

    Ok((started.elapsed().as_secs_f64() * 1000.0, response))
}

fn summarize(samples: &[f64]) -> Value {
    let mut ordered = samples.to_vec();
    ordered.sort_by(f64::total_cmp);
    let percentile = |fraction: f64| {
        ordered[((ordered.len() as f64 * fraction).ceil() as usize).saturating_sub(1)]
    };

    json!({"samples":ordered.len(),"p50_ms":percentile(0.50),"p95_ms":percentile(0.95),"p99_ms":percentile(0.99)})
}

fn inspect_query_plan(
    connection: &Connection,
    predicate: &str,
) -> Result<Vec<String>> {
    let sql = format!(
        "EXPLAIN QUERY PLAN SELECT id, key, title, body, type, system_token, system_created_at, system_updated_at, summary, basis, scope, observed_at, verified_at, review_after, system_created_by, system_updated_by FROM current_memories WHERE {predicate} ORDER BY system_updated_at DESC, id DESC LIMIT 20"
    );
    let mut statement = connection.prepare(&sql)?;

    Ok(statement
        .query_map([], |row| row.get(3))?
        .collect::<Result<_, _>>()?)
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    ensure!(
        arguments.memories >= 100 && arguments.iterations > 0 && arguments.repetitions > 0,
        "Use at least 100 memories and positive iterations and repetitions."
    );
    let body = format!("reviewneedle {}", "readable text ".repeat(315));
    let mut results = Vec::new();

    for repetition in 0..arguments.repetitions {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("memory.sqlite3");
        let mut store = Store::open(&database)?;
        let fixture = (0..arguments.memories).map(|index| {
            serde_json::from_value::<CreateMemory>(json!({"key":format!("fixture-{index:08}"),"body":body,"review_after":if index % 100 == 0 { Some(100) } else { None },"verified_at":if index % 100 == 0 { None } else { Some(100) }}))
        }).collect::<Result<Vec<_>, _>>()?;
        store.create_memories(&fixture, "benchmark", "Seed dedicated review fixture.")?;
        drop(store);
        let connection = Connection::open(&database)?;
        connection.execute_batch("ANALYZE")?;
        let due_plan = inspect_query_plan(
            &connection,
            "review_after IS NOT NULL AND review_after <= 100",
        )?;
        let unverified_plan = inspect_query_plan(&connection, "verified_at IS NULL")?;
        ensure!(
            due_plan
                .iter()
                .any(|line| line.contains("memories_review_due")),
            "Due-date query did not use its partial index: {due_plan:?}"
        );
        ensure!(
            unverified_plan
                .iter()
                .any(|line| line.contains("memories_unverified_recent")),
            "Unverified query did not use its partial index: {unverified_plan:?}"
        );
        let service = Service::open(directory.path())?;
        let mut measurements = BTreeMap::<&str, Vec<f64>>::new();

        for (name, operation, query) in [
            ("browse_baseline", "list_memories", json!({"limit":20})),
            (
                "browse_due",
                "list_memories",
                json!({"review_due_by":100,"limit":20}),
            ),
            (
                "browse_unverified",
                "list_memories",
                json!({"is_verified":false,"limit":20}),
            ),
            (
                "search_baseline",
                "search_memories",
                json!({"query":"reviewneedle","fuzzy":false,"limit":20}),
            ),
            (
                "search_due",
                "search_memories",
                json!({"query":"reviewneedle","fuzzy":false,"review_due_by":100,"limit":20}),
            ),
            (
                "search_unverified",
                "search_memories",
                json!({"query":"reviewneedle","fuzzy":false,"is_verified":false,"limit":20}),
            ),
        ] {
            for _ in 0..3 {
                service.dispatch(operation, query.clone(), "benchmark")?;
            }

            for _ in 0..arguments.iterations {
                let (milliseconds, _) =
                    measure(|| Ok(service.dispatch(operation, query.clone(), "benchmark")?))?;
                measurements.entry(name).or_default().push(milliseconds);
            }
        }

        let mut full_bytes = 0;
        let mut compact_bytes = 0;

        for iteration in 0..arguments.iterations {
            // Alternate measurement order so neither mode always follows the other.
            let modes = if iteration % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            };

            for is_compact in modes {
                let key = format!("write-{iteration}-{is_compact}");
                let mut request = json!({"commit_summary":"Measure durable searchable creation.","memories":[{"key":key,"body":body}]});

                if is_compact {
                    request["request_id"] =
                        json!(uuid::Uuid::from_u128(iteration as u128 + 1).to_string());
                }

                let (milliseconds, response) = measure(|| {
                    Ok(service.dispatch("create_memories", request.clone(), "benchmark")?)
                })?;
                let scenario = if is_compact {
                    "compact_first_write"
                } else {
                    "legacy_first_write"
                };
                measurements.entry(scenario).or_default().push(milliseconds);

                if is_compact {
                    compact_bytes += serde_json::to_vec(&response)?.len();
                    let (milliseconds, replay) = measure(|| {
                        Ok(service.dispatch("create_memories", request, "benchmark")?)
                    })?;
                    ensure!(response == replay, "Replay changed the receipt.");
                    measurements
                        .entry("compact_replay")
                        .or_default()
                        .push(milliseconds);
                } else {
                    full_bytes += serde_json::to_vec(&response)?.len();
                }
            }
        }

        let summaries = measurements
            .iter()
            .map(|(name, samples)| (*name, summarize(samples)))
            .collect::<BTreeMap<_, _>>();
        results.push(json!({"repetition":repetition,"measurements":summaries,"query_plans":{"due":due_plan,"unverified":unverified_plan},"legacy_response_bytes":full_bytes,"compact_response_bytes":compact_bytes,"response_reduction_percent":100.0*(1.0-compact_bytes as f64/full_bytes as f64)}));
        eprintln!("Completed repetition {}.", repetition + 1);
    }

    let source_paths = [
        "Cargo.toml",
        "Cargo.lock",
        "database/initialize.sql",
        "src/storage.rs",
        "src/service.rs",
        "src/search.rs",
        "src/models.rs",
        "tests/perf/review_mutations.rs",
    ];
    let hashes = source_paths
        .into_iter()
        .map(|path| {
            Ok((
                path,
                format!("{:x}", Sha256::digest(std::fs::read(Path::new(path))?)),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let output = |program: &str, arguments: &[&str]| {
        Command::new(program)
            .args(arguments)
            .output()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let report = json!({"revision":output("git", &["rev-parse","HEAD"]),"source_sha256":hashes,"rustc":output("rustc", &["--version"]),"operating_system":std::env::consts::OS,"architecture":std::env::consts::ARCH,"logical_cpus":std::thread::available_parallelism()?.get(),"cpu":std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|text| text.lines().find(|line| line.starts_with("model name")).map(str::to_owned)),"workload":{"memories":arguments.memories,"body_bytes":body.len(),"iterations":arguments.iterations,"repetitions":arguments.repetitions,"notes":"Release build, sequential in-process Service::dispatch including service response construction, durable SQLite FULL commit and search publication; transport/OAuth/MCP/model latency excluded. Three read warmups per scenario, no write warmups, OS caches not flushed, no result cache. Query plans use ANALYZE on each dedicated fixture. Latencies are per request; response bytes use compact JSON serialization, not tokenizer counts."},"results":results});
    println!("{}", serde_json::to_string_pretty(&report)?);

    Ok(())
}
