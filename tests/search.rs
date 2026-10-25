use semmem::models::{Basis, MemoryMetadata, MemoryRecord, SearchRequest};
use semmem::search::{InvalidSearchQuery, SearchIndex};

fn create_memory(id: i64, key: &str, title: &str, body: &str) -> MemoryRecord {
    MemoryRecord {
        id,
        key: key.into(),
        title: title.into(),
        body: body.into(),
        metadata: MemoryMetadata::default(),
        r#type: "note".into(),
        tags: vec![],
        references: vec![],
        author: "test".into(),
        system_version: 1,
        system_created_at: 1,
        system_updated_at: 1,
        system_deleted_at: None,
        system_created_by: "test".into(),
        system_updated_by: "test".into(),
        system_deleted_by: None,
    }
}

fn create_request(query: &str) -> SearchRequest {
    SearchRequest {
        query: query.into(),
        scope: None,
        basis: None,
        r#type: None,
        tags: vec![],
        prefix: false,
        fuzzy: true,
        limit: 20,
        history: false,
        as_of: None,
        include_deleted: false,
    }
}

#[test]
fn should_globally_rank_exact_and_typo_alternatives() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    index
        .upsert(&create_memory(1, "exact", "", "memory"))
        .unwrap();
    index
        .upsert(&create_memory(2, "typo", "memroy", "alternative"))
        .unwrap();
    index.commit(1).unwrap();

    let response = index.search(&create_request("memory")).unwrap();

    assert_eq!(response.total, 2);
    assert_eq!(response.hits[0].key, "typo");
    assert_eq!(response.hits[1].key, "exact");
    assert!(response.hits[0].score >= response.hits[1].score);
    assert!(response.applied_fuzzy);
    assert!(!response.truncated);
}

#[test]
fn should_support_transpositions_and_two_edits_for_long_terms() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    index
        .upsert(&create_memory(1, "record", "", "persistent memory"))
        .unwrap();
    index.commit(1).unwrap();

    assert_eq!(index.search(&create_request("memroy")).unwrap().total, 1);
    assert_eq!(
        index.search(&create_request("persxstentt")).unwrap().total,
        1
    );

    let mut exact_only = create_request("memroy");
    exact_only.fuzzy = false;

    assert_eq!(index.search(&exact_only).unwrap().total, 0);
    assert!(!index.search(&exact_only).unwrap().applied_fuzzy);
}

#[test]
fn should_boost_title_matches_and_return_the_stored_title() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    index
        .upsert(&create_memory(1, "body", "Unrelated", "registry"))
        .unwrap();
    index
        .upsert(&create_memory(2, "title", "Registry", "Unrelated"))
        .unwrap();
    index.commit(1).unwrap();

    let response = index.search(&create_request("registry")).unwrap();

    assert_eq!(response.hits[0].key, "title");
    assert_eq!(response.hits[0].title, "Registry");
}

#[test]
fn should_keep_short_terms_exact_in_fuzzy_mode() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    index
        .upsert(&create_memory(1, "record", "", "cat"))
        .unwrap();
    index.commit(1).unwrap();

    let response = index.search(&create_request("car")).unwrap();

    assert_eq!(response.total, 0);
    assert!(!response.applied_fuzzy);
}

#[test]
fn should_keep_quoted_phrases_exact_and_ordered() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    index
        .upsert(&create_memory(
            1,
            "phrase",
            "",
            "persistent memory registry",
        ))
        .unwrap();
    index
        .upsert(&create_memory(
            2,
            "reversed",
            "",
            "memory persistent registry",
        ))
        .unwrap();
    index.commit(1).unwrap();

    let response = index
        .search(&create_request("\"persistent memory\""))
        .unwrap();

    assert_eq!(response.total, 1);
    assert_eq!(response.hits[0].key, "phrase");
    assert!(!response.applied_fuzzy);
    assert_eq!(
        index
            .search(&create_request("\"persxstent memory\""))
            .unwrap()
            .total,
        0
    );
}

#[test]
fn should_apply_conjunction_across_title_and_body() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    index
        .upsert(&create_memory(1, "both", "SQLite", "A durable registry"))
        .unwrap();
    index
        .upsert(&create_memory(2, "one", "SQLite", "Unrelated topic"))
        .unwrap();
    index.commit(1).unwrap();

    let response = index.search(&create_request("sqlite durable")).unwrap();

    assert_eq!(response.total, 1);
    assert_eq!(response.hits[0].key, "both");
}

