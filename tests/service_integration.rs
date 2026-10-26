use std::sync::{Arc, Barrier};

use semmem::models::CreateMemory;
use semmem::service::Service;
use semmem::storage::Store;
use serde_json::json;

#[test]
fn concurrent_clients_conflict_without_overwriting_and_publish_immediately() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path())?;
    let created = service.dispatch(
        "create_memories",
        json!({"commit_summary":"Test change", "memories": [{"key":"shared", "body":"Original memory"}]}),
        "human",
    )?;
    let token = created["memories"][0]["system_token"].as_str().unwrap().to_owned();
    let barrier = Arc::new(Barrier::new(16));
    let workers = (0..16).map(|index| {
        let service = Arc::clone(&service);
        let barrier = Arc::clone(&barrier);
        let token = token.clone();
        std::thread::spawn(move || {
            barrier.wait();
            service.dispatch(
                "update_memories",
                json!({"commit_summary":"Test change", "memories": [{"key":"shared", "expected_token":token,"body":format!("winner{index} publication")}]}),
                "ai client",
            )
        })
    }).collect::<Vec<_>>();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .all(|error| error.code == "token_conflict")
    );
    let result = service.dispatch(
        "search_memories",
        json!({"query":"publication"}),
        "human",
    )?;
    assert_ne!(result["hits"][0]["system_token"], token);

    Ok(())
}

#[test]
fn startup_rebuilds_index_and_replays_unpublished_sqlite_commits() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("memory.sqlite3");
    {
        let mut store = Store::open(&database)?;
        store.create_memories(
            &[CreateMemory {
                key: "recovered".into(),
                title: "Recovery".into(),
                body: "durability recovery words".into(),
                r#type: "general".into(),
                tags: vec![],
                references: vec![],
                metadata: Default::default(),
            }],
            "human",
            "Prepare recovery fixture",
        )?;
    }
    let service = Service::open(directory.path())?;
    let found = service.dispatch(
        "search_memories",
        json!({"query":"recovery"}),
        "human",
    )?;
    assert_eq!(found["hits"][0]["key"], "recovered");
    assert!(Service::open(directory.path()).is_err());
    service.dispatch(
        "delete_memories",
        json!({"commit_summary":"Test change", "memories":[{"key":"recovered","expected_token":found["hits"][0]["system_token"]}]}),
        "human",
    )?;
    assert_eq!(
        service.dispatch("search_memories", json!({"query":"recovery"}), "human")?["hits"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    Ok(())
}

#[test]
fn create_retry_is_idempotent_and_multiwrite_failure_is_atomic() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path())?;
    let request =
        json!({"commit_summary":"Test change", "memories":[{"key":"repeat","body":"exact bytes\n"}]});
    let first = service.dispatch(
        "create_memories",
        request.clone(),
        "human",
    )?;
    let second = service.dispatch(
        "create_memories",
        request,
        "human",
    )?;
    assert_eq!(first["memories"], second["memories"]);
    let result = service.dispatch(
        "create_memories",
        json!({"commit_summary":"Test change", "memories":[{"key":"new","body":"new"},{"key":"repeat","body":"different"}]}),
        "human",
    );
    assert!(result.is_err());
    assert!(
        service
            .dispatch("get_memories", json!({"keys":["new"]}), "human")
            .is_err()
    );

    Ok(())
}

#[test]
fn service_accepts_large_batches_and_always_returns_full_content() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path())?;
    let memories = (0..1001)
        .map(|index| json!({"key":format!("record{index}"),"body":"preserved Markdown"}))
        .collect::<Vec<_>>();
    let created = service.dispatch(
        "create_memories",
        json!({"commit_summary":"Test change", "memories":memories}),
        "human",
    )?;
    assert_eq!(created["memories"].as_array().unwrap().len(), 1001);
    let keys = (0..1001)
        .map(|index| format!("record{index}"))
        .collect::<Vec<_>>();
    let summaries = service.dispatch(
        "get_memories",
        json!({"keys": keys, "view": "summary"}),
        "human",
    )?;
    assert_eq!(summaries["memories"].as_array().unwrap().len(), 1001);
    assert!(summaries["memories"][0].get("body").is_some());
    let arbitrary = service.dispatch(
        "create_memories",
        json!({"commit_summary":"Test change", "memories":[{"key":"arbitrary","body":"text","metadata":{"freeform":"unsupported"}}]}),
        "human",
    ).unwrap_err();
    assert_eq!(arbitrary.code, "invalid_input");
    let created = service.dispatch(
        "create_memories",
        json!({"commit_summary":"Test change", "memories":[{"key":"summary","body":"# Preserved Markdown\n"}]}),
        "human",
    )?;
    assert!(created["memories"][0].get("body").is_some());
    let listed = service.dispatch(
        "list_memories",
        json!({}),
        "human",
    )?;
    assert!(listed["memories"][0].get("body").is_some());
    let complete = service.dispatch(
        "get_memories",
        json!({"keys":["summary"]}),
        "human",
    )?;
    assert_eq!(complete["memories"][0]["body"], "# Preserved Markdown\n");
    assert_eq!(
        service
            .dispatch("search_memories", json!({"query":"\"unfinished"}), "human")
            .unwrap_err()
            .code,
        "invalid_input"
    );

    Ok(())
}

