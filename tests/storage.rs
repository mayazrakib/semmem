use std::sync::{Arc, Barrier};

use rusqlite::Connection;
use semmem::models::{
    CreateMemory, GetCommits, GetMemories, ListCommits, ListMemories, MemoryToken, TaxonomyInput,
    UpdateMemory,
};
use semmem::storage::Store;
use serde_json::json;

fn memory(key: &str) -> CreateMemory {
    serde_json::from_value(
        json!({"key":key,"body":"# Exact Markdown\r\n\r\ncafé 東京\n","summary":"Manual summary"}),
    )
    .unwrap()
}

fn update(key: &str, token: &str, body: &str) -> UpdateMemory {
    serde_json::from_value(json!({"key":key,"expected_token":token,"body":body})).unwrap()
}

fn commits(store: &Store) -> Vec<semmem::models::CommitRecord> {
    store
        .list_commits(&ListCommits {
            limit: 10000,
            cursor: None,
        })
        .unwrap()
}

#[test]
fn initializes_current_schema_and_audits_initialization() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("memory.sqlite3");
    let store = Store::open(&path).unwrap();
    let key = store.database_key().unwrap();
    let connection = Connection::open(&path).unwrap();
    let tables = connection
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(tables.contains(&"commits".into()));
    assert!(
        !tables
            .iter()
            .any(|name| name.contains("revision") || name == "authors")
    );
    assert_eq!(commits(&store).len(), 1);
    assert_eq!(commits(&store)[0].operation, "initialize");
    assert_eq!(store.last_commit_id().unwrap(), None);
    drop(store);
    assert_eq!(Store::open(&path).unwrap().database_key().unwrap(), key);
}

#[test]
fn preserves_markdown_and_idempotent_create_has_no_commit_receipt() {
    let mut store = Store::open(":memory:").unwrap();
    let created = store
        .create_memories(&[memory("one")], "trusted actor", "Save the first memory.")
        .unwrap();
    assert_eq!(created[0].body, memory("one").body);
    assert_eq!(created[0].system_created_by, "trusted actor");
    let first = store.last_commit_id().unwrap().unwrap();
    let again = store
        .create_memories(&[memory("one")], "another actor", "Retry the first memory.")
        .unwrap();
    assert_eq!(created, again);
    assert_eq!(store.last_commit_id().unwrap(), None);
    assert_eq!(commits(&store)[0].id, first);
    let mut different = memory("one");
    different.body = "Different".into();
    assert_eq!(
        store
            .create_memories(&[different], "actor", "Reject duplicate.")
            .unwrap_err()
            .code,
        "already_exists"
    );
    assert_eq!(commits(&store).len(), 2);
}

#[test]
fn audit_message_is_required_for_every_mutation() {
    let mut store = Store::open(":memory:").unwrap();
    assert!(
        store
            .create_memories(&[memory("one")], "actor", " \n")
            .is_err()
    );
    let created = store
        .create_memories(&[memory("one")], "actor", "Create one.")
        .unwrap();
    let token = &created[0].system_token;
    assert!(
        store
            .update_memories(&[update("one", token, "changed")], "actor", "")
            .is_err()
    );
    assert!(
        store
            .delete_memories(
                &[MemoryToken {
                    key: "one".into(),
                    expected_token: token.clone()
                }],
                "actor",
                ""
            )
            .is_err()
    );
    let taxonomy = TaxonomyInput {
        key: "example".into(),
        name: "Example".into(),
    };
    assert!(
        store
            .create_taxonomy("tags", &taxonomy, "actor", "")
            .is_err()
    );
    store
        .create_taxonomy("tags", &taxonomy, "actor", "Create example tag.")
        .unwrap();
    assert!(
        store
            .update_taxonomy("tags", &taxonomy, "actor", "")
            .is_err()
    );
    assert!(
        store
            .delete_taxonomy("tags", "example", "actor", "")
            .is_err()
    );
    assert_eq!(commits(&store).len(), 3);
}