#[test]
fn should_match_prefix_only_on_final_unquoted_term() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    index
        .upsert(&create_memory(1, "record", "", "durable registry"))
        .unwrap();
    index.commit(1).unwrap();

    let mut request = create_request("durable reg");
    request.prefix = true;
    request.fuzzy = false;

    assert_eq!(index.search(&request).unwrap().total, 1);

    request.query = "dur reg".into();

    assert_eq!(index.search(&request).unwrap().total, 0);

    request.query = "\"durable reg\"".into();

    assert_eq!(index.search(&request).unwrap().total, 0);
}

#[test]
fn should_preserve_unicode_bodies_and_fold_case_and_accents() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    index
        .upsert(&create_memory(
            1,
            "unicode",
            "",
            "Café 東京 🦀 UTF-8 Markdown",
        ))
        .unwrap();
    index.commit(1).unwrap();

    let response = index.search(&create_request("CAFE")).unwrap();

    assert_eq!(response.total, 1);
    assert_eq!(response.hits[0].body, "Café 東京 🦀 UTF-8 Markdown");
    assert_eq!(index.search(&create_request("東京")).unwrap().total, 1);
}

#[test]
fn should_require_every_tag_filter_and_exact_type() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    let mut first = create_memory(1, "both", "", "Shared memory");
    first.r#type = "decision".into();
    first.tags = vec!["rust".into(), "database".into()];
    let mut second = create_memory(2, "one", "", "Shared memory");
    second.r#type = "decision".into();
    second.tags = vec!["rust".into()];
    index.upsert(&first).unwrap();
    index.upsert(&second).unwrap();
    index.commit(1).unwrap();

    let mut request = create_request("memory");
    request.r#type = Some("decision".into());
    request.tags = vec!["rust".into(), "database".into()];

    let response = index.search(&request).unwrap();

    assert_eq!(response.total, 1);
    assert_eq!(response.hits[0].key, "both");

    request.r#type = Some("note".into());

    assert_eq!(index.search(&request).unwrap().total, 0);
}

#[test]
fn should_publish_cloned_reader_and_replace_and_delete_without_duplicates() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    let reader = index.reader();
    let mut memory = create_memory(1, "record", "", "oldword");
    index.upsert(&memory).unwrap();

    assert_eq!(reader.search(&create_request("oldword")).unwrap().total, 0);

    index.commit(1).unwrap();

    assert_eq!(reader.search(&create_request("oldword")).unwrap().total, 1);

    memory.body = "newword".into();
    memory.system_version = 2;
    index.upsert(&memory).unwrap();
    index.upsert(&memory).unwrap();
    index.commit(2).unwrap();

    let response = reader.search(&create_request("newword")).unwrap();

    assert_eq!(response.total, 1);
    assert_eq!(response.hits[0].version, 2);

    let mut old_query = create_request("oldword");
    old_query.fuzzy = false;

    assert_eq!(reader.search(&old_query).unwrap().total, 0);

    index.remove("record").unwrap();
    index.commit(3).unwrap();

    assert_eq!(reader.search(&create_request("newword")).unwrap().total, 0);
}

#[test]
fn should_persist_watermark_and_search_after_reopening() {
    let directory = tempfile::tempdir().unwrap();

    {
        let mut index = SearchIndex::open(directory.path()).unwrap();
        index
            .upsert(&create_memory(1, "record", "", "durable"))
            .unwrap();
        index.commit(42).unwrap();

        assert!(index.commit(41).is_err());
    }

    let index = SearchIndex::open(directory.path()).unwrap();

    assert_eq!(index.watermark().unwrap(), 42);
    assert_eq!(index.search(&create_request("durable")).unwrap().total, 1);
}

#[test]
fn should_sort_ties_by_key_and_report_result_truncation() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    index
        .upsert(&create_memory(20, "second", "", "sameword"))
        .unwrap();
    index
        .upsert(&create_memory(10, "first", "", "sameword"))
        .unwrap();
    index.commit(1).unwrap();

    let mut request = create_request("sameword");
    request.limit = 1;

    let response = index.search(&request).unwrap();

    assert_eq!(response.hits[0].key, "first");
    assert_eq!(response.total, 2);
    assert!(response.truncated);
}

