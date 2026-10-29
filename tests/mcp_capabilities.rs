use axum::{Json, Router, routing::post};
use semmem::cli::LocalClient;
use semmem::service::Service;
use serde_json::{Value, json};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capability_check_uses_running_mcp_without_appending_events() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path().join("data"))?;
    let socket = directory.path().join("service.sock");
    let listener = tokio::net::UnixListener::bind(&socket)?;
    let router = semmem::server::local_router(service);
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let client = LocalClient::connect(&socket)?;
    let original_commits = client.request("list_commits", json!({})).await?;
    let command_socket = socket.clone();
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new(env!("CARGO_BIN_EXE_semmem"))
            .arg("--socket")
            .arg(command_socket)
            .arg("check_mcp")
            .output()
    }).await??;
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let result: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(result["server_info"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(result["ready"], true);
    let logs = client.request("list_logs", json!({})).await?;
    assert_eq!(logs["events"], json!([]));
    let commits = client.request("list_commits", json!({})).await?;
    assert_eq!(commits, original_commits);
    server.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capability_check_reports_stale_catalog_and_follows_pages() -> anyhow::Result<()> {
    for is_current in [false, true] {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("service.sock");
        let listener = tokio::net::UnixListener::bind(&socket)?;
        let router = Router::new().route("/mcp", post(move |Json(request): Json<Value>| async move {
            let result = match request["method"].as_str().unwrap() {
                "initialize" => json!({"protocolVersion": "2025-11-25", "serverInfo": {"name": "semmem", "version": "fixture"}}),
                "tools/list" if request["params"]["cursor"].is_null() => {
                    json!({"tools": [{"name": "get_logs"}, {"name": "list_logs"}], "nextCursor": "second"})
                }
                "tools/list" => {
                    assert_eq!(request["params"]["cursor"], "second");

                    if is_current {
                        json!({"tools": [{"name": "append_logs"}]})
                    } else {
                        json!({"tools": []})
                    }
                }
                method => panic!("Unexpected MCP method: {method}"),
            };

            Json(json!({"jsonrpc": "2.0", "id": request["id"], "result": result}))
        }));
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let result = LocalClient::connect(&socket)?.check_mcp().await;

        if is_current {
            assert_eq!(result?["ready"], true);
        } else {
            let message = result.unwrap_err().to_string();
            assert!(message.contains("missing append_logs"));
            assert!(message.contains("not an authentication denial"));
        }

        server.abort();
    }

    Ok(())
}