#[test]
fn metadata_survives_current_reads_and_updates() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path())?;
    let metadata = json!({
        "key": "metadata",
        "body": "# Full record\n\nExact original Markdown.\n",
        "summary": "Manually supplied retrieval summary.",
        "basis": "observed",
        "scope": "project semantic memory",
        "observed_at": 1000,
        "verified_at": 2000,
        "review_after": 3000
    });
    service.dispatch(
        "create_memories",
        json!({"commit_summary":"Test change", "memories": [metadata]}),
        "human",
    )?;
    let compact = service.dispatch(
        "get_memories",
        json!({"keys": ["metadata"], "view": "summary"}),
        "human",
    )?;
    assert!(compact["memories"][0].get("body").is_some());
    assert_eq!(compact["memories"][0]["summary"], metadata["summary"]);
    assert_eq!(compact["memories"][0]["scope"], metadata["scope"]);
    let full = service.dispatch(
        "get_memories",
        json!({"keys": ["metadata"]}),
        "human",
    )?;
    assert_eq!(full["memories"][0]["body"], metadata["body"]);
    service.dispatch(
        "update_memories",
        json!({"commit_summary":"Test change", "memories": [{"key": "metadata", "expected_token": full["memories"][0]["system_token"], "summary": null, "verified_at": null}]}),
        "human",
    )?;
    let updated = service.dispatch(
        "get_memories",
        json!({"keys": ["metadata"], "view": "summary"}),
        "human",
    )?;
    assert!(updated["memories"][0]["summary"].is_null());
    assert!(updated["memories"][0]["verified_at"].is_null());
    assert_eq!(updated["memories"][0]["scope"], metadata["scope"]);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_transport_accepts_requests_and_full_responses_above_former_body_caps()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path())?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = format!("http://{}", listener.local_addr()?);
    let router = semmem::server::local_router(service);
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let client = reqwest::Client::new();
    let body = format!("largepayloadneedle\n\n{}", " ".repeat(5 * 1024 * 1024));
    let response: serde_json::Value = client
        .post(format!("{address}/api/create_memories"))
        .json(&json!({"commit_summary":"Test change", "memories": [{"key": "large", "body": body, "summary": "Small manual summary"}]}))
        .send().await?.error_for_status()?.json().await?;
    assert!(response["memories"][0].get("body").is_some());
    let compact: serde_json::Value = client
        .post(format!("{address}/api/get_memories"))
        .json(&json!({"keys": ["large"], "view": "summary"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(compact["memories"][0]["summary"], "Small manual summary");
    assert!(compact["memories"][0].get("body").is_some());
    let complete: serde_json::Value = client
        .post(format!("{address}/api/get_memories"))
        .json(&json!({"keys": ["large"]}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(complete["memories"][0]["body"], body);
    server.abort();

    Ok(())
}

#[test]
fn current_state_rejects_retired_inputs_and_requires_audit_messages() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path())?;
    let invalid = [
        ("create_memories", json!({"memories": [{"key": "missing", "body": "body"}]})),
        ("create_memories", json!({"commit_summary": "  ", "memories": [{"key": "blank", "body": "body"}]})),
        ("create_memories", json!({"commit_summary": "Create", "memories": [{"key": "author", "body": "body", "author": "Ada"}]})),
        ("search_memories", json!({"query": "word", "history": true})),
        ("search_memories", json!({"query": "word", "as_of": 0})),
        ("search_memories", json!({"query": "word", "include_deleted": true})),
        ("get_memories", json!({"keys": ["word"], "version": 1})),
        ("create_type", json!({"key": "missing", "name": "Missing"})),
        ("delete_tag", json!({"key": "missing"})),
    ];
    for (operation, arguments) in invalid {
        assert_eq!(service.dispatch(operation, arguments, "tester").unwrap_err().code, "invalid_input");
    }
    let created = service.dispatch("create_memories", json!({"commit_summary": "Record original", "memories": [{"key": "current", "body": "originalneedle", "summary": "Memory summary"}]}), "first writer")?;
    let first = &created["memories"][0];
    assert!(first.get("author").is_none());
    assert!(first.get("system_version").is_none());
    let updated = service.dispatch("update_memories", json!({"commit_summary": "Replace original", "memories": [{"key": "current", "expected_token": first["system_token"], "body": "replacementneedle"}]}), "second writer")?;
    assert_eq!(service.dispatch("search_memories", json!({"query": "originalneedle", "fuzzy": false}), "reader")?["total"], 0);
    let commits = service.dispatch("get_commits", json!({"ids": [updated["commit_id"]]}), "reader")?;
    assert_eq!(commits["commits"][0]["summary"], "Replace original");
    assert_eq!(commits["commits"][0]["actor"], "second writer");
    let deleted = service.dispatch("delete_memories", json!({"commit_summary": "Remove current", "memories": [{"key": "current", "expected_token": updated["memories"][0]["system_token"]}]}), "deleter")?;
    assert_eq!(deleted["memories"][0]["key"], "current");
    assert_eq!(deleted["memories"][0].as_object().unwrap().len(), 2);
    assert!(deleted["memories"][0].get("body").is_none());
    assert!(deleted["memories"][0].get("summary").is_none());
    assert!(service.dispatch("get_memories", json!({"keys": ["current"]}), "reader").is_err());
    for operation in ["list_memory_revisions", "restore_memories", "purge_memories", "list_authors", "register_author", "deregister_author"] {
        assert!(service.dispatch(operation, json!({}), "reader").is_err());
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn startup_refuses_derived_index_symlinks_and_preserves_external_files() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir()?;
    let external = tempfile::tempdir()?;
    std::fs::write(external.path().join("preserved"), b"unrelated contents")?;
    for name in ["search", "search.identity"] {
        let data = directory.path().join(name.replace('.', "_"));
        std::fs::create_dir(&data)?;
        let target = if name == "search" { external.path().to_path_buf() } else { external.path().join("preserved") };
        symlink(target, data.join(name))?;
        let result = Service::open(&data);
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("symbolic link"));
        assert_eq!(std::fs::read(external.path().join("preserved"))?, b"unrelated contents");
    }

    Ok(())
}