#[test]
fn batch_creation_supports_forward_references_and_rolls_back_invalid_references() {
    let mut store = Store::open(":memory:").unwrap();
    let mut source = memory("source");
    source.references =
        serde_json::from_value(json!([{"target_key":"target","reference_type":"related to"}]))
            .unwrap();
    source.tags = vec!["example".into()];
    store
        .create_memories(
            &[source.clone(), memory("target")],
            "actor",
            "Create a linked pair.",
        )
        .unwrap();
    assert_eq!(
        store.get_memory("source").unwrap().references,
        source.references
    );
    let audit = &commits(&store)[0];
    assert!(
        audit
            .changes
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["entity"] == "reference_types")
    );
    assert!(
        audit
            .changes
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["entity"] == "tags")
    );
    source.key = "invalid".into();
    source.references[0].target_key = "missing".into();
    source.tags = vec!["rolled back".into()];
    let count = commits(&store).len();
    assert!(
        store
            .create_memories(&[source], "actor", "This transaction must roll back.")
            .is_err()
    );
    assert!(store.get_memory("invalid").is_err());
    assert!(
        !store
            .list_taxonomy("tags")
            .unwrap()
            .iter()
            .any(|tag| tag.key == "rolled back")
    );
    assert_eq!(commits(&store).len(), count);
}

#[test]
fn stale_token_rolls_back_entire_batch_and_audit() {
    let mut store = Store::open(":memory:").unwrap();
    let records = store
        .create_memories(&[memory("one"), memory("two")], "actor", "Create two.")
        .unwrap();
    let count = commits(&store).len();
    let err = store
        .update_memories(
            &[
                update("one", &records[0].system_token, "changed"),
                update("two", "stale", "changed"),
            ],
            "actor",
            "Atomic update.",
        )
        .unwrap_err();
    assert_eq!(err.code, "token_conflict");
    assert_eq!(store.get_memory("one").unwrap(), records[0]);
    assert_eq!(commits(&store).len(), count);
    assert_eq!(store.last_commit_id().unwrap(), None);
}

#[test]
fn overwrite_preserves_only_current_content_and_noop_preserves_token() {
    let mut store = Store::open(":memory:").unwrap();
    let record = store
        .create_memories(&[memory("one")], "actor", "Create one.")
        .unwrap()
        .remove(0);
    let noop = update("one", &record.system_token, &record.body);
    assert_eq!(
        store
            .update_memories(&[noop], "actor", "No change.")
            .unwrap()[0],
        record
    );
    assert_eq!(store.last_commit_id().unwrap(), None);
    let changed = store
        .update_memories(
            &[update("one", &record.system_token, "new content")],
            "editor",
            "Replace obsolete content.",
        )
        .unwrap()
        .remove(0);
    assert_ne!(changed.system_token, record.system_token);
    assert_eq!(changed.system_created_at, record.system_created_at);
    assert_eq!(changed.system_updated_by, "editor");
    assert_eq!(store.get_memory("one").unwrap().body, "new content");
    assert!(
        !serde_json::to_string(&commits(&store))
            .unwrap()
            .contains("Exact Markdown")
    );
    assert!(
        !serde_json::to_value(changed)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("author")
    );
}

#[test]
fn hard_delete_removes_links_and_invalidates_surviving_source_tokens() {
    let mut store = Store::open(":memory:").unwrap();
    let source: CreateMemory = serde_json::from_value(json!({"key":"source","body":"source","references":[{"target_key":"target","reference_type":"related"}]})).unwrap();
    let records = store
        .create_memories(
            &[source, memory("target")],
            "actor",
            "Create linked memories.",
        )
        .unwrap();
    let before = store.index_events(0, 100).unwrap().last().unwrap().sequence;
    let deleted = store
        .delete_memories(
            &[MemoryToken {
                key: "target".into(),
                expected_token: records[1].system_token.clone(),
            }],
            "actor",
            "Remove target and incoming links.",
        )
        .unwrap();
    assert_eq!(deleted[0].key, "target");
    assert!(store.get_memory("target").is_err());
    let source = store.get_memory("source").unwrap();
    assert!(source.references.is_empty());
    assert_ne!(source.system_token, records[0].system_token);
    assert_eq!(
        store
            .update_memories(
                &[update("source", &records[0].system_token, "stale")],
                "actor",
                "Stale source update."
            )
            .unwrap_err()
            .code,
        "token_conflict"
    );
    assert_eq!(store.index_events(before, 100).unwrap().len(), 2);
    let recreated = store
        .create_memories(&[memory("target")], "actor", "Recreate target.")
        .unwrap();
    assert_ne!(recreated[0].system_token, records[1].system_token);
    assert_eq!(
        store
            .delete_memories(
                &[MemoryToken {
                    key: "target".into(),
                    expected_token: records[1].system_token.clone()
                }],
                "actor",
                "Reject stale deletion."
            )
            .unwrap_err()
            .code,
        "token_conflict"
    );
}

