use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

use semantic_memory::models::{
    CreateMemory, GetMemories, HistoryRequest, ListMemories, MemoryLink, MemoryVersion,
    RestoreMemory, TaxonomyInput, UpdateMemory,
};
use semantic_memory::storage::Store;

fn create_memory(key: &str) -> CreateMemory {
    CreateMemory {
        key: key.into(),
        title: format!("Title {key}"),
        body: format!("# Memory {key}\n\nUTF-8 content: café, 日本語, 🦀.\n"),
        kind: "note".into(),
        tags: vec!["project".into()],
        links: Vec::new(),
        actor: "human".into(),
    }
}

fn update_memory(key: &str, version: i64, body: &str) -> UpdateMemory {
    UpdateMemory {
        key: key.into(),
        expected_version: version,
        title: None,
        body: Some(body.into()),
        kind: None,
        tags: None,
        links: None,
        actor: "agent".into(),
    }
}

#[test]
fn preserves_exact_markdown_and_idempotent_creation() {
    let mut store = Store::open(":memory:").unwrap();
    let input = create_memory("preferences");
    let first = store.create_memories(std::slice::from_ref(&input)).unwrap();
    let repeated = store.create_memories(std::slice::from_ref(&input)).unwrap();
    assert_eq!(first, repeated);
    assert_eq!(first[0].body, input.body);
    assert_eq!(store.index_events(0, 100).unwrap().len(), 1);
    let mut changed = input;
    changed.body = "Changed content".into();
    assert_eq!(
        store.create_memories(&[changed]).unwrap_err().code,
        "already_exists"
    );
}

#[test]
fn batch_creation_supports_forward_links_and_rolls_back_invalid_links() {
    let mut store = Store::open(":memory:").unwrap();
    let mut first = create_memory("first");
    first.links.push(MemoryLink {
        target_key: "second".into(),
        relation: "related to".into(),
    });
    let records = store
        .create_memories(&[first, create_memory("second")])
        .unwrap();
    assert_eq!(records[0].links[0].target_key, "second");
    let mut invalid = create_memory("invalid");
    invalid.links.push(MemoryLink {
        target_key: "missing".into(),
        relation: "related to".into(),
    });
    assert!(
        store
            .create_memories(&[create_memory("rolled_back"), invalid])
            .is_err()
    );
    assert_eq!(
        store.get_memory("rolled_back", true).unwrap_err().code,
        "not_found"
    );
    assert_eq!(store.index_events(0, 100).unwrap().len(), 2);
}

#[test]
fn atomic_update_rejects_stale_versions_without_partial_changes() {
    let mut store = Store::open(":memory:").unwrap();
    store
        .create_memories(&[create_memory("first"), create_memory("second")])
        .unwrap();
    let error = store
        .update_memories(&[
            update_memory("first", 1, "First changed"),
            update_memory("second", 99, "Invalid"),
        ])
        .unwrap_err();
    assert_eq!(error.code, "version_conflict");
    assert_eq!(store.get_memory("first", false).unwrap().system_version, 1);
    assert_eq!(store.index_events(0, 100).unwrap().len(), 2);
    assert_eq!(
        store
            .update_memories(&[update_memory("first", 1, "Changed")])
            .unwrap()[0]
            .system_version,
        2
    );
}

#[test]
fn history_includes_normalized_tags_links_and_restoration() {
    let mut store = Store::open(":memory:").unwrap();
    let mut input = create_memory("first");
    input.links.push(MemoryLink {
        target_key: "second".into(),
        relation: "related to".into(),
    });
    store
        .create_memories(&[input.clone(), create_memory("second")])
        .unwrap();
    let mut update = update_memory("first", 1, "Changed body");
    update.tags = Some(vec!["new".into()]);
    update.links = Some(Vec::new());
    store.update_memories(&[update]).unwrap();
    let previous = store.get_revision("first", 1).unwrap();
    assert_eq!(previous.body, input.body);
    assert_eq!(previous.tags, input.tags);
    assert_eq!(previous.links, input.links);
    let restored = store
        .restore_memories(&[RestoreMemory {
            key: "first".into(),
            expected_version: 2,
            revision: Some(1),
            actor: "human".into(),
        }])
        .unwrap();
    assert_eq!(restored[0].system_version, 3);
    assert_eq!(restored[0].body, input.body);
    assert_eq!(restored[0].tags, input.tags);
    assert_eq!(restored[0].links, input.links);
    let history = store
        .list_revisions(&HistoryRequest {
            key: "first".into(),
            limit: 20,
            before_version: None,
        })
        .unwrap();
    assert_eq!(
        history
            .iter()
            .map(|record| record.system_version)
            .collect::<Vec<_>>(),
        vec![3, 2, 1]
    );
}

