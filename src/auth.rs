use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct OAuth {
    issuer: String,
    audience: String,
    jwks_uri: String,
    public_url: String,
    client: reqwest::Client,
    keys: Arc<RwLock<CachedKeys>>,
}

struct CachedKeys {
    keys: JwkSet,
    refreshed_at: Instant,
}

#[derive(Debug, Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Principal {
    pub sub: String,
    #[serde(default)]
    pub scope: String,
}

impl OAuth {
    pub async fn discover(
        issuer: String,
        audience: String,
        public_url: String,
    ) -> anyhow::Result<Self> {
        Self::discover_with_http(
            issuer,
            audience,
            public_url,
            false,
        ).await
    }

    pub async fn discover_with_http(
        issuer: String,
        audience: String,
        public_url: String,
        allow_http: bool,
    ) -> anyhow::Result<Self> {
        if !allow_http && (!issuer.starts_with("https://") || !public_url.starts_with("https://")) {
            anyhow::bail!("OAuth issuer and public URL must use HTTPS.");
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let discovery: Discovery = client
            .get(format!(
                "{}/.well-known/openid-configuration",
                issuer.trim_end_matches('/')
            ))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        anyhow::ensure!(
            discovery.issuer == issuer,
            "OAuth discovery issuer does not match configured issuer."
        );
        anyhow::ensure!(
            allow_http || discovery.jwks_uri.starts_with("https://"),
            "JWKS must use HTTPS."
        );
        let keys = client
            .get(&discovery.jwks_uri)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        Ok(Self {
            issuer,
            audience,
            jwks_uri: discovery.jwks_uri,
            public_url,
            client,
            keys: Arc::new(RwLock::new(CachedKeys {
                keys,
                refreshed_at: Instant::now(),
            })),
        })
    }

    pub fn metadata(&self) -> Value {
        json!({
            "resource": format!("{}/mcp", self.public_url.trim_end_matches('/')),
            "authorization_servers": [self.issuer],
            "scopes_supported": ["memory:read", "memory:write", "memory:purge"],
            "bearer_methods_supported": ["header"]
        })
    }

    pub async fn validate(
        &self,
        token: &str,
        required_scope: &str,
    ) -> Result<Principal, String> {
        let header = decode_header(token).map_err(|_| "Malformed access token.")?;
        if !matches!(
            header.alg,
            Algorithm::RS256 | Algorithm::ES256 | Algorithm::EdDSA
        ) {
            return Err("Unsupported access-token algorithm.".into());
        }
        let kid = header.kid.ok_or("Access token has no signing key ID.")?;
        {
            let keys = self.keys.read().await;
            if keys.refreshed_at.elapsed() > Duration::from_secs(300)
                || (keys.keys.find(&kid).is_none()
                    && keys.refreshed_at.elapsed() > Duration::from_secs(30))
            {
                drop(keys);
                let mut keys = self.keys.write().await;
                if keys.refreshed_at.elapsed() > Duration::from_secs(30) {
                    let refreshed = self
                        .client
                        .get(&self.jwks_uri)
                        .send()
                        .await
                        .map_err(|_| "Cannot refresh signing keys.")?
                        .error_for_status()
                        .map_err(|_| "Cannot refresh signing keys.")?
                        .json::<JwkSet>()
                        .await
                        .map_err(|_| "Invalid signing-key document.")?;
                    keys.keys = refreshed;
                    keys.refreshed_at = Instant::now();
                }
            }
        }
        let keys = self.keys.read().await;
        let jwk = keys.keys.find(&kid).ok_or("Unknown signing key.")?;
        let key = DecodingKey::from_jwk(jwk).map_err(|_| "Invalid signing key.")?;
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[self.issuer.as_str()]);
        validation.set_audience(&[self.audience.as_str()]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.validate_nbf = true;
        validation.leeway = 5;
        let principal = decode::<Principal>(
            token,
            &key,
            &validation,
        )
            .map_err(|_| "Access token is expired or invalid.")?
            .claims;
        let scopes: HashSet<_> = principal.scope.split_whitespace().collect();
        if !scopes.contains(required_scope) {
            return Err(format!("Missing required scope: {required_scope}"));
        }

        Ok(principal)
    }
}

pub fn operation_scope(operation: &str) -> &'static str {
    if operation == "delete_memories" {
        "memory:purge"
    } else if operation.starts_with("get_")
        || operation.starts_with("list_")
        || operation == "search_memories"
    {
        "memory:read"
    } else {
        "memory:write"
    }
}

pub async fn authorize(
    State(auth): State<Arc<OAuth>>,
    request: Request,
    next: Next,
) -> Response {
    let token = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned);
    let (mut parts, body) = request.into_parts();
    let bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                "The request body could not be read.",
            )
                .into_response();
        }
    };
    let operation = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|value| {
            value
                .pointer("/params/name")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let required = operation
        .as_deref()
        .map(operation_scope)
        .unwrap_or("memory:read");
    let principal = match token {
        Some(token) => auth.validate(&token, required).await,
        None => Err("A Bearer access token is required.".into()),
    };

    match principal {
        Ok(principal) => {
            parts.extensions.insert(principal);
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
        Err(message) => {
            let is_scope_error = message.starts_with("Missing required scope:");
            let status = if is_scope_error {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::UNAUTHORIZED
            };
            let mut response = (status, Json(json!({"error": if is_scope_error { "insufficient_scope" } else { "invalid_token" }, "message": message}))).into_response();
            let challenge = format!(
                "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\"{}",
                auth.public_url.trim_end_matches('/'),
                if is_scope_error {
                    format!(", error=\"insufficient_scope\", scope=\"{required}\"")
                } else {
                    String::new()
                }
            );
            if let Ok(value) = HeaderValue::from_str(&challenge) {
                response.headers_mut().insert("www-authenticate", value);
            }

            response
        }
    }
}
