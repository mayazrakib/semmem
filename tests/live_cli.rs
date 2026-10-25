#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use semantic_memory::server::local_router;
use semantic_memory::service::Service;
use serde_json::Value;

async fn run_cli(socket: &Path, editor: &Path, arguments: &[&str]) -> anyhow::Result<Value> {
    let socket = socket.to_path_buf();
    let editor = editor.to_path_buf();
    let arguments: Vec<String> = arguments.iter().map(|value| value.to_string()).collect();
    let output = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_semantic-memory"))
            .arg("--socket")
            .arg(socket)
            .arg("--json")
            .args(arguments)
            .env("VISUAL", format!("'{}'", editor.display()))
            .output()
    })
    .await??;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(serde_json::from_slice(&output.stdout)?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_edits_and_searches_the_actual_sqlite_service() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let service = Service::open(root.path().join("data"))?;
    let socket = root.path().join("service.sock");
    let listener = tokio::net::UnixListener::bind(&socket)?;
    let server = tokio::spawn(async move { axum::serve(listener, local_router(service)).await });
    let editor = root.path().join("markdown editor");
    fs::write(
        &editor,
        "#!/bin/sh\nprintf '# Research\\n\\nStudy the algorithm café.\\n' > \"$1\"\n",
    )?;
    fs::set_permissions(&editor, fs::Permissions::from_mode(0o700))?;
    run_cli(
        &socket,
        &editor,
        &["taxonomy", "tags", "create", "research", "Research"],
    )
    .await?;
    run_cli(
        &socket,
        &editor,
        &["taxonomy", "tags", "create", "travel", "Travel"],
    )
    .await?;
    let first_tags = run_cli(
        &socket,
        &editor,
        &["taxonomy", "tags", "list", "--limit", "1"],
    )
    .await?;
    assert_eq!(first_tags["items"][0]["key"], "research");
    let next_cursor = first_tags["next_cursor"].as_str().unwrap();
    let next_tags = run_cli(
        &socket,
        &editor,
        &[
            "taxonomy",
            "tags",
            "list",
            "--limit",
            "1",
            "--cursor",
            next_cursor,
        ],
    )
    .await?;
    assert_eq!(next_tags["items"][0]["key"], "travel");
    let created = run_cli(
        &socket,
        &editor,
        &[
            "add", "research", "--title", "Research", "--tag", "research",
        ],
    )
    .await?;
    assert_eq!(created["memories"][0]["system_version"], 1);
    let found = run_cli(
        &socket,
        &editor,
        &["search", "algoritm", "--tag", "research"],
    )
    .await?;
    assert_eq!(found["hits"][0]["key"], "research");
    fs::write(
        &editor,
        "#!/bin/sh\nprintf '# Revision\\n\\nStudy the revised algorithm.\\n' > \"$1\"\n",
    )?;
    let edited = run_cli(
        &socket,
        &editor,
        &["edit", "research", "--title", "Revised research"],
    )
    .await?;
    assert_eq!(edited["memories"][0]["system_version"], 2);
    let shown = run_cli(&socket, &editor, &["show", "research"]).await?;
    assert_eq!(shown["memories"][0]["title"], "Revised research");
    assert_eq!(
        shown["memories"][0]["body"],
        "# Revision\n\nStudy the revised algorithm.\n"
    );
    let original = run_cli(&socket, &editor, &["show", "research", "--version", "1"]).await?;
    assert_eq!(
        original["memories"][0]["body"],
        "# Research\n\nStudy the algorithm café.\n"
    );
    run_cli(
        &socket,
        &editor,
        &["delete", "research", "--expected-version", "2"],
    )
    .await?;
    let deleted_search = run_cli(&socket, &editor, &["search", "algorithm"]).await?;
    assert!(deleted_search["hits"].as_array().unwrap().is_empty());
    let restored = run_cli(
        &socket,
        &editor,
        &["restore", "research", "--expected-version", "3"],
    )
    .await?;
    assert_eq!(restored["memories"][0]["system_version"], 4);
    run_cli(
        &socket,
        &editor,
        &["delete", "research", "--expected-version", "4"],
    )
    .await?;
    let purged = run_cli(
        &socket,
        &editor,
        &["purge", "research", "--expected-version", "5", "--confirm"],
    )
    .await?;
    assert_eq!(purged["keys"][0], "research");
    server.abort();

    Ok(())
}
