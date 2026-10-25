#![cfg(unix)]

use std::fs::{self, File};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use reqwest::Client;
#[cfg(feature = "fault-injection")]
use semmem::storage::Store;
use serde_json::{Value, json};

struct ServerProcess {
    child: Child,
    log: PathBuf,
}

impl ServerProcess {
    fn start(directory: &Path, socket: &Path, fault: Option<&str>, label: &str) -> Self {
        let log = directory.join(format!("{label}.log"));
        let output = File::create(&log).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_semmem"));
        command
            .arg("--socket")
            .arg(socket)
            .arg("run")
            .arg("--data-dir")
            .arg(directory)
            .env_remove("SEMMEM_FAILPOINT")
            .stdin(Stdio::null())
            .stdout(output.try_clone().unwrap())
            .stderr(output);

        if let Some(fault) = fault {
            command.env("SEMMEM_FAILPOINT", fault);
        }

        Self {
            child: command.spawn().unwrap(),
            log,
        }
    }

    fn logs(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    async fn wait_ready(&mut self, socket: &Path) -> Client {
        let client = Client::builder()
            .unix_socket(socket)
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let ready = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    panic!("Server exited before readiness: {status}. {}", self.logs());
                }

                if let Ok(response) = client.get("http://localhost/health").send().await
                    && response.status().is_success()
                {
                    break;
                }

                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(ready.is_ok(), "Server readiness timed out. {}", self.logs());

        client
    }

    async fn wait_exit(&mut self) -> ExitStatus {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    break status;
                }

                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("Server did not exit at its failpoint. {}", self.logs()))
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn invoke(client: &Client, operation: &str, arguments: Value) -> Value {
    let response = client
        .post(format!("http://localhost/api/{operation}"))
        .json(&arguments)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert!(status.is_success(), "{operation} failed: {status}. {body}");

    serde_json::from_str(&body).unwrap()
}

