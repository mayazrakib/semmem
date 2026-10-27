use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use semmem::mcp::MemoryMcp;
use semmem::service::Service;
use serde_json::{Value, json};

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unix_socket_mcp_and_local_api_share_the_same_registry() -> anyhow::Result<()> {
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;

    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path().join("data"))?;
    let socket = directory.path().join("service.sock");
    let listener = tokio::net::UnixListener::bind(&socket)?;
    let router = semmem::server::local_router(service);
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let http = reqwest::Client::builder()
        .unix_socket(socket.as_path())
        .no_proxy()
        .build()?;
    let created: Value = http
        .post("http://localhost/api/create_memories")
        .json(&json!({"commit_summary":"Test change", "memories": [{
            "key": "local-api",
            "body": "# Shared registry\n\nunixregistryneedle exact Markdown.\n",
            "summary": "Shared local registry summary",
            "scope": "unix socket",
            "basis": "observed",
            "observed_at": 100,
            "verified_at": 200,
            "review_after": 300
        }]}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(created["memories"][0]["system_updated_by"], "human");
    let request_id = "00000000-0000-0000-0000-000000000009";
    let mut request = json!({"request_id":request_id,"commit_summary":"Test local receipts.","memories":[{"key":"local-receipt","body":"Original"}]});
    let local_receipt: Value = http.post("http://localhost/api/create_memories").json(&request).send().await?.error_for_status()?.json().await?;
    request["memories"][0]["body"] = json!("Changed");
    assert_eq!(http.post("http://localhost/api/create_memories").json(&request).send().await?.status(), 409);
    let looked_up: Value = http.post("http://localhost/api/get_mutation_receipts").json(&json!({"request_ids":[request_id]})).send().await?.error_for_status()?.json().await?;
    assert_eq!(looked_up["receipts"][0], local_receipt);
    let transport = StreamableHttpClientTransport::with_client(
        http.clone(),
        StreamableHttpClientTransportConfig::with_uri("http://localhost/mcp"),
    );
    let peer = ().serve(transport).await?;
    assert!(
        peer.list_tools(None)
            .await?
            .tools
            .iter()
            .any(|tool| tool.name == "search_memories")
    );
    let invoke = |name: &'static str, arguments: Value| {
        let peer = peer.peer().clone();

        async move {
            peer.call_tool(
                CallToolRequestParams::new(name)
                    .with_arguments(arguments.as_object().unwrap().clone()),
            )
            .await
        }
    };
    let other_actor = invoke("get_mutation_receipts", json!({"request_ids":[request_id]})).await?;
    assert_eq!(other_actor.is_error, Some(true));
    assert_eq!(other_actor.structured_content.unwrap()["code"], "not_found");
    let found = invoke(
        "search_memories",
        json!({"query": "unixregistryneedle", "fuzzy": false}),
    )
    .await?;
    assert_ne!(found.is_error, Some(true));
    assert_eq!(
        found.structured_content.as_ref().unwrap()["hits"][0]["key"],
        "local-api"
    );
    let compact = invoke(
        "get_memories",
        json!({"keys": ["local-api"], "view": "summary"}),
    )
    .await?;
    let compact = &compact.structured_content.as_ref().unwrap()["memories"][0];
    assert_eq!(compact["summary"], "Shared local registry summary");
    assert_eq!(compact["review_after"], 300);
    assert!(compact.get("body").is_some());
    let full = invoke("get_memories", json!({"keys": ["local-api"]})).await?;
    assert_eq!(
        full.structured_content.as_ref().unwrap()["memories"][0]["body"],
        "# Shared registry\n\nunixregistryneedle exact Markdown.\n"
    );
    let created = invoke("create_memories", json!({"commit_summary":"Test change", "memories": [{"key": "mcp-created", "body": "Written through private MCP."}]})).await?;
    assert_ne!(created.is_error, Some(true));
    let full: Value = http
        .post("http://localhost/api/get_memories")
        .json(&json!({"keys": ["mcp-created"]}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(full["memories"][0]["body"], "Written through private MCP.");
    assert_eq!(full["memories"][0]["system_updated_by"], "ai client");
    assert_eq!(
        http.get("http://localhost/health").send().await?.status(),
        200
    );
    peer.cancel().await?;
    server.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_roundtrip_supports_discovery_writes_fuzzy_reads_and_conflicts() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path())?;
    let (server_transport, client_transport) = tokio::io::duplex(65536);
    let server = tokio::spawn(async move {
        MemoryMcp::new(service, "mcp-test")
            .serve(server_transport)
            .await?
            .waiting()
            .await?;

        anyhow::Ok(())
    });
    let client = ().serve(client_transport).await?;
    assert_eq!(
        client
            .peer_info()
            .unwrap()
            .server_info
            .as_ref()
            .unwrap()
            .name,
        "semmem"
    );
    let tools = client.list_tools(None).await?.tools;

    for name in [
        "search_memories",
        "list_memories",
        "get_memories",
        "create_memories",
        "update_memories",
        "delete_memories",
        "list_commits",
        "get_commits",
        "get_mutation_receipts",
        "create_type",
        "update_tag",
        "delete_reference_type",
    ] {
        assert!(
            tools.iter().any(|tool| tool.name == name),
            "Missing tool {name}."
        );
    }

    let invoke = |name: &'static str, arguments: Value| {
        let peer = client.peer().clone();

        async move {
            peer.call_tool(
                CallToolRequestParams::new(name)
                    .with_arguments(arguments.as_object().unwrap().clone()),
            )
            .await
        }
    };
    let request_id = "00000000-0000-0000-0000-000000000001";
    let arguments = json!({"request_id":request_id,"commit_summary":"Create a review fixture.","memories":[{"key":"review","body":"reviewneedle","verified_at":null,"review_after":100}]});
    let receipt = invoke("create_memories", arguments.clone()).await?;
    assert_ne!(receipt.is_error, Some(true));
    let receipt = receipt.structured_content.unwrap();
    assert!(receipt["memories"][0].get("body").is_none());
    assert_eq!(
        invoke("create_memories", arguments)
            .await?
            .structured_content
            .unwrap(),
        receipt
    );
    assert_eq!(
        invoke("get_mutation_receipts", json!({"request_ids":[request_id]}))
            .await?
            .structured_content
            .unwrap()["receipts"][0],
        receipt
    );
    let reviewed = invoke(
        "search_memories",
        json!({"query":"reviewneedle","review_due_by":100,"is_verified":false}),
    )
    .await?;
    assert_eq!(reviewed.structured_content.unwrap()["total"], 1);
    let excluded = invoke(
        "list_memories",
        json!({"review_due_by":99,"is_verified":false}),
    )
    .await?;
    assert!(
        excluded.structured_content.unwrap()["memories"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let created = invoke("create_memories", json!({"commit_summary":"Test change", "memories": [{"key": "reading", "title": "Reading", "body": "Remember the beautiful library.", "summary": "Library knowledge", "scope": "personal", "basis": "observed", "observed_at": 100, "verified_at": 200, "review_after": 300}]})).await?;
    assert_ne!(created.is_error, Some(true));
    assert_eq!(
        created.structured_content.as_ref().unwrap()["memories"][0]["system_updated_by"],
        "mcp-test"
    );
    let found = invoke(
        "search_memories",
        json!({"query": "libary", "fuzzy": true, "limit": 10}),
    )
    .await?;
    assert_ne!(found.is_error, Some(true));
    assert_eq!(
        found.structured_content.as_ref().unwrap()["hits"][0]["key"],
        "reading"
    );
    let compact = invoke(
        "get_memories",
        json!({"keys": ["reading"], "view": "summary"}),
    )
    .await?;
    let compact_record = &compact.structured_content.as_ref().unwrap()["memories"][0];
    assert!(compact_record.get("body").is_some());
    assert_eq!(compact_record["summary"], "Library knowledge");
    assert_eq!(compact_record["review_after"], 300);
    let full = invoke("get_memories", json!({"keys": ["reading"]})).await?;
    assert_eq!(
        full.structured_content.as_ref().unwrap()["memories"][0]["body"],
        "Remember the beautiful library."
    );
    let scoped = invoke(
        "search_memories",
        json!({"query": "knowledge", "scope": "personal", "basis": "observed"}),
    )
    .await?;
    assert_eq!(
        scoped.structured_content.as_ref().unwrap()["hits"][0]["key"],
        "reading"
    );
    let token = created.structured_content.as_ref().unwrap()["memories"][0]["system_token"].clone();
    let updated = invoke("update_memories", json!({"commit_summary":"Test change", "memories": [{"key": "reading", "expected_token": token, "body": "Remember the enormous library.", "summary": null, "review_after": null}]})).await?;
    assert_ne!(updated.is_error, Some(true));
    assert!(updated.structured_content.as_ref().unwrap()["memories"][0]["summary"].is_null());
    assert_eq!(
        updated.structured_content.as_ref().unwrap()["memories"][0]["verified_at"],
        200
    );
    let conflict = invoke(
        "update_memories",
        json!({"commit_summary":"Test change", "memories": [{"key": "reading", "expected_token": token, "body": "Stale content."}]}),
    )
    .await?;
    assert_eq!(conflict.is_error, Some(true));
    assert_eq!(
        conflict.structured_content.as_ref().unwrap()["code"],
        "token_conflict"
    );
    let current = invoke("search_memories", json!({"query": "library"})).await?;
    let hits = current.structured_content.as_ref().unwrap()["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["body"], "Remember the enormous library.");
    assert!(hits[0].get("author").is_none());
    assert!(hits[0].get("system_version").is_none());
    let audit = invoke("get_commits", json!({"ids": [updated.structured_content.as_ref().unwrap()["commit_id"]]})).await?;
    assert_eq!(audit.structured_content.as_ref().unwrap()["commits"][0]["summary"], "Test change");
    for retired in ["history", "as_of", "include_deleted"] {
        let mut arguments = json!({"query": "library"});
        arguments[retired] = json!(true);
        let rejected = invoke("search_memories", arguments).await;
        assert!(rejected.is_err() || rejected.as_ref().is_ok_and(|result| result.is_error == Some(true)));
    }
    for retired in ["restore_memories", "purge_memories", "list_memory_revisions", "list_authors", "register_author", "deregister_author"] {
        assert!(!tools.iter().any(|tool| tool.name == retired));
    }
    let assumed = invoke("create_memories", json!({"commit_summary":"Test change", "memories": [{"key": "assumption", "body": "Assumptionneedle complete body.", "basis": "assumed"}]})).await?;
    assert_ne!(assumed.is_error, Some(true));
    let assumed = invoke(
        "search_memories",
        json!({"query": "assumptionneedle", "basis": "assumed"}),
    )
    .await?;
    let hit = &assumed.structured_content.as_ref().unwrap()["hits"][0];
    assert_eq!(hit["system_created_by"], "mcp-test");
    assert_eq!(hit["body"], "Assumptionneedle complete body.");

    for legacy in ["author", "actor", "kind", "claim_type", "links"] {
        let mut memory = json!({"key": "legacy", "body": "Body"});
        memory[legacy] = json!("legacy");
        let rejected = invoke("create_memories", json!({"commit_summary":"Test change", "memories": [memory]})).await;
        assert!(
            rejected.is_err()
                || rejected
                    .as_ref()
                    .is_ok_and(|result| result.is_error == Some(true)),
            "Accepted retired field {legacy}"
        );
    }

    let malformed = invoke("create_memories", json!({"commit_summary":"Test change", "memories": [{"key": "broken"}]})).await;
    assert!(
        malformed.is_err()
            || malformed
                .as_ref()
                .is_ok_and(|result| result.is_error == Some(true))
    );
    client.cancel().await?;
    server.await??;

    Ok(())
}
