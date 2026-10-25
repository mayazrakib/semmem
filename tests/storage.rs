use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

use semmem::models::{
    Basis, CreateMemory, GetMemories, HistoryRequest, ListMemories, MemoryMetadata,
    MemoryReference, MemoryVersion, RestoreMemory, TaxonomyInput, UpdateMemory,
};
use semmem::storage::Store;

fn create_memory(key: &str) -> CreateMemory {
    CreateMemory {
        key: key.into(),
        title: format!("Title {key}"),
        metadata: Default::default(),
        body: format!("# Memory {key}\n\nUTF-8 content: café, 日本語, 🦀.\n"),
        r#type: "note".into(),
        tags: vec!["project".into()],
        references: Vec::new(),
        author: "human".into(),
    }
}

fn update_memory(key: &str, version: i64, body: &str) -> UpdateMemory {
    UpdateMemory {
        key: key.into(),
        expected_version: version,
        metadata: Default::default(),
        title: None,
        body: Some(body.into()),
        r#type: None,
        tags: None,
        references: None,
        author: None,
    }
}

#[test]
fn preserves_exact_markdown_and_idempotent_creation() {
    let mut store = Store::open(":memory:").unwrap();
    let input = create_memory("preferences");
    let first = store
        .create_memories(std::slice::from_ref(&input), "test writer")
        .unwrap();
    let repeated = store
        .create_memories(std::slice::from_ref(&input), "test writer")
        .unwrap();
    assert_eq!(first, repeated);
    assert_eq!(first[0].body, input.body);
    assert_eq!(store.index_events(0, 100).unwrap().len(), 1);
    let mut changed = input;
    changed.body = "Changed content".into();
    assert_eq!(
        store
            .create_memories(&[changed], "test writer")
            .unwrap_err()
            .code,
        "already_exists"
    );
}

#[test]
fn batch_creation_supports_forward_references_and_rolls_back_invalid_references() {
    let mut store = Store::open(":memory:").unwrap();
    let mut first = create_memory("first");
    first.references.push(MemoryReference {
        target_key: "second".into(),
        reference_type: "related to".into(),
    });
    let records = store
        .create_memories(&[first, create_memory("second")], "test writer")
        .unwrap();
    assert_eq!(records[0].references[0].target_key, "second");
    let mut invalid = create_memory("invalid");
    invalid.references.push(MemoryReference {
        target_key: "missing".into(),
        reference_type: "related to".into(),
    });
    assert!(
        store
            .create_memories(&[create_memory("rolled_back"), invalid], "test writer")
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
        .create_memories(
            &[create_memory("first"), create_memory("second")],
            "test writer",
        )
        .unwrap();
    let error = store
        .update_memories(
            &[
                update_memory("first", 1, "First changed"),
                update_memory("second", 99, "Invalid"),
            ],
            "test writer",
        )
        .unwrap_err();
    assert_eq!(error.code, "version_conflict");
    assert_eq!(store.get_memory("first", false).unwrap().system_version, 1);
    assert_eq!(store.index_events(0, 100).unwrap().len(), 2);
    assert_eq!(
        store
            .update_memories(&[update_memory("first", 1, "Changed")], "test writer")
            .unwrap()[0]
            .system_version,
        2
    );
}

#[test]
fn history_includes_normalized_tags_references_and_restoration() {
    let mut store = Store::open(":memory:").unwrap();
    let mut input = create_memory("first");
    input.references.push(MemoryReference {
        target_key: "second".into(),
        reference_type: "related to".into(),
    });
    store
        .create_memories(&[input.clone(), create_memory("second")], "test writer")
        .unwrap();
    let mut update = update_memory("first", 1, "Changed body");
    update.tags = Some(vec!["new".into()]);
    update.references = Some(Vec::new());
    store.update_memories(&[update], "test writer").unwrap();
    let previous = store.get_revision("first", 1).unwrap();
    assert_eq!(previous.body, input.body);
    assert_eq!(previous.tags, input.tags);
    assert_eq!(previous.references, input.references);
    let restored = store
        .restore_memories(
            &[RestoreMemory {
                key: "first".into(),
                expected_version: 2,
                revision: Some(1),
            }],
            "test writer",
        )
        .unwrap();
    assert_eq!(restored[0].system_version, 3);
    assert_eq!(restored[0].body, input.body);
    assert_eq!(restored[0].tags, input.tags);
    assert_eq!(restored[0].references, input.references);
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
    store
        .create_memories(&[create_memory("first")], "test writer")
        .unwrap();
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
        .restore_memories(
            &[RestoreMemory {
                key: "first".into(),
                expected_version: 2,
                revision: None,
            }],
            "test writer",
        )
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
            .purge_memories(
                &[MemoryVersion {
                    key: "first".into(),
                    expected_version: 3
                }],
                "test writer"
            )
            .unwrap_err()
            .code,
        "version_conflict"
    );
    store
        .purge_memories(
            &[MemoryVersion {
                key: "first".into(),
                expected_version: 4,
            }],
            "test writer",
        )
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
    first.references.push(MemoryReference {
        target_key: "second".into(),
        reference_type: "related to".into(),
    });
    second.references.push(MemoryReference {
        target_key: "first".into(),
        reference_type: "related to".into(),
    });
    store
        .create_memories(&[first, second], "test writer")
        .unwrap();
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
    assert_eq!(
        store.purge_memories(&purges[..1], "test writer").unwrap(),
        vec!["first"]
    );
    let remaining = store.get_memory("second", true).unwrap();
    assert!(remaining.references.is_empty());
    assert_eq!(remaining.system_version, 3);
    assert_eq!(remaining.author, "human");
    assert_eq!(remaining.system_updated_by, "test writer");
    assert!(remaining.system_deleted_at.is_some());
    assert!(
        store
            .get_revision("second", 1)
            .unwrap()
            .references
            .is_empty()
    );
    assert_eq!(
        store.get_revision("second", 3).unwrap().system_deleted_at,
        remaining.system_deleted_at
    );
    assert_eq!(
        store
            .purge_memories(&purges[1..], "test writer")
            .unwrap_err()
            .code,
        "version_conflict"
    );
    assert_eq!(
        store
            .purge_memories(
                &[MemoryVersion {
                    key: "second".into(),
                    expected_version: 3
                }],
                "test writer"
            )
            .unwrap(),
        vec!["second"]
    );
}

