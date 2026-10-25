use std::env;
use std::fs;
use std::io::{IsTerminal, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use comfy_table::{ContentArrangement, Table};
use serde_json::{Value, json};

use crate::models::{
    CreateMemory, GetMemories, HistoryRequest, ListMemories, MemoryLink, MemoryRecord,
    MemoryVersion, RestoreMemory, SearchRequest, UpdateMemory,
};

#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    #[arg(long, global = true, env = "SEMANTIC_MEMORY_SOCKET", default_value_os_t = default_socket_path())]
    pub socket: PathBuf,
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
pub enum Command {
    #[command(
        name = "serve",
        about = "Run the local memory service and authenticated MCP endpoint"
    )]
    SERVE {
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8765")]
        listen: SocketAddr,
        #[arg(long)]
        issuer: Option<String>,
        #[arg(long)]
        audience: Option<String>,
        #[arg(long)]
        public_url: Option<String>,
    },
    #[command(
        name = "search",
        about = "Search memory text, tolerating spelling mistakes by default"
    )]
    SEARCH {
        query: String,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        exact: bool,
        #[arg(long)]
        prefix: bool,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    #[command(name = "list", about = "Browse memories by kind, tag, or relationship")]
    LIST(BrowseArguments),
    #[command(name = "show", about = "Read complete memories or historical versions")]
    SHOW {
        keys: Vec<String>,
        #[arg(long)]
        include_deleted: bool,
        #[arg(long)]
        version: Option<i64>,
    },
    #[command(
        name = "add",
        about = "Write a new Markdown memory in VISUAL, EDITOR, or Vim"
    )]
    ADD {
        key: String,
        #[arg(long, default_value = "")]
        title: String,
        #[arg(long, default_value = "note")]
        kind: String,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long = "link", value_parser = parse_link)]
        links: Vec<MemoryLink>,
    },
    #[command(
        name = "edit",
        about = "Edit Markdown and update explicit fields without overwriting concurrent changes"
    )]
    EDIT {
        key: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long = "tag", conflicts_with = "clear_tags")]
        tags: Option<Vec<String>>,
        #[arg(long)]
        clear_tags: bool,
        #[arg(long = "link", value_parser = parse_link, conflicts_with = "clear_links")]
        links: Option<Vec<MemoryLink>>,
        #[arg(long)]
        clear_links: bool,
    },
    #[command(
        name = "delete",
        about = "Move a memory to the recoverable deleted state"
    )]
    DELETE(VersionArguments),
    #[command(
        name = "purge",
        about = "Permanently remove a memory and its history; requires --confirm"
    )]
    PURGE {
        #[command(flatten)]
        version: VersionArguments,
        #[arg(long, required = true)]
        confirm: bool,
    },
    #[command(
        name = "restore",
        about = "Restore a deleted memory or an earlier revision"
    )]
    RESTORE {
        #[command(flatten)]
        version: VersionArguments,
        #[arg(long)]
        revision: Option<i64>,
    },
    #[command(name = "history", about = "Inspect a memory's revision history")]
    HISTORY {
        key: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        before_version: Option<i64>,
    },
    #[command(
        name = "taxonomy",
        about = "List, create, rename, or delete kinds, tags, and relationship types"
    )]
    TAXONOMY {
        #[arg(value_enum)]
        category: TaxonomyCategory,
        #[command(subcommand)]
        action: TaxonomyAction,
    },
}

#[derive(Debug, Args)]
pub struct BrowseArguments {
    #[arg(long)]
    kind: Option<String>,
    #[arg(long = "tag")]
    tags: Vec<String>,
    #[arg(long)]
    linked_to: Option<String>,
    #[arg(long)]
    linked_from: Option<String>,
    #[arg(long)]
    include_deleted: bool,
    #[arg(long, default_value_t = 20)]
    limit: usize,
    #[arg(long)]
    cursor: Option<String>,
}

#[derive(Debug, Args)]
pub struct VersionArguments {
    key: String,
    #[arg(long)]
    expected_version: i64,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
pub enum TaxonomyCategory {
    #[value(name = "kinds")]
    KINDS,
    #[value(name = "tags")]
    TAGS,
    #[value(name = "relations")]
    RELATIONS,
}

#[derive(Debug, Subcommand)]
#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
pub enum TaxonomyAction {
    #[command(name = "list")]
    LIST {
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        cursor: Option<String>,
    },
    #[command(name = "create")]
    CREATE { key: String, name: String },
    #[command(name = "update")]
    UPDATE { key: String, name: String },
    #[command(name = "delete")]
    DELETE { key: String },
}

pub fn default_data_dir() -> PathBuf {
    let root = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".local/share")
        });

    root.join("semantic-memory")
}

pub fn default_socket_path() -> PathBuf {
    default_data_dir().join("service.sock")
}

