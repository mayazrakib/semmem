use std::env;
use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use comfy_table::{ContentArrangement, Table};
use serde_json::{Value, json};

use crate::models::{
    ClaimType, CreateMemory, GetMemories, HistoryRequest, ListMemories, MemoryLink, MemoryMetadata,
    MemoryRecord, MemoryVersion, MemoryView, RestoreMemory, SearchRequest,
};

#[derive(Debug, Parser)]
#[command(name = "semmem", version, about)]
pub struct Cli {
    #[arg(long, global = true, env = "SEMMEM_SOCKET", default_value_os_t = default_socket_path())]
    pub socket: PathBuf,
    #[arg(short = 'j', long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
pub enum Command {
    #[command(
        name = "run",
        hide = true,
        about = "Run the local memory service and authenticated MCP endpoint."
    )]
    RUN {
        #[arg(long, default_value_os_t = default_data_dir())]
        data_dir: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8765")]
        listen: SocketAddr,
        #[arg(long, env = "SEMMEM_ISSUER")]
        issuer: Option<String>,
        #[arg(long, env = "SEMMEM_AUDIENCE")]
        audience: Option<String>,
        #[arg(long, env = "SEMMEM_PUBLIC_URL")]
        public_url: Option<String>,
    },
    #[command(name = "start", about = "Start the semmem systemd user service.")]
    START,
    #[command(name = "stop", about = "Stop the semmem systemd user service.")]
    STOP,
    #[command(name = "status", about = "Show structured service status.")]
    STATUS,
    #[command(
        name = "search",
        about = "Search memory text, tolerating spelling mistakes by default."
    )]
    SEARCH {
        query: String,
        #[arg(long)]
        scope: Option<String>,
        #[arg(long, value_enum)]
        claim_type: Option<ClaimType>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        exact: bool,
        #[arg(long)]
        prefix: bool,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    #[command(name = "list", about = "Browse memories by kind, tag, or relationship.")]
    LIST(BrowseArguments),
    #[command(name = "show", about = "Read complete memories or historical versions.")]
    SHOW {
        keys: Vec<String>,
        #[arg(long, value_enum, default_value = "full")]
        view: MemoryView,
        #[arg(long)]
        include_deleted: bool,
        #[arg(long)]
        version: Option<i64>,
    },
    #[command(
        name = "add",
        about = "Prompt for metadata, then write Markdown in VISUAL, EDITOR, or Vim."
    )]
    ADD {
        key: Option<String>,
        #[arg(
            long,
            help = "Use supplied flags and defaults without prompting. This requires KEY."
        )]
        no_input: bool,
        #[command(flatten)]
        metadata: MetadataArguments,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long = "link", value_parser = parse_link)]
        links: Vec<MemoryLink>,
    },
    #[command(
        name = "edit",
        about = "Edit a memory as Markdown with YAML frontmatter."
    )]
    EDIT { key: String },
    #[command(
        name = "delete",
        about = "Move a memory to the recoverable deleted state."
    )]
    DELETE(VersionArguments),
    #[command(
        name = "purge",
        about = "Permanently remove a memory and its history. This requires --confirm."
    )]
    PURGE {
        #[command(flatten)]
        version: VersionArguments,
        #[arg(long, required = true)]
        confirm: bool,
    },
    #[command(
        name = "restore",
        about = "Restore a deleted memory or an earlier revision."
    )]
    RESTORE {
        #[command(flatten)]
        version: VersionArguments,
        #[arg(long)]
        revision: Option<i64>,
    },
    #[command(name = "history", about = "Inspect a memory's revision history.")]
    HISTORY {
        key: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        before_version: Option<i64>,
    },
    #[command(
        name = "taxonomy",
        about = "List, create, rename, or delete kinds, tags, and relationship types."
    )]
    TAXONOMY {
        #[arg(value_enum)]
        category: TaxonomyCategory,
        #[command(subcommand)]
        action: TaxonomyAction,
    },
}