#[test]
fn purging_active_targets_revises_surviving_sources_and_rejects_stale_edits() {
    let mut store = Store::open(":memory:").unwrap();
    let mut source = create_memory("source");
    source.references = vec![
        MemoryReference {
            target_key: "target".into(),
            reference_type: "related to".into(),
        },
        MemoryReference {
            target_key: "retained".into(),
            reference_type: "related to".into(),
        },
    ];
    store
        .create_memories(
            &[
                source.clone(),
                create_memory("target"),
                create_memory("retained"),
            ],
            "test writer",
        )
        .unwrap();
    assert_eq!(
        store
            .purge_memories(
                &[MemoryVersion {
                    key: "target".into(),
                    expected_version: 99
                }],
                "test writer"
            )
            .unwrap_err()
            .code,
        "version_conflict"
    );
    assert_eq!(store.get_memory("source", false).unwrap().system_version, 1);
    assert_eq!(
        store
            .purge_memories(
                &[MemoryVersion {
                    key: "target".into(),
                    expected_version: 1
                }],
                "test writer"
            )
            .unwrap(),
        vec!["target"]
    );
    let current = store.get_memory("source", false).unwrap();
    assert_eq!(current.system_version, 2);
    assert_eq!(current.author, "human");
    assert_eq!(current.system_updated_by, "test writer");
    assert_eq!(current.body, source.body);
    assert_eq!(current.tags, source.tags);
    assert_eq!(
        current.references,
        vec![MemoryReference {
            target_key: "retained".into(),
            reference_type: "related to".into()
        }]
    );
    assert_eq!(
        store
            .update_memories(&[update_memory("source", 1, "Stale edit")], "test writer")
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
    assert!(history.iter().all(|record| {
        record
            .references
            .iter()
            .all(|link| link.target_key != "target")
    }));
    let events = store.index_events(0, 100).unwrap();
    assert_eq!(events.last().unwrap().memory_key, "source");
    assert!(store.get_memory("target", true).is_err());
}

#[test]
fn initializes_current_schema_and_preserves_records_on_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("initialized.sqlite3");
    let mut store = Store::open(&path).unwrap();
    let identity = store.database_key().unwrap();
    let records = store
        .create_memories(&[create_memory("initialized")], "test writer")
        .unwrap();
    let version: i64 = store
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);
    assert_eq!(records[0].system_deleted_at, None);
    drop(store);

    let reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.database_key().unwrap(), identity);
    assert_eq!(
        reopened.get_memory("initialized", false).unwrap(),
        records[0]
    );
    assert!(Store::open_read_only(&path).is_ok());
}

