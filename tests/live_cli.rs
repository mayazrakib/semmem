#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use semmem::server::local_router;
use semmem::service::Service;
use serde_json::Value;

async fn run_cli(
    socket: &Path,
    editor: &Path,
    arguments: &[&str],
) -> anyhow::Result<Value> {
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
    editor: &Path,
    arguments: &[&str],
    input: Option<&str>,
) -> anyhow::Result<Value> {
    let socket = socket.to_path_buf();
    let editor = editor.to_path_buf();
    let mut arguments: Vec<String> = arguments.iter().map(|value| value.to_string()).collect();

    let is_write = arguments.first().is_some_and(|name| ["add", "edit", "delete"].contains(&name.as_str()))
        || (arguments.first().is_some_and(|name| name == "taxonomy") && arguments.iter().any(|value| ["create", "update", "delete"].contains(&value.as_str())));
    if is_write && !arguments.iter().any(|value| value == "--message") {
        arguments.extend(["--message".into(), "Test CLI change".into()]);
    }

    let input = input.map(str::to_owned);
    let output = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_semmem"));
        command
            .arg("--socket")
            .arg(socket)
            .arg("--json")
            .args(arguments)
            .env("VISUAL", format!("'{}'", editor.display()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let mut child = command.spawn()?;

        if let Some(input) = input {
            child.stdin.take().unwrap().write_all(input.as_bytes())?;
        }

        child.wait_with_output()
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
async fn interactive_add_persists_prompted_metadata_and_body_without_frontmatter()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let service = Service::open(root.path().join("data"))?;
    service.dispatch(
        "create_memories",
        serde_json::json!({"commit_summary":"Test change", "memories": [{"key": "target", "body": "Related target."}]}),
        "human",
    )?;
    let socket = root.path().join("service.sock");
    let listener = tokio::net::UnixListener::bind(&socket)?;
    let server = tokio::spawn(async move { axum::serve(listener, local_router(service)).await });
    let editor = root.path().join("body editor");
    fs::write(
        &editor,
        "#!/bin/sh\n[ ! -s \"$1\" ] || exit 23\nprintf '# Prompted body\\n\\nExact café.\\n' > \"$1\"\n",
    )?;
    fs::set_permissions(&editor, fs::Permissions::from_mode(0o700))?;
    let input = "Prompted title\ndecision\nManual summary\n1\nproject\n100\n200\n300\nresearch\n\nsupports=target\n\n";
    let created = run_cli_with_input(
        &socket,
        &editor,
        &["add", "prompted"],
        Some(input),
    ).await?;
    assert!(created["memories"][0]["system_token"].is_string());
    let shown = run_cli(
        &socket,
        &editor,
        &["show", "prompted"],
    ).await?;
    let record = &shown["memories"][0];
    assert_eq!(record["title"], "Prompted title");
    assert_eq!(record["type"], "decision");
    assert_eq!(record["summary"], "Manual summary");
    assert_eq!(record["basis"], "stated");
    assert_eq!(record["scope"], "project");
    assert_eq!(record["observed_at"], 100);
    assert_eq!(record["verified_at"], 200);
    assert_eq!(record["review_after"], 300);
    assert_eq!(record["tags"], serde_json::json!(["research"]));
    assert_eq!(
        record["references"],
        serde_json::json!([{"reference_type": "supports", "target_key": "target"}])
    );
    assert_eq!(record["body"], "# Prompted body\n\nExact café.\n");
    let search = run_cli(
        &socket,
        &editor,
        &["search", "summary", "--scope", "project"],
    )
    .await?;
    assert_eq!(search["hits"][0]["key"], "prompted");
    server.abort();

    Ok(())
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
    assert!(created["memories"][0]["system_token"].is_string());
    let found = run_cli(
        &socket,
        &editor,
        &["search", "algoritm", "--tag", "research"],
    )
    .await?;
    assert_eq!(found["hits"][0]["key"], "research");
    fs::write(
        &editor,
        r#"#!/bin/sh
sed -i -e 's/^title: Research$/title: Revised research/' -e 's/# Research/# Revision/' -e 's/Study the algorithm café./Study the revised algorithm./' "$1"
"#,
    )?;
    let edited = run_cli(
        &socket,
        &editor,
        &["edit", "research"],
    ).await?;
    assert_ne!(edited["memories"][0]["system_token"], created["memories"][0]["system_token"]);
    let shown = run_cli(
        &socket,
        &editor,
        &["show", "research"],
    ).await?;
    assert_eq!(shown["memories"][0]["title"], "Revised research");
    assert_eq!(
        shown["memories"][0]["body"],
        "# Revision\n\nStudy the revised algorithm.\n"
    );
    run_cli(
        &socket,
        &editor,
        &["delete", "research", "--expected-token", edited["memories"][0]["system_token"].as_str().unwrap()],
    ).await?;
    let deleted_search = run_cli(&socket, &editor, &["search", "algorithm"]).await?;
    assert!(deleted_search["hits"].as_array().unwrap().is_empty());
    let commits = run_cli(&socket, &editor, &["commits"]).await?;
    assert!(!commits["commits"].as_array().unwrap().is_empty());
    server.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_roundtrips_all_metadata_and_staged_views() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let service = Service::open(root.path().join("data"))?;
    let socket = root.path().join("service.sock");
    let listener = tokio::net::UnixListener::bind(&socket)?;
    let server = tokio::spawn(async move { axum::serve(listener, local_router(service)).await });
    let editor = root.path().join("editor");
    fs::write(
        &editor,
        "#!/bin/sh\nprintf '# Exact body\\n\\nUnicode: café ♟.\\n' > \"$1\"\n",
    )?;
    fs::set_permissions(&editor, fs::Permissions::from_mode(0o700))?;
    let created = run_cli(
        &socket,
        &editor,
        &[
            "add",
            "metadata",
            "--summary",
            "Durable knowledge summary",
            "--basis",
            "stated",
            "--scope",
            "project",
            "--observed-at",
            "100",
            "--verified-at",
            "200",
            "--review-after",
            "300",
        ],
    )
    .await?;
    assert_eq!(
        created["memories"][0]["summary"],
        "Durable knowledge summary"
    );
    let filtered = run_cli(
        &socket,
        &editor,
        &[
            "search",
            "knowledge",
            "--scope",
            "project",
            "--basis",
            "stated",
        ],
    )
    .await?;
    assert_eq!(filtered["hits"][0]["key"], "metadata");
    let compact = run_cli(
        &socket,
        &editor,
        &["show", "metadata", "--view", "summary"],
    ).await?;
    assert!(compact["memories"][0].get("body").is_some());
    assert_eq!(compact["memories"][0]["review_after"], 300);
    let full = run_cli(
        &socket,
        &editor,
        &["show", "metadata"],
    ).await?;
    assert_eq!(
        full["memories"][0]["body"],
        "# Exact body\n\nUnicode: café ♟.\n"
    );
    fs::write(
        &editor,
        r#"#!/bin/sh
sed -i -e 's/^summary: .*/summary: null/' -e 's/^basis: .*/basis: null/' -e 's/^scope: .*/scope: null/' -e 's/^observed_at: .*/observed_at: null/' -e 's/^verified_at: .*/verified_at: null/' -e 's/^review_after: .*/review_after: null/' "$1"
"#,
    )?;
    let edited = run_cli(
        &socket,
        &editor,
        &["edit", "metadata"],
    ).await?;
    for field in [
        "summary",
        "basis",
        "scope",
        "observed_at",
        "verified_at",
        "review_after",
    ] {
        assert!(edited["memories"][0][field].is_null(), "{field}");
    }
    let listed = run_cli(
        &socket,
        &editor,
        &["list", "--scope", "project"],
    ).await?;
    assert!(listed["memories"].as_array().unwrap().is_empty());
    server.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_audit_messages_are_separate_from_memory_summaries() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let service = Service::open(root.path().join("data"))?;
    let socket = root.path().join("service.sock");
    let listener = tokio::net::UnixListener::bind(&socket)?;
    let server = tokio::spawn(async move { axum::serve(listener, local_router(service)).await });
    let editor = root.path().join("editor");
    fs::write(&editor, r#"#!/bin/sh
printf 'currentneedle body' > "$1"
"#)?;
    fs::set_permissions(&editor, fs::Permissions::from_mode(0o700))?;
    let created = run_cli(&socket, &editor, &["add", "audit", "--summary", "Memory summary", "--message", "Create a current memory"]).await?;
    let id = created["commit_id"].as_i64().unwrap().to_string();
    let audit = run_cli(&socket, &editor, &["commit", &id]).await?;
    assert_eq!(audit["commits"][0]["summary"], "Create a current memory");
    assert_eq!(created["memories"][0]["summary"], "Memory summary");
    assert!(created["memories"][0].get("author").is_none());
    server.abort();
    Ok(())
}