#[derive(Debug, Args)]
pub struct MetadataArguments {
    #[arg(long)]
    summary: Option<String>,
    #[arg(long, value_enum)]
    claim_type: Option<ClaimType>,
    #[arg(long)]
    scope: Option<String>,
    #[arg(
        long,
        allow_hyphen_values = true,
        help = "Specify UTC milliseconds since Unix epoch."
    )]
    observed_at: Option<i64>,
    #[arg(
        long,
        allow_hyphen_values = true,
        help = "Specify UTC milliseconds since Unix epoch."
    )]
    verified_at: Option<i64>,
    #[arg(
        long,
        allow_hyphen_values = true,
        help = "Specify UTC milliseconds since Unix epoch."
    )]
    review_after: Option<i64>,
}

impl From<MetadataArguments> for MemoryMetadata {
    fn from(arguments: MetadataArguments) -> Self {
        Self {
            summary: arguments.summary,
            claim_type: arguments.claim_type,
            scope: arguments.scope,
            observed_at: arguments.observed_at,
            verified_at: arguments.verified_at,
            review_after: arguments.review_after,
        }
    }
}

#[derive(Debug, Args)]
pub struct BrowseArguments {
    #[arg(long)]
    scope: Option<String>,
    #[arg(long, value_enum)]
    claim_type: Option<ClaimType>,
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

pub fn parse_cli() -> Cli {
    match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let text = crate::output::format_help(&error.render().to_string());

            if error.use_stderr() {
                eprint!("{text}");
            } else {
                print!("{text}");
            }

            process::exit(error.exit_code());
        }
    }
}

