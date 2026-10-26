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
    Basis, CreateMemory, GetCommits, GetMemories, ListCommits, ListMemories, MemoryMetadata, MemoryRecord,
    MemoryReference, MemoryToken, MemoryView, MetadataPatch, SearchRequest,
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
        basis: Option<Basis>,
        #[arg(long)]
        r#type: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        exact: bool,
        #[arg(long)]
        prefix: bool,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    #[command(
        name = "list",
        about = "Browse memories by type, tag, or relationship."
    )]
    LIST(BrowseArguments),
    #[command(
        name = "show",
        about = "Read complete current memories."
    )]
    SHOW {
        keys: Vec<String>,
        #[arg(long, value_enum, default_value = "full")]
        view: MemoryView,
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
        message: Option<String>,
        #[arg(long)]
        r#type: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long = "reference", value_parser = parse_reference)]
        references: Vec<MemoryReference>,
    },
    #[command(
        name = "edit",
        about = "Edit a memory as Markdown with YAML frontmatter."
    )]
    EDIT {
        key: String,
        #[arg(long)]
        message: Option<String>,
    },
    #[command(
        name = "delete",
        about = "Permanently delete a memory and its incoming references."
    )]
    DELETE(TokenArguments),
    #[command(name = "commits", about = "List audit commits, newest first.")]
    COMMITS {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        cursor: Option<String>,
    },
    #[command(name = "commit", about = "Read audit commits by ID.")]
    COMMIT { ids: Vec<i64> },
    #[command(
        name = "taxonomy",
        about = "List, create, rename, or delete types, tags, and reference types."
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
    basis: Option<Basis>,
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
            basis: arguments.basis,
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
    basis: Option<Basis>,
    #[arg(long)]
    r#type: Option<String>,
    #[arg(long = "tag")]
    tags: Vec<String>,
    #[arg(long)]
    referenced_to: Option<String>,
    #[arg(long)]
    referenced_from: Option<String>,
    #[arg(long, default_value_t = 20)]
    limit: usize,
    #[arg(long)]
    cursor: Option<String>,
}

