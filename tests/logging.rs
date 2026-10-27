use std::sync::{Arc, Barrier};
use std::time::Duration;

use rusqlite::Connection;
use semmem::logging::{AppendLogs, GetLogs};
use semmem::models::{AppError, ListCommits};
use semmem::service::Service;
use semmem::storage::Store;
use serde_json::{Value, json};

fn event(number: u128) -> Value {
    json!({"id":uuid::Uuid::from_u128(number).to_string(),"level":"info","source":"Atlas","message":"Saved a report.","timestamp":"2026-10-27T14:30:00.123+11:00","task_id":"report","scope":"work"})
}

fn batch(events: Vec<Value>) -> Value {
    json!({"summary":"Record report preparation outcomes.","events":events})
}

#[test]
fn batch_savepoints_replays_and_actor_conflicts_preserve_atomicity() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let mut store = Store::open(directory.path().join("memory.sqlite3"))?;
    let good: AppendLogs = serde_json::from_value(batch(vec![event(1), event(2)]))?;
    let mut bad = good.clone();
    bad.events[0].id = uuid::Uuid::from_u128(3).to_string();
    bad.events[1].message = "bad\nmessage".into();
    let next: AppendLogs = serde_json::from_value(batch(vec![event(4)]))?;
    let results = store.append_log_batches(&[
        (Ok(good.clone()), "actor".into()),
        (Ok(bad), "actor".into()),
        (Err(AppError::invalid("Malformed request.")), "actor".into()),
        (Ok(next), "actor".into()),
    ])?;
    assert_eq!(results[0].as_ref().unwrap().inserted, 2);
    assert!(results[1].is_err());
    assert!(results[2].is_err());
    assert_eq!(results[3].as_ref().unwrap().inserted, 1);
    assert!(
        store
            .get_logs(&GetLogs {
                ids: vec![uuid::Uuid::from_u128(3).to_string()]
            })
            .is_err()
    );
    let replay = store
        .append_log_batches(&[(Ok(good.clone()), "actor".into())])?
        .remove(0)?;
    assert_eq!(replay.replayed, 2);
    assert_eq!(replay.commit_id, None);
    assert!(store.append_log_batches(&[(Ok(good), "other actor".into())])?[0].is_err());
    let mixed: AppendLogs = serde_json::from_value(batch(vec![event(1), event(5)]))?;
    let mixed = store
        .append_log_batches(&[(Ok(mixed), "actor".into())])?
        .remove(0)?;
    assert_eq!((mixed.inserted, mixed.replayed), (1, 1));
    let commits = store.list_commits(&ListCommits {
        limit: 100,
        cursor: None,
    })?;
    assert_eq!(commits.len(), 4);
    assert_eq!(
        commits[0].changes,
        json!([{"entity":"log","key":uuid::Uuid::from_u128(5).to_string(),"operation":"create"}])
    );
    let connection = Connection::open(directory.path().join("memory.sqlite3"))?;
    assert_eq!(
        connection.query_row("SELECT COUNT(*) FROM index_events", [], |row| row
            .get::<_, i64>(0))?,
        0
    );
    assert!(
        connection
            .execute("UPDATE log SET message = 'changed'", [])
            .is_err()
    );
    assert!(connection.execute("DELETE FROM log", []).is_err());
    assert!(connection.execute("INSERT OR REPLACE INTO log (id,level,source,message,timestamp,system_created_by,system_created_at) VALUES ('00000000-0000-0000-0000-000000000001','info','Atlas','tampered',0,'actor',0)", []).is_err());
    assert!(connection.execute("INSERT OR REPLACE INTO commits (id,key,summary,operation,actor,system_created_at,system_updated_at) VALUES (1,'other','tampered','update','actor',0,0)", []).is_err());
    assert!(connection.execute("INSERT OR REPLACE INTO commit_changes (commit_id,ordinal,entity,operation,has_fields) VALUES (1,0,'log','tampered',0)", []).is_err());

    assert!(
        connection
            .execute("UPDATE commit_changes SET operation = 'changed'", [])
            .is_err()
    );
    assert!(
        connection
            .execute("DELETE FROM commit_changes", [])
            .is_err()
    );
    assert!(
        !connection
            .prepare("PRAGMA table_info(commits)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .contains(&"changes".into())
    );
    let reopened = Store::open_read_only(directory.path().join("memory.sqlite3"))?;
    let records = reopened.get_logs(&GetLogs {
        ids: vec![
            uuid::Uuid::from_u128(2).to_string(),
            uuid::Uuid::from_u128(1).to_string(),
        ],
    })?;
    assert_eq!(records[0].event.id, uuid::Uuid::from_u128(2).to_string());
    assert_eq!(records[0].event.timestamp, "2026-10-27T14:30:00.123+11:00");
    assert_eq!(records[0].system_created_by, "actor");

    Ok(())
}

