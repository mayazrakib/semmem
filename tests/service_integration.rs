use std::sync::{Arc, Barrier};

use semantic_memory::models::CreateMemory;
use semantic_memory::service::Service;
use semantic_memory::storage::Store;
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
fn service_bounds_batches_and_returns_summaries_until_full_content_is_requested()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path())?;
    let memories = (0..65)
        .map(|index| json!({"key":format!("record{index}"),"body":"preserved Markdown"}))
        .collect::<Vec<_>>();
    let error = service
        .dispatch("create_memories", json!({"memories":memories}), "human")
        .unwrap_err();
    assert_eq!(error.code, "invalid_input");
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