#[derive(Debug, Args)]
pub struct TokenArguments {
    key: String,
    #[arg(long)]
    expected_token: String,
    #[arg(long)]
    message: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
pub enum TaxonomyCategory {
    #[value(name = "types")]
    TYPES,
    #[value(name = "tags")]
    TAGS,
    #[value(name = "references")]
    REFERENCES,
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
    CREATE {
        key: String,
        name: String,
        #[arg(long)]
        message: String,
    },
    #[command(name = "update")]
    UPDATE {
        key: String,
        name: String,
        #[arg(long)]
        message: String,
    },
    #[command(name = "delete")]
    DELETE {
        key: String,
        #[arg(long)]
        message: String,
    },
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

fn parse_reference(value: &str) -> Result<MemoryReference, String> {
    let (relation, target_key) = value
        .split_once('=')
        .ok_or_else(|| "Use --reference reference_type=target_key.".to_string())?;

    if relation.is_empty() || target_key.is_empty() {
        return Err("Both relationship type and target key must be nonempty.".into());
    }

    Ok(MemoryReference {
        target_key: target_key.into(),
        reference_type: relation.into(),
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

    pub async fn request(
        &self,
        operation: &str,
        arguments: Value,
    ) -> Result<Value> {
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
        Command::SEARCH { query, scope, basis, r#type, tags, exact, prefix, limit } => {
            let request = SearchRequest { query, scope, basis, r#type, tags, prefix, fuzzy: !exact, limit };
            client.request("search_memories", serde_json::to_value(request)?).await?
        },
        Command::LIST(arguments) => {
            let request = ListMemories {
                scope: arguments.scope,
                basis: arguments.basis,
                r#type: arguments.r#type,
                tags: arguments.tags,
                referenced_to: arguments.referenced_to,
                referenced_from: arguments.referenced_from,
                limit: arguments.limit,
                cursor: arguments.cursor,
            };
            client.request("list_memories", serde_json::to_value(request)?).await?
        },
        Command::SHOW { keys, view } => {
            client.request("get_memories", serde_json::to_value(GetMemories { keys, view })?).await?
        },
        Command::ADD { key, no_input, metadata, title, r#type, tags, references, message } => {
            if no_input && message.as_ref().is_none_or(|message| message.trim().is_empty()) {
                bail!("An audit message is required with --no-input. Supply --message.");
            }

            let mut fields = NewMemoryFields { key, title, r#type, metadata: metadata.into(), tags, references };

            if !no_input {
                prompt_missing_metadata(
                    &mut std::io::stdin().lock(),
                    &mut std::io::stderr().lock(),
                    &mut fields,
                )?;
            }

            let key = fields.key.context("A key is required with --no-input.")?;
            let title = fields.title.unwrap_or_default();
            let r#type = fields.r#type.unwrap_or_else(|| "general".into());
            let draft = Draft::open("")?;
            let body = draft.read()?;

            if body.is_empty() {
                eprintln!("The draft is empty. No memory was created.");
                draft.discard()?;

                return Ok(());
            }

            let memory = CreateMemory { key, title, body, metadata: create_metadata(fields.metadata), r#type, tags: fields.tags, references: fields.references };
            let commit_summary = resolve_message(message, !no_input).with_context(|| format!("The draft was preserved at {}.", draft.path.display()))?;
            let result = client.request("create_memories", json!({"memories": [memory], "commit_summary": commit_summary})).await;
            finish_draft(draft, result)?
        },
        Command::EDIT { key, message } => {
            let value = client.request("get_memories", json!({"keys": [&key]})).await?;
            let memory = decode_memory(&value)?;
            let original = crate::document::encode_memory(&memory)?;
            let draft = Draft::open(&original)?;
            let edited = draft.read()?;

            if edited == original {
                eprintln!("No changes were made. No change was saved.");
                draft.discard()?;

                return Ok(());
            }

            let request = crate::document::decode_memory(&edited, &memory).with_context(|| {
                format!("The memory document is invalid. The draft was preserved at {}.", draft.path.display())
            })?;
            let has_changes = request.body.as_ref().is_some_and(|body| body != &memory.body)
                || request.metadata.apply(&memory.metadata) != memory.metadata
                || request.title.as_ref().is_some_and(|title| title != &memory.title)
                || request.r#type.as_ref().is_some_and(|r#type| r#type != &memory.r#type)
                || request.tags.as_ref().is_some_and(|tags| tags != &memory.tags)
                || request.references.as_ref().is_some_and(|references| references != &memory.references);

            if !has_changes {
                eprintln!("No changes were made. No change was saved.");
                draft.discard()?;

                return Ok(());
            }

            let commit_summary = resolve_message(message, std::io::stdin().is_terminal()).with_context(|| format!("The draft was preserved at {}.", draft.path.display()))?;
            let result = client.request("update_memories", json!({"memories": [request], "commit_summary": commit_summary})).await;
            finish_draft(draft, result)?
        },
        Command::DELETE(arguments) => {
            client.request("delete_memories", json!({"memories": [MemoryToken {key: arguments.key, expected_token: arguments.expected_token}], "commit_summary": validate_message(arguments.message)?})).await?
        },
        Command::COMMITS { limit, cursor } => {
            client.request("list_commits", serde_json::to_value(ListCommits { limit, cursor })?).await?
        },
        Command::COMMIT { ids } => {
            client.request("get_commits", serde_json::to_value(GetCommits { ids })?).await?
        },
        Command::TAXONOMY { category, action } => {
            let (list, suffix) = match category {
                TaxonomyCategory::TYPES => ("list_types", "type"),
                TaxonomyCategory::TAGS => ("list_tags", "tag"),
                TaxonomyCategory::REFERENCES => ("list_reference_types", "reference_type"),
            };

            match action {
                TaxonomyAction::LIST { limit, cursor } => client.request(list, json!({"limit": limit, "cursor": cursor})).await?,
                TaxonomyAction::CREATE { key, name, message } => client.request(&format!("create_{suffix}"), json!({"key": key, "name": name, "commit_summary": validate_message(message)?})).await?,
                TaxonomyAction::UPDATE { key, name, message } => client.request(&format!("update_{suffix}"), json!({"key": key, "name": name, "commit_summary": validate_message(message)?})).await?,
                TaxonomyAction::DELETE { key, message } => client.request(&format!("delete_{suffix}"), json!({"key": key, "commit_summary": validate_message(message)?})).await?,
            }
        },
    };

    print_output(&output, cli.json || is_structured)?;

    Ok(())
}

fn create_metadata(metadata: MemoryMetadata) -> MetadataPatch {
    MetadataPatch {
        summary: metadata.summary.map(Some),
        basis: metadata.basis.map(Some),
        scope: metadata.scope.map(Some),
        observed_at: metadata.observed_at.map(Some),
        verified_at: metadata.verified_at.map(Some),
        review_after: metadata.review_after.map(Some),
    }
}

fn validate_message(message: String) -> Result<String> {
    if message.trim().is_empty() {
        bail!("An audit message is required. Supply --message with a nonblank change summary.");
    }

    Ok(message.trim().to_owned())
}

fn resolve_message(message: Option<String>, can_prompt: bool) -> Result<String> {
    if let Some(message) = message {
        return validate_message(message);
    }

    if !can_prompt {
        bail!("An audit message is required for noninteractive writes. Supply --message.");
    }

    loop {
        let message = prompt_line(
            &mut std::io::stdin().lock(),
            &mut std::io::stderr().lock(),
            "Change summary (required)",
        )?;

        match validate_message(message) {
            Ok(message) => return Ok(message),
            Err(error) => eprintln!("{error}"),
        }
    }
}

struct NewMemoryFields {
    key: Option<String>,
    title: Option<String>,
    r#type: Option<String>,
    metadata: MemoryMetadata,
    tags: Vec<String>,
    references: Vec<MemoryReference>,
}

fn prompt_line(
    input: &mut impl BufRead,
    output: &mut impl Write,
    label: &str,
) -> Result<String> {
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
        let value = prompt_line(
            input,
            output,
            label,
        )?;

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
    let value = prompt_line(
        input,
        output,
        label,
    )?;

    Ok((!value.trim().is_empty()).then_some(value))
}

fn prompt_missing_metadata(
    input: &mut impl BufRead,
    output: &mut impl Write,
    fields: &mut NewMemoryFields,
) -> Result<()> {
    while fields.key.is_none() {
        let key = prompt_line(
            input,
            output,
            "Key (required)",
        )?;

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
        let value = prompt_line(
            input,
            output,
            &format!("Title [{default}]"),
        )?;
        fields.title = Some(if value.trim().is_empty() {
            default.into()
        } else {
            value
        });
    }

    if fields.r#type.is_none() {
        let value = prompt_line(
            input,
            output,
            "Type [general]",
        )?;
        fields.r#type = Some(if value.trim().is_empty() {
            "general".into()
        } else {
            value
        });
    }

    if fields.metadata.summary.is_none() {
        fields.metadata.summary = prompt_optional_text(
            input,
            output,
            "Summary (optional)",
        )?;
    }

    if fields.metadata.basis.is_none() {
        loop {
            let value = prompt_line(
                input,
                output,
                "Basis (1 stated, 2 observed, 3 inferred, 4 assumed; blank for unknown)",
            )?;
            let value = value.trim();

            if value.is_empty() {
                break;
            }

            let basis = match value {
                "1" => Ok(Basis::STATED),
                "2" => Ok(Basis::OBSERVED),
                "3" => Ok(Basis::INFERRED),
                "4" => Ok(Basis::ASSUMED),
                _ => Basis::from_str(value, false),
            };

            match basis {
                Ok(basis) => {
                    fields.metadata.basis = Some(basis);

                    break;
                }
                Err(_) => writeln!(
                    output,
                    "Choose 1, 2, 3, 4, a listed basis, or leave blank for unknown."
                )?,
            }
        }
    }

    if fields.metadata.scope.is_none() {
        fields.metadata.scope = prompt_optional_text(
            input,
            output,
            "Scope (optional)",
        )?;
    }

    if fields.metadata.observed_at.is_none() {
        fields.metadata.observed_at = prompt_timestamp(
            input,
            output,
            "Observed at (UTC Unix milliseconds; blank for current time)",
        )?;
    }

    if fields.metadata.verified_at.is_none() {
        fields.metadata.verified_at = prompt_timestamp(
            input,
            output,
            "Verified at (UTC Unix milliseconds; blank for current time)",
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
        while let Some(tag) = prompt_optional_text(
            input,
            output,
            "Tag (blank to finish)",
        )? {
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

    if fields.references.is_empty() {
        loop {
            let value = prompt_line(
                input,
                output,
                "Reference (reference_type=target_key; blank to finish)",
            )?;

            if value.trim().is_empty() {
                break;
            }

            match parse_reference(&value) {
                Ok(link) => fields.references.push(link),
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

fn finish_draft(
    draft: Draft,
    result: Result<Value>,
) -> Result<Value> {
    match result {
        Ok(value) => {
            draft.discard()?;

            Ok(value)
        }
        Err(error) => bail!(
            "{error}\nThe draft was preserved at {}.",
            draft.path.display()
        ),
    }
}

fn print_output(
    value: &Value,
    is_json: bool,
) -> Result<()> {
    if is_json || !std::io::stdout().is_terminal() {
        crate::output::print_json(value)?;

        return Ok(());
    }

    let records = value
        .get("hits")
        .or_else(|| value.get("memories"))
        .or_else(|| value.get("items"))
        .or_else(|| value.get("commits"))
        .unwrap_or(value);

    if let Some(records) = records.as_array() {
        let mut table = Table::new();
        table.set_content_arrangement(ContentArrangement::Dynamic);
        table.load_preset(comfy_table::presets::UTF8_BORDERS_ONLY);
        table.set_header([
            "Key",
            "Title / Summary",
            "Type",
            "Scope",
            "Basis",
            "Token",
        ]);

        for record in records {
            let text = ["summary", "title", "name"]
                .into_iter()
                .find_map(|field| {
                    record
                        .get(field)
                        .and_then(Value::as_str)
                        .filter(|text| !text.trim().is_empty())
                })
                .unwrap_or("");
            let token = record
                .get("system_token")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            table.add_row([
                record
                    .get("key")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                clean_terminal_text(text),
                record
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                clean_terminal_text(record.get("scope").and_then(Value::as_str).unwrap_or("")),
                record
                    .get("basis")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                token,
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

        for record in records {
            if let Some(body) = record.get("body").and_then(Value::as_str) {
                let key =
                    clean_terminal_text(record.get("key").and_then(Value::as_str).unwrap_or(""));
                println!("\n{key}");
                termimad::print_text(&clean_terminal_text(body));
            }
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