#[test]
fn validation_rolls_back_conflicts_and_rejects_untrusted_fields() -> anyhow::Result<()> {
    let mut store = Store::open(":memory:")?;
    let input: AppendLogs = serde_json::from_value(batch(vec![event(1)]))?;
    store
        .append_log_batches(&[(Ok(input), "actor".into())])?
        .remove(0)?;

    for (field, value) in [
        ("id", json!("bad")),
        ("source", json!(" ")),
        ("message", json!("bad\rmessage")),
        ("timestamp", json!("2026-10-27T14:30:00.123Z")),
        ("timestamp", json!("2026-02-30T14:30:00.123+11:00")),
        ("timestamp", json!("2026-10-27T14:30:00+11:00")),
        ("timestamp", json!("2026-10-27T14:30:60.123+11:00")),
    ] {
        let mut invalid = event(2);
        invalid[field] = value;
        let input: AppendLogs = serde_json::from_value(batch(vec![invalid]))?;
        assert!(
            store.append_log_batches(&[(Ok(input), "actor".into())])?[0].is_err(),
            "{field}"
        );
    }

    for invalid in [
        batch(vec![]),
        json!({"summary":" ","events":[event(2)]}),
        batch(vec![event(2), event(2)]),
    ] {
        let input: AppendLogs = serde_json::from_value(invalid)?;
        assert!(store.append_log_batches(&[(Ok(input), "actor".into())])?[0].is_err());
    }

    let mut conflict = event(1);
    conflict["message"] = json!("Conflicting content.");
    let input: AppendLogs = serde_json::from_value(batch(vec![event(2), conflict]))?;
    assert!(store.append_log_batches(&[(Ok(input), "actor".into())])?[0].is_err());
    assert!(
        store
            .get_logs(&GetLogs {
                ids: vec![uuid::Uuid::from_u128(2).to_string()]
            })
            .is_err()
    );
    let mut untrusted = event(3);
    untrusted["system_created_by"] = json!("spoof");
    assert!(serde_json::from_value::<AppendLogs>(batch(vec![untrusted])).is_err());
    let mut invalid_level = event(3);
    invalid_level["level"] = json!("fatal");
    assert!(serde_json::from_value::<AppendLogs>(batch(vec![invalid_level])).is_err());

    Ok(())
}

#[test]
fn filtered_cursors_handle_equal_timestamps_and_new_inserts() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path())?;
    service.dispatch(
        "append_logs",
        batch(vec![event(1), event(3), event(5)]),
        "actor",
    )?;
    let first = service.dispatch("list_logs", json!({"limit":1,"task_id":"report","scope":"work","source":"Atlas","level":"info","since":"2026-10-27T14:30:00.123+11:00","until":"2026-10-27T14:30:00.123+11:00"}), "actor")?;
    assert_eq!(first["events"][0]["id"], event(1)["id"]);
    service.dispatch("append_logs", batch(vec![event(2), event(4)]), "actor")?;
    let second = service.dispatch(
        "list_logs",
        json!({"limit":2,"cursor":first["next_cursor"]}),
        "actor",
    )?;
    assert_eq!(second["events"][0]["id"], event(2)["id"]);
    assert_eq!(second["events"][1]["id"], event(3)["id"]);
    let third = service.dispatch(
        "list_logs",
        json!({"limit":2,"cursor":second["next_cursor"]}),
        "actor",
    )?;
    assert_eq!(third["events"][1]["id"], event(5)["id"]);
    assert!(third["next_cursor"].is_null());

    for filter in [
        json!({"task_id":"missing"}),
        json!({"scope":"missing"}),
        json!({"source":"missing"}),
        json!({"level":"warning"}),
    ] {
        assert_eq!(
            service.dispatch("list_logs", filter, "actor")?["events"],
            json!([])
        );
    }

    assert!(
        service
            .dispatch("list_logs", json!({"limit":0}), "actor")
            .is_err()
    );
    assert!(
        service
            .dispatch("list_logs", json!({"cursor":"bad"}), "actor")
            .is_err()
    );
    let connection = Connection::open(directory.path().join("memory.sqlite3"))?;

    for (filter, index) in [
        ("", "log_chronological"),
        ("WHERE task_id = 'report'", "log_task"),
        ("WHERE scope = 'work'", "log_scope"),
        ("WHERE source = 'Atlas'", "log_source"),
        ("WHERE level = 'info'", "log_level"),
    ] {
        let details = connection
            .prepare(&format!(
                "EXPLAIN QUERY PLAN SELECT id FROM log {filter} ORDER BY timestamp, id LIMIT 20"
            ))?
            .query_map([], |row| row.get::<_, String>(3))?
            .collect::<Result<Vec<_>, _>>()?
            .join(" ");
        assert!(details.contains(index), "{details}");
        assert!(!details.contains("TEMP B-TREE"), "{details}");
    }

    Ok(())
}

