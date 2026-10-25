use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use semantic_memory::mcp::MemoryMcp;
use semantic_memory::service::Service;
use serde_json::{Value, json};

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
    let tools = client.list_tools(None).await?.tools;

    for name in [
        "search_memories",
        "list_memories",
        "get_memories",
        "create_memories",
        "update_memories",
        "delete_memories",
        "restore_memories",
        "purge_memories",
        "list_memory_revisions",
        "create_kind",
        "update_tag",
        "delete_relation_type",
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
    let created = invoke("create_memories", json!({"memories": [{"key": "reading", "title": "Reading", "body": "Remember the beautiful library.", "actor": "untrusted-actor"}]})).await?;
    assert_ne!(created.is_error, Some(true));
    assert_eq!(
        created.structured_content.as_ref().unwrap()["memories"][0]["actor"],
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
    let updated = invoke("update_memories", json!({"memories": [{"key": "reading", "expected_version": 1, "body": "Remember the enormous library."}]})).await?;
    assert_ne!(updated.is_error, Some(true));
    let conflict = invoke(
        "update_memories",
        json!({"memories": [{"key": "reading", "expected_version": 1, "body": "Stale content."}]}),
    )
    .await?;
    assert_eq!(conflict.is_error, Some(true));
    assert_eq!(
        conflict.structured_content.as_ref().unwrap()["code"],
        "version_conflict"
    );
    let history = invoke("list_memory_revisions", json!({"key": "reading"})).await?;
    assert_eq!(
        history.structured_content.as_ref().unwrap()["memories"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let malformed = invoke("create_memories", json!({"memories": [{"key": "broken"}]})).await;
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
