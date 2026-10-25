use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tantivy::collector::{Count, TopDocs};
use tantivy::indexer::LogMergePolicy;
use tantivy::query::{
    BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, PhraseQuery, Query, RangeQuery, TermQuery,
    TermSetQuery,
};
use tantivy::schema::{
    FAST, Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::tokenizer::{
    AsciiFoldingFilter, LowerCaser, SimpleTokenizer, TextAnalyzer, TokenStream,
};
use tantivy::{
    DocId, Index, IndexReader, IndexWriter, ReloadPolicy, SegmentReader, TantivyDocument, Term,
};

use crate::models::{MemoryRecord, SearchHit, SearchRequest, SearchResponse};

const INDEX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const MIN_MERGE_SEGMENTS: usize = 4;
const MAX_MERGE_INPUT_DOCUMENTS: usize = 50_000;
const MERGE_DELETED_DOCUMENT_RATIO: f32 = 0.2;
const TOKENIZER_NAME: &str = "memory_text";
const TITLE_BOOST: f32 = 4.0;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct InvalidSearchQuery(pub String);

#[derive(Default)]
struct ExpansionCache {
    cached: BTreeMap<(String, u8, bool), Arc<Vec<String>>>,
}

#[derive(Clone)]
struct SearchFields {
    id: Field,
    key: Field,
    version: Field,
    title: Field,
    summary: Field,
    scope: Field,
    basis: Field,
    metadata: Field,
    body: Field,
    memory_type: Field,
    is_current: Field,
    is_deleted: Field,
    valid_from: Field,
    valid_until: Field,
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
            .unwrap_or(1);
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
        self.replace_revisions(&memory.key, std::slice::from_ref(memory))
    }

    pub fn replace_revisions(&mut self, key: &str, revisions: &[MemoryRecord]) -> Result<()> {
        let mut ordered = revisions.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|memory| memory.system_version);

        for memory in &ordered {
            if memory.id <= 0 || memory.system_version <= 0 || memory.key != key {
                bail!("Indexed revision identity, key, and version must be valid");
            }
        }

        if ordered
            .windows(2)
            .any(|pair| pair[0].system_version == pair[1].system_version)
        {
            bail!("Indexed revision versions must be unique");
        }

        self.remove(key)?;

        for (position, memory) in ordered.iter().enumerate() {
            let fields = &self.reader.fields;
            let mut document = TantivyDocument::default();
            document.add_u64(fields.id, memory.id as u64);
            document.add_text(fields.key, &memory.key);
            document.add_i64(fields.version, memory.system_version);
            document.add_text(fields.title, &memory.title);
            document.add_text(fields.metadata, serde_json::to_string(memory)?);
            document.add_u64(fields.is_current, u64::from(position + 1 == ordered.len()));
            document.add_u64(
                fields.is_deleted,
                u64::from(memory.system_deleted_at.is_some()),
            );
            document.add_i64(fields.valid_from, memory.system_updated_at);

            if let Some(next) = ordered.get(position + 1) {
                document.add_i64(fields.valid_until, next.system_updated_at);
            }

            if let Some(summary) = &memory.metadata.summary {
                document.add_text(fields.summary, summary);
            }

            if let Some(scope) = &memory.metadata.scope {
                document.add_text(fields.scope, scope);
            }

            if let Some(basis) = memory.metadata.basis {
                document.add_text(
                    fields.basis,
                    serde_json::to_value(basis)?
                        .as_str()
                        .context("Basis must be text")?,
                );
            }

            document.add_text(fields.body, &memory.body);
            document.add_text(fields.memory_type, &memory.r#type);

            for tag in &memory.tags {
                document.add_text(fields.tags, tag);
            }

            self.writer.add_document(document)?;
        }

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
        if std::env::var("SEMMEM_FAILPOINT").as_deref() == Ok("after_index_commit") {
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
        if request.limit == 0 {
            return Err(InvalidSearchQuery("Search limit must be positive.".into()).into());
        }

        let parts = parse_query(&request.query)?;
        let searcher = self.reader.searcher();
        let mut cache = ExpansionCache::default();
        if request.history && request.as_of.is_some() {
            return Err(InvalidSearchQuery("History and as_of cannot be combined.".into()).into());
        }

        let has_fuzzy_terms = request.fuzzy
            && parts.iter().any(|part| {
                !part.is_phrase && part.terms.iter().any(|term| term.chars().count() >= 4)
            });
        let query = self.build_query(&parts, request, has_fuzzy_terms, &searcher, &mut cache)?;
        let (hits, total) = self.collect_hits(&searcher, query.as_ref(), request.limit)?;
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
        cache: &mut ExpansionCache,
    ) -> Result<Box<dyn Query>> {
        let mut clauses = Vec::with_capacity(parts.len() + request.tags.len() + 1);

        for (part_index, part) in parts.iter().enumerate() {
            let is_last_part = part_index + 1 == parts.len();

            if part.is_phrase {
                let phrase_queries = [self.fields.title, self.fields.summary, self.fields.body]
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
                        Some(self.expand_terms(searcher, term, distance, is_prefix, cache)?)
                    } else {
                        None
                    };
                    let term_queries = [self.fields.title, self.fields.summary, self.fields.body]
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

        if let Some(memory_type) = &request.r#type {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.fields.memory_type, memory_type),
                    IndexRecordOption::Basic,
                )),
            ));
        }

        if let Some(scope) = &request.scope {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.fields.scope, scope),
                    IndexRecordOption::Basic,
                )),
            ));
        }

        if let Some(basis) = request.basis {
            let value = serde_json::to_value(basis)?;
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(
                        self.fields.basis,
                        value.as_str().context("Basis must be text")?,
                    ),
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

        if !request.include_deleted {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_u64(self.fields.is_deleted, 0),
                    IndexRecordOption::Basic,
                )),
            ));
        }

        if let Some(timestamp) = request.as_of {
            clauses.push((
                Occur::Must,
                Box::new(RangeQuery::new(
                    Bound::Unbounded,
                    Bound::Included(Term::from_field_i64(self.fields.valid_from, timestamp)),
                )),
            ));
            clauses.push((
                Occur::MustNot,
                Box::new(RangeQuery::new(
                    Bound::Unbounded,
                    Bound::Included(Term::from_field_i64(self.fields.valid_until, timestamp)),
                )),
            ));
        } else if !request.history {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_u64(self.fields.is_current, 1),
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
        cache: &mut ExpansionCache,
    ) -> Result<Arc<Vec<String>>> {
        let cache_key = (term.to_owned(), distance, is_prefix);

        if let Some(cached) = cache.cached.get(&cache_key) {
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
        let mut expanded = BTreeSet::new();

        for segment in searcher.segment_readers() {
            for field in [self.fields.title, self.fields.summary, self.fields.body] {
                let inverted = segment.inverted_index(field)?;
                let mut stream = inverted.terms().search(&automaton).into_stream()?;

                while stream.advance() {
                    let candidate = std::str::from_utf8(stream.key())
                        .context("Indexed text term is not valid UTF-8")?;

                    if !expanded.contains(candidate) {
                        expanded.insert(candidate.to_owned());
                    }
                }
            }
        }

        let expanded = Arc::new(expanded.into_iter().collect::<Vec<_>>());
        cache.cached.insert(cache_key, Arc::clone(&expanded));

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
            let keys = segment
                .fast_fields()
                .str("key")
                .expect("Search schema always has a key fast field")
                .expect("Search schema always has a key string column");
            let versions = segment
                .fast_fields()
                .i64("version")
                .expect("Search schema always has a version fast field")
                .first_or_default_col(0);

            move |document: DocId, score: f32| {
                let mut key = String::new();
                let ordinal = keys
                    .term_ords(document)
                    .next()
                    .expect("Indexed revision always has a key");
                keys.ord_to_str(ordinal, &mut key)
                    .expect("Indexed revision key must be readable");

                (score, Reverse(key), versions.get_val(document))
            }
        });
        let (documents, total) = searcher.search(query, &(collector, Count))?;
        let mut hits = Vec::with_capacity(documents.len());

        for ((score, _, _), address) in documents {
            let document: TantivyDocument = searcher.doc(address)?;
            let memory: MemoryRecord = serde_json::from_str(
                document
                    .get_first(self.fields.metadata)
                    .and_then(|value| value.as_str())
                    .context("Indexed revision has no stored record")?,
            )?;
            hits.push(SearchHit {
                key: memory.key,
                title: memory.title,
                body: memory.body,
                version: memory.system_version,
                score,
                metadata: memory.metadata,
                r#type: memory.r#type,
                author: memory.author,
                tags: memory.tags,
                references: memory.references,
                system_created_at: memory.system_created_at,
                system_updated_at: memory.system_updated_at,
                system_deleted_at: memory.system_deleted_at,
                system_created_by: memory.system_created_by,
                system_updated_by: memory.system_updated_by,
                system_deleted_by: memory.system_deleted_by,
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
        key: builder.add_text_field("key", STRING | STORED | FAST),
        version: builder.add_i64_field("version", STORED | FAST),
        title: builder.add_text_field("title", text_options.clone()),
        summary: builder.add_text_field("summary", text_options.clone()),
        scope: builder.add_text_field("scope", STRING),
        basis: builder.add_text_field("basis", STRING),
        metadata: builder.add_text_field("metadata", STORED),
        body: builder.add_text_field("body", text_options),
        memory_type: builder.add_text_field("type", STRING),
        is_current: builder.add_u64_field("is_current", FAST | tantivy::schema::INDEXED),
        is_deleted: builder.add_u64_field("is_deleted", FAST | tantivy::schema::INDEXED),
        valid_from: builder.add_i64_field("valid_from", FAST),
        valid_until: builder.add_i64_field("valid_until", FAST),
        tags: builder.add_text_field("tags", STRING),
    };

    (builder.build(), fields)
}

fn parse_query(query: &str) -> Result<Vec<QueryPart>> {
    let mut analyzer = build_analyzer();
    let mut parts = Vec::new();
    let mut is_phrase = false;
    let mut segment = String::new();

    for character in query.chars().chain(std::iter::once('"')) {
        if character != '"' {
            segment.push(character);
            continue;
        }

        let mut stream = analyzer.token_stream(&segment);
        let mut terms = Vec::new();

        while stream.advance() {
            let term = &stream.token().text;

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
