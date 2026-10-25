#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use axum::extract::{self, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

#[test]
fn purge_requires_confirmation_and_an_expected_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_semantic-memory"))
        .args(["purge", "example", "--expected-version", "1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--confirm"));
    let output = Command::new(env!("CARGO_BIN_EXE_semantic-memory"))
        .args(["purge", "example", "--confirm"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--expected-version"));
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
            Json(json!({"memories": [{
                "id": 1, "key": "example", "title": "Example", "body": "Original body\n",
                "kind": "note", "tags": [], "links": [], "actor": "human",
                "system_version": 4, "system_created_at": 1, "system_updated_at": 2,
                "system_deleted_at": null
            }]})),
        );
    }

    if state.has_conflict {
        return (
            StatusCode::CONFLICT,
            Json(json!({"code": "version_conflict", "message": "A newer revision exists."})),
        );
    }

    (StatusCode::OK, Json(json!({"saved": arguments})))
}

async fn start_service(socket: &Path, state: ServerState) -> tokio::task::JoinHandle<()> {
    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    let router = Router::new()
        .route("/api/{operation}", post(handle_request))
        .with_state(state);

    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    })
}

fn write_editor(root: &Path, script: &str) -> String {
    let path = root.join("editor with spaces");
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();

    format!("'{}'", path.display())
}

async fn run_cli(socket: &Path, editor: String, arguments: &[&str]) -> Output {
    let socket = socket.to_path_buf();
    let arguments: Vec<String> = arguments.iter().map(|value| value.to_string()).collect();

    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_semantic-memory"))
            .arg("--socket")
            .arg(socket)
            .arg("--json")
            .args(arguments)
            .env("VISUAL", editor)
            .env("EDITOR", "/bin/false")
            .output()
            .unwrap()
    })
    .await
    .unwrap()
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
    let output = run_cli(&socket, editor, &["add", "example", "--title", "Example"]).await;
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
async fn unchanged_editor_does_not_create_a_revision() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("service.sock");
    let state = ServerState::default();
    let server = start_service(&socket, state.clone()).await;
    let editor = write_editor(root.path(), "exit 0");
    let output = run_cli(&socket, editor, &["edit", "example"]).await;
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
async fn conflict_preserves_the_edited_draft_and_sends_expected_version() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("service.sock");
    let state = ServerState {
        has_conflict: true,
        ..ServerState::default()
    };
    let server = start_service(&socket, state.clone()).await;
    let editor = write_editor(root.path(), "printf 'Unsaved correction\\n' > \"$1\"");
    let output = run_cli(&socket, editor, &["edit", "example"]).await;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("version_conflict"), "{stderr}");
    let path = stderr
        .split("Draft preserved at ")
        .nth(1)
        .unwrap()
        .trim()
        .trim_end_matches('.');
    let draft = Path::new(path);
    assert_eq!(fs::read_to_string(draft).unwrap(), "Unsaved correction\n");
    let requests = state.requests.lock().unwrap();
    assert_eq!(requests[1].0, "update_memory");
    assert_eq!(requests[1].1["expected_version"], 4);
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
    let output = run_cli(&socket, editor, &["add", "example"]).await;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Editor exited"), "{stderr}");
    let path = stderr
        .split("Draft preserved at ")
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