#[test]
fn audit_log_is_append_only_and_cursor_pages_and_ids_are_stable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("memory.sqlite3");
    let mut store = Store::open(&path).unwrap();
    store
        .create_memories(&[memory("one")], "actor", "Create one.")
        .unwrap();
    let id = store.last_commit_id().unwrap().unwrap();
    let fetched = store.get_commits(&GetCommits { ids: vec![id] }).unwrap();
    assert_eq!(fetched[0].summary, "Create one.");
    let page = store
        .list_commits(&ListCommits {
            limit: 1,
            cursor: Some(id.to_string()),
        })
        .unwrap();
    assert_eq!(page[0].operation, "initialize");
    let connection = Connection::open(&path).unwrap();
    assert!(
        connection
            .execute(
                "UPDATE commits SET summary = 'tampered' WHERE id = ?1",
                [id]
            )
            .is_err()
    );
    assert!(
        connection
            .execute("DELETE FROM commits WHERE id = ?1", [id])
            .is_err()
    );
    assert!(connection.execute("INSERT INTO commits (key,summary,operation,actor,changes,system_created_at,system_updated_at) VALUES ('bad',' ','update','actor','[]',1,1)", []).is_err());
}

#[test]
fn search_event_acknowledgment_is_audited_only_when_it_changes_rows() {
    let mut store = Store::open(":memory:").unwrap();
    store
        .create_memories(&[memory("one")], "actor", "Create one.")
        .unwrap();
    let watermark = store.index_events(0, 100).unwrap()[0].sequence;
    store.acknowledge_events(watermark).unwrap();
    let count = commits(&store).len();
    assert_eq!(commits(&store)[0].operation, "acknowledge_index_events");
    store.acknowledge_events(watermark).unwrap();
    assert_eq!(commits(&store).len(), count);
    assert!(store.index_events(0, 100).unwrap().is_empty());
}

#[test]
fn taxonomy_has_distinct_names_and_restricts_current_references() {
    let mut store = Store::open(":memory:").unwrap();
    let taxonomy = TaxonomyInput {
        key: "custom".into(),
        name: "Custom Type".into(),
    };
    store
        .create_taxonomy("types", &taxonomy, "actor", "Create custom type.")
        .unwrap();
    assert_eq!(
        store
            .list_taxonomy("types")
            .unwrap()
            .iter()
            .find(|item| item.key == "custom")
            .unwrap()
            .name,
        "Custom Type"
    );
    let mut input = memory("one");
    input.r#type = "custom".into();
    let record = store
        .create_memories(&[input], "actor", "Use custom type.")
        .unwrap()
        .remove(0);
    assert_eq!(
        store
            .delete_taxonomy("types", "custom", "actor", "Remove referenced type.")
            .unwrap_err()
            .code,
        "referenced_taxonomy"
    );
    store
        .delete_memories(
            &[MemoryToken {
                key: record.key,
                expected_token: record.system_token,
            }],
            "actor",
            "Remove memory.",
        )
        .unwrap();
    store
        .delete_taxonomy("types", "custom", "actor", "Remove unused type.")
        .unwrap();
    assert_eq!(
        store
            .delete_taxonomy("types", "general", "actor", "Keep required default.")
            .unwrap_err()
            .code,
        "default_type"
    );
}

