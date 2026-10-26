use std::net::SocketAddr;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::{Value, json};
use tokio::net::{TcpListener, UnixListener};
use tokio_util::sync::CancellationToken;

use crate::auth::{OAuth, authorize};
use crate::mcp::MemoryMcp;
use crate::models::AppError;
use crate::service::Service;

pub async fn run(
    data_dir: PathBuf,
    socket: PathBuf,
    listen: SocketAddr,
    issuer: Option<String>,
    audience: Option<String>,
    public_url: Option<String>,
) -> anyhow::Result<()> {
    let service = Service::open(&data_dir)?;
    std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700))?;
    let local = local_router(Arc::clone(&service));
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if let Ok(metadata) = socket.symlink_metadata() {
        anyhow::ensure!(
            metadata.file_type().is_socket(),
            "Refusing to replace a non-socket path."
        );
        match std::os::unix::net::UnixStream::connect(&socket) {
            Ok(_) => anyhow::bail!("Another service is listening at the requested socket path."),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => (),
            Err(error) => return Err(error.into()),
        }
        std::fs::rename(
            &socket,
            socket.with_extension(format!("stale.{}", uuid::Uuid::now_v7())),
        )?;
    }
    let unix = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let cancellation = CancellationToken::new();
    let local_cancel = cancellation.clone();
    let local_task = tokio::spawn(async move {
        axum::serve(unix, local)
            .with_graceful_shutdown(local_cancel.cancelled_owned())
            .await
    });

    let remote = match (issuer, audience, public_url) {
        (Some(issuer), Some(audience), Some(public_url)) => {
            let auth = Arc::new(OAuth::discover(
                issuer,
                audience,
                public_url.clone(),
            ).await?);
            let application = remote_router(
                service,
                auth,
                &public_url,
                cancellation.clone(),
            )?;
            let tcp = TcpListener::bind(listen).await?;
            tracing::info!(%listen, "Remote OAuth-protected MCP listener ready");
            let remote_cancel = cancellation.clone();

            Some(tokio::spawn(async move {
                axum::serve(tcp, application)
                    .with_graceful_shutdown(remote_cancel.cancelled_owned())
                    .await
            }))
        }
        (None, None, None) => {
            tracing::info!(
                "Remote MCP disabled; configure issuer, audience, and public URL to enable it"
            );

            None
        }
        _ => anyhow::bail!("Remote MCP requires --issuer, --audience, and --public-url together."),
    };
    tracing::info!(socket = %socket.display(), "Local memory service ready");
    let mut termination =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result?,
        _ = termination.recv() => (),
    }
    cancellation.cancel();
    local_task.await??;
    if let Some(remote) = remote {
        remote.await??;
    }

    Ok(())
}

pub fn local_router(service: Arc<Service>) -> Router {
    let mcp_service = Arc::clone(&service);
    let mcp = StreamableHttpService::new(
        move || Ok(MemoryMcp::new(Arc::clone(&mcp_service), "ai client")),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_json_response(true)
            .with_legacy_session_mode(false)
            .with_max_request_body_bytes(usize::MAX),
    );

    Router::new()
        .route("/api/{operation}", post(local_operation))
        .nest_service("/mcp", mcp)
        .route("/health", get(|| async { Json(json!({"ready": true})) }))
        .layer(DefaultBodyLimit::disable())
        .with_state(service)
}

pub fn remote_router(
    service: Arc<Service>,
    auth: Arc<OAuth>,
    public_url: &str,
    cancellation: CancellationToken,
) -> anyhow::Result<Router> {
    let url = reqwest::Url::parse(public_url)?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("Public URL must have a hostname."))?;
    let authority = if let Some(port) = url.port() {
        format!("{host}:{port}")
    } else {
        host.to_owned()
    };
    let config = StreamableHttpServerConfig::default()
        .with_json_response(true)
        .with_legacy_session_mode(false)
        .with_max_request_body_bytes(usize::MAX)
        .with_allowed_hosts(vec![authority, "localhost".into(), "127.0.0.1".into()])
        .with_allowed_origins(vec![public_url.trim_end_matches('/').to_owned()])
        .with_cancellation_token(cancellation);
    let mcp = StreamableHttpService::new(
        move || Ok(MemoryMcp::new(Arc::clone(&service), "ai client")),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let metadata = auth.metadata();
    let metadata_path = metadata.clone();
    let protected = Router::new()
        .nest_service("/mcp", mcp)
        .layer(middleware::from_fn_with_state(auth, authorize));

    Ok(Router::new()
        .merge(protected)
        .route(
            "/.well-known/oauth-protected-resource",
            get(move || {
                let metadata = metadata.clone();
                async move { Json(metadata) }
            }),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(move || {
                let metadata = metadata_path.clone();
                async move { Json(metadata) }
            }),
        )
        .route("/health", get(|| async { Json(json!({"ready": true})) })))
}

async fn local_operation(
    State(service): State<Arc<Service>>,
    Path(operation): Path<String>,
    Json(arguments): Json<Value>,
) -> Response {
    match tokio::task::spawn_blocking(move || service.dispatch(
        &operation,
        arguments,
        "human",
    ))
        .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => error_response(error),
        Err(_) => error_response(AppError::new(
            "internal_failure",
            "The request worker failed.",
        )),
    }
}

fn error_response(error: AppError) -> Response {
    let status = match error.code.as_str() {
        "invalid_input" => StatusCode::BAD_REQUEST,
        "not_found" => StatusCode::NOT_FOUND,
        "token_conflict"
        | "already_exists"
        | "referenced"
        | "referenced_taxonomy"
        | "default_type" => StatusCode::CONFLICT,
        "database_busy" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };

    (status, Json(error)).into_response()
}
