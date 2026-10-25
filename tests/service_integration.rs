use std::sync::{Arc, Barrier};

use semmem::models::CreateMemory;
use semmem::service::Service;
use semmem::storage::Store;
use serde_json::json;

#[test]
fn concurrent_clients_conflict_without_overwriting_and_publish_immediately() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path())?;
    service.dispatch(
        "create_memories",
        json!({"memories": [{"key":"shared", "body":"Original memory"}]}),
        "human",
    )?;
    let barrier = Arc::new(Barrier::new(16));
    let workers = (0..16).map(|index| {
        let service = Arc::clone(&service);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            service.dispatch("update_memories", json!({"memories": [{"key":"shared", "expected_version":1,"body":format!("winner{index} publication")}]}), "ai client")
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
            .all(|error| error.code == "version_conflict")
    );
    let result = service.dispatch("search_memories", json!({"query":"publication"}), "human")?;
    assert_eq!(result["hits"][0]["version"], 2);

    Ok(())
}

#[test]
fn startup_rebuilds_index_and_replays_unpublished_sqlite_commits() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("memory.sqlite3");
    {
        let mut store = Store::open(&database)?;
        store.create_memories(&[CreateMemory {
            key: "recovered".into(),
            title: "Recovery".into(),
            body: "durability recovery words".into(),
            kind: "note".into(),
            tags: vec![],
            links: vec![],
            actor: "human".into(),
            metadata: Default::default(),
        }])?;
    }
    let service = Service::open(directory.path())?;
    let found = service.dispatch("search_memories", json!({"query":"recovery"}), "human")?;
    assert_eq!(found["hits"][0]["key"], "recovered");
    assert!(Service::open(directory.path()).is_err());
    service.dispatch(
        "delete_memories",
        json!({"memories":[{"key":"recovered","expected_version":1}]}),
        "human",
    )?;
    assert_eq!(
        service.dispatch("search_memories", json!({"query":"recovery"}), "human")?["hits"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    service.dispatch(
        "restore_memories",
        json!({"memories":[{"key":"recovered","expected_version":2}]}),
        "human",
    )?;
    assert_eq!(
        service.dispatch("search_memories", json!({"query":"recovery"}), "human")?["hits"][0]["version"],
        3
    );

    Ok(())
}

#[test]
fn create_retry_is_idempotent_and_multiwrite_failure_is_atomic() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path())?;
    let request = json!({"memories":[{"key":"repeat","body":"exact bytes\n"}]});
    let first = service.dispatch("create_memories", request.clone(), "human")?;
    let second = service.dispatch("create_memories", request, "human")?;
    assert_eq!(first, second);
    let result = service.dispatch(
        "create_memories",
        json!({"memories":[{"key":"new","body":"new"},{"key":"repeat","body":"different"}]}),
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
fn service_accepts_large_batches_and_returns_summaries_until_full_content_is_requested()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path())?;
    let memories = (0..1001)
        .map(|index| json!({"key":format!("record{index}"),"body":"preserved Markdown"}))
        .collect::<Vec<_>>();
    let created = service.dispatch("create_memories", json!({"memories":memories}), "human")?;
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
    assert!(summaries["memories"][0].get("body").is_none());
    let arbitrary = service.dispatch("create_memories", json!({"memories":[{"key":"arbitrary","body":"text","metadata":{"freeform":"unsupported"}}]}), "human").unwrap_err();
    assert_eq!(arbitrary.code, "invalid_input");
    let created = service.dispatch(
        "create_memories",
        json!({"memories":[{"key":"summary","body":"# Preserved Markdown\n"}]}),
        "human",
    )?;
    assert!(created["memories"][0].get("body").is_none());
    let listed = service.dispatch("list_memories", json!({}), "human")?;
    assert!(listed["memories"][0].get("body").is_none());
    let complete = service.dispatch("get_memories", json!({"keys":["summary"]}), "human")?;
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
fn metadata_survives_staged_reads_updates_and_historical_restoration() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path())?;
    let metadata = json!({
        "key": "metadata",
        "body": "# Full record\n\nExact original Markdown.\n",
        "summary": "Manually supplied retrieval summary.",
        "claim_type": "verified observation",
        "scope": "project semantic memory",
        "observed_at": 1000,
        "verified_at": 2000,
        "review_after": 3000
    });
    service.dispatch("create_memories", json!({"memories": [metadata]}), "human")?;
    let compact = service.dispatch(
        "get_memories",
        json!({"keys": ["metadata"], "view": "summary"}),
        "human",
    )?;
    assert!(compact["memories"][0].get("body").is_none());
    assert_eq!(compact["memories"][0]["summary"], metadata["summary"]);
    assert_eq!(compact["memories"][0]["scope"], metadata["scope"]);
    let full = service.dispatch("get_memories", json!({"keys": ["metadata"]}), "human")?;
    assert_eq!(full["memories"][0]["body"], metadata["body"]);
    service.dispatch("update_memories", json!({"memories": [{"key": "metadata", "expected_version": 1, "summary": null, "verified_at": null}]}), "human")?;
    let updated = service.dispatch(
        "get_memories",
        json!({"keys": ["metadata"], "view": "summary"}),
        "human",
    )?;
    assert!(updated["memories"][0]["summary"].is_null());
    assert!(updated["memories"][0]["verified_at"].is_null());
    assert_eq!(updated["memories"][0]["scope"], metadata["scope"]);
    service.dispatch(
        "restore_memories",
        json!({"memories": [{"key": "metadata", "expected_version": 2, "revision": 1}]}),
        "human",
    )?;
    let restored = service.dispatch("get_memories", json!({"keys": ["metadata"]}), "human")?;
    for field in [
        "body",
        "summary",
        "claim_type",
        "scope",
        "observed_at",
        "verified_at",
        "review_after",
    ] {
        assert_eq!(restored["memories"][0][field], metadata[field]);
    }

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
        .json(&json!({"memories": [{"key": "large", "body": body, "summary": "Small manual summary"}]}))
        .send().await?.error_for_status()?.json().await?;
    assert!(response["memories"][0].get("body").is_none());
    let compact: serde_json::Value = client
        .post(format!("{address}/api/get_memories"))
        .json(&json!({"keys": ["large"], "view": "summary"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(compact["memories"][0]["summary"], "Small manual summary");
    assert!(compact["memories"][0].get("body").is_none());
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