pub fn default_data_dir() -> PathBuf {
    let root = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".local/share")
        });

    root.join("semmem")
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
            .build()?;

        Ok(Self { client })
    }

    pub async fn request(&self, operation: &str, arguments: Value) -> Result<Value> {
        let response = self
            .client
            .post(format!("http://localhost/api/{operation}"))
            .json(&arguments)
            .send()
            .await
            .context(
                "Could not connect to the memory service. Run semmem start or select --socket.",
            )?;
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
    let is_structured = matches!(&cli.command, Command::SEARCH { .. } | Command::STATUS);
    let output = match cli.command {
        Command::RUN { .. } => bail!("The run command must be dispatched by the application entry point."),
        Command::START => crate::lifecycle::execute("start").await?,
        Command::STOP => crate::lifecycle::execute("stop").await?,
        Command::STATUS => crate::lifecycle::execute("status").await?,
        Command::SEARCH { query, scope, claim_type, kind, tags, exact, prefix, limit } => {
            let request = SearchRequest { query, scope, claim_type, kind, tags, prefix, fuzzy: !exact, limit };
            client.request("search_memories", serde_json::to_value(request)?).await?
        },
        Command::LIST(arguments) => {
            let request = ListMemories {
                scope: arguments.scope,
                claim_type: arguments.claim_type,
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
        Command::SHOW { keys, view, include_deleted, version } => {
            client.request("get_memories", serde_json::to_value(GetMemories { keys, view, include_deleted, version })?).await?
        },
        Command::ADD { key, no_input, metadata, title, kind, tags, links } => {
            let mut fields = NewMemoryFields { key, title, kind, metadata: metadata.into(), tags, links };

            if !no_input {
                prompt_missing_metadata(&mut std::io::stdin().lock(), &mut std::io::stderr().lock(), &mut fields)?;
            }

            let key = fields.key.context("A key is required with --no-input.")?;
            let title = fields.title.unwrap_or_default();
            let kind = fields.kind.unwrap_or_else(|| "note".into());
            let draft = Draft::open("")?;
            let body = draft.read()?;

            if body.is_empty() {
                eprintln!("The draft is empty. No memory was created.");
                draft.discard()?;

                return Ok(());
            }

            let memory = CreateMemory { key, title, body, metadata: fields.metadata, kind, tags: fields.tags, links: fields.links, actor: "human".into() };
            let result = client.request("create_memories", json!({"memories": [memory]})).await;
            finish_draft(draft, result)?
        },
        Command::EDIT { key } => {
            let value = client.request("get_memories", json!({"keys": [&key]})).await?;
            let memory = decode_memory(&value)?;
            let original = crate::document::encode_memory(&memory)?;
            let draft = Draft::open(&original)?;
            let edited = draft.read()?;

            if edited == original {
                eprintln!("No changes were made. No revision was created.");
                draft.discard()?;

                return Ok(());
            }

            let request = crate::document::decode_memory(&edited, &memory).with_context(|| {
                format!("The memory document is invalid. The draft was preserved at {}.", draft.path.display())
            })?;
            let has_changes = request.body.as_ref().is_some_and(|body| body != &memory.body)
                || request.metadata.apply(&memory.metadata) != memory.metadata
                || request.title.as_ref().is_some_and(|title| title != &memory.title)
                || request.kind.as_ref().is_some_and(|kind| kind != &memory.kind)
                || request.tags.as_ref().is_some_and(|tags| tags != &memory.tags)
                || request.links.as_ref().is_some_and(|links| links != &memory.links);

            if !has_changes {
                eprintln!("No changes were made. No revision was created.");
                draft.discard()?;

                return Ok(());
            }

            let result = client.request("update_memories", json!({"memories": [request]})).await;
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

    print_output(&output, cli.json || is_structured)?;

    Ok(())
}

struct NewMemoryFields {
    key: Option<String>,
    title: Option<String>,
    kind: Option<String>,
    metadata: MemoryMetadata,
    tags: Vec<String>,
    links: Vec<MemoryLink>,
}

fn prompt_line(input: &mut impl BufRead, output: &mut impl Write, label: &str) -> Result<String> {
    write!(output, "{label}: ")?;
    output.flush()?;
    let mut value = String::new();

    if input.read_line(&mut value)? == 0 {
        bail!("Input ended before metadata was completed. No memory was created.");
    }

    if value.ends_with('\n') {
        value.pop();

        if value.ends_with('\r') {
            value.pop();
        }
    }

    Ok(value)
}

fn prompt_timestamp(
    input: &mut impl BufRead,
    output: &mut impl Write,
    label: &str,
) -> Result<Option<i64>> {
    loop {
        let value = prompt_line(input, output, label)?;

        if value.trim().is_empty() {
            return Ok(None);
        }

        match value.trim().parse() {
            Ok(timestamp) => return Ok(Some(timestamp)),
            Err(_) => writeln!(
                output,
                "Enter integer UTC milliseconds since Unix epoch, or leave blank for unknown."
            )?,
        }
    }
}

fn prompt_optional_text(
    input: &mut impl BufRead,
    output: &mut impl Write,
    label: &str,
) -> Result<Option<String>> {
    let value = prompt_line(input, output, label)?;

    Ok((!value.trim().is_empty()).then_some(value))
}

fn prompt_missing_metadata(
    input: &mut impl BufRead,
    output: &mut impl Write,
    fields: &mut NewMemoryFields,
) -> Result<()> {
    while fields.key.is_none() {
        let key = prompt_line(input, output, "Key (required)")?;

        if key.trim().is_empty() || key.trim() != key || key.chars().any(char::is_control) {
            writeln!(
                output,
                "Enter a nonempty key without surrounding whitespace or control characters."
            )?;
        } else {
            fields.key = Some(key);
        }
    }

    if fields.title.is_none() {
        let default = fields.key.as_deref().expect("The key has been collected.");
        let value = prompt_line(input, output, &format!("Title [{default}]"))?;
        fields.title = Some(if value.trim().is_empty() {
            default.into()
        } else {
            value
        });
    }

    if fields.kind.is_none() {
        let value = prompt_line(input, output, "Kind [note]")?;
        fields.kind = Some(if value.trim().is_empty() {
            "note".into()
        } else {
            value
        });
    }

    if fields.metadata.summary.is_none() {
        fields.metadata.summary = prompt_optional_text(input, output, "Summary (optional)")?;
    }

    if fields.metadata.claim_type.is_none() {
        loop {
            let value = prompt_line(
                input,
                output,
                "Claim type (1 explicit user statement, 2 verified observation, 3 inference; blank for unknown)",
            )?;
            let value = value.trim();

            if value.is_empty() {
                break;
            }

            let claim_type = match value {
                "1" => Ok(ClaimType::EXPLICIT_USER_STATEMENT),
                "2" => Ok(ClaimType::VERIFIED_OBSERVATION),
                "3" => Ok(ClaimType::INFERENCE),
                _ => ClaimType::from_str(value, false),
            };

            match claim_type {
                Ok(claim_type) => {
                    fields.metadata.claim_type = Some(claim_type);

                    break;
                }
                Err(_) => writeln!(
                    output,
                    "Choose 1, 2, 3, a listed claim type, or leave blank for unknown."
                )?,
            }
        }
    }

    if fields.metadata.scope.is_none() {
        fields.metadata.scope = prompt_optional_text(input, output, "Scope (optional)")?;
    }

    if fields.metadata.observed_at.is_none() {
        fields.metadata.observed_at = prompt_timestamp(
            input,
            output,
            "Observed at (UTC Unix milliseconds; blank for unknown)",
        )?;
    }

    if fields.metadata.verified_at.is_none() {
        fields.metadata.verified_at = prompt_timestamp(
            input,
            output,
            "Verified at (UTC Unix milliseconds; blank for unknown)",
        )?;
    }

    if fields.metadata.review_after.is_none() {
        fields.metadata.review_after = prompt_timestamp(
            input,
            output,
            "Review after (UTC Unix milliseconds; blank for none)",
        )?;
    }

    if fields.tags.is_empty() {
        while let Some(tag) = prompt_optional_text(input, output, "Tag (blank to finish)")? {
            if tag.trim() != tag || tag.chars().any(char::is_control) {
                writeln!(
                    output,
                    "Enter a tag without surrounding whitespace or control characters."
                )?;
            } else {
                fields.tags.push(tag);
            }
        }
    }

    if fields.links.is_empty() {
        loop {
            let value = prompt_line(input, output, "Link (relation=target_key; blank to finish)")?;

            if value.trim().is_empty() {
                break;
            }

            match parse_link(&value) {
                Ok(link) => fields.links.push(link),
                Err(message) => writeln!(output, "{message}")?,
            }
        }
    }

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
            .prefix("semmem-")
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
                "The editor command is invalid. The draft was preserved at {}.",
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
                "The editor exited with {status}. The draft was preserved at {}.",
                draft.path.display()
            ),
            Err(error) => bail!(
                "The editor could not be opened: {error}. The draft was preserved at {}.",
                draft.path.display()
            ),
        }
    }

    fn read(&self) -> Result<String> {
        fs::read_to_string(&self.path).with_context(|| {
            format!(
                "The UTF-8 draft could not be read. The draft was preserved at {}.",
                self.path.display()
            )
        })
    }

    fn discard(self) -> Result<()> {
        fs::remove_file(self.path).context("The completed editor draft could not be removed.")
    }
}

fn finish_draft(draft: Draft, result: Result<Value>) -> Result<Value> {
    match result {
        Ok(value) => {
            draft.discard()?;

            Ok(value)
        }
        Err(error) => bail!("{error}\nThe draft was preserved at {}.", draft.path.display()),
    }
}

fn print_output(value: &Value, is_json: bool) -> Result<()> {
    if is_json || !std::io::stdout().is_terminal() {
        crate::output::print_json(value)?;

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
        table.set_header([
            "Key",
            "Title / Summary / Excerpt",
            "Kind",
            "Scope",
            "Claim",
            "Version",
        ]);

        for record in records {
            let text = ["summary", "excerpt", "title", "name"]
                .into_iter()
                .find_map(|field| {
                    record
                        .get(field)
                        .and_then(Value::as_str)
                        .filter(|text| !text.trim().is_empty())
                })
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
                clean_terminal_text(record.get("scope").and_then(Value::as_str).unwrap_or("")),
                record
                    .get("claim_type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                version,
            ]);
        }

        for column in table.column_iter_mut() {
            column.set_padding((4, 4));
        }

        println!("{table}");

        for record in records {
            for field in ["observed_at", "verified_at", "review_after"] {
                if let Some(timestamp) = record.get(field).and_then(Value::as_i64) {
                    let key = clean_terminal_text(
                        record.get("key").and_then(Value::as_str).unwrap_or(""),
                    );
                    println!("{key} {field}: {timestamp} UTC milliseconds since Unix epoch");
                }
            }
        }

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
        crate::output::print_json(value)?;
    }

    Ok(())
}

fn clean_terminal_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect()
}