#[test]
fn browsing_filters_metadata_tags_and_uses_stable_cursors() {
    let mut store = Store::open(":memory:").unwrap();
    let mut input = memory("one");
    input.tags = vec!["tag".into()];
    input.metadata.scope = Some(Some("project".into()));
    store
        .create_memories(&[input, memory("two")], "actor", "Create browse fixtures.")
        .unwrap();
    assert_eq!(
        store
            .list_memories(&ListMemories {
                tags: vec!["tag".into()],
                scope: Some("project".into()),
                ..Default::default()
            })
            .unwrap()
            .len(),
        1
    );
    let page = store
        .list_memories(&ListMemories {
            limit: 1,
            ..Default::default()
        })
        .unwrap();
    let cursor = format!("{}:{}", page[0].system_updated_at, page[0].id);
    let next = store
        .list_memories(&ListMemories {
            limit: 1,
            cursor: Some(cursor),
            ..Default::default()
        })
        .unwrap();
    assert_ne!(page[0].key, next[0].key);
}

#[test]
fn nullable_metadata_preserves_omission_and_explicit_unknown_dates() {
    let mut store = Store::open(":memory:").unwrap();
    let input: CreateMemory = serde_json::from_value(json!({"key":"one","body":"body","summary":"summary","observed_at":null,"verified_at":null})).unwrap();
    let record = store
        .create_memories(&[input], "actor", "Save unknown dates.")
        .unwrap()
        .remove(0);
    assert_eq!(record.metadata.observed_at, None);
    assert_eq!(record.metadata.verified_at, None);
    let input: UpdateMemory = serde_json::from_value(
        json!({"key":"one","expected_token":record.system_token,"summary":null,"verified_at":123}),
    )
    .unwrap();
    let record = store
        .update_memories(&[input], "actor", "Clear summary and verify.")
        .unwrap()
        .remove(0);
    assert_eq!(record.metadata.summary, None);
    assert_eq!(record.metadata.observed_at, None);
    assert_eq!(record.metadata.verified_at, Some(123));
}

#[test]
fn accepts_large_batches_and_reads_without_bind_variable_or_body_caps() {
    let mut store = Store::open(":memory:").unwrap();
    let inputs = (0..1500)
        .map(|index| memory(&format!("memory-{index}")))
        .collect::<Vec<_>>();
    store
        .create_memories(&inputs, "actor", "Create a large batch.")
        .unwrap();
    let read = store
        .get_memories(&GetMemories {
            keys: inputs.iter().map(|input| input.key.clone()).collect(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(read.len(), 1500);
    assert_eq!(
        store
            .list_memories(&ListMemories {
                limit: 2000,
                ..Default::default()
            })
            .unwrap()
            .len(),
        1500
    );
}

#[test]
fn batch_reads_are_consistent_during_atomic_concurrent_updates() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("memory.sqlite3");
    let mut store = Store::open(&path).unwrap();
    store
        .create_memories(
            &[memory("one"), memory("two")],
            "actor",
            "Create concurrent fixtures.",
        )
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let worker_barrier = Arc::clone(&barrier);
    let worker_path = path.clone();
    let worker = std::thread::spawn(move || {
        let mut writer = Store::open(&worker_path).unwrap();
        worker_barrier.wait();
        for index in 0..30 {
            let one = writer.get_memory("one").unwrap();
            let two = writer.get_memory("two").unwrap();
            writer
                .update_memories(
                    &[
                        update("one", &one.system_token, &index.to_string()),
                        update("two", &two.system_token, &index.to_string()),
                    ],
                    "actor",
                    "Update pair atomically.",
                )
                .unwrap();
        }
    });
    let reader = Store::open_read_only(&path).unwrap();
    barrier.wait();
    for _ in 0..100 {
        let records = reader
            .get_memories(&GetMemories {
                keys: vec!["one".into(), "two".into()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(records[0].body, records[1].body);
    }
    worker.join().unwrap();
}

#[test]
fn rejects_old_schemas_without_modifying_them() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("old.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TABLE legacy (body TEXT); INSERT INTO legacy (body) VALUES ('preserve'); PRAGMA user_version = 1;").unwrap();
    assert_eq!(Store::open(&path).err().unwrap().code, "unsupported_schema");
    assert_eq!(
        connection
            .query_row("SELECT body FROM legacy", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "preserve"
    );
}
