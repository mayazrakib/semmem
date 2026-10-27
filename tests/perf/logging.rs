use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use semmem::service::Service;
use serde_json::{Value, json};

fn directory_bytes(path: &Path) -> Result<u64> {
    let mut size = 0;

    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        size += if metadata.is_dir() {
            directory_bytes(&entry.path())?
        } else {
            metadata.len()
        };
    }

    Ok(size)
}

fn main() -> Result<()> {
    let events = 2000;
    let mut measurements = Vec::new();

    for repetition in 0..3 {
        for batch_size in [1, 100] {
            let directory = tempfile::tempdir()?;
            let service = Service::open(directory.path())?;
            let initial_bytes = directory_bytes(directory.path())?;
            let batches = (0..events).step_by(batch_size).map(|first| {
                let events = (first..(first + batch_size).min(events)).map(|number| json!({"id":uuid::Uuid::from_u128(number as u128 + 1).to_string(),"level":"info","source":"Atlas","message":"Saved the report and verified its destination.","timestamp":"2026-10-27T14:30:00.123+11:00","task_id":"benchmark","scope":"work"})).collect::<Vec<_>>();

                json!({"summary":"Record verified report outcomes.","events":events})
            }).collect::<Vec<_>>();
            let mut latency_ms = Vec::new();
            let started = Instant::now();

            for batch in batches {
                let started = Instant::now();
                service.dispatch("append_logs", batch, "benchmark")?;
                latency_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            }

            let elapsed_seconds = started.elapsed().as_secs_f64();
            latency_ms.sort_by(f64::total_cmp);
            let metrics = service.write_metrics();
            let connection = rusqlite::Connection::open(directory.path().join("memory.sqlite3"))?;
            let event_count: i64 =
                connection.query_row("SELECT COUNT(*) FROM log", [], |row| row.get(0))?;
            let index_event_count: i64 =
                connection.query_row("SELECT COUNT(*) FROM index_events", [], |row| row.get(0))?;
            assert_eq!(event_count, events as i64);
            assert_eq!(index_event_count, 0);
            measurements.push(json!({"repetition":repetition,"batch_size":batch_size,"events":events,"elapsed_seconds":elapsed_seconds,"events_per_second":events as f64 / elapsed_seconds,"request_p50_ms":latency_ms[latency_ms.len()/2],"request_p95_ms":latency_ms[(latency_ms.len()*95/100).min(latency_ms.len()-1)],"transactions":metrics["log_transactions"],"storage_growth_bytes":directory_bytes(directory.path())? - initial_bytes,"index_events":index_event_count}));
        }
    }

    let report: Value = json!({"workload":"Sequential in-process durable appends; identical events, three repetitions, fresh databases, no warmup, transport excluded, OS caches not flushed.","revision":std::process::Command::new("git").args(["rev-parse","HEAD"]).output().ok().map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned()),"working_tree":"Logging implementation under verification; recorded source hashes accompany results.","operating_system":std::env::consts::OS,"architecture":std::env::consts::ARCH,"measurements":measurements});
    println!("{}", serde_json::to_string_pretty(&report)?);

    Ok(())
}
