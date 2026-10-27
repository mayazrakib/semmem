#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use axum::extract::{self, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

#[test]
fn delete_requires_a_token_and_an_audit_message_and_retired_commands_are_rejected() {
    for arguments in [
        vec!["delete", "example", "--expected-token", "token"],
        vec!["delete", "example", "--message", "Remove example"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_semmem")).args(arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
    for command in ["history", "restore", "purge", "register", "deregister"] {
        let output = Command::new(env!("CARGO_BIN_EXE_semmem")).arg(command).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
}

#[derive(Clone, Default)]
struct ServerState {
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    has_conflict: bool,
}

async fn handle_request(
    extract::Path(operation): extract::Path<String>,
    State(state): State<ServerState>,
    Json(arguments): Json<Value>,
) -> (StatusCode, Json<Value>) {
    state
        .requests
        .lock()
        .unwrap()
        .push((operation.clone(), arguments.clone()));

    if operation == "get_memories" {
        return (
            StatusCode::OK,
            Json(json!({"commit_summary":"Test change", "memories": [{
                "id": 1, "key": "example", "title": "Example", "body": "Original body\n",
                "type": "general", "tags": [], "references": [], "system_created_by": "human", "system_updated_by": "human",
                "system_token": "fixture-token", "system_created_at": 1, "system_updated_at": 2
            }]})),
        );
    }

    if state.has_conflict {
        return (
            StatusCode::CONFLICT,
            Json(json!({"code": "token_conflict", "message": "A newer change exists."})),
        );
    }

    (StatusCode::OK, Json(json!({"saved": arguments})))
}

async fn start_service(
    socket: &Path,
    state: ServerState,
) -> tokio::task::JoinHandle<()> {
    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    let router = Router::new()
        .route("/api/{operation}", post(handle_request))
        .with_state(state);

    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    })
}

fn write_editor(
    root: &Path,
    script: &str,
) -> String {
    let path = root.join("editor with spaces");
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();

    format!("'{}'", path.display())
}

async fn run_cli(
    socket: &Path,
    editor: String,
    arguments: &[&str],
) -> Output {
    let mut arguments = arguments.to_vec();

    if arguments.first() == Some(&"add") && !arguments.contains(&"--no-input") {
        arguments.push("--no-input");
    }

    run_cli_with_input(
        socket,
        editor,
        &arguments,
        None,
    ).await
}

async fn run_cli_with_input(
    socket: &Path,
    editor: String,
    arguments: &[&str],
    input: Option<&str>,
) -> Output {
    let socket = socket.to_path_buf();
    let mut arguments: Vec<String> = arguments.iter().map(|value| value.to_string()).collect();

    let is_write = arguments.first().is_some_and(|name| ["add", "edit", "delete"].contains(&name.as_str()))
        || (arguments.first().is_some_and(|name| name == "taxonomy") && arguments.iter().any(|value| ["create", "update", "delete"].contains(&value.as_str())));
    if is_write && !arguments.iter().any(|value| value == "--message") {
        arguments.extend(["--message".into(), "Test CLI change".into()]);
    }

    let input = input.map(str::to_owned);

    tokio::task::spawn_blocking(move || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_semmem"));
        command
            .arg("--socket")
            .arg(socket)
            .arg("--json")
            .args(arguments)
            .env("VISUAL", editor)
            .env("EDITOR", "/bin/false")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let mut child = command.spawn().unwrap();

        if let Some(input) = input {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }

        child.wait_with_output().unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_add_collects_metadata_before_opening_an_empty_body_editor() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("service.sock");
    let state = ServerState::default();
    let server = start_service(&socket, state.clone()).await;
    let editor = write_editor(
        root.path(),
        "[ ! -s \"$1\" ] || exit 23\nprintf '# Body only\\n\\nExact café.\\n' > \"$1\"",
    );
    let input = "prompted\nPrompted title\ndecision\nManual summary\n2\nproject\n100\n200\n300\nresearch\npersistent\n\nsupports=target\n\n";
    let output = run_cli_with_input(
        &socket,
        editor,
        &["add"],
        Some(input),
    ).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout.clone()).unwrap();
    assert!(!text.contains('\u{1b}'));
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    let record = &stdout["saved"]["memories"][0];
    assert_eq!(record["key"], "prompted");
    assert_eq!(record["title"], "Prompted title");
    assert_eq!(record["type"], "decision");
    assert_eq!(record["summary"], "Manual summary");
    assert_eq!(record["basis"], "observed");
    assert_eq!(record["scope"], "project");
    assert_eq!(record["observed_at"], 100);
    assert_eq!(record["verified_at"], 200);
    assert_eq!(record["review_after"], 300);
    assert_eq!(record["tags"], json!(["research", "persistent"]));
    assert_eq!(
        record["references"],
        json!([{"reference_type": "supports", "target_key": "target"}])
    );
    assert_eq!(record["body"], "# Body only\n\nExact café.\n");
    assert_eq!(state.requests.lock().unwrap().len(), 1);
    let prompts = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();

    for field in [
        "key",
        "title",
        "type",
        "summary",
        "basis",
        "scope",
        "observed",
        "verified",
        "review",
        "tag",
        "reference",
    ] {
        assert!(prompts.contains(field), "Missing {field} prompt: {prompts}");
    }

    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_add_reprompts_invalid_values_and_keeps_blank_metadata_unknown() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("service.sock");
    let state = ServerState::default();
    let server = start_service(&socket, state.clone()).await;
    let editor = write_editor(
        root.path(),
        "[ ! -s \"$1\" ] || exit 23\nprintf 'Body\\n' > \"$1\"",
    );
    let input = "\nvalid-key\n\n\n\nnot-a-basis\n5\ninferred\n\nyesterday\n123\n1.5\n200\n\n\ninvalid-link\nsupports=\nsupports=target\n\n";
    let output = run_cli_with_input(
        &socket,
        editor,
        &["add"],
        Some(input),
    ).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = state.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let record = &requests[0].1["memories"][0];
    assert_eq!(record["key"], "valid-key");
    assert_eq!(record["title"], "valid-key");
    assert_eq!(record["type"], "general");
    assert_eq!(record["basis"], "inferred");
    assert_eq!(record["observed_at"], 123);
    assert_eq!(record["verified_at"], 200);
    assert!(record["summary"].is_null());
    assert!(record["scope"].is_null());
    assert!(record["review_after"].is_null());
    assert_eq!(record["tags"], json!([]));
    assert_eq!(
        record["references"],
        json!([{"reference_type": "supports", "target_key": "target"}])
    );
    let prompts = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    assert!(prompts.matches("basis").count() >= 3, "{prompts}");
    assert!(prompts.matches("observed").count() >= 2, "{prompts}");
    assert!(prompts.matches("verified").count() >= 2, "{prompts}");
    assert!(prompts.matches("reference").count() >= 3, "{prompts}");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_add_eof_aborts_before_editor_or_save() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("service.sock");
    let state = ServerState::default();
    let server = start_service(&socket, state.clone()).await;
    let marker = root.path().join("editor-started");
    let editor = write_editor(
        root.path(),
        &format!("touch '{}'; printf 'Body\\n' > \"$1\"", marker.display()),
    );
    let output = run_cli_with_input(
        &socket,
        editor,
        &["add", "example"],
        Some("Incomplete title\n"),
    )
    .await;
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty());
    assert!(!marker.exists());
    assert!(state.requests.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fully_supplied_add_flags_skip_prompts_without_no_input() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("service.sock");
    let state = ServerState::default();
    let server = start_service(&socket, state.clone()).await;
    let editor = write_editor(
        root.path(),
        "[ ! -s \"$1\" ] || exit 23\nprintf 'Flag body\\n' > \"$1\"",
    );
    let output = run_cli_with_input(
        &socket,
        editor,
        &[
            "add",
            "flags",
            "--title",
            "Flags",
            "--type",
            "decision",
            "--summary",
            "Flag summary",
            "--basis",
            "stated",
            "--scope",
            "project",
            "--observed-at",
            "-100",
            "--verified-at",
            "200",
            "--review-after",
            "300",
            "--tag",
            "research",
            "--reference",
            "supports=target",
        ],
        Some(""),
    )
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = state.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].1["memories"][0]["observed_at"], -100);
    assert_eq!(requests[0].1["memories"][0]["body"], "Flag body\n");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn noninteractive_add_preserves_defaults_and_requires_a_key() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("service.sock");
    let state = ServerState::default();
    let server = start_service(&socket, state.clone()).await;
    let editor = write_editor(
        root.path(),
        "[ ! -s \"$1\" ] || exit 23\nprintf 'Unprompted body\\n' > \"$1\"",
    );
    let output = run_cli(
        &socket,
        editor.clone(),
        &["add", "explicit", "--no-input"],
    ).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    {
        let requests = state.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let record = &requests[0].1["memories"][0];
        assert_eq!(record["title"], "");
        assert_eq!(record["type"], "general");
        assert!(record["summary"].is_null());
        assert!(record["basis"].is_null());
        assert_eq!(record["tags"], json!([]));
    }
    let missing = run_cli(
        &socket,
        editor,
        &["add", "--no-input"],
    ).await;
    assert!(!missing.status.success());
    assert_eq!(state.requests.lock().unwrap().len(), 1);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creates_markdown_using_the_quoted_visual_editor() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("service.sock");
    let state = ServerState::default();
    let server = start_service(&socket, state.clone()).await;
    let editor = write_editor(
        root.path(),
        "printf '# Memory\\n\\nUnicode: café ♟\\n' > \"$1\"",
    );
    let output = run_cli(
        &socket,
        editor,
        &["add", "example", "--title", "Example"],
    ).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = state.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "create_memories");
    assert_eq!(
        requests[0].1["memories"][0]["body"],
        "# Memory\n\nUnicode: café ♟\n"
    );
    assert_eq!(requests[0].1["memories"][0]["title"], "Example");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unchanged_editor_does_not_write() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("service.sock");
    let state = ServerState::default();
    let server = start_service(&socket, state.clone()).await;
    let editor = write_editor(root.path(), "exit 0");
    let output = run_cli(
        &socket,
        editor,
        &["edit", "example"],
    ).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = state.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "get_memories");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conflict_preserves_the_edited_draft_and_sends_expected_token() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("service.sock");
    let state = ServerState {
        has_conflict: true,
        ..ServerState::default()
    };
    let server = start_service(&socket, state.clone()).await;
    let editor = write_editor(root.path(), "printf 'Unsaved correction\\n' >> \"$1\"");
    let output = run_cli(
        &socket,
        editor,
        &["edit", "example"],
    ).await;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("token_conflict"), "{stderr}");
    let path = stderr
        .split("The draft was preserved at ")
        .nth(1)
        .unwrap()
        .trim()
        .trim_end_matches('.');
    let draft = Path::new(path);
    assert!(
        fs::read_to_string(draft)
            .unwrap()
            .ends_with("Unsaved correction\n")
    );
    let requests = state.requests.lock().unwrap();
    assert_eq!(requests[1].0, "update_memories");
    assert_eq!(requests[1].1["memories"][0]["expected_token"], "fixture-token");
    fs::remove_file(draft).unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn editor_failure_preserves_the_draft_without_sending_a_write() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("service.sock");
    let state = ServerState::default();
    let server = start_service(&socket, state.clone()).await;
    let editor = write_editor(
        root.path(),
        "printf 'Recover this draft\\n' > \"$1\"; exit 7",
    );
    let output = run_cli(
        &socket,
        editor,
        &["add", "example"],
    ).await;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("The editor exited"), "{stderr}");
    let path = stderr
        .split("The draft was preserved at ")
        .nth(1)
        .unwrap()
        .trim()
        .trim_end_matches('.');
    let draft = Path::new(path);
    assert_eq!(fs::read_to_string(draft).unwrap(), "Recover this draft\n");
    assert!(state.requests.lock().unwrap().is_empty());
    fs::remove_file(draft).unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frontmatter_edits_update_metadata_and_body() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("service.sock");
    let state = ServerState::default();
    let server = start_service(&socket, state.clone()).await;
    let editor = write_editor(
        root.path(),
        r#"sed -i -e 's/^summary: null$/summary: Complete summary/' -e 's/^scope: null$/scope: project/' -e 's/^basis: null$/basis: observed/' -e 's/^observed_at: null$/observed_at: 100/' -e 's/^verified_at: null$/verified_at: 200/' -e 's/^review_after: null$/review_after: 300/' "$1"
printf 'Additional content\n' >> "$1""#,
    );
    let output = run_cli(
        &socket,
        editor,
        &["edit", "example"],
    ).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = state.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].0, "update_memories");
    let record = &requests[1].1["memories"][0];
    assert_eq!(record["summary"], "Complete summary");
    assert_eq!(record["scope"], "project");
    assert_eq!(record["basis"], "observed");
    assert_eq!(record["observed_at"], 100);
    assert_eq!(record["verified_at"], 200);
    assert_eq!(record["review_after"], 300);
    assert_eq!(record["body"], "Original body\nAdditional content\n");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readonly_frontmatter_change_preserves_draft_and_does_not_write() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("service.sock");
    let state = ServerState::default();
    let server = start_service(&socket, state.clone()).await;
    let editor = write_editor(
        root.path(),
        r#"sed -i 's/^system_token: fixture-token$/system_token: changed/' "$1""#,
    );
    let output = run_cli(
        &socket,
        editor,
        &["edit", "example"],
    ).await;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("The draft was preserved at"), "{stderr}");
    assert_eq!(state.requests.lock().unwrap().len(), 1);
    let path = stderr
        .split("The draft was preserved at ")
        .nth(1)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .trim()
        .trim_end_matches('.');
    assert!(Path::new(path).exists(), "{stderr}");
    fs::remove_file(path).unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_review_filters_and_request_ids_reach_the_service() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("service.sock");
    let state = ServerState::default();
    let server = start_service(&socket, state.clone()).await;
    let editor = write_editor(
        root.path(),
        "if [ -s \"$1\" ]; then sed -i 's/Original body/Changed body/' \"$1\"; else printf 'Body\\n' > \"$1\"; fi",
    );
    let id = "00000000-0000-0000-0000-000000000001";

    for arguments in [
        vec!["list", "--review-due-by", "100", "--never-verified"],
        vec!["search", "needle", "--due-for-review", "--never-verified"],
        vec!["mutation_receipts", id],
        vec!["add", "example", "--request-id", id],
        vec!["edit", "example", "--request-id", id],
        vec![
            "delete",
            "example",
            "--expected-token",
            "token",
            "--request-id",
            id,
        ],
    ] {
        let output = run_cli(&socket, editor.clone(), &arguments).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let requests = state.requests.lock().unwrap();
    let list = requests
        .iter()
        .find(|(operation, _)| operation == "list_memories")
        .unwrap();
    assert_eq!(list.1["review_due_by"], 100);
    assert_eq!(list.1["is_verified"], false);
    let search = requests
        .iter()
        .find(|(operation, _)| operation == "search_memories")
        .unwrap();
    assert!(search.1["review_due_by"].as_i64().unwrap() > 0);
    assert_eq!(search.1["is_verified"], false);

    for operation in ["create_memories", "update_memories", "delete_memories"] {
        let request = requests.iter().find(|(name, _)| name == operation).unwrap();
        assert_eq!(request.1["request_id"], id);
    }

    let receipt = requests
        .iter()
        .find(|(operation, _)| operation == "get_mutation_receipts")
        .unwrap();
    assert_eq!(receipt.1["request_ids"], json!([id]));
    server.abort();
}

#[test]
fn cli_review_cutoffs_are_mutually_exclusive() {
    let output = Command::new(env!("CARGO_BIN_EXE_semmem"))
        .args(["list", "--due-for-review", "--review-due-by", "100"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}