#[test]
fn should_reject_only_malformed_queries_and_zero_page_size() {
    let directory = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(directory.path()).unwrap();

    for query in ["", "!!!", "\"unclosed"] {
        let error = index.search(&create_request(query)).unwrap_err();
        assert!(error.downcast_ref::<InvalidSearchQuery>().is_some());
    }

    assert!(index.search(&create_request(&"word ".repeat(17))).is_ok());
    let mut long_request = create_request(&"x".repeat(4097));
    long_request.fuzzy = false;
    assert!(index.search(&long_request).is_ok());

    let mut request = create_request("word");
    request.limit = 101;

    assert!(index.search(&request).is_ok());

    request.limit = 0;

    assert!(index.search(&request).is_err());
}

fn create_variants(source: &str, count: usize) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut variants = Vec::with_capacity(count);

    for first in 0..bytes.len() {
        for second in (first + 1)..bytes.len() {
            for first_value in b'a'..=b'z' {
                for second_value in b'a'..=b'z' {
                    if first_value == bytes[first] || second_value == bytes[second] {
                        continue;
                    }

                    let mut candidate = bytes.to_vec();
                    candidate[first] = first_value;
                    candidate[second] = second_value;
                    variants.push(String::from_utf8(candidate).unwrap());

                    if variants.len() == count {
                        return variants;
                    }
                }
            }
        }
    }

    panic!("The fixture does not have enough two-edit variants.");
}

#[test]
fn should_search_fuzzy_expansion_above_the_former_per_term_cap() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    let body = create_variants("abcdefgh", 2049).join(" ");
    index.upsert(&create_memory(1, "broad", "", &body)).unwrap();
    index.commit(1).unwrap();
    assert_eq!(index.search(&create_request("abcdefgh")).unwrap().total, 1);
    let mut exact = create_request("abcdefgh");
    exact.fuzzy = false;
    assert_eq!(index.search(&exact).unwrap().total, 0);
}

#[test]
fn should_search_prefix_expansion_above_the_former_per_term_cap() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    let body = (0..2049)
        .map(|value| format!("prefix{value:05}"))
        .collect::<Vec<_>>()
        .join(" ");
    index.upsert(&create_memory(1, "broad", "", &body)).unwrap();
    index.commit(1).unwrap();
    let mut request = create_request("prefix");
    request.prefix = true;
    request.fuzzy = false;
    assert_eq!(index.search(&request).unwrap().total, 1);
}

#[test]
fn should_search_expansion_above_the_former_query_cap() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    let terms = ["abcdefgh", "ijklmnop", "qrstuvwx", "zyxwvuts", "fedcbaqr"];
    let body = terms
        .iter()
        .flat_map(|term| create_variants(term, 1800))
        .collect::<Vec<_>>()
        .join(" ");
    index.upsert(&create_memory(1, "broad", "", &body)).unwrap();
    index.commit(1).unwrap();
    assert_eq!(
        index
            .search(&create_request(&terms.join(" ")))
            .unwrap()
            .total,
        1
    );
}

#[test]
fn should_deduplicate_expansion_across_fields_and_repeated_query_terms() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    let body = create_variants("abcdefgh", 1800).join(" ");
    index
        .upsert(&create_memory(1, "shared", &body, &body))
        .unwrap();
    index.commit(1).unwrap();
    let response = index
        .search(&create_request(
            "abcdefgh abcdefgh abcdefgh abcdefgh abcdefgh",
        ))
        .unwrap();
    assert_eq!(response.total, 1);
    assert_eq!(response.hits[0].key, "shared");
}

#[test]
fn should_return_complete_manual_summary_and_filter_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    let mut memory = create_memory(1, "summary", "Title", "Unrelated body");
    memory.metadata = MemoryMetadata {
        summary: Some(format!("Durable registry {}", "café 東京 ".repeat(100))),
        basis: Some(Basis::OBSERVED),
        scope: Some("project/matrix".into()),
        observed_at: Some(100),
        verified_at: Some(200),
        review_after: Some(300),
    };
    index.upsert(&memory).unwrap();
    index.commit(1).unwrap();
    let mut request = create_request("\"durable registry\"");
    request.scope = memory.metadata.scope.clone();
    request.basis = memory.metadata.basis;
    let response = index.search(&request).unwrap();
    assert_eq!(response.total, 1);
    assert_eq!(response.hits[0].metadata, memory.metadata);
    assert_eq!(response.hits[0].body, memory.body);
    assert_eq!(index.search(&create_request("regsitry")).unwrap().total, 1);
    request.scope = Some("other".into());
    assert_eq!(index.search(&request).unwrap().total, 0);
    request.scope = memory.metadata.scope.clone();
    request.basis = Some(Basis::INFERRED);
    assert_eq!(index.search(&request).unwrap().total, 0);
}

