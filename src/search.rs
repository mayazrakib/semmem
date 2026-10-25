use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tantivy::collector::{Count, TopDocs};
use tantivy::indexer::LogMergePolicy;
use tantivy::query::{
    BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, PhraseQuery, Query, TermQuery, TermSetQuery,
};
use tantivy::schema::{
    FAST, Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::snippet::SnippetGenerator;
use tantivy::tokenizer::{
    AsciiFoldingFilter, LowerCaser, SimpleTokenizer, TextAnalyzer, TokenStream,
};
use tantivy::{
    DocId, Index, IndexReader, IndexWriter, ReloadPolicy, SegmentReader, TantivyDocument, Term,
};

use crate::models::{MemoryRecord, SearchHit, SearchRequest, SearchResponse};

const MAX_QUERY_BYTES: usize = 4096;
const MAX_QUERY_TERMS: usize = 16;
const MAX_TERM_CHARACTERS: usize = 128;
const MAX_RESULTS: usize = 100;
const MAX_EXPANSIONS_PER_TERM: usize = 2048;
const MAX_EXPANSIONS_PER_QUERY: usize = 8192;
const EXCERPT_CHARACTERS: usize = 240;
const INDEX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const MIN_MERGE_SEGMENTS: usize = 4;
const MAX_MERGE_INPUT_DOCUMENTS: usize = 50_000;
const MERGE_DELETED_DOCUMENT_RATIO: f32 = 0.2;
const TOKENIZER_NAME: &str = "memory_text";
const TITLE_BOOST: f32 = 4.0;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct InvalidSearchQuery(pub String);

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SearchBudgetExceeded(pub String);

#[derive(Default)]
struct ExpansionBudget {
    candidates: BTreeMap<String, BTreeSet<String>>,
    cached: BTreeMap<(String, u8, bool), Arc<Vec<String>>>,
    total: usize,
}

#[derive(Clone)]
struct SearchFields {
    id: Field,
    key: Field,
    version: Field,
    title: Field,
    body: Field,
    kind: Field,
    tags: Field,
}

#[derive(Clone)]
pub struct SearchReader {
    reader: IndexReader,
    fields: SearchFields,
}

pub struct SearchIndex {
    writer: IndexWriter,
    reader: SearchReader,
    watermark: i64,
}

struct QueryPart {
    terms: Vec<String>,
    is_phrase: bool,
}

impl SearchIndex {
    pub fn open(path: &Path) -> Result<Self> {
        std::fs::create_dir_all(path).context("Create search index directory")?;

        let (schema, fields) = build_schema();
        let mut index = if path.join("meta.json").exists() {
            let index = Index::open_in_dir(path).context("Open search index")?;

            if index.schema() != schema {
                bail!("Search index schema differs from this version; rebuild the index");
            }

            index
        } else {
            Index::create_in_dir(path, schema).context("Create search index")?
        };
        let executor_threads = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .min(4);
        index.set_multithread_executor(executor_threads)?;

        index
            .tokenizers()
            .register(TOKENIZER_NAME, build_analyzer());

        let watermark = index
            .load_metas()?
            .payload
            .map(|payload| payload.parse::<i64>())
            .transpose()
            .context("Invalid search index publication watermark")?
            .unwrap_or(0);
        let index_reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let writer = index.writer_with_num_threads(1, INDEX_MEMORY_BYTES)?;
        let mut merge_policy = LogMergePolicy::default();
        merge_policy.set_min_num_segments(MIN_MERGE_SEGMENTS);
        merge_policy.set_max_docs_before_merge(MAX_MERGE_INPUT_DOCUMENTS);
        merge_policy.set_del_docs_ratio_before_merge(MERGE_DELETED_DOCUMENT_RATIO);
        writer.set_merge_policy(Box::new(merge_policy));
        let reader = SearchReader {
            reader: index_reader,
            fields,
        };

        Ok(Self {
            writer,
            reader,
            watermark,
        })
    }

    pub fn reader(&self) -> SearchReader {
        self.reader.clone()
    }

    pub fn watermark(&self) -> Result<i64> {
        Ok(self.watermark)
    }

    pub fn upsert(&mut self, memory: &MemoryRecord) -> Result<()> {
        if memory.id <= 0 || memory.system_version <= 0 {
            bail!("Indexed memory identity and version must be positive");
        }

        self.remove(&memory.key)?;

        if memory.system_deleted_at.is_some() {
            return Ok(());
        }

        let fields = &self.reader.fields;
        let mut document = TantivyDocument::default();
        document.add_u64(fields.id, memory.id as u64);
        document.add_text(fields.key, &memory.key);
        document.add_i64(fields.version, memory.system_version);
        document.add_text(fields.title, &memory.title);
        document.add_text(fields.body, &memory.body);
        document.add_text(fields.kind, &memory.kind);

        for tag in &memory.tags {
            document.add_text(fields.tags, tag);
        }

        self.writer.add_document(document)?;

        Ok(())
    }

    pub fn remove(&mut self, key: &str) -> Result<()> {
        self.writer
            .delete_term(Term::from_field_text(self.reader.fields.key, key));

        Ok(())
    }

    pub fn commit(&mut self, watermark: i64) -> Result<()> {
        if watermark < self.watermark {
            bail!("Search publication watermark cannot move backwards");
        }

        let mut prepared_commit = self.writer.prepare_commit()?;
        prepared_commit.set_payload(&watermark.to_string());
        prepared_commit.commit()?;

        #[cfg(feature = "fault-injection")]
        if std::env::var("SEMANTIC_MEMORY_FAILPOINT").as_deref() == Ok("after_index_commit") {
            std::process::exit(86);
        }

        self.reader.reader.reload()?;
        self.watermark = watermark;

        Ok(())
    }

    pub fn search(&self, request: &SearchRequest) -> Result<SearchResponse> {
        self.reader.search(request)
    }
}

impl SearchReader {
    pub fn search(&self, request: &SearchRequest) -> Result<SearchResponse> {
        if request.limit == 0 || request.limit > MAX_RESULTS {
            return Err(InvalidSearchQuery(format!(
                "Search limit must be between 1 and {MAX_RESULTS}."
            ))
            .into());
        }

        if request.tags.len() > 32 {
            return Err(
                InvalidSearchQuery("Search supports at most 32 tag filters.".into()).into(),
            );
        }

        let parts = parse_query(&request.query)?;
        let searcher = self.reader.searcher();
        let mut budget = ExpansionBudget::default();
        let exact = self.build_query(&parts, request, false, &searcher, &mut budget)?;
        let (mut hits, exact_total) =
            self.collect_hits(&searcher, exact.as_ref(), request.limit)?;
        let has_fuzzy_terms = request.fuzzy
            && parts.iter().any(|part| {
                !part.is_phrase && part.terms.iter().any(|term| term.chars().count() >= 4)
            });
        let mut fuzzy_total = 0;

        if has_fuzzy_terms {
            let expanded = self.build_query(&parts, request, true, &searcher, &mut budget)?;
            let approximate =
                BooleanQuery::new(vec![(Occur::Must, expanded), (Occur::MustNot, exact)]);
            let remaining = request.limit - hits.len();

            if remaining == 0 {
                fuzzy_total = searcher.search(&approximate, &Count)? as u64;
            } else {
                let (approximate_hits, count) =
                    self.collect_hits(&searcher, &approximate, remaining)?;
                hits.extend(approximate_hits);
                fuzzy_total = count;
            }
        }

        let total = exact_total + fuzzy_total;
        let truncated = total > hits.len() as u64;

        Ok(SearchResponse {
            hits,
            total,
            applied_fuzzy: has_fuzzy_terms,
            truncated,
        })
    }

    fn build_query(
        &self,
        parts: &[QueryPart],
        request: &SearchRequest,
        is_fuzzy: bool,
        searcher: &tantivy::Searcher,
        budget: &mut ExpansionBudget,
    ) -> Result<Box<dyn Query>> {
        let mut clauses = Vec::with_capacity(parts.len() + request.tags.len() + 1);

        for (part_index, part) in parts.iter().enumerate() {
            let is_last_part = part_index + 1 == parts.len();

            if part.is_phrase {
                let phrase_queries = [self.fields.title, self.fields.body]
                    .into_iter()
                    .map(|field| {
                        let terms = part
                            .terms
                            .iter()
                            .map(|term| Term::from_field_text(field, term))
                            .collect::<Vec<_>>();
                        let query: Box<dyn Query> = if terms.len() == 1 {
                            Box::new(TermQuery::new(
                                terms[0].clone(),
                                IndexRecordOption::WithFreqs,
                            ))
                        } else {
                            Box::new(PhraseQuery::new(terms))
                        };

                        self.boost_field(query, field)
                    })
                    .collect();
                clauses.push((
                    Occur::Must,
                    Box::new(BooleanQuery::union(phrase_queries)) as Box<dyn Query>,
                ));
            } else {
                for (term_index, term) in part.terms.iter().enumerate() {
                    let is_prefix =
                        request.prefix && is_last_part && term_index + 1 == part.terms.len();
                    let distance = if is_fuzzy {
                        match term.chars().count() {
                            0..=3 => 0,
                            4..=7 => 1,
                            _ => 2,
                        }
                    } else {
                        0
                    };
                    let expanded = if is_prefix || distance > 0 {
                        Some(self.expand_terms(searcher, term, distance, is_prefix, budget)?)
                    } else {
                        None
                    };
                    let term_queries = [self.fields.title, self.fields.body]
                        .into_iter()
                        .map(|field| {
                            let query: Box<dyn Query> = if let Some(expanded) = &expanded {
                                Box::new(TermSetQuery::new(
                                    expanded
                                        .iter()
                                        .map(|candidate| Term::from_field_text(field, candidate)),
                                ))
                            } else {
                                Box::new(TermQuery::new(
                                    Term::from_field_text(field, term),
                                    IndexRecordOption::WithFreqs,
                                ))
                            };

                            self.boost_field(query, field)
                        })
                        .collect();
                    clauses.push((
                        Occur::Must,
                        Box::new(BooleanQuery::union(term_queries)) as Box<dyn Query>,
                    ));
                }
            }
        }

        if let Some(kind) = &request.kind {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.fields.kind, kind),
                    IndexRecordOption::Basic,
                )),
            ));
        }

        for tag in &request.tags {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.fields.tags, tag),
                    IndexRecordOption::Basic,
                )),
            ));
        }

        Ok(Box::new(BooleanQuery::new(clauses)))
    }

    fn expand_terms(
        &self,
        searcher: &tantivy::Searcher,
        term: &str,
        distance: u8,
        is_prefix: bool,
        budget: &mut ExpansionBudget,
    ) -> Result<Arc<Vec<String>>> {
        let cache_key = (term.to_owned(), distance, is_prefix);

        if let Some(cached) = budget.cached.get(&cache_key) {
            return Ok(Arc::clone(cached));
        }

        let query = if is_prefix {
            FuzzyTermQuery::new_prefix(
                Term::from_field_text(self.fields.body, term),
                distance,
                true,
            )
        } else {
            FuzzyTermQuery::new(
                Term::from_field_text(self.fields.body, term),
                distance,
                true,
            )
        };
        let automaton = query.automaton()?;
        let known = budget.candidates.entry(term.to_owned()).or_default();
        let mut expanded = BTreeSet::new();

        for segment in searcher.segment_readers() {
            for field in [self.fields.title, self.fields.body] {
                let inverted = segment.inverted_index(field)?;
                let mut stream = inverted.terms().search(&automaton).into_stream()?;

                while stream.advance() {
                    let candidate = std::str::from_utf8(stream.key())
                        .context("Indexed text term is not valid UTF-8")?;

                    if !known.contains(candidate) {
                        if known.len() >= MAX_EXPANSIONS_PER_TERM {
                            return Err(SearchBudgetExceeded(format!("Search expands one term beyond {MAX_EXPANSIONS_PER_TERM} vocabulary candidates. Use a longer term or disable fuzzy matching.")).into());
                        }

                        if budget.total >= MAX_EXPANSIONS_PER_QUERY {
                            return Err(SearchBudgetExceeded(format!("Search expands beyond {MAX_EXPANSIONS_PER_QUERY} vocabulary candidates across the query. Use more specific terms.")).into());
                        }

                        known.insert(candidate.to_owned());
                        budget.total += 1;
                    }

                    if !expanded.contains(candidate) {
                        expanded.insert(candidate.to_owned());
                    }
                }
            }
        }

        let expanded = Arc::new(expanded.into_iter().collect::<Vec<_>>());
        budget.cached.insert(cache_key, Arc::clone(&expanded));

        Ok(expanded)
    }

    fn boost_field(&self, query: Box<dyn Query>, field: Field) -> Box<dyn Query> {
        if field == self.fields.title {
            Box::new(BoostQuery::new(query, TITLE_BOOST))
        } else {
            query
        }
    }

    fn collect_hits(
        &self,
        searcher: &tantivy::Searcher,
        query: &dyn Query,
        limit: usize,
    ) -> Result<(Vec<SearchHit>, u64)> {
        let collector = TopDocs::with_limit(limit).tweak_score(|segment: &SegmentReader| {
            let identities = segment
                .fast_fields()
                .u64("id")
                .expect("Search schema always has an unsigned identity fast field")
                .first_or_default_col(0);

            move |document: DocId, score: f32| (score, Reverse(identities.get_val(document)))
        });
        let (documents, total) = searcher.search(query, &(collector, Count))?;
        let mut snippet_generator = SnippetGenerator::create(searcher, query, self.fields.body)?;
        snippet_generator.set_max_num_chars(EXCERPT_CHARACTERS);
        let mut hits = Vec::with_capacity(documents.len());

        for ((score, _), address) in documents {
            let document: TantivyDocument = searcher.doc(address)?;
            let key = document
                .get_first(self.fields.key)
                .and_then(|value| value.as_str())
                .context("Indexed memory has no public key")?
                .to_owned();
            let version = document
                .get_first(self.fields.version)
                .and_then(|value| value.as_i64())
                .context("Indexed memory has no version")?;
            let title = document
                .get_first(self.fields.title)
                .and_then(|value| value.as_str())
                .context("Indexed memory has no title")?
                .to_owned();
            let body = document
                .get_first(self.fields.body)
                .and_then(|value| value.as_str())
                .context("Indexed memory has no body")?;
            let snippet = snippet_generator.snippet(body);
            let excerpt = if snippet.fragment().is_empty() {
                body.chars().take(EXCERPT_CHARACTERS).collect()
            } else {
                snippet.fragment().to_owned()
            };
            hits.push(SearchHit {
                key,
                title,
                version,
                score,
                excerpt,
            });
        }

        Ok((hits, total as u64))
    }
}