#[test]
fn rejects_existing_schemas_without_modifying_the_database() {
    for version in [0, 1, 2, 3, 4] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("existing.sqlite3");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE preserved (body TEXT); INSERT INTO preserved VALUES ('Original data');").unwrap();
        connection
            .pragma_update(None, "user_version", version)
            .unwrap();
        drop(connection);
        let before = std::fs::read(&path).unwrap();
        assert_eq!(Store::open(&path).err().unwrap().code, "unsupported_schema");
        assert_eq!(
            Store::open_read_only(&path).err().unwrap().code,
            "unsupported_schema"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let connection = rusqlite::Connection::open(&path).unwrap();
        let body: String = connection
            .query_row("SELECT body FROM preserved", [], |row| row.get(0))
            .unwrap();
        assert_eq!(body, "Original data");
    }
}

#[test]
fn taxonomy_has_distinct_keys_names_and_restricts_referenced_deletion() {
    let mut store = Store::open(":memory:").unwrap();
    store
        .create_taxonomy(
            "types",
            &TaxonomyInput {
                key: "preference".into(),
                name: "Personal preference".into(),
                author: "human".into(),
            },
            "test writer",
        )
        .unwrap();
    store
        .update_taxonomy(
            "types",
            &TaxonomyInput {
                key: "preference".into(),
                name: "Preference".into(),
                author: "human".into(),
            },
            "test writer",
        )
        .unwrap();
    let mut input = create_memory("first");
    input.r#type = "preference".into();
    store.create_memories(&[input], "test writer").unwrap();
    assert_eq!(
        store.get_memory("first", false).unwrap().r#type,
        "preference"
    );
    assert_eq!(
        store
            .delete_taxonomy("types", "preference", "test writer")
            .unwrap_err()
            .code,
        "referenced_taxonomy"
    );
    assert_eq!(
        store
            .list_taxonomy("types")
            .unwrap()
            .into_iter()
            .find(|item| item.key == "preference")
            .unwrap()
            .name,
        "Preference"
    );
    assert!(
        store
            .list_taxonomy("malicious; DROP TABLE memories")
            .is_err()
    );
}