#[cfg(feature = "fault-injection")]
async fn assert_recovers_after_crash(fault: &str) {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("service.sock");
    let mut initial = ServerProcess::start(directory.path(), &socket, None, "initialize");
    initial.wait_ready(&socket).await;
    drop(initial);

    let mut interrupted =
        ServerProcess::start(directory.path(), &socket, Some(fault), "interrupted");
    let client = interrupted.wait_ready(&socket).await;
    let markdown =
        "# Durable memory\n\ncrashrecoveryneedle café 日本語 🦀\n\nExact trailing bytes.\n";
    let input = json!({
        "key": "crash-recovery",
        "title": "Durable memory",
        "body": markdown,
        "kind": "note",
        "tags": ["recovery"],
        "links": [],
        "summary": "Durable manual summary",
        "claim_type": "verified observation",
        "scope": "crash recovery",
        "observed_at": 1000,
        "verified_at": 2000,
        "review_after": 3000
    });
    let arguments = json!({"memories": [input]});
    let response = client
        .post("http://localhost/api/create_memories")
        .json(&arguments)
        .send()
        .await;
    if let Ok(response) = response {
        let status = response.status();
        let body = response.text().await.unwrap();
        panic!("{fault} unexpectedly returned {status}: {body}");
    }
    assert_eq!(
        interrupted.wait_exit().await.code(),
        Some(86),
        "Wrong exit at {fault}. {}",
        interrupted.logs()
    );
    drop(interrupted);
    drop(client);

    let persisted = Store::open_read_only(directory.path().join("memory.sqlite3")).unwrap();
    let committed = persisted.get_memory("crash-recovery", false).unwrap();
    assert_eq!(
        committed.body, markdown,
        "SQLite lost committed text at {fault}."
    );
    assert_eq!(committed.system_version, 1);
    assert_eq!(
        committed.metadata.summary.as_deref(),
        Some("Durable manual summary")
    );
    let integrity: String = persisted
        .connection()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    drop(persisted);

    let mut recovered = ServerProcess::start(directory.path(), &socket, None, "recovered");
    let client = recovered.wait_ready(&socket).await;
    let fetched = invoke(&client, "get_memories", json!({"keys": ["crash-recovery"]})).await;
    assert_eq!(fetched["memories"][0]["body"], markdown);
    assert_eq!(fetched["memories"][0]["system_version"], 1);
    assert_eq!(fetched["memories"][0]["tags"], json!(["recovery"]));
    assert_eq!(fetched["memories"][0]["summary"], "Durable manual summary");
    assert_eq!(fetched["memories"][0]["observed_at"], 1000);
    assert_eq!(fetched["memories"][0]["verified_at"], 2000);
    assert_eq!(fetched["memories"][0]["review_after"], 3000);

    let search = invoke(
        &client,
        "search_memories",
        json!({"query": "crashrecoveryneedle", "fuzzy": false}),
    )
    .await;
    assert_eq!(
        search["total"], 1,
        "Recovery duplicated or lost an index document at {fault}."
    );
    assert_eq!(search["hits"][0]["key"], "crash-recovery");
    assert_eq!(search["hits"][0]["version"], 1);

    let retried = invoke(&client, "create_memories", arguments).await;
    assert_eq!(retried["memories"][0]["system_version"], 1);
    let history = invoke(
        &client,
        "list_memory_revisions",
        json!({"key": "crash-recovery"}),
    )
    .await;
    assert_eq!(history["memories"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg(feature = "fault-injection")]
async fn recovers_after_sqlite_commit_before_index_publication() {
    assert_recovers_after_crash("after_sqlite_commit").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg(feature = "fault-injection")]
async fn recovers_after_index_commit_before_event_acknowledgment() {
    assert_recovers_after_crash("after_index_commit").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg(feature = "fault-injection")]
async fn recovers_before_client_acknowledgment_without_duplicate_retries() {
    assert_recovers_after_crash("before_acknowledgment").await;
}

async fn assert_rebuilds_search_index(is_corrupted: bool) {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("data");
    fs::create_dir(&data).unwrap();
    let socket = data.join("service.sock");
    let mut initial = ServerProcess::start(&data, &socket, None, "initialize");
    let client = initial.wait_ready(&socket).await;
    invoke(
        &client,
        "create_memories",
        json!({"memories": [{"key": "rebuild", "body": "rebuildneedle café 日本語\n"}]}),
    )
    .await;
    drop(client);
    drop(initial);
    assert!(data.join("search.identity").exists());

    if is_corrupted {
        fs::write(data.join("search/meta.json"), b"invalid metadata").unwrap();
    } else {
        fs::rename(data.join("search"), directory.path().join("search.saved")).unwrap();
    }

    let mut restarted = ServerProcess::start(&data, &socket, None, "restarted");
    let client = restarted.wait_ready(&socket).await;
    let found = invoke(
        &client,
        "search_memories",
        json!({"query": "rebuildneedle", "fuzzy": false}),
    )
    .await;
    assert_eq!(found["total"], 1);
    assert_eq!(found["hits"][0]["key"], "rebuild");
    assert_eq!(found["hits"][0]["version"], 1);
    let fetched = invoke(&client, "get_memories", json!({"keys": ["rebuild"]})).await;
    assert_eq!(
        fetched["memories"][0]["body"],
        "rebuildneedle café 日本語\n"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rebuilds_missing_index_when_identity_marker_survives() {
    assert_rebuilds_search_index(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rebuilds_corrupted_index_when_identity_marker_survives() {
    assert_rebuilds_search_index(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refuses_regular_file_socket_path_without_changing_its_contents() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("valuable.txt");
    let original = b"Existing user document.\n";
    fs::write(&socket, original).unwrap();
    let mut process = ServerProcess::start(directory.path(), &socket, None, "invalid-socket");
    let status = process.wait_exit().await;
    assert!(
        !status.success(),
        "Server unexpectedly replaced a regular file."
    );
    assert_eq!(fs::read(&socket).unwrap(), original);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refuses_live_socket_without_displacing_its_listener() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("existing.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let original_inode = fs::metadata(&socket).unwrap().ino();
    let mut process = ServerProcess::start(directory.path(), &socket, None, "occupied-socket");
    let status = process.wait_exit().await;
    assert!(
        !status.success(),
        "Server unexpectedly displaced a live socket."
    );
    assert_eq!(fs::metadata(&socket).unwrap().ino(), original_inode);
    assert!(std::os::unix::net::UnixStream::connect(&socket).is_ok());
    drop(listener);
}
