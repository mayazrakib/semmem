use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use semmem::service::Service;
use serde_json::json;

fn reopen(path: &Path) -> Result<Arc<Service>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match Service::open(path) {
            Ok(service) => return Ok(service),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::yield_now(),
        }
    }
}

#[test]
fn overwritten_and_deleted_content_is_absent_after_reopen_and_index_rebuild() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path())?;
    let created = service.dispatch("create_memories", json!({"commit_summary":"Create linked current records.","memories":[{"key":"target","body":"deletedneedle"},{"key":"source","body":"obsoleteneedle","references":[{"target_key":"target","reference_type":"related"}]}]}), "test actor")?;
    let target_token = created["memories"][0]["system_token"].as_str().unwrap();
    let source_token = created["memories"][1]["system_token"].as_str().unwrap();
    service.dispatch("update_memories", json!({"commit_summary":"Replace obsolete content.","memories":[{"key":"source","expected_token":source_token,"body":"currentneedle"}]}), "test actor")?;
    service.dispatch("delete_memories", json!({"commit_summary":"Delete target and remove incoming references.","memories":[{"key":"target","expected_token":target_token}]}), "test actor")?;
    for query in ["obsoleteneedle", "deletedneedle"] {
        let response = service.dispatch(
            "search_memories",
            json!({"query":query,"fuzzy":false}),
            "test actor",
        )?;
        assert!(response["hits"].as_array().unwrap().is_empty());
    }
    assert!(
        service
            .dispatch(
                "search_memories",
                json!({"query":"obsoleteneedle","history":true}),
                "test actor"
            )
            .is_err()
    );
    assert!(
        service
            .dispatch(
                "search_memories",
                json!({"query":"obsoleteneedle","as_of":1}),
                "test actor"
            )
            .is_err()
    );
    drop(service);
    let reopened = reopen(directory.path())?;
    let current = reopened.dispatch(
        "search_memories",
        json!({"query":"currentneedle","fuzzy":false}),
        "test actor",
    )?;
    assert_eq!(current["hits"].as_array().unwrap().len(), 1);
    assert_eq!(current["hits"][0]["references"], json!([]));
    drop(reopened);
    std::fs::write(
        directory.path().join("search.identity"),
        "force fixture rebuild",
    )?;
    let rebuilt = reopen(directory.path())?;
    let current = rebuilt.dispatch(
        "search_memories",
        json!({"query":"currentneedle","fuzzy":false}),
        "test actor",
    )?;
    assert_eq!(current["hits"].as_array().unwrap().len(), 1);
    assert_eq!(current["hits"][0]["references"], json!([]));
    for query in ["obsoleteneedle", "deletedneedle"] {
        assert!(
            rebuilt.dispatch(
                "search_memories",
                json!({"query":query,"fuzzy":false}),
                "test actor"
            )?["hits"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    Ok(())
}