#[test]
fn browsing_filters_tags_references_and_cursor_without_offsets() {
    let mut store = Store::open(":memory:").unwrap();
    let mut first = create_memory("first");
    first.references.push(MemoryReference {
        target_key: "second".into(),
        reference_type: "related to".into(),
    });
    store
        .create_memories(
            &[first, create_memory("second"), create_memory("third")],
            "test writer",
        )
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
            referenced_to: Some("second".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(incoming[0].key, "first");
    let outgoing = store
        .list_memories(&ListMemories {
            referenced_from: Some("first".into()),
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
    store
        .create_memories(&[create_memory("first")], "test writer")
        .unwrap();
    let first_sequence = store.index_events(0, 10).unwrap()[0].sequence;
    store.acknowledge_events(first_sequence).unwrap();
    drop(store);
    let mut reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.database_key().unwrap(), identity);
    reopened
        .update_memories(&[update_memory("first", 1, "Changed")], "test writer")
        .unwrap();
    assert!(reopened.index_events(0, 10).unwrap()[0].sequence > first_sequence);
    let mut reader = Store::open_read_only(&path).unwrap();
    assert_eq!(reader.get_memory("first", false).unwrap().body, "Changed");
    assert!(
        reader
            .create_memories(&[create_memory("forbidden")], "test writer")
            .is_err()
    );
}

#[test]
fn foreign_keys_prevent_cross_memory_revision_heads() {
    let mut store = Store::open(":memory:").unwrap();
    store
        .create_memories(
            &[create_memory("first"), create_memory("second")],
            "test writer",
        )
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
    writer
        .create_memories(&[first, second], "test writer")
        .unwrap();
    let reader = Store::open_read_only(&path).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let done = Arc::new(AtomicBool::new(false));
    let writer_barrier = Arc::clone(&barrier);
    let writer_done = Arc::clone(&done);
    let worker = std::thread::spawn(move || {
        writer_barrier.wait();
        let result = (|| -> semmem::models::AppResult<()> {
            for version in 1..=100 {
                let body = format!("generation{version}");
                writer.update_memories(
                    &[
                        update_memory("first", version, &body),
                        update_memory("second", version, &body),
                    ],
                    "test writer",
                )?;
            }

            Ok(())
        })();
        writer_done.store(true, Ordering::Release);

        result
    });
    let request = GetMemories {
        view: Default::default(),
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
            view: Default::default(),
            keys: vec!["second".into(), "first".into()],
            include_deleted: false,
            version: Some(1),
        })
        .unwrap();
    assert_eq!(historical[0].key, "second");
    assert!(historical.iter().all(|record| record.body == "generation0"));
}

#[test]
fn metadata_survives_revisions_deletion_restoration_and_retry_checks() {
    let mut store = Store::open(":memory:").unwrap();
    let mut input = create_memory("metadata");
    input.metadata = MemoryMetadata {
        summary: Some("A supplied summary".into()),
        basis: Some(Basis::STATED),
        scope: Some("project/environment".into()),
        observed_at: Some(100),
        verified_at: Some(200),
        review_after: Some(300),
    }
    .into();
    let created = store
        .create_memories(std::slice::from_ref(&input), "test writer")
        .unwrap();
    assert_eq!(
        created[0].metadata,
        input.metadata.apply(&Default::default())
    );
    assert_eq!(
        store
            .create_memories(std::slice::from_ref(&input), "test writer")
            .unwrap(),
        created
    );
    let mut different = input.clone();
    different.metadata.review_after = Some(None);
    assert_eq!(
        store
            .create_memories(&[different], "test writer")
            .unwrap_err()
            .code,
        "already_exists"
    );
    let update: UpdateMemory = serde_json::from_value(serde_json::json!({
        "key": "metadata", "expected_version": 1,
        "summary": null, "basis": "observed", "verified_at": 400
    }))
    .unwrap();
    let updated = store.update_memories(&[update], "test writer").unwrap();
    assert_eq!(updated[0].metadata.summary, None);
    assert_eq!(updated[0].metadata.basis, Some(Basis::OBSERVED));
    assert_eq!(updated[0].metadata.verified_at, Some(400));
    assert_eq!(
        updated[0].metadata.scope,
        input.metadata.scope.clone().unwrap()
    );
    assert_eq!(
        updated[0].metadata.observed_at,
        input.metadata.observed_at.unwrap()
    );
    assert_eq!(
        updated[0].metadata.review_after,
        input.metadata.review_after.unwrap()
    );
    let deleted = store
        .delete_memories(
            &[MemoryVersion {
                key: "metadata".into(),
                expected_version: 2,
            }],
            "human",
        )
        .unwrap();
    assert_eq!(deleted[0].metadata, updated[0].metadata);
    let restored = store
        .restore_memories(
            &[RestoreMemory {
                key: "metadata".into(),
                expected_version: 3,
                revision: Some(1),
            }],
            "test writer",
        )
        .unwrap();
    assert_eq!(
        restored[0].metadata,
        input.metadata.apply(&Default::default())
    );
    assert_eq!(
        store.get_revision("metadata", 2).unwrap().metadata,
        updated[0].metadata
    );
    let filtered = store
        .list_memories(&ListMemories {
            scope: input.metadata.scope.unwrap(),
            basis: input.metadata.basis.unwrap(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert!(
        store
            .list_memories(&ListMemories {
                scope: Some("another project".into()),
                ..Default::default()
            })
            .unwrap()
            .is_empty()
    );
    assert!(
        serde_json::from_value::<CreateMemory>(
            serde_json::json!({"key":"invalid", "body":"Text", "basis":"guessed"})
        )
        .is_err()
    );
}

#[test]
fn legacy_summary_aliases_return_full_records_without_excerpts() {
    let mut store = Store::open(":memory:").unwrap();
    let mut input = create_memory("complete");
    input.body = "First paragraph.\n\nFull remaining body 日本語.".into();
    input.metadata.summary = Some(Some("Manual summary".into()));
    store.create_memories(&[input.clone()], "writer").unwrap();
    let request = GetMemories {
        keys: vec!["complete".into()],
        ..Default::default()
    };
    let records = store.get_memory_summaries(&request).unwrap();
    assert_eq!(records[0]["body"], input.body);
    assert_eq!(records[0]["summary"], "Manual summary");
    assert!(records[0].get("excerpt").is_none());
    assert!(records[0].get("references").is_some());
    let history = store
        .list_revision_summaries(&HistoryRequest {
            key: "complete".into(),
            limit: 20,
            before_version: None,
        })
        .unwrap();
    assert_eq!(history[0]["body"], input.body);
    let listed = store
        .list_memory_summaries(&ListMemories::default())
        .unwrap();
    assert_eq!(listed[0]["body"], input.body);
}

#[test]
fn accepts_batches_and_reads_above_previous_caps_and_bind_variable_counts() {
    let mut store = Store::open(":memory:").unwrap();
    let inputs = (0..1100)
        .map(|index| create_memory(&format!("large-{index}")))
        .collect::<Vec<_>>();
    assert_eq!(
        store.create_memories(&inputs, "test writer").unwrap().len(),
        inputs.len()
    );
    let records = store
        .get_memories(&GetMemories {
            keys: (0..33000)
                .map(|index| format!("large-{}", index % inputs.len()))
                .collect(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(records.len(), 33000);
    assert_eq!(records.last().unwrap().key, "large-1099");
    assert_eq!(
        store
            .list_memories(&ListMemories {
                limit: 2000,
                ..Default::default()
            })
            .unwrap()
            .len(),
        inputs.len()
    );
}

#[test]
fn defaults_preserve_nulls_and_trusted_audit_identity() {
    let mut store = Store::open(":memory:").unwrap();
    let input: CreateMemory = serde_json::from_value(serde_json::json!({
        "key": "defaults", "body": "Complete content", "author": "Content author"
    }))
    .unwrap();
    let created = store
        .create_memories(std::slice::from_ref(&input), "authenticated writer")
        .unwrap();
    assert_eq!(created[0].r#type, "general");
    assert_eq!(created[0].author, "Content author");
    assert_eq!(
        created[0].metadata.observed_at,
        Some(created[0].system_created_at)
    );
    assert_eq!(
        created[0].metadata.verified_at,
        Some(created[0].system_created_at)
    );
    assert_eq!(created[0].system_created_by, "authenticated writer");
    assert_eq!(created[0].system_updated_by, "authenticated writer");
    assert_eq!(
        store.create_memories(&[input], "retry writer").unwrap(),
        created
    );
    let nulls: CreateMemory = serde_json::from_value(serde_json::json!({
        "key": "nulls", "body": "Null timestamps", "author": "Content author",
        "observed_at": null, "verified_at": null
    }))
    .unwrap();
    let null_record = store.create_memories(&[nulls], "writer").unwrap().remove(0);
    assert_eq!(null_record.metadata.observed_at, None);
    assert_eq!(null_record.metadata.verified_at, None);
    let update: UpdateMemory = serde_json::from_value(serde_json::json!({
        "key": "defaults", "expected_version": 1, "author": "Second author", "basis": "assumed"
    }))
    .unwrap();
    let updated = store
        .update_memories(&[update], "editor identity")
        .unwrap()
        .remove(0);
    assert_eq!(updated.author, "Second author");
    assert_eq!(updated.system_created_by, "authenticated writer");
    assert_eq!(updated.system_updated_by, "editor identity");
    assert_eq!(
        updated.metadata.observed_at,
        created[0].metadata.observed_at
    );
    assert_eq!(updated.metadata.basis, Some(Basis::ASSUMED));
    let deleted = store
        .delete_memories(
            &[MemoryVersion {
                key: "defaults".into(),
                expected_version: 2,
            }],
            "deleter identity",
        )
        .unwrap()
        .remove(0);
    assert_eq!(deleted.author, "Second author");
    assert_eq!(
        deleted.system_deleted_by.as_deref(),
        Some("deleter identity")
    );
    let restored = store
        .restore_memories(
            &[RestoreMemory {
                key: "defaults".into(),
                expected_version: 3,
                revision: Some(1),
            }],
            "restorer identity",
        )
        .unwrap()
        .remove(0);
    assert_eq!(restored.author, "Content author");
    assert_eq!(restored.system_updated_by, "restorer identity");
    assert_eq!(restored.system_deleted_by, None);
    assert_eq!(store.revisions_for_key("defaults").unwrap().len(), 4);
    assert_eq!(store.all_memory_keys(0, 100).unwrap().len(), 2);
}

#[test]
fn authors_register_list_and_reject_referenced_or_reserved_deregistration() {
    let mut store = Store::open(":memory:").unwrap();
    assert!(store.list_authors().unwrap().is_empty());
    let registered = store
        .register_author("Available author", "registrar")
        .unwrap();
    assert_eq!(registered.author, "Available author");
    assert_eq!(registered.system_created_by, "registrar");
    assert_eq!(store.list_authors().unwrap().len(), 1);
    store
        .deregister_author("Available author", "registrar")
        .unwrap();
    assert!(store.list_authors().unwrap().is_empty());
    assert!(store.register_author(" ", "registrar").is_err());
    assert!(store.register_author("system", "registrar").is_err());
    assert!(store.deregister_author("system", "registrar").is_err());
    store
        .create_memories(&[create_memory("authored")], "writer")
        .unwrap();
    assert_eq!(
        store
            .deregister_author("human", "registrar")
            .unwrap_err()
            .code,
        "referenced_author"
    );
}

#[test]
fn every_application_table_has_required_author_and_system_attribution() {
    let store = Store::open(":memory:").unwrap();
    let tables = [
        "database",
        "authors",
        "memory_types",
        "reference_types",
        "tags",
        "memories",
        "memory_revisions",
        "memory_revision_tags",
        "memory_revision_references",
        "index_events",
    ];

    for table in tables {
        let sql = format!(
            "SELECT count(*) FROM pragma_table_info('{table}') WHERE name = 'author' AND \"notnull\" = 1"
        );
        let required: i64 = store
            .connection()
            .query_row(&sql, [], |row| row.get(0))
            .unwrap();
        assert_eq!(required, 1, "{table} requires author");
        let sql = format!(
            "SELECT count(*) FROM pragma_table_info('{table}') WHERE name IN ('system_created_by', 'system_updated_by') AND \"notnull\" = 1"
        );
        let audit: i64 = store
            .connection()
            .query_row(&sql, [], |row| row.get(0))
            .unwrap();
        assert_eq!(audit, 2, "{table} requires system attribution");
        let sql = format!(
            "SELECT count(*) FROM pragma_foreign_key_list('{table}') WHERE \"from\" = 'author' AND \"table\" = 'authors'"
        );
        let foreign_key: i64 = store
            .connection()
            .query_row(&sql, [], |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_key, 1, "{table} has normalized author");
    }
}

#[test]
fn purge_requeues_sources_with_only_historical_references() {
    let mut store = Store::open(":memory:").unwrap();
    let mut source = create_memory("historical_source");
    source.references.push(MemoryReference {
        target_key: "historical_target".into(),
        reference_type: "related to".into(),
    });
    store
        .create_memories(&[source, create_memory("historical_target")], "writer")
        .unwrap();
    let mut update = update_memory("historical_source", 1, "Current body");
    update.references = Some(Vec::new());
    store.update_memories(&[update], "writer").unwrap();
    let watermark = store.index_events(0, 100).unwrap().last().unwrap().sequence;
    store.acknowledge_events(watermark).unwrap();
    store
        .purge_memories(
            &[MemoryVersion {
                key: "historical_target".into(),
                expected_version: 1,
            }],
            "purger",
        )
        .unwrap();
    let events = store.index_events(watermark, 100).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.memory_key == "historical_source")
    );
    assert!(
        store
            .get_revision("historical_source", 1)
            .unwrap()
            .references
            .is_empty()
    );
    assert_eq!(
        store
            .get_memory("historical_source", false)
            .unwrap()
            .system_version,
        2
    );
}

#[test]
fn sql_defaults_use_general_and_current_timestamps_and_keep_default_type() {
    let mut store = Store::open(":memory:").unwrap();
    store.register_author("SQL author", "registrar").unwrap();
    assert_eq!(
        store
            .delete_taxonomy("types", "general", "writer")
            .unwrap_err()
            .code,
        "default_type"
    );
    let transaction = store.connection().unchecked_transaction().unwrap();
    transaction.execute("INSERT INTO memories (key, author, system_created_by, system_updated_by, system_created_at, system_updated_at, system_version) SELECT 'sql-defaults', id, 'writer', 'writer', 1, 1, 1 FROM authors WHERE name = 'SQL author'", []).unwrap();
    let id = transaction.last_insert_rowid();
    transaction.execute("INSERT INTO memory_revisions (memory_id, title, body, author, system_version, system_operation, system_created_by, system_updated_by, system_created_at, system_updated_at) SELECT ?1, 'SQL defaults', 'Full SQL body', id, 1, 'create', 'writer', 'writer', 1, 1 FROM authors WHERE name = 'SQL author'", [id]).unwrap();
    transaction.commit().unwrap();
    let record = store.get_memory("sql-defaults", false).unwrap();
    assert_eq!(record.r#type, "general");
    assert!(record.metadata.observed_at.unwrap() > 1_000_000_000_000);
    assert!(record.metadata.verified_at.unwrap() > 1_000_000_000_000);
}