#[test]
fn should_return_full_body_without_a_character_cap() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    let paragraph = format!("The registry preserves {}", "café 東京 🦀 ".repeat(100));
    let memory = create_memory(
        1,
        "paragraph",
        "",
        &format!("# Heading\n\n{paragraph}\n\nOther context"),
    );
    index.upsert(&memory).unwrap();
    index.commit(1).unwrap();
    let response = index.search(&create_request("regsitry")).unwrap();
    assert_eq!(response.hits[0].body, memory.body);
    assert!(response.hits[0].metadata.summary.is_none());
}

#[test]
fn should_accept_more_results_tags_and_terms_than_the_former_caps() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    let tags = (0..40)
        .map(|number| format!("tag{number}"))
        .collect::<Vec<_>>();

    for id in 1..=125 {
        let mut memory = create_memory(id, &format!("record{id}"), "", "shared registry");
        memory.tags = tags.clone();
        index.upsert(&memory).unwrap();
    }

    index.commit(1).unwrap();
    let mut request = create_request(&"shared ".repeat(1000));
    request.tags = tags;
    request.limit = 125;
    request.fuzzy = false;
    let response = index.search(&request).unwrap();
    assert_eq!(response.hits.len(), 125);
    assert_eq!(response.total, 125);
    assert!(!response.truncated);
}

#[test]
fn should_publish_summary_only_updates_and_remove_old_scope_matches() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    let reader = index.reader();
    let mut memory = create_memory(1, "changed", "", "body");
    memory.metadata.summary = Some("oldsummary".into());
    memory.metadata.scope = Some("oldscope".into());
    index.upsert(&memory).unwrap();
    index.commit(1).unwrap();
    memory.system_version = 2;
    memory.metadata.summary = Some("newsummary".into());
    memory.metadata.scope = Some("newscope".into());
    index.upsert(&memory).unwrap();
    index.commit(2).unwrap();
    let mut old = create_request("oldsummary");
    old.fuzzy = false;
    assert_eq!(reader.search(&old).unwrap().total, 0);
    let mut current = create_request("newsummary");
    current.scope = Some("newscope".into());
    assert_eq!(reader.search(&current).unwrap().hits[0].version, 2);
    current.scope = Some("oldscope".into());
    assert_eq!(reader.search(&current).unwrap().total, 0);
}

#[test]
fn should_return_full_bodies_for_empty_or_whitespace_summaries() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();

    for (id, summary) in [(1, ""), (2, " \n ")] {
        let mut memory = create_memory(
            id,
            &format!("empty{id}"),
            "",
            "Readable registry paragraph.",
        );
        memory.metadata.summary = Some(summary.into());
        index.upsert(&memory).unwrap();
    }

    index.commit(1).unwrap();
    let response = index.search(&create_request("registry")).unwrap();
    assert_eq!(response.hits.len(), 2);

    for hit in response.hits {
        assert_eq!(hit.body, "Readable registry paragraph.");
        assert!(hit.metadata.summary.is_some());
    }
}

#[test]
fn should_preserve_matching_paragraphs_with_crlf_and_whitespace_blank_lines() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    let paragraph = format!(
        "  Registry paragraph {}\r\nSecond line stays intact.  ",
        "café 東京 🦀 ".repeat(100)
    );
    let body = format!("\r\n# Heading\r\n \t\r\n{paragraph}\r\n\u{2003}\r\nOther context");
    index.upsert(&create_memory(1, "crlf", "", &body)).unwrap();
    index.commit(1).unwrap();
    let response = index.search(&create_request("regsitry")).unwrap();
    assert_eq!(response.hits[0].body, body);
}

#[test]
fn should_preserve_full_body_when_a_later_phrase_matches() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    let paragraph = "The durable registry preserves the complete decision.";
    let body = format!("A durable preliminary note.\n \t\n{paragraph}\n\nOther context");
    index
        .upsert(&create_memory(1, "phrase-preview", "", &body))
        .unwrap();
    index.commit(1).unwrap();
    let response = index
        .search(&create_request("\"durable registry\""))
        .unwrap();
    assert_eq!(response.hits[0].body, body);
}

#[test]
fn should_return_full_body_when_only_the_title_matches() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    let paragraph = "  First body paragraph.\r\nContinued context.  ";
    let body = format!(" \t\r\n{paragraph}\r\n \t\r\nSecond body paragraph.");
    index
        .upsert(&create_memory(1, "fallback-preview", "registry", &body))
        .unwrap();
    index.commit(1).unwrap();
    let response = index.search(&create_request("registry")).unwrap();
    assert_eq!(response.hits[0].body, body);
}

