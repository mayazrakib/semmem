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
    pub commit_id: Option<i64>,
    #[serde(default)]
    pub affected: Vec<MemoryReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryReceipt {
    pub key: String,
    pub token: Option<String>,
}

impl AppError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            committed: false,
            searchable: false,
            commit_id: None,
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
pub struct MemoryReference {
    pub target_key: String,
    pub reference_type: String,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, clap::ValueEnum,
)]
#[allow(non_camel_case_types)]
pub enum Basis {
    #[serde(rename = "stated")]
    #[value(name = "stated")]
    STATED,
    #[serde(rename = "observed")]
    #[value(name = "observed")]
    OBSERVED,
    #[serde(rename = "inferred")]
    #[value(name = "inferred")]
    INFERRED,
    #[serde(rename = "assumed")]
    #[value(name = "assumed")]
    ASSUMED,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryMetadata {
    pub summary: Option<String>,
    pub basis: Option<Basis>,
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
    pub basis: Option<Option<Basis>>,
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
            basis: self.basis.unwrap_or(current.basis),
            scope: self.scope.clone().unwrap_or_else(|| current.scope.clone()),
            observed_at: self.observed_at.unwrap_or(current.observed_at),
            verified_at: self.verified_at.unwrap_or(current.verified_at),
            review_after: self.review_after.unwrap_or(current.review_after),
        }
    }
}

impl From<MemoryMetadata> for MetadataPatch {
    fn from(metadata: MemoryMetadata) -> Self {
        Self {
            summary: Some(metadata.summary),
            basis: Some(metadata.basis),
            scope: Some(metadata.scope),
            observed_at: Some(metadata.observed_at),
            verified_at: Some(metadata.verified_at),
            review_after: Some(metadata.review_after),
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
    pub r#type: String,
    pub tags: Vec<String>,
    pub references: Vec<MemoryReference>,
    pub system_token: String,
    pub system_created_at: i64,
    pub system_updated_at: i64,
    pub system_created_by: String,
    pub system_updated_by: String,
}

fn default_type() -> String {
    "general".into()
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
    pub metadata: MetadataPatch,
    #[serde(default = "default_type")]
    pub r#type: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub references: Vec<MemoryReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateMemory {
    pub key: String,
    pub expected_token: String,
    pub title: Option<String>,
    pub body: Option<String>,
    #[serde(flatten)]
    pub metadata: MetadataPatch,
    pub r#type: Option<String>,
    pub tags: Option<Vec<String>>,
    pub references: Option<Vec<MemoryReference>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryToken {
    pub key: String,
    pub expected_token: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetMemories {
    #[serde(default)]
    pub view: MemoryView,
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListMemories {
    pub scope: Option<String>,
    pub basis: Option<Basis>,
    pub r#type: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub referenced_to: Option<String>,
    pub referenced_from: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub cursor: Option<String>,
}

impl Default for ListMemories {
    fn default() -> Self {
        Self {
            scope: None,
            basis: None,
            r#type: None,
            tags: vec![],
            referenced_to: None,
            referenced_from: None,
            limit: 20,
            cursor: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub scope: Option<String>,
    pub basis: Option<Basis>,
    pub query: String,
    pub r#type: Option<String>,
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
    pub body: String,
    pub system_token: String,
    pub score: f32,
    #[serde(flatten)]
    pub metadata: MemoryMetadata,
    pub r#type: String,
    pub tags: Vec<String>,
    pub references: Vec<MemoryReference>,
    pub system_created_at: i64,
    pub system_updated_at: i64,
    pub system_created_by: String,
    pub system_updated_by: String,
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
    pub system_created_by: String,
    pub system_updated_by: String,
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
pub struct CommitRecord {
    pub id: i64,
    pub key: String,
    pub summary: String,
    pub operation: String,
    pub actor: String,
    pub changes: serde_json::Value,
    pub system_created_at: i64,
    pub system_updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListCommits {
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetCommits {
    pub ids: Vec<i64>,
}

#[cfg(test)]
mod tests {
    use super::{GetMemories, MemoryMetadata, UpdateMemory};
    use serde_json::json;

    #[test]
    fn metadata_updates_distinguish_omission_null_and_values() {
        let created: MemoryMetadata = serde_json::from_value(json!({
            "summary": "Saved summary", "scope": "project"
        }))
        .unwrap();
        let update: UpdateMemory = serde_json::from_value(json!({
            "key": "example", "expected_token": "token", "summary": null, "verified_at": 123
        }))
        .unwrap();
        let metadata = update.metadata.apply(&created);
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
        assert_eq!(request.keys, vec!["example"]);
        assert!(
            serde_json::from_value::<super::CreateMemory>(json!({
                "key": "example", "body": "Exact Markdown", "basis": "certain"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<super::CreateMemory>(json!({
                "key": "example", "body": "Exact Markdown", "verified_at": 1.5
            }))
            .is_err()
        );
    }
}
