use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

use semmem::models::{
    ClaimType, CreateMemory, GetMemories, HistoryRequest, ListMemories, MemoryLink, MemoryMetadata,
    MemoryVersion, RestoreMemory, TaxonomyInput, UpdateMemory,
};
use semmem::storage::Store;

fn create_memory(key: &str) -> CreateMemory {
    CreateMemory {
        key: key.into(),
        title: format!("Title {key}"),
        metadata: Default::default(),
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
        metadata: Default::default(),
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
fn initializes_current_schema_and_preserves_records_on_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("initialized.sqlite3");
    let mut store = Store::open(&path).unwrap();
    let identity = store.database_key().unwrap();
    let records = store
        .create_memories(&[create_memory("initialized")])
        .unwrap();
    let version: i64 = store
        .connection()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3);
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
    for version in [0, 1, 2, 4] {
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
        let result = (|| -> semmem::models::AppResult<()> {
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
        claim_type: Some(ClaimType::EXPLICIT_USER_STATEMENT),
        scope: Some("project/environment".into()),
        observed_at: Some(100),
        verified_at: Some(200),
        review_after: Some(300),
    };
    let created = store.create_memories(std::slice::from_ref(&input)).unwrap();
    assert_eq!(created[0].metadata, input.metadata);
    assert_eq!(
        store.create_memories(std::slice::from_ref(&input)).unwrap(),
        created
    );
    let mut different = input.clone();
    different.metadata.review_after = None;
    assert_eq!(
        store.create_memories(&[different]).unwrap_err().code,
        "already_exists"
    );
    let update: UpdateMemory = serde_json::from_value(serde_json::json!({
        "key": "metadata", "expected_version": 1,
        "summary": null, "claim_type": "verified observation", "verified_at": 400
    }))
    .unwrap();
    let updated = store.update_memories(&[update]).unwrap();
    assert_eq!(updated[0].metadata.summary, None);
    assert_eq!(
        updated[0].metadata.claim_type,
        Some(ClaimType::VERIFIED_OBSERVATION)
    );
    assert_eq!(updated[0].metadata.verified_at, Some(400));
    assert_eq!(updated[0].metadata.scope, input.metadata.scope);
    assert_eq!(updated[0].metadata.observed_at, input.metadata.observed_at);
    assert_eq!(
        updated[0].metadata.review_after,
        input.metadata.review_after
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
        .restore_memories(&[RestoreMemory {
            key: "metadata".into(),
            expected_version: 3,
            revision: Some(1),
            actor: "human".into(),
        }])
        .unwrap();
    assert_eq!(restored[0].metadata, input.metadata);
    assert_eq!(
        store.get_revision("metadata", 2).unwrap().metadata,
        updated[0].metadata
    );
    let filtered = store
        .list_memories(&ListMemories {
            scope: input.metadata.scope,
            claim_type: input.metadata.claim_type,
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
            serde_json::json!({"key":"invalid", "body":"Text", "claim_type":"guessed"})
        )
        .is_err()
    );
}

#[test]
fn compact_reads_preserve_manual_summaries_and_complete_paragraphs() {
    let mut store = Store::open(":memory:").unwrap();
    let paragraph = "A complete paragraph with Unicode 日本語. ".repeat(100);
    let mut automatic = create_memory("automatic");
    automatic.body = format!("\n\n{paragraph}\n\nAdditional full body content.");
    let mut whitespace = create_memory("whitespace");
    whitespace.metadata.summary = Some("\u{2003} \n".into());
    whitespace.body = "Whitespace summary fallback\r\n\r\nFurther text".into();
    let mut manual = create_memory("manual");
    manual.metadata.summary = Some("Manually supplied summary".repeat(100));
    store
        .create_memories(&[automatic.clone(), manual.clone(), whitespace])
        .unwrap();
    let request = GetMemories {
        keys: vec!["automatic".into(), "manual".into()],
        ..Default::default()
    };
    let summaries = store.get_memory_summaries(&request).unwrap();
    assert_eq!(summaries[0]["excerpt"], paragraph);
    assert_eq!(summaries[1]["summary"], manual.metadata.summary.unwrap());
    assert!(summaries[1].get("excerpt").is_none());
    for summary in &summaries {
        for absent in ["body", "tags", "links"] {
            assert!(summary.get(absent).is_none());
        }
        assert!(summary.get("system_version").is_some());
    }
    assert_eq!(
        store.get_memories(&request).unwrap()[0].body,
        automatic.body
    );
    assert_eq!(
        store
            .list_memory_summaries(&ListMemories::default())
            .unwrap()
            .len(),
        3
    );
    let whitespace = store
        .get_memory_summaries(&GetMemories {
            keys: vec!["whitespace".into()],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(whitespace[0]["excerpt"], "Whitespace summary fallback");
    let history = store
        .list_revision_summaries(&HistoryRequest {
            key: "automatic".into(),
            limit: 20,
            before_version: None,
        })
        .unwrap();
    assert_eq!(history[0]["excerpt"], paragraph);
    assert!(history[0].get("body").is_none());
}

#[test]
fn accepts_batches_and_reads_above_previous_caps_and_bind_variable_counts() {
    let mut store = Store::open(":memory:").unwrap();
    let inputs = (0..1100)
        .map(|index| create_memory(&format!("large-{index}")))
        .collect::<Vec<_>>();
    assert_eq!(store.create_memories(&inputs).unwrap().len(), inputs.len());
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
fn compact_paragraph_projection_preserves_crlf_and_indentation() {
    let mut store = Store::open(":memory:").unwrap();
    let mut input = create_memory("paragraph");
    input.body =
        "\r\n \t\r\n    First line.\r\n    日本語 second line.  \r\n \t\r\nOther paragraph.".into();
    store.create_memories(&[input]).unwrap();
    let summaries = store
        .get_memory_summaries(&GetMemories {
            keys: vec!["paragraph".into()],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        summaries[0]["excerpt"],
        "    First line.\r\n    日本語 second line.  "
    );
}