#[test]
fn should_search_retained_revisions_and_as_of_snapshots_before_limiting() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    let mut original = create_memory(1, "revised", "", "Original registry body\n\n東京");
    original.system_updated_at = 10;
    original.metadata.scope = Some("original".into());
    let mut changed = original.clone();
    changed.system_version = 2;
    changed.system_updated_at = 20;
    changed.body = "Changed registry body".into();
    changed.metadata.scope = Some("changed".into());
    let mut deleted = changed.clone();
    deleted.system_version = 3;
    deleted.system_updated_at = 30;
    deleted.system_deleted_at = Some(30);
    deleted.system_deleted_by = Some("test".into());
    index
        .replace_revisions(
            "revised",
            &[original.clone(), changed.clone(), deleted.clone()],
        )
        .unwrap();
    index.commit(1).unwrap();
    let mut request = create_request("registry");
    request.limit = 1;
    assert_eq!(index.search(&request).unwrap().total, 0);
    request.include_deleted = true;
    assert_eq!(index.search(&request).unwrap().hits[0].version, 3);
    request.include_deleted = false;
    request.history = true;
    let historical = index.search(&request).unwrap();
    assert_eq!(historical.total, 2);
    assert_eq!(historical.hits.len(), 1);
    assert!(historical.truncated);
    request.scope = Some("original".into());
    let historical = index.search(&request).unwrap();
    assert_eq!(historical.total, 1);
    assert_eq!(historical.hits[0].body, original.body);
    assert_eq!(historical.hits[0].system_updated_at, 10);
    assert_eq!(historical.hits[0].system_created_at, 1);
    assert_eq!(historical.hits[0].system_created_by, "test");
    assert_eq!(historical.hits[0].author, "test");
    request.history = false;
    request.scope = None;
    request.as_of = Some(9);
    assert_eq!(index.search(&request).unwrap().total, 0);
    request.as_of = Some(10);
    assert_eq!(index.search(&request).unwrap().hits[0].version, 1);
    request.as_of = Some(20);
    assert_eq!(index.search(&request).unwrap().hits[0].version, 2);
    request.as_of = Some(30);
    assert_eq!(index.search(&request).unwrap().total, 0);
    request.include_deleted = true;
    assert_eq!(index.search(&request).unwrap().hits[0].version, 3);
    request.history = true;
    assert!(index.search(&request).is_err());
    request.as_of = None;
    index.remove("revised").unwrap();
    index.commit(2).unwrap();
    assert_eq!(index.search(&request).unwrap().total, 0);
}

#[test]
fn should_choose_highest_version_at_equal_timestamps_and_reopen_history() {
    let directory = tempfile::tempdir().unwrap();
    let mut original = create_memory(1, "equal", "", "Shared registry original");
    original.system_updated_at = 20;
    let mut changed = original.clone();
    changed.system_version = 2;
    changed.body = "Shared registry updated".into();
    {
        let mut index = SearchIndex::open(directory.path()).unwrap();
        index
            .replace_revisions("equal", &[changed.clone(), original.clone()])
            .unwrap();
        index.commit(1).unwrap();
    }
    let index = SearchIndex::open(directory.path()).unwrap();
    let mut request = create_request("registry");
    request.as_of = Some(20);
    assert_eq!(index.search(&request).unwrap().hits[0].version, 2);
    request.as_of = None;
    request.history = true;
    let response = index.search(&request).unwrap();
    assert_eq!(response.total, 2);
    assert_eq!(response.hits[0].version, 2);
    assert_eq!(response.hits[1].version, 1);
}

#[test]
fn should_apply_global_ranking_before_the_limit_and_break_ties_by_key() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    index
        .upsert(&create_memory(1, "z-key", "", "memory"))
        .unwrap();
    index
        .upsert(&create_memory(2, "a-key", "", "memory"))
        .unwrap();
    index
        .upsert(&create_memory(3, "typo-title", "memroy", "other"))
        .unwrap();
    index.commit(1).unwrap();
    let mut request = create_request("memory");
    let response = index.search(&request).unwrap();
    assert!(
        response
            .hits
            .windows(2)
            .all(|pair| pair[0].score >= pair[1].score)
    );
    assert_eq!(response.hits[0].key, "typo-title");
    assert_eq!(response.hits[1].key, "a-key");
    assert_eq!(response.hits[2].key, "z-key");
    request.limit = 1;
    assert_eq!(index.search(&request).unwrap().hits[0].key, "typo-title");
}