#[test]
fn deletion_is_hidden_and_recoverable_then_purge_is_permanent() {
    let mut store = Store::open(":memory:").unwrap();
    store.create_memories(&[create_memory("first")]).unwrap();
    let deleted = store
        .delete_memories(
            &[MemoryVersion {
                key: "first".into(),
                expected_version: 1,
            }],
            "human",
        )
        .unwrap();
    assert!(deleted[0].system_deleted_at.is_some());
    assert!(store.get_memory("first", false).is_err());
    assert!(
        store
            .list_memories(&ListMemories::default())
            .unwrap()
            .is_empty()
    );
    store
        .restore_memories(&[RestoreMemory {
            key: "first".into(),
            expected_version: 2,
            revision: None,
            actor: "human".into(),
        }])
        .unwrap();
    assert!(
        store
            .get_memory("first", false)
            .unwrap()
            .system_deleted_at
            .is_none()
    );
    store
        .delete_memories(
            &[MemoryVersion {
                key: "first".into(),
                expected_version: 3,
            }],
            "human",
        )
        .unwrap();
    assert_eq!(
        store
            .purge_memories(&[MemoryVersion {
                key: "first".into(),
                expected_version: 3
            }])
            .unwrap_err()
            .code,
        "version_conflict"
    );
    store
        .purge_memories(&[MemoryVersion {
            key: "first".into(),
            expected_version: 4,
        }])
        .unwrap();
    assert!(store.get_revision("first", 1).is_err());
    let violations: i64 = store
        .connection()
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
}

#[test]
fn purge_removes_incoming_references_from_current_and_historical_revisions() {
    let mut store = Store::open(":memory:").unwrap();
    let mut first = create_memory("first");
    let mut second = create_memory("second");
    first.links.push(MemoryLink {
        target_key: "second".into(),
        relation: "related to".into(),
    });
    second.links.push(MemoryLink {
        target_key: "first".into(),
        relation: "related to".into(),
    });
    store.create_memories(&[first, second]).unwrap();
    let versions = vec![
        MemoryVersion {
            key: "first".into(),
            expected_version: 1,
        },
        MemoryVersion {
            key: "second".into(),
            expected_version: 1,
        },
    ];
    store.delete_memories(&versions, "human").unwrap();
    let purges = [
        MemoryVersion {
            key: "first".into(),
            expected_version: 2,
        },
        MemoryVersion {
            key: "second".into(),
            expected_version: 2,
        },
    ];
    assert_eq!(store.purge_memories(&purges[..1]).unwrap(), vec!["first"]);
    let remaining = store.get_memory("second", true).unwrap();
    assert!(remaining.links.is_empty());
    assert_eq!(remaining.system_version, 3);
    assert_eq!(remaining.actor, "system");
    assert!(remaining.system_deleted_at.is_some());
    assert!(store.get_revision("second", 1).unwrap().links.is_empty());
    assert_eq!(
        store.get_revision("second", 3).unwrap().system_deleted_at,
        remaining.system_deleted_at
    );
    assert_eq!(
        store.purge_memories(&purges[1..]).unwrap_err().code,
        "version_conflict"
    );
    assert_eq!(
        store
            .purge_memories(&[MemoryVersion {
                key: "second".into(),
                expected_version: 3
            }])
            .unwrap(),
        vec!["second"]
    );
}

