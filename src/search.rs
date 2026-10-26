use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{
    BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, PhraseQuery, Query, TermQuery, TermSetQuery,
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
    title: Field,
    summary: Field,
    scope: Field,
    basis: Field,
    metadata: Field,
    body: Field,
    memory_type: Field,
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
        let available_threads = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        let memory_bytes = std::env::var("SEMMEM_INDEX_MEMORY_BYTES")
            .ok()
            .map(|value| value.parse::<usize>())
            .transpose()
            .context("SEMMEM_INDEX_MEMORY_BYTES must be a positive integer")?
            .unwrap_or(64 * 1024 * 1024);
        let threads = std::env::var("SEMMEM_INDEX_THREADS")
            .ok()
            .map(|value| value.parse::<usize>())
            .transpose()
            .context("SEMMEM_INDEX_THREADS must be a positive integer")?
            .unwrap_or_else(|| available_threads.min((memory_bytes / 15_000_000).max(1)));

        if threads == 0 || memory_bytes == 0 {
            bail!("Search indexing threads and memory budget must be positive");
        }

        let writer = index.writer_with_num_threads(threads, memory_bytes)?;
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
        if memory.id <= 0 || memory.system_token.is_empty() {
            bail!("Indexed memory identity and concurrency token must be valid");
        }

        self.remove(&memory.key)?;
        let fields = &self.reader.fields;
        let mut document = TantivyDocument::default();
        document.add_u64(fields.id, memory.id as u64);
        document.add_text(fields.key, &memory.key);
        document.add_text(fields.title, &memory.title);
        document.add_text(fields.metadata, serde_json::to_string(memory)?);

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
            move |document: DocId, score: f32| {
                let mut key = String::new();
                let ordinal = keys
                    .term_ords(document)
                    .next()
                    .expect("Indexed memory always has a key");
                keys.ord_to_str(ordinal, &mut key)
                    .expect("Indexed memory key must be readable");

                (score, Reverse(key))
            }
        });
        let (documents, total) = searcher.search(query, &(collector, Count))?;
        let mut hits = Vec::with_capacity(documents.len());

        for ((score, _), address) in documents {
            let document: TantivyDocument = searcher.doc(address)?;
            let memory: MemoryRecord = serde_json::from_str(
                document
                    .get_first(self.fields.metadata)
                    .and_then(|value| value.as_str())
                    .context("Indexed memory has no stored record")?,
            )?;
            hits.push(SearchHit {
                key: memory.key,
                title: memory.title,
                body: memory.body,
                system_token: memory.system_token,
                score,
                metadata: memory.metadata,
                r#type: memory.r#type,
                tags: memory.tags,
                references: memory.references,
                system_created_at: memory.system_created_at,
                system_updated_at: memory.system_updated_at,
                system_created_by: memory.system_created_by,
                system_updated_by: memory.system_updated_by,
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
        title: builder.add_text_field("title", text_options.clone()),
        summary: builder.add_text_field("summary", text_options.clone()),
        scope: builder.add_text_field("scope", STRING),
        basis: builder.add_text_field("basis", STRING),
        metadata: builder.add_text_field("metadata", STORED),
        body: builder.add_text_field("body", text_options),
        memory_type: builder.add_text_field("type", STRING),
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
