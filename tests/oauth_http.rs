use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{Json, Router, routing::get};
use base64::Engine;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rsa::pkcs8::EncodePrivateKey;
use rsa::traits::PublicKeyParts;
use semmem::auth::OAuth;
use semmem::server::remote_router;
use semmem::service::Service;
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oauth_issuer_tokens_scopes_and_http_discovery_are_enforced() -> anyhow::Result<()> {
    let key = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048)?;
    let pem = key.to_pkcs8_pem(Default::default())?;
    let encoder = EncodingKey::from_rsa_pem(pem.as_bytes())?;
    let public = key.to_public_key();
    let base64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let keys = json!({"keys":[{"kty":"RSA","kid":"test-key","use":"sig","alg":"RS256","n":base64.encode(public.n().to_bytes_be()),"e":base64.encode(public.e().to_bytes_be())}]});
    let issuer_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let issuer_url = format!("http://{}", issuer_listener.local_addr()?);
    let discovery = json!({"issuer":issuer_url,"jwks_uri":format!("{issuer_url}/jwks")});
    let issuer_router = Router::new()
        .route(
            "/.well-known/openid-configuration",
            get(move || {
                let value = discovery.clone();
                async move { Json(value) }
            }),
        )
        .route(
            "/jwks",
            get(move || {
                let value = keys.clone();
                async move { Json(value) }
            }),
        );
    let issuer_task =
        tokio::spawn(async move { axum::serve(issuer_listener, issuer_router).await });
    let directory = tempfile::tempdir()?;
    let service = Service::open(directory.path())?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = format!("http://{}", listener.local_addr()?);
    let audience = format!("{address}/mcp");
    let auth = Arc::new(
        OAuth::discover_with_http(
            issuer_url.clone(),
            audience.clone(),
            address.clone(),
            true,
        )
            .await?,
    );
    let application = remote_router(
        service,
        Arc::clone(&auth),
        &address,
        CancellationToken::new(),
    )?;
    let application_task = tokio::spawn(async move { axum::serve(listener, application).await });
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".into());
    let make_token = |scope: &str, issuer: &str, audience: &str, expiration: u64| {
        encode(
            &header,
            &json!({"sub":"test-human","iss":issuer,"aud":audience,"exp":expiration,"scope":scope}),
            &encoder,
        )
    };
    let token = make_token(
        "memory:read",
        &issuer_url,
        &audience,
        now + 300,
    )?;
    assert_eq!(
        auth.validate(&token, "memory:read").await.unwrap().sub,
        "test-human"
    );
    assert!(auth.validate(&token, "memory:purge").await.is_err());
    assert!(
        auth.validate(
            &make_token("memory:read", "wrong", &audience, now + 300)?,
            "memory:read"
        )
        .await
        .is_err()
    );
    assert!(
        auth.validate(
            &make_token("memory:read", &issuer_url, "wrong", now + 300)?,
            "memory:read"
        )
        .await
        .is_err()
    );
    assert!(
        auth.validate(
            &make_token("memory:read", &issuer_url, &audience, now - 30)?,
            "memory:read"
        )
        .await
        .is_err()
    );
    let client = reqwest::Client::new();
    let metadata: serde_json::Value = client
        .get(format!("{address}/.well-known/oauth-protected-resource"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(metadata["authorization_servers"][0], issuer_url);
    let denied = client
        .post(format!("{address}/mcp"))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
        .send()
        .await?;
    assert_eq!(denied.status(), 401);
    assert!(
        denied
            .headers()
            .get("www-authenticate")
            .unwrap()
            .to_str()?
            .contains("resource_metadata")
    );
    let denied_delete = client.post(format!("{address}/mcp")).bearer_auth(&token).json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"delete_memories","arguments":{"memories":[]}}})).send().await?;
    assert_eq!(denied_delete.status(), 403);
    let token = make_token(
        "memory:read memory:write memory:purge",
        &issuer_url,
        &audience,
        now + 300,
    )?;
    let transport = StreamableHttpClientTransport::with_client(
        client.clone(),
        StreamableHttpClientTransportConfig::with_uri(format!("{address}/mcp")).auth_header(token),
    );
    let peer = ().serve(transport).await?;
    assert!(
        peer.list_tools(None)
            .await?
            .tools
            .iter()
            .any(|tool| tool.name == "create_memories")
    );
    let created = peer
        .call_tool(
            CallToolRequestParams::new("create_memories").with_arguments(
                json!({"commit_summary":"Test change", "memories":[{"key":"network","body":"Network MCP memory"}]})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await?;
    assert_ne!(created.is_error, Some(true));
    let record = &created.structured_content.as_ref().unwrap()["memories"][0];
    assert!(record.get("author").is_none());
    assert_eq!(record["system_created_by"], "test-human");
    assert_eq!(record["system_updated_by"], "test-human");
    let large_body = format!(
        "largeauthenticatedneedle\n\n{}",
        " ".repeat(5 * 1024 * 1024)
    );
    let created_large = peer
        .call_tool(
            CallToolRequestParams::new("create_memories").with_arguments(
                json!({"commit_summary":"Test change", "memories":[{"key":"network-large", "body": large_body, "summary":"Authenticated manual summary", "scope":"network"}]})
                    .as_object().unwrap().clone(),
            ),
        ).await?;
    assert_ne!(created_large.is_error, Some(true));
    let compact = peer
        .call_tool(
            CallToolRequestParams::new("get_memories").with_arguments(
                json!({"keys":["network-large"], "view":"summary"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await?;
    let compact = compact.structured_content.unwrap();
    assert_eq!(
        compact["memories"][0]["summary"],
        "Authenticated manual summary"
    );
    assert!(compact["memories"][0].get("body").is_some());
    let found = peer
        .call_tool(
            CallToolRequestParams::new("search_memories")
                .with_arguments(json!({"query":"network"}).as_object().unwrap().clone()),
        )
        .await?;
    assert_eq!(
        found.structured_content.unwrap()["hits"][0]["key"],
        "network"
    );
    peer.cancel().await?;
    application_task.abort();
    issuer_task.abort();

    Ok(())
}
