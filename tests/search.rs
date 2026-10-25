use semantic_memory::models::{MemoryRecord, SearchRequest};
use semantic_memory::search::{InvalidSearchQuery, SearchBudgetExceeded, SearchIndex};

fn create_memory(id: i64, key: &str, title: &str, body: &str) -> MemoryRecord {
    MemoryRecord {
        id,
        key: key.into(),
        title: title.into(),
        body: body.into(),
        kind: "note".into(),
        tags: vec![],
        links: vec![],
        actor: "test".into(),
        system_version: 1,
        system_created_at: 1,
        system_updated_at: 1,
        system_deleted_at: None,
    }
}

fn create_request(query: &str) -> SearchRequest {
    SearchRequest {
        query: query.into(),
        kind: None,
        tags: vec![],
        prefix: false,
        fuzzy: true,
        limit: 20,
    }
}

#[test]
fn should_return_exact_results_before_typo_alternatives() {
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
    assert_eq!(response.hits[0].key, "exact");
    assert_eq!(response.hits[1].key, "typo");
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
    assert!(response.hits[0].excerpt.contains("Café"));
    assert_eq!(index.search(&create_request("東京")).unwrap().total, 1);
}

#[test]
fn should_require_every_tag_filter_and_exact_kind() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    let mut first = create_memory(1, "both", "", "Shared memory");
    first.kind = "decision".into();
    first.tags = vec!["rust".into(), "database".into()];
    let mut second = create_memory(2, "one", "", "Shared memory");
    second.kind = "decision".into();
    second.tags = vec!["rust".into()];
    index.upsert(&first).unwrap();
    index.upsert(&second).unwrap();
    index.commit(1).unwrap();

    let mut request = create_request("memory");
    request.kind = Some("decision".into());
    request.tags = vec!["rust".into(), "database".into()];

    let response = index.search(&request).unwrap();

    assert_eq!(response.total, 1);
    assert_eq!(response.hits[0].key, "both");

    request.kind = Some("note".into());

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
fn should_sort_ties_by_identity_and_report_result_truncation() {
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
fn should_reject_unbounded_or_malformed_queries() {
    let directory = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(directory.path()).unwrap();

    for query in ["", "!!!", "\"unclosed"] {
        let error = index.search(&create_request(query)).unwrap_err();
        assert!(error.downcast_ref::<InvalidSearchQuery>().is_some());
    }

    assert!(index.search(&create_request(&"word ".repeat(17))).is_err());
    assert!(index.search(&create_request(&"x".repeat(4097))).is_err());

    let mut request = create_request("word");
    request.limit = 101;

    assert!(index.search(&request).is_err());

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
fn should_reject_fuzzy_expansion_above_the_per_term_budget() {
    let directory = tempfile::tempdir().unwrap();
    let mut index = SearchIndex::open(directory.path()).unwrap();
    let body = create_variants("abcdefgh", 2049).join(" ");
    index.upsert(&create_memory(1, "broad", "", &body)).unwrap();
    index.commit(1).unwrap();
    let error = index.search(&create_request("abcdefgh")).unwrap_err();
    assert!(error.downcast_ref::<SearchBudgetExceeded>().is_some());
    assert!(error.to_string().contains("2048"));
    let mut exact = create_request("abcdefgh");
    exact.fuzzy = false;
    assert_eq!(index.search(&exact).unwrap().total, 0);
}

#[test]
fn should_reject_prefix_expansion_above_the_per_term_budget() {
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
    let error = index.search(&request).unwrap_err();
    assert!(error.downcast_ref::<SearchBudgetExceeded>().is_some());
}

#[test]
fn should_reject_expansion_above_the_complete_query_budget() {
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
    let error = index.search(&create_request(&terms.join(" "))).unwrap_err();
    assert!(error.downcast_ref::<SearchBudgetExceeded>().is_some());
    assert!(error.to_string().contains("8192"));
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