fn parse_link(value: &str) -> Result<MemoryLink, String> {
    let (relation, target_key) = value
        .split_once('=')
        .ok_or_else(|| "Use --link relation=target_key.".to_string())?;

    if relation.is_empty() || target_key.is_empty() {
        return Err("Both relationship type and target key must be nonempty.".into());
    }

    Ok(MemoryLink {
        target_key: target_key.into(),
        relation: relation.into(),
    })
}

pub struct LocalClient {
    client: reqwest::Client,
}

impl LocalClient {
    pub fn connect(socket: &Path) -> Result<Self> {
        let client = reqwest::Client::builder()
            .unix_socket(socket)
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .build()?;

        Ok(Self { client })
    }

    pub async fn request(&self, operation: &str, arguments: Value) -> Result<Value> {
        let response = self.client
            .post(format!("http://localhost/api/{operation}"))
            .json(&arguments)
            .send()
            .await
            .context("Could not connect to the memory service. Start semantic-memory serve or select --socket.")?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .context("The memory service returned an invalid response.")?;

        if !status.is_success() {
            let error = value.get("error").unwrap_or(&value);
            let code = error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("request_failed");
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("The request failed.");

            if error.get("committed").and_then(Value::as_bool) == Some(true) {
                let searchable = error
                    .get("searchable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                bail!(
                    "{code}: {message} The write was committed; searchable={searchable}. Read the current record before retrying."
                );
            }

            bail!("{code}: {message}");
        }

        Ok(value)
    }
}

pub async fn execute(cli: Cli) -> Result<()> {
    let client = LocalClient::connect(&cli.socket)?;
    let output = match cli.command {
        Command::SERVE { .. } => bail!("The serve command must be dispatched by the application entry point."),
        Command::SEARCH { query, kind, tags, exact, prefix, limit } => {
            let request = SearchRequest { query, kind, tags, prefix, fuzzy: !exact, limit };
            client.request("search_memories", serde_json::to_value(request)?).await?
        },
        Command::LIST(arguments) => {
            let request = ListMemories {
                kind: arguments.kind,
                tags: arguments.tags,
                linked_to: arguments.linked_to,
                linked_from: arguments.linked_from,
                include_deleted: arguments.include_deleted,
                limit: arguments.limit,
                cursor: arguments.cursor,
            };
            client.request("list_memories", serde_json::to_value(request)?).await?
        },
        Command::SHOW { keys, include_deleted, version } => {
            client.request("get_memories", serde_json::to_value(GetMemories { keys, include_deleted, version })?).await?
        },
        Command::ADD { key, title, kind, tags, links } => {
            let draft = Draft::open("")?;
            let body = draft.read()?;

            if body.is_empty() {
                eprintln!("Empty draft; no memory created.");
                draft.discard()?;

                return Ok(());
            }

            let memory = CreateMemory { key, title, body, kind, tags, links, actor: "human".into() };
            let result = client.request("create_memories", json!({"memories": [memory]})).await;
            finish_draft(draft, result)?
        },
        Command::EDIT { key, title, kind, tags, clear_tags, links, clear_links } => {
            let value = client.request("get_memories", json!({"keys": [&key], "include_deleted": false})).await?;
            let memory = decode_memory(&value)?;
            let draft = Draft::open(&memory.body)?;
            let body = draft.read()?;
            let tags = if clear_tags { Some(Vec::new()) } else { tags };
            let links = if clear_links { Some(Vec::new()) } else { links };
            let has_changes = body != memory.body || title.as_ref().is_some_and(|value| value != &memory.title)
                || kind.as_ref().is_some_and(|value| value != &memory.kind)
                || tags.as_ref().is_some_and(|value| value != &memory.tags)
                || links.as_ref().is_some_and(|value| value != &memory.links);

            if !has_changes {
                eprintln!("No changes; no revision created.");
                draft.discard()?;

                return Ok(());
            }

            let request = UpdateMemory { key, expected_version: memory.system_version, title, body: Some(body), kind, tags, links, actor: "human".into() };
            let result = client.request("update_memory", serde_json::to_value(request)?).await;
            finish_draft(draft, result)?
        },
        Command::DELETE(arguments) => {
            client.request("delete_memories", json!({"memories": [MemoryVersion {key: arguments.key, expected_version: arguments.expected_version}]})).await?
        },
        Command::PURGE { version, .. } => {
            client.request("purge_memories", json!({"memories": [MemoryVersion {key: version.key, expected_version: version.expected_version}]})).await?
        },
        Command::RESTORE { version, revision } => {
            let request = RestoreMemory {key: version.key, expected_version: version.expected_version, revision, actor: "human".into()};
            client.request("restore_memories", json!({"memories": [request]})).await?
        },
        Command::HISTORY { key, limit, before_version } => {
            client.request("list_memory_revisions", serde_json::to_value(HistoryRequest { key, limit, before_version })?).await?
        },
        Command::TAXONOMY { category, action } => {
            let (list, suffix) = match category {
                TaxonomyCategory::KINDS => ("list_kinds", "kind"),
                TaxonomyCategory::TAGS => ("list_tags", "tag"),
                TaxonomyCategory::RELATIONS => ("list_relation_types", "relation_type"),
            };

            match action {
                TaxonomyAction::LIST { limit, cursor } => client.request(list, json!({"limit": limit, "cursor": cursor})).await?,
                TaxonomyAction::CREATE { key, name } => client.request(&format!("create_{suffix}"), json!({"key": key, "name": name})).await?,
                TaxonomyAction::UPDATE { key, name } => client.request(&format!("update_{suffix}"), json!({"key": key, "name": name})).await?,
                TaxonomyAction::DELETE { key } => client.request(&format!("delete_{suffix}"), json!({"key": key})).await?,
            }
        },
    };

    print_output(&output, cli.json)?;

    Ok(())
}