#[test]
fn purging_active_targets_revises_surviving_sources_and_rejects_stale_edits() {
    let mut store = Store::open(":memory:").unwrap();
    let mut source = create_memory("source");
    source.links = vec![
        MemoryLink {
            target_key: "target".into(),
            relation: "related to".into(),
        },
        MemoryLink {
            target_key: "retained".into(),
            relation: "related to".into(),
        },
    ];
    store
        .create_memories(&[
            source.clone(),
            create_memory("target"),
            create_memory("retained"),
        ])
        .unwrap();
    assert_eq!(
        store
            .purge_memories(&[MemoryVersion {
                key: "target".into(),
                expected_version: 99
            }])
            .unwrap_err()
            .code,
        "version_conflict"
    );
    assert_eq!(store.get_memory("source", false).unwrap().system_version, 1);
    assert_eq!(
        store
            .purge_memories(&[MemoryVersion {
                key: "target".into(),
                expected_version: 1
            }])
            .unwrap(),
        vec!["target"]
    );
    let current = store.get_memory("source", false).unwrap();
    assert_eq!(current.system_version, 2);
    assert_eq!(current.actor, "system");
    assert_eq!(current.body, source.body);
    assert_eq!(current.tags, source.tags);
    assert_eq!(
        current.links,
        vec![MemoryLink {
            target_key: "retained".into(),
            relation: "related to".into()
        }]
    );
    assert_eq!(
        store
            .update_memories(&[update_memory("source", 1, "Stale edit")])
            .unwrap_err()
            .code,
        "version_conflict"
    );
    let history = store
        .list_revisions(&HistoryRequest {
            key: "source".into(),
            limit: 100,
            before_version: None,
        })
        .unwrap();
    assert_eq!(history.len(), 2);
    assert!(
        history
            .iter()
            .all(|record| record.links.iter().all(|link| link.target_key != "target"))
    );
    let events = store.index_events(0, 100).unwrap();
    assert_eq!(events.last().unwrap().memory_key, "source");
    assert!(store.get_memory("target", true).is_err());
}

#[test]
fn migrates_schema_one_and_backfills_historical_deletion_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(include_str!("../migrations/0001_initial.sql"))
        .unwrap();
    connection.execute_batch("PRAGMA foreign_keys = ON;
        BEGIN;
        INSERT INTO database_identity (id, key, system_created_at, system_updated_at) VALUES (1, 'legacy-identity', 100, 100);
        INSERT INTO memory_kinds (id, key, name, system_created_at, system_updated_at) VALUES (1, 'note', 'Note', 100, 100);
        INSERT INTO actors (id, name, system_created_at, system_updated_at) VALUES (1, 'human', 100, 100);
        INSERT INTO memories (id, key, system_created_at, system_updated_at, system_version, system_deleted_at) VALUES (1, 'legacy', 100, 200, 1, 200);
        INSERT INTO memory_revisions (memory_id, title, body, kind_id, actor_id, system_version, system_operation, system_created_at, system_updated_at) VALUES (1, 'Legacy', 'Preserved Markdown', 1, 1, 1, 'delete', 200, 200);
        COMMIT;").unwrap();
    drop(connection);
    let mut migrated = Store::open(&path).unwrap();
    assert_eq!(migrated.database_key().unwrap(), "legacy-identity");
    let version: i64 = migrated
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
    assert_eq!(
        migrated
            .get_revision("legacy", 1)
            .unwrap()
            .system_deleted_at,
        Some(200)
    );
    assert_eq!(
        migrated.get_memory("legacy", true).unwrap().body,
        "Preserved Markdown"
    );
    migrated.create_memories(&[create_memory("new")]).unwrap();
    assert!(Store::open_read_only(&path).is_ok());
}

#[test]
fn taxonomy_has_distinct_keys_names_and_restricts_referenced_deletion() {
    let mut store = Store::open(":memory:").unwrap();
    store
        .create_taxonomy(
            "kinds",
            &TaxonomyInput {
                key: "preference".into(),
                name: "Personal preference".into(),
            },
        )
        .unwrap();
    store
        .update_taxonomy(
            "kinds",
            &TaxonomyInput {
                key: "preference".into(),
                name: "Preference".into(),
            },
        )
        .unwrap();
    let mut input = create_memory("first");
    input.kind = "preference".into();
    store.create_memories(&[input]).unwrap();
    assert_eq!(store.get_memory("first", false).unwrap().kind, "preference");
    assert_eq!(
        store
            .delete_taxonomy("kinds", "preference")
            .unwrap_err()
            .code,
        "referenced_taxonomy"
    );
    assert_eq!(store.list_taxonomy("kinds").unwrap()[0].name, "Preference");
    assert!(
        store
            .list_taxonomy("malicious; DROP TABLE memories")
            .is_err()
    );
}