#[test]
fn concurrent_producers_share_commits_and_conflicting_retries_are_safe() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let service = Service::open_with_batch_window(directory.path(), Duration::from_millis(10))?;
    let barrier = Arc::new(Barrier::new(16));
    let handles = (0..16)
        .map(|number| {
            let service = Arc::clone(&service);
            let barrier = Arc::clone(&barrier);

            std::thread::spawn(move || {
                barrier.wait();
                let mut value = event(1);

                if number == 15 {
                    value["message"] = json!("Conflicting producer.");
                }

                service.dispatch("append_logs", batch(vec![value]), "actor")
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert!(results.iter().any(Result::is_err));
    let inserted = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .map(|value| value["inserted"].as_u64().unwrap())
        .sum::<u64>();
    assert_eq!(inserted, 1);
    assert!(
        service.write_metrics()["log_transactions"]
            .as_u64()
            .unwrap()
            < 16
    );
    assert_eq!(
        service.dispatch("list_logs", json!({}), "actor")?["events"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        service.dispatch("list_commits", json!({}), "actor")?["commits"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    Ok(())
}

#[test]
fn previous_schema_versions_are_rejected_without_changes() -> anyhow::Result<()> {
    for version in [1, 2, 3, 4] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("old.sqlite3");
        let connection = Connection::open(&path)?;
        connection.execute_batch(&format!("CREATE TABLE legacy (body TEXT); INSERT INTO legacy VALUES ('preserve'); PRAGMA user_version = {version};"))?;
        drop(connection);
        let before = std::fs::read(&path)?;
        assert!(Store::open(&path).is_err());
        assert_eq!(std::fs::read(&path)?, before);
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_local_api_and_cli_share_logging_and_trusted_attribution() -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    use rmcp::model::CallToolRequestParams;
    use semmem::mcp::MemoryMcp;

    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path().join("data"))?;
    let (client_transport, server_transport) = tokio::io::duplex(65536);
    let mcp_service = Arc::clone(&service);
    let server = tokio::spawn(async move {
        MemoryMcp::new(mcp_service, "mcp actor")
            .serve(server_transport)
            .await
    });
    let peer = ().serve(client_transport).await?;
    let server_peer = server.await??;
    let tools = peer.list_tools(None).await?;

    for name in ["append_logs", "get_logs", "list_logs"] {
        assert!(tools.tools.iter().any(|tool| tool.name == name));
    }

    let appended = peer
        .call_tool(
            CallToolRequestParams::new("append_logs")
                .with_arguments(batch(vec![event(1)]).as_object().unwrap().clone()),
        )
        .await?;
    let receipt = appended.structured_content.unwrap();
    assert_eq!(receipt["inserted"], 1);
    assert!(receipt.get("events").is_none());
    let fetched = peer
        .call_tool(
            CallToolRequestParams::new("get_logs")
                .with_arguments(json!({"ids":[event(1)["id"]]}).as_object().unwrap().clone()),
        )
        .await?;
    assert_eq!(
        fetched.structured_content.unwrap()["events"][0]["system_created_by"],
        "mcp actor"
    );
    let socket = directory.path().join("service.sock");
    let listener = tokio::net::UnixListener::bind(&socket)?;
    let router = semmem::server::local_router(Arc::clone(&service));
    let http_server = tokio::spawn(async move { axum::serve(listener, router).await });
    let cli_socket = socket.clone();
    let input = batch(vec![event(2)]).to_string();
    let output = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut child = Command::new(env!("CARGO_BIN_EXE_semmem"))
            .arg("--socket")
            .arg(cli_socket)
            .arg("append_logs")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    })
    .await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout)?["inserted"],
        1
    );
    let http = reqwest::Client::builder()
        .unix_socket(socket.as_path())
        .no_proxy()
        .build()?;
    let fetched: Value = http
        .post("http://localhost/api/get_logs")
        .json(&json!({"ids":[event(2)["id"]]}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(fetched["events"][0]["system_created_by"], "human");

    for arguments in [
        vec!["get_logs", "00000000-0000-0000-0000-000000000002"],
        vec![
            "list_logs",
            "--source",
            "Atlas",
            "--level",
            "info",
            "--limit",
            "1",
        ],
    ] {
        let cli_socket = socket.clone();
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_semmem"))
                .arg("--socket")
                .arg(cli_socket)
                .args(arguments)
                .output()
                .unwrap()
        })
        .await?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout)?["events"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    assert_eq!(semmem::auth::operation_scope("append_logs"), "memory:write");
    assert_eq!(semmem::auth::operation_scope("get_logs"), "memory:read");
    assert_eq!(semmem::auth::operation_scope("list_logs"), "memory:read");
    peer.cancel().await?;
    server_peer.cancel().await?;
    http_server.abort();

    Ok(())
}