fn build_analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build()
}

fn build_schema() -> (Schema, SearchFields) {
    let mut builder = Schema::builder();
    let text_options = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(TOKENIZER_NAME)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        )
        .set_stored();
    let fields = SearchFields {
        id: builder.add_u64_field("id", FAST),
        key: builder.add_text_field("key", STRING | STORED),
        version: builder.add_i64_field("version", STORED),
        title: builder.add_text_field("title", text_options.clone()),
        body: builder.add_text_field("body", text_options),
        kind: builder.add_text_field("kind", STRING),
        tags: builder.add_text_field("tags", STRING),
    };

    (builder.build(), fields)
}

fn parse_query(query: &str) -> Result<Vec<QueryPart>> {
    if query.len() > MAX_QUERY_BYTES {
        return Err(InvalidSearchQuery(format!(
            "Search query exceeds {MAX_QUERY_BYTES} UTF-8 bytes."
        ))
        .into());
    }

    let mut analyzer = build_analyzer();
    let mut parts = Vec::new();
    let mut is_phrase = false;
    let mut segment = String::new();
    let mut term_count = 0;

    for character in query.chars().chain(std::iter::once('"')) {
        if character != '"' {
            segment.push(character);
            continue;
        }

        let mut stream = analyzer.token_stream(&segment);
        let mut terms = Vec::new();

        while stream.advance() {
            let term = &stream.token().text;

            if term.chars().count() > MAX_TERM_CHARACTERS {
                return Err(InvalidSearchQuery(format!(
                    "Search terms exceed {MAX_TERM_CHARACTERS} characters."
                ))
                .into());
            }

            term_count += 1;

            if term_count > MAX_QUERY_TERMS {
                return Err(InvalidSearchQuery(format!(
                    "Search supports at most {MAX_QUERY_TERMS} terms."
                ))
                .into());
            }

            terms.push(term.clone());
        }

        if !terms.is_empty() {
            parts.push(QueryPart { terms, is_phrase });
        }

        drop(stream);
        segment.clear();
        is_phrase = !is_phrase;
    }

    if !is_phrase {
        return Err(
            InvalidSearchQuery("Search query has an unclosed quoted phrase.".into()).into(),
        );
    }

    if parts.is_empty() {
        return Err(InvalidSearchQuery(
            "Search query must contain at least one searchable word.".into(),
        )
        .into());
    }

    Ok(parts)
}