fn decode_memory(value: &Value) -> Result<MemoryRecord> {
    let records = value
        .get("memories")
        .or_else(|| value.get("items"))
        .unwrap_or(value);
    let record = records
        .as_array()
        .and_then(|records| records.first())
        .unwrap_or(records);

    serde_json::from_value(record.clone())
        .context("The service did not return the requested memory.")
}

struct Draft {
    path: PathBuf,
}

impl Draft {
    fn open(body: &str) -> Result<Self> {
        let mut file = tempfile::Builder::new()
            .prefix("semantic-memory-")
            .suffix(".md")
            .tempfile()?;
        file.write_all(body.as_bytes())?;
        file.flush()?;
        let (_, path) = file.keep()?;
        let draft = Self { path };
        let editor = env::var("VISUAL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                env::var("EDITOR")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .unwrap_or_else(|| "vim".into());
        let arguments = shell_words::split(&editor).with_context(|| {
            format!(
                "Invalid editor command. Draft preserved at {}.",
                draft.path.display()
            )
        })?;
        let (program, arguments) = arguments
            .split_first()
            .context("The editor command is empty.")?;
        let result = process::Command::new(program)
            .args(arguments)
            .arg(&draft.path)
            .status();

        match result {
            Ok(status) if status.success() => Ok(draft),
            Ok(status) => bail!(
                "Editor exited with {status}. Draft preserved at {}.",
                draft.path.display()
            ),
            Err(error) => bail!(
                "Could not open editor: {error}. Draft preserved at {}.",
                draft.path.display()
            ),
        }
    }

    fn read(&self) -> Result<String> {
        fs::read_to_string(&self.path).with_context(|| {
            format!(
                "Could not read UTF-8 draft. Draft preserved at {}.",
                self.path.display()
            )
        })
    }

    fn discard(self) -> Result<()> {
        fs::remove_file(self.path).context("Could not remove the completed editor draft.")
    }
}

fn finish_draft(draft: Draft, result: Result<Value>) -> Result<Value> {
    match result {
        Ok(value) => {
            draft.discard()?;

            Ok(value)
        }
        Err(error) => bail!("{error}\nDraft preserved at {}.", draft.path.display()),
    }
}

fn print_output(value: &Value, is_json: bool) -> Result<()> {
    if is_json || !std::io::stdout().is_terminal() {
        println!("{}", serde_json::to_string_pretty(value)?);

        return Ok(());
    }

    let records = value
        .get("hits")
        .or_else(|| value.get("memories"))
        .or_else(|| value.get("items"))
        .or_else(|| value.get("revisions"))
        .unwrap_or(value);

    if let Some(records) = records.as_array() {
        let mut table = Table::new();
        table.set_content_arrangement(ContentArrangement::Dynamic);
        table.load_preset(comfy_table::presets::UTF8_BORDERS_ONLY);
        table.set_header(["Key", "Title / Excerpt", "Kind", "Version"]);

        for record in records {
            let text = record
                .get("excerpt")
                .or_else(|| record.get("title"))
                .or_else(|| record.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let version = record
                .get("system_version")
                .or_else(|| record.get("version"))
                .map(Value::to_string)
                .unwrap_or_default();
            table.add_row([
                record
                    .get("key")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                clean_terminal_text(text),
                record
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                version,
            ]);
        }

        println!("{table}");

        if records.len() == 1
            && let Some(body) = records[0].get("body").and_then(Value::as_str)
        {
            termimad::print_text(&clean_terminal_text(body));
        }

        if let Some(cursor) = value.get("next_cursor").and_then(Value::as_str) {
            println!("Next cursor: {cursor}");
        }
    } else if let Some(body) = value.get("body").and_then(Value::as_str) {
        termimad::print_text(&clean_terminal_text(body));
    } else {
        println!("{}", serde_json::to_string_pretty(value)?);
    }

    Ok(())
}

fn clean_terminal_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect()
}
