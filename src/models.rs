use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct AppError {
    pub code: String,
    pub message: String,
    pub committed: bool,
    pub searchable: bool,
    #[serde(default)]
    pub affected: Vec<MemoryReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryReceipt {
    pub key: String,
    pub version: Option<i64>,
}

impl AppError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            committed: false,
            searchable: false,
            affected: Vec::new(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_input", message)
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(error: rusqlite::Error) -> Self {
        let code = match &error {
            rusqlite::Error::SqliteFailure(failure, _)
                if failure.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
            {
                "already_exists"
            }
            rusqlite::Error::SqliteFailure(failure, _)
                if matches!(
                    failure.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) =>
            {
                "database_busy"
            }
            rusqlite::Error::SqliteFailure(failure, _)
                if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                "invalid_input"
            }
            _ => "storage_failure",
        };

        Self::new(code, error.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(error: serde_json::Error) -> Self {
        Self::invalid(error.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryLink {
    pub target_key: String,
    pub relation: String,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, clap::ValueEnum,
)]
#[allow(non_camel_case_types)]
pub enum ClaimType {
    #[serde(rename = "explicit user statement")]
    #[value(name = "explicit user statement")]
    EXPLICIT_USER_STATEMENT,
    #[serde(rename = "verified observation")]
    #[value(name = "verified observation")]
    VERIFIED_OBSERVATION,
    #[serde(rename = "inference")]
    #[value(name = "inference")]
    INFERENCE,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryMetadata {
    pub summary: Option<String>,
    pub claim_type: Option<ClaimType>,
    pub scope: Option<String>,
    pub observed_at: Option<i64>,
    pub verified_at: Option<i64>,
    pub review_after: Option<i64>,
}

fn deserialize_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetadataPatch {
    #[serde(
        default,
        deserialize_with = "deserialize_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub summary: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "deserialize_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub claim_type: Option<Option<ClaimType>>,
    #[serde(
        default,
        deserialize_with = "deserialize_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub scope: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "deserialize_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub observed_at: Option<Option<i64>>,
    #[serde(
        default,
        deserialize_with = "deserialize_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub verified_at: Option<Option<i64>>,
    #[serde(
        default,
        deserialize_with = "deserialize_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub review_after: Option<Option<i64>>,
}

impl MetadataPatch {
    pub fn apply(&self, current: &MemoryMetadata) -> MemoryMetadata {
        MemoryMetadata {
            summary: self
                .summary
                .clone()
                .unwrap_or_else(|| current.summary.clone()),
            claim_type: self.claim_type.unwrap_or(current.claim_type),
            scope: self.scope.clone().unwrap_or_else(|| current.scope.clone()),
            observed_at: self.observed_at.unwrap_or(current.observed_at),
            verified_at: self.verified_at.unwrap_or(current.verified_at),
            review_after: self.review_after.unwrap_or(current.review_after),
        }
    }
}

#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
#[allow(clippy::upper_case_acronyms)]
pub enum MemoryView {
    #[default]
    FULL,
    SUMMARY,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct MemoryRecord {
    pub id: i64,
    pub key: String,
    pub title: String,
    pub body: String,
    #[serde(flatten)]
    pub metadata: MemoryMetadata,
    pub kind: String,
    pub tags: Vec<String>,
    pub links: Vec<MemoryLink>,
    pub actor: String,
    pub system_version: i64,
    pub system_created_at: i64,
    pub system_updated_at: i64,
    pub system_deleted_at: Option<i64>,
}

fn default_kind() -> String {
    "note".into()
}
fn default_actor() -> String {
    "human".into()
}
fn default_limit() -> usize {
    20
}
fn default_fuzzy() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateMemory {
    pub key: String,
    #[serde(default)]
    pub title: String,
    pub body: String,
    #[serde(flatten)]
    pub metadata: MemoryMetadata,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub links: Vec<MemoryLink>,
    #[serde(default = "default_actor")]
    pub actor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateMemory {
    pub key: String,
    pub expected_version: i64,
    pub title: Option<String>,
    pub body: Option<String>,
    #[serde(flatten)]
    pub metadata: MetadataPatch,
    pub kind: Option<String>,
    pub tags: Option<Vec<String>>,
    pub links: Option<Vec<MemoryLink>>,
    #[serde(default = "default_actor")]
    pub actor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryVersion {
    pub key: String,
    pub expected_version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreMemory {
    pub key: String,
    pub expected_version: i64,
    pub revision: Option<i64>,
    #[serde(default = "default_actor")]
    pub actor: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetMemories {
    #[serde(default)]
    pub view: MemoryView,
    pub keys: Vec<String>,
    #[serde(default)]
    pub include_deleted: bool,
    pub version: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListMemories {
    pub scope: Option<String>,
    pub claim_type: Option<ClaimType>,
    pub kind: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub linked_to: Option<String>,
    pub linked_from: Option<String>,
    #[serde(default)]
    pub include_deleted: bool,
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub cursor: Option<String>,
}

impl Default for ListMemories {
    fn default() -> Self {
        Self {
            scope: None,
            claim_type: None,
            kind: None,
            tags: vec![],
            linked_to: None,
            linked_from: None,
            include_deleted: false,
            limit: 20,
            cursor: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub scope: Option<String>,
    pub claim_type: Option<ClaimType>,
    pub query: String,
    pub kind: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub prefix: bool,
    #[serde(default = "default_fuzzy")]
    pub fuzzy: bool,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchHit {
    pub key: String,
    pub title: String,
    pub version: i64,
    pub score: f32,
    #[serde(flatten)]
    pub metadata: MemoryMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    pub total: u64,
    pub applied_fuzzy: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaxonomyRecord {
    pub id: i64,
    pub key: String,
    pub name: String,
    pub system_created_at: i64,
    pub system_updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaxonomyInput {
    pub key: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEvent {
    pub sequence: i64,
    pub memory_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryRequest {
    pub key: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub before_version: Option<i64>,
}

pub fn markdown_paragraphs(body: &str) -> impl Iterator<Item = &str> {
    let mut lines = body.split_inclusive('\n');
    let mut position = 0;

    std::iter::from_fn(move || {
        let mut start = None;
        let mut end = position;

        for line in lines.by_ref() {
            let line_start = position;
            position += line.len();

            if line.trim().is_empty() {
                if let Some(start) = start {
                    return Some(&body[start..end]);
                }

                continue;
            }

            start.get_or_insert(line_start);
            end = line_start + line.trim_end_matches(['\r', '\n']).len();
        }

        start.map(|start| &body[start..end])
    })
}

#[cfg(test)]
mod tests {
    use super::{CreateMemory, GetMemories, MemoryView, UpdateMemory};
    use serde_json::json;

    #[test]
    fn metadata_updates_distinguish_omission_null_and_values() {
        let created: CreateMemory = serde_json::from_value(json!({
            "key": "example", "body": "Exact Markdown", "summary": "Saved summary", "scope": "project"
        })).unwrap();
        let update: UpdateMemory = serde_json::from_value(json!({
            "key": "example", "expected_version": 1, "summary": null, "verified_at": 123
        }))
        .unwrap();
        let metadata = update.metadata.apply(&created.metadata);
        assert_eq!(metadata.summary, None);
        assert_eq!(metadata.scope.as_deref(), Some("project"));
        assert_eq!(metadata.verified_at, Some(123));
        let serialized = serde_json::to_value(update).unwrap();
        assert!(serialized["summary"].is_null());
        assert!(serialized.get("scope").is_none());
    }

    #[test]
    fn reads_default_to_full_and_metadata_rejects_invalid_types() {
        let request: GetMemories = serde_json::from_value(json!({"keys": ["example"]})).unwrap();
        assert_eq!(request.view, MemoryView::FULL);
        assert!(
            serde_json::from_value::<CreateMemory>(json!({
                "key": "example", "body": "Exact Markdown", "claim_type": "certain"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<CreateMemory>(json!({
                "key": "example", "body": "Exact Markdown", "verified_at": 1.5
            }))
            .is_err()
        );
    }
}