#[test]
fn browsing_filters_tags_links_and_cursor_without_offsets() {
    let mut store = Store::open(":memory:").unwrap();
    let mut first = create_memory("first");
    first.links.push(MemoryLink {
        target_key: "second".into(),
        relation: "related to".into(),
    });
    store
        .create_memories(&[first, create_memory("second"), create_memory("third")])
        .unwrap();
    let first_page = store
        .list_memories(&ListMemories {
            limit: 1,
            ..Default::default()
        })
        .unwrap();
    let cursor = format!("{}:{}", first_page[0].system_updated_at, first_page[0].id);
    let second_page = store
        .list_memories(&ListMemories {
            limit: 1,
            cursor: Some(cursor),
            ..Default::default()
        })
        .unwrap();
    assert_ne!(first_page[0].key, second_page[0].key);
    let incoming = store
        .list_memories(&ListMemories {
            linked_to: Some("second".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(incoming[0].key, "first");
    let outgoing = store
        .list_memories(&ListMemories {
            linked_from: Some("first".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(outgoing[0].key, "second");
    assert!(
        store
            .list_memories(&ListMemories {
                tags: vec!["missing".into()],
                ..Default::default()
            })
            .unwrap()
            .is_empty()
    );
}

#[test]
fn database_identity_and_index_sequences_survive_reopening() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("memory.sqlite3");
    let mut store = Store::open(&path).unwrap();
    let identity = store.database_key().unwrap();
    store.create_memories(&[create_memory("first")]).unwrap();
    let first_sequence = store.index_events(0, 10).unwrap()[0].sequence;
    store.acknowledge_events(first_sequence).unwrap();
    drop(store);
    let mut reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.database_key().unwrap(), identity);
    reopened
        .update_memories(&[update_memory("first", 1, "Changed")])
        .unwrap();
    assert!(reopened.index_events(0, 10).unwrap()[0].sequence > first_sequence);
    let mut reader = Store::open_read_only(&path).unwrap();
    assert_eq!(reader.get_memory("first", false).unwrap().body, "Changed");
    assert!(
        reader
            .create_memories(&[create_memory("forbidden")])
            .is_err()
    );
}

#[test]
fn foreign_keys_prevent_cross_memory_revision_heads() {
    let mut store = Store::open(":memory:").unwrap();
    store
        .create_memories(&[create_memory("first"), create_memory("second")])
        .unwrap();
    assert!(
        store
            .connection()
            .execute(
                "UPDATE memories SET system_version = 99 WHERE key = 'first'",
                []
            )
            .is_err()
    );
    let json_columns: i64 = store.connection().query_row("SELECT count(*) FROM pragma_table_info('memory_revisions') WHERE name IN ('metadata', 'snapshot', 'properties')", [], |row| row.get(0)).unwrap();
    assert_eq!(json_columns, 0);
}

#[test]
fn batch_get_uses_one_snapshot_during_atomic_concurrent_updates() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("memory.sqlite3");
    let mut writer = Store::open(&path).unwrap();
    let mut first = create_memory("first");
    let mut second = create_memory("second");
    first.body = "generation0".into();
    second.body = "generation0".into();
    writer.create_memories(&[first, second]).unwrap();
    let reader = Store::open_read_only(&path).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let done = Arc::new(AtomicBool::new(false));
    let writer_barrier = Arc::clone(&barrier);
    let writer_done = Arc::clone(&done);
    let worker = std::thread::spawn(move || {
        writer_barrier.wait();
        let result = (|| -> semantic_memory::models::AppResult<()> {
            for version in 1..=100 {
                let body = format!("generation{version}");
                writer.update_memories(&[
                    update_memory("first", version, &body),
                    update_memory("second", version, &body),
                ])?;
            }

            Ok(())
        })();
        writer_done.store(true, Ordering::Release);

        result
    });
    let request = GetMemories {
        keys: (0..64)
            .map(|index| {
                if index % 2 == 0 {
                    "first".into()
                } else {
                    "second".into()
                }
            })
            .collect(),
        include_deleted: false,
        version: None,
    };
    let mut reads = 0;
    barrier.wait();

    while !done.load(Ordering::Acquire) || reads < 20 {
        let records = reader.get_memories(&request).unwrap();
        assert_eq!(records.len(), 64);
        assert!(records.iter().all(|record| record.body == records[0].body));
        assert!(
            records
                .iter()
                .all(|record| record.system_version == records[0].system_version)
        );
        reads += 1;
    }

    worker.join().unwrap().unwrap();
    let historical = reader
        .get_memories(&GetMemories {
            keys: vec!["second".into(), "first".into()],
            include_deleted: false,
            version: Some(1),
        })
        .unwrap();
    assert_eq!(historical[0].key, "second");
    assert!(historical.iter().all(|record| record.body == "generation0"));
}
