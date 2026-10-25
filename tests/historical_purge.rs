use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use semmem::models::SearchRequest;
use semmem::search::SearchIndex;
use semmem::service::Service;
use serde_json::{Value, json};

const ORIGINAL_BODY: &str =
    "# Historical Record\n\narchiveneedle original café 東京.\n\nComplete trailing context.\n";
const CURRENT_BODY: &str =
    "# Current Record\n\ncurrentneedle revised café 東京.\n\nComplete trailing context.\n";

fn search(service: &Service, arguments: Value) -> Result<Value> {
    Ok(service.dispatch("search_memories", arguments, "test writer")?)
}

fn assert_temporal_results(service: &Service, original_at: i64) -> Result<()> {
    let current = search(service, json!({"query": "currentneedle", "fuzzy": false}))?;
    assert_eq!(current["hits"][0]["body"], CURRENT_BODY);
    assert_eq!(current["hits"][0]["version"], 2);
    assert!(current["hits"][0]["system_created_at"].is_i64());
    assert!(current["hits"][0]["system_updated_at"].is_i64());
    assert!(
        search(service, json!({"query": "archiveneedle", "fuzzy": false}))?["hits"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let historical = search(
        service,
        json!({"query": "archiveneedle", "fuzzy": false, "history": true}),
    )?;
    assert_eq!(historical["hits"][0]["body"], ORIGINAL_BODY);
    assert_eq!(historical["hits"][0]["version"], 1);
    assert_eq!(historical["hits"][0]["references"], json!([]));
    let snapshot = search(
        service,
        json!({"query": "archiveneedle", "fuzzy": false, "as_of": original_at}),
    )?;
    assert_eq!(snapshot["hits"][0]["body"], ORIGINAL_BODY);
    assert_eq!(snapshot["hits"][0]["version"], 1);
    assert_eq!(snapshot["hits"][0]["references"], json!([]));
    assert!(search(service, json!({"query": "purgedtargetneedle", "fuzzy": false, "history": true, "include_deleted": true}))?["hits"].as_array().unwrap().is_empty());

    Ok(())
}

fn open_index_after_shutdown(path: &Path) -> Result<SearchIndex> {
    let deadline = Instant::now() + Duration::from_secs(5);

    loop {
        match SearchIndex::open(path) {
            Ok(index) => return Ok(index),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

fn reopen_service(path: &Path) -> Result<Arc<Service>> {
    let deadline = Instant::now() + Duration::from_secs(5);

    loop {
        match Service::open(path) {
            Ok(service) => return Ok(service),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

#[test]
fn purge_removes_historical_only_references_from_published_and_rebuilt_indexes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path())?;
    service.dispatch(
        "create_memories",
        json!({"memories": [{"key": "target", "author": "test author", "body": "purgedtargetneedle target contents"}]}),
        "test writer",
    )?;
    let original = service.dispatch(
        "create_memories",
        json!({"memories": [{
            "key": "source",
            "author": "test author",
            "body": ORIGINAL_BODY,
            "summary": "A manual summary must not replace the full body.",
            "references": [{"target_key": "target", "reference_type": "related to"}]
        }]}),
        "test writer",
    )?;
    let original_at = original["memories"][0]["system_updated_at"]
        .as_i64()
        .context("Missing revision timestamp")?;
    std::thread::sleep(Duration::from_millis(2));
    service.dispatch(
        "update_memories",
        json!({"memories": [{"key": "source", "expected_version": 1, "body": CURRENT_BODY, "references": []}]}),
        "test writer",
    )?;
    let before = search(
        &service,
        json!({"query": "archiveneedle", "fuzzy": false, "history": true}),
    )?;
    assert_eq!(before["hits"][0]["references"][0]["target_key"], "target");
    service.dispatch(
        "purge_memories",
        json!({"memories": [{"key": "target", "expected_version": 1}]}),
        "test writer",
    )?;
    assert_temporal_results(&service, original_at)?;
    drop(service);
    {
        let index = open_index_after_shutdown(&directory.path().join("search"))?;
        let request: SearchRequest = serde_json::from_value(
            json!({"query": "archiveneedle", "fuzzy": false, "history": true}),
        )?;
        let raw = index.search(&request)?;
        assert_eq!(raw.hits.len(), 1);
        assert_eq!(raw.hits[0].body, ORIGINAL_BODY);
        assert!(raw.hits[0].references.is_empty());
    }
    let service = reopen_service(directory.path())?;
    assert_temporal_results(&service, original_at)?;
    drop(service);
    drop(open_index_after_shutdown(&directory.path().join("search"))?);
    std::fs::write(
        directory.path().join("search.identity"),
        "force fixture rebuild",
    )?;
    let rebuilt = reopen_service(directory.path())?;
    assert_temporal_results(&rebuilt, original_at)?;

    Ok(())
}
