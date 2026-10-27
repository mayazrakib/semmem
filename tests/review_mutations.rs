use std::sync::{Arc, Barrier};

use rusqlite::Connection;
use semmem::service::Service;
use serde_json::{Value, json};

fn create_request_id(value: u128) -> String {
    uuid::Uuid::from_u128(value).to_string()
}

fn dispatch(
    service: &Service,
    operation: &str,
    arguments: Value,
) -> Value {
    service.dispatch(operation, arguments, "actor").unwrap()
}

fn create_request(
    value: u128,
    key: &str,
) -> Value {
    json!({"request_id":create_request_id(value),"commit_summary":"Create a memory.","memories":[{"key":key,"body":"reviewneedle private body","review_after":100,"verified_at":null}]})
}

fn read_memory(
    service: &Service,
    key: &str,
) -> Value {
    dispatch(service, "get_memories", json!({"keys":[key]}))["memories"][0].clone()
}

fn count_domain_commits(service: &Service) -> usize {
    dispatch(service, "list_commits", json!({"limit":1000}))["commits"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|commit| {
            ["create_memories", "update_memories", "delete_memories"]
                .contains(&commit["operation"].as_str().unwrap())
        })
        .count()
}

#[test]
fn review_filters_preserve_boundaries_composition_scores_and_pagination() {
    let directory = tempfile::tempdir().unwrap();
    let service = Service::open(directory.path()).unwrap();
    dispatch(
        &service,
        "create_memories",
        json!({"commit_summary":"Create review fixtures.","memories":[
            {"key":"past","body":"reviewneedle","scope":"project","tags":["review"],"review_after":99,"verified_at":null},
            {"key":"equal","body":"reviewneedle","scope":"project","tags":["review"],"review_after":100,"verified_at":null},
            {"key":"future","body":"reviewneedle","scope":"project","tags":["review"],"review_after":101,"verified_at":null},
            {"key":"undated","body":"reviewneedle","scope":"project","tags":["review"],"review_after":null,"verified_at":null},
            {"key":"verified","body":"reviewneedle","scope":"project","tags":["review"],"review_after":99,"verified_at":0},
            {"key":"other","body":"reviewneedle","scope":"other","review_after":99,"verified_at":null},
            {"key":"minimum","body":"reviewneedle","review_after":i64::MIN,"verified_at":null},
            {"key":"maximum","body":"reviewneedle","review_after":i64::MAX,"verified_at":1}
        ]}),
    );
    let arguments = json!({"query":"reviewneedle","fuzzy":false,"review_due_by":100,"is_verified":false,"scope":"project","tags":["review"],"limit":1});
    let search = dispatch(&service, "search_memories", arguments);
    assert_eq!(search["total"], 2);
    assert_eq!(search["hits"].as_array().unwrap().len(), 1);
    assert_eq!(search["truncated"], true);
    let baseline = dispatch(
        &service,
        "search_memories",
        json!({"query":"reviewneedle","fuzzy":false,"scope":"project","tags":["review"]}),
    );
    let selected = &search["hits"][0];
    let original = baseline["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["key"] == selected["key"])
        .unwrap();
    assert_eq!(selected["score"], original["score"]);
    let first = dispatch(
        &service,
        "list_memories",
        json!({"review_due_by":100,"is_verified":false,"scope":"project","tags":["review"],"limit":1}),
    );
    let second = dispatch(
        &service,
        "list_memories",
        json!({"review_due_by":100,"is_verified":false,"scope":"project","tags":["review"],"limit":1,"cursor":first["next_cursor"]}),
    );
    assert_ne!(first["memories"][0]["key"], second["memories"][0]["key"]);
    let end = dispatch(
        &service,
        "list_memories",
        json!({"review_due_by":100,"is_verified":false,"scope":"project","tags":["review"],"limit":1,"cursor":second["next_cursor"]}),
    );
    assert!(end["memories"].as_array().unwrap().is_empty());

    for (cutoff, count) in [(i64::MIN, 1), (100, 5), (i64::MAX, 7)] {
        let search = dispatch(
            &service,
            "search_memories",
            json!({"query":"reviewneedle","review_due_by":cutoff}),
        );
        let list = dispatch(&service, "list_memories", json!({"review_due_by":cutoff}));
        assert_eq!(search["total"], count);
        assert_eq!(list["memories"].as_array().unwrap().len(), count as usize);
    }

    let verified = dispatch(
        &service,
        "search_memories",
        json!({"query":"reviewneedle","is_verified":true}),
    );
    assert_eq!(verified["total"], 2);
    let never_verified = dispatch(&service, "list_memories", json!({"is_verified":false}));
    assert_eq!(never_verified["memories"].as_array().unwrap().len(), 6);
    assert_eq!(count_domain_commits(&service), 1);
    assert_eq!(read_memory(&service, "equal")["verified_at"], Value::Null);
}

#[test]
fn receipts_survive_edits_deletion_recreation_and_restart_without_reapplying() {
    let directory = tempfile::tempdir().unwrap();
    let service = Service::open(directory.path()).unwrap();
    let create = create_request(1, "memory");
    let created = dispatch(&service, "create_memories", create.clone());
    assert!(created["memories"][0].get("body").is_none());
    let update = json!({"request_id":create_request_id(2),"commit_summary":"Update a memory.","memories":[{"key":"memory","expected_token":created["memories"][0]["system_token"],"body":"Updated reviewneedle","review_after":null,"verified_at":123}]});
    let updated = dispatch(&service, "update_memories", update.clone());
    assert_ne!(
        created["memories"][0]["system_token"],
        updated["memories"][0]["system_token"]
    );
    assert_eq!(
        dispatch(
            &service,
            "search_memories",
            json!({"query":"reviewneedle","review_due_by":i64::MAX})
        )["total"],
        0
    );
    assert_eq!(
        dispatch(
            &service,
            "search_memories",
            json!({"query":"reviewneedle","is_verified":true})
        )["total"],
        1
    );
    let deletion = json!({"request_id":create_request_id(3),"commit_summary":"Delete a memory.","memories":[{"key":"memory","expected_token":updated["memories"][0]["system_token"]}]});
    let deleted = dispatch(&service, "delete_memories", deletion.clone());
    assert_eq!(
        deleted["memories"][0]["system_token"],
        updated["memories"][0]["system_token"]
    );
    assert_eq!(
        dispatch(&service, "delete_memories", deletion.clone()),
        deleted
    );
    let recreated = dispatch(
        &service,
        "create_memories",
        json!({"commit_summary":"Recreate a memory.","memories":[{"key":"memory","body":"New current state"}]}),
    );
    let current = recreated["memories"][0].clone();

    for (operation, arguments, receipt) in [
        ("create_memories", create.clone(), created.clone()),
        ("update_memories", update.clone(), updated.clone()),
        ("delete_memories", deletion.clone(), deleted.clone()),
    ] {
        assert_eq!(dispatch(&service, operation, arguments), receipt);
        assert_eq!(read_memory(&service, "memory"), current);
    }

    assert_eq!(count_domain_commits(&service), 4);
    let recovered = dispatch(
        &service,
        "get_mutation_receipts",
        json!({"request_ids":[create_request_id(3),create_request_id(1),create_request_id(2)]}),
    );
    assert_eq!(recovered["receipts"], json!([deleted, created, updated]));
    drop(service);
    let ownership = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.path().join("service.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&ownership).unwrap();
    fs2::FileExt::unlock(&ownership).unwrap();
    drop(ownership);
    let service = Service::open(directory.path()).unwrap();
    assert_eq!(dispatch(&service, "update_memories", update), updated);
    assert_eq!(read_memory(&service, "memory"), current);
    let connection = Connection::open(directory.path().join("memory.sqlite3")).unwrap();
    let stored: Vec<String> = connection
        .prepare("SELECT receipt FROM mutation_receipts")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        stored
            .iter()
            .all(|receipt| !receipt.contains("private body")
                && !receipt.contains("Updated reviewneedle")
                && !receipt.contains("body"))
    );
}

#[test]
fn failed_batches_do_not_reserve_ids_and_new_ids_still_require_current_tokens() {
    let directory = tempfile::tempdir().unwrap();
    let service = Service::open(directory.path()).unwrap();
    let mut request = create_request(1, "memory");
    request["memories"][0]["references"] =
        json!([{"target_key":"missing","reference_type":"related"}]);
    assert!(
        service
            .dispatch("create_memories", request.clone(), "actor")
            .is_err()
    );
    assert!(
        service
            .dispatch(
                "get_mutation_receipts",
                json!({"request_ids":[create_request_id(1)]}),
                "actor"
            )
            .is_err()
    );
    assert_eq!(count_domain_commits(&service), 0);
    request["memories"][0]["references"] = json!([]);
    let created = dispatch(&service, "create_memories", request);
    let old_token = created["memories"][0]["system_token"].clone();
    let update = json!({"request_id":create_request_id(2),"commit_summary":"Edit.","memories":[{"key":"memory","expected_token":old_token,"body":"Changed"}]});
    dispatch(&service, "update_memories", update.clone());
    let mut stale = update;
    stale["request_id"] = json!(create_request_id(3));
    assert_eq!(
        service
            .dispatch("update_memories", stale, "actor")
            .unwrap_err()
            .code,
        "token_conflict"
    );
    assert!(
        service
            .dispatch(
                "get_mutation_receipts",
                json!({"request_ids":[create_request_id(3)]}),
                "actor"
            )
            .is_err()
    );
    assert_eq!(count_domain_commits(&service), 2);
}

#[test]
fn fingerprints_preserve_metadata_omission_null_arrays_and_operation() {
    let directory = tempfile::tempdir().unwrap();
    let service = Service::open(directory.path()).unwrap();
    let mut request = create_request(1, "memory");
    request["memories"][0]["tags"] = json!(["a", "b"]);
    let receipt = dispatch(&service, "create_memories", request.clone());
    let reordered: Value = serde_json::from_str(&serde_json::to_string(&request).unwrap()).unwrap();
    assert_eq!(dispatch(&service, "create_memories", reordered), receipt);

    for altered in [
        {
            let mut value = request.clone();
            value["commit_summary"] = json!("Different summary");
            value
        },
        {
            let mut value = request.clone();
            value["memories"][0]["tags"] = json!(["b", "a"]);
            value
        },
        {
            let mut value = request.clone();
            value["memories"][0]
                .as_object_mut()
                .unwrap()
                .remove("verified_at");
            value
        },
    ] {
        assert_eq!(
            service
                .dispatch("create_memories", altered, "actor")
                .unwrap_err()
                .code,
            "conflict"
        );
    }

    let update = json!({"request_id":create_request_id(1),"commit_summary":"Create a memory.","memories":[{"key":"memory","expected_token":receipt["memories"][0]["system_token"]}]});
    assert_eq!(
        service
            .dispatch("update_memories", update, "actor")
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(count_domain_commits(&service), 1);
}

#[test]
fn actor_isolation_noop_receipts_and_canonical_request_ids() {
    let directory = tempfile::tempdir().unwrap();
    let service = Service::open(directory.path()).unwrap();
    let request = create_request(1, "memory");
    let created = dispatch(&service, "create_memories", request.clone());
    assert_eq!(
        service
            .dispatch(
                "get_mutation_receipts",
                json!({"request_ids":[create_request_id(1)]}),
                "other"
            )
            .unwrap_err()
            .code,
        "not_found"
    );
    let other = service
        .dispatch("create_memories", request, "other")
        .unwrap();
    assert_eq!(other["commit_id"], Value::Null);
    assert_eq!(other["memories"], created["memories"]);
    let no_op = json!({"request_id":create_request_id(2),"commit_summary":"Confirm unchanged content.","memories":[{"key":"memory","expected_token":created["memories"][0]["system_token"]}]});
    let receipt = dispatch(&service, "update_memories", no_op.clone());
    assert_eq!(receipt["commit_id"], Value::Null);
    dispatch(
        &service,
        "update_memories",
        json!({"commit_summary":"Change content.","memories":[{"key":"memory","expected_token":created["memories"][0]["system_token"],"body":"Changed"}]}),
    );
    assert_eq!(dispatch(&service, "update_memories", no_op), receipt);
    assert_eq!(count_domain_commits(&service), 2);

    for invalid in [
        "",
        "not-a-uuid",
        "00000000000000000000000000000001",
        "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA",
    ] {
        let mut request = create_request(3, "invalid");
        request["request_id"] = json!(invalid);
        assert_eq!(
            service
                .dispatch("create_memories", request, "actor")
                .unwrap_err()
                .code,
            "invalid_input"
        );
    }
}

#[test]
fn concurrent_identical_requests_commit_once() {
    let directory = tempfile::tempdir().unwrap();
    let service = Service::open(directory.path()).unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let handles = (0..8)
        .map(|_| {
            let service = Arc::clone(&service);
            let barrier = Arc::clone(&barrier);

            std::thread::spawn(move || {
                barrier.wait();
                dispatch(&service, "create_memories", create_request(1, "memory"))
            })
        })
        .collect::<Vec<_>>();
    let receipts = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert!(receipts.iter().all(|receipt| receipt == &receipts[0]));
    assert_eq!(count_domain_commits(&service), 1);
    assert_eq!(
        dispatch(&service, "search_memories", json!({"query":"reviewneedle"}))["total"],
        1
    );
}

#[test]
fn deletion_receipts_include_changed_sources_and_are_immutable() {
    let directory = tempfile::tempdir().unwrap();
    let service = Service::open(directory.path()).unwrap();
    let created = dispatch(
        &service,
        "create_memories",
        json!({"request_id":create_request_id(1),"commit_summary":"Create related memories.","memories":[{"key":"target","body":"Target"},{"key":"source","body":"Source","references":[{"target_key":"target","reference_type":"related"}]}]}),
    );
    let deletion = json!({"request_id":create_request_id(2),"commit_summary":"Remove target.","memories":[{"key":"target","expected_token":created["memories"][0]["system_token"]}]});
    let deleted = dispatch(&service, "delete_memories", deletion.clone());
    assert_eq!(deleted["memories"].as_array().unwrap().len(), 2);
    assert_eq!(deleted["memories"][1]["key"], "source");
    assert_ne!(
        deleted["memories"][1]["system_token"],
        created["memories"][1]["system_token"]
    );
    assert_eq!(
        deleted["memories"][1]["system_token"],
        read_memory(&service, "source")["system_token"]
    );
    assert!(
        read_memory(&service, "source")["references"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(dispatch(&service, "delete_memories", deletion), deleted);
    let connection = Connection::open(directory.path().join("memory.sqlite3")).unwrap();

    for sql in [
        "UPDATE mutation_receipts SET receipt = '{}'",
        "DELETE FROM mutation_receipts",
        "INSERT OR REPLACE INTO mutation_receipts SELECT request_id, actor, operation, fingerprint, receipt, publication_watermark, system_created_at FROM mutation_receipts",
    ] {
        assert!(connection.execute_batch(sql).is_err());
    }

    assert_eq!(
        dispatch(
            &service,
            "get_mutation_receipts",
            json!({"request_ids":[create_request_id(2)]})
        )["receipts"][0],
        deleted
    );
}
