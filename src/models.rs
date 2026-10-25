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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct MemoryRecord {
    pub id: i64,
    pub key: String,
    pub title: String,
    pub body: String,
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
    pub keys: Vec<String>,
    #[serde(default)]
    pub include_deleted: bool,
    pub version: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListMemories {
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
    pub excerpt: String,
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
