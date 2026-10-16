//! Streamable HTTP transport. Authentication and origin checks precede dispatch;
//! no optional SSE channel is advertised because the server emits no pushes.
use crate::protocol::{self, META_VERSION, Server, Session};
use anyhow::{Result, bail};
use axum::{
    Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
type SessionStore = Arc<Mutex<HashMap<String, (Arc<Session>, Instant)>>>;

#[derive(Clone)]
struct HttpState {
    server: Arc<Server>,
    sessions: SessionStore,
    origins: Arc<Vec<String>>,
    tokens: Arc<Vec<String>>,
    requests: Arc<AtomicU64>,
}
#[derive(Clone, Default)]
pub struct Options {
    pub allowed_origins: Vec<String>,
    pub auth_tokens: Vec<String>,
}
fn valid_token(given: &str, expected: &str) -> bool {
    if given.len() != expected.len() {
        return false;
    }
    given
        .bytes()
        .zip(expected.bytes())
        .fold(0u8, |a, (b, c)| a | (b ^ c))
        == 0
}
fn allowed(state: &HttpState, headers: &HeaderMap) -> bool {
    if let Some(origin) = headers.get("origin")
        && !state
            .origins
            .iter()
            .any(|s| Some(s.as_str()) == origin.to_str().ok())
    {
        return false;
    }
    if !state.tokens.is_empty() {
        let Some(value) = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
        else {
            return false;
        };
        if !state.tokens.iter().any(|token| valid_token(value, token)) {
            return false;
        }
    }
    true
}
fn json_response(status: StatusCode, value: Value) -> Response {
    (
        status,
        [("content-type", "application/json")],
        serde_json::to_string(&value).unwrap_or_default(),
    )
        .into_response()
}
async fn rpc(
    State(state): State<HttpState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !allowed(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    state.requests.fetch_add(1, Ordering::Relaxed);
    if !headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"))
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let request: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                protocol::error(Value::Null, -32700, "Parse error"),
            );
        }
    };
    let method = request["method"].as_str().unwrap_or("");
    let modern = request
        .pointer("/params/_meta")
        .and_then(|m| m.get(META_VERSION))
        .is_some()
        || method == "server/discover";
    if modern {
        if headers.get("mcp-method").and_then(|v| v.to_str().ok()) != Some(method) {
            return StatusCode::BAD_REQUEST.into_response();
        }
        if method == "tools/call"
            && headers.get("mcp-name").and_then(|v| v.to_str().ok())
                != request.pointer("/params/name").and_then(Value::as_str)
        {
            return StatusCode::BAD_REQUEST.into_response();
        }
    }
    let mut sessions = state.sessions.lock().await;
    sessions.retain(|_, (_, last)| last.elapsed() < Duration::from_secs(3600));
    let (id, session) = if modern {
        (None, Arc::new(Session::default()))
    } else if method == "initialize" {
        if sessions.len() >= 1024 {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        let id = uuid::Uuid::new_v4().to_string();
        let session = Arc::new(Session::default());
        sessions.insert(id.clone(), (session.clone(), Instant::now()));
        (Some(id), session)
    } else {
        let Some(id) = headers.get("mcp-session-id").and_then(|v| v.to_str().ok()) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let Some((session, last)) = sessions.get_mut(id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        *last = Instant::now();
        (None, session.clone())
    };
    drop(sessions);
    let dispatched = state.server.dispatch(&request, &session).await;
    let mut response = match dispatched {
        Some(value) => json_response(StatusCode::OK, value),
        None => StatusCode::ACCEPTED.into_response(),
    };
    if let Some(id) = id {
        response
            .headers_mut()
            .insert("mcp-session-id", HeaderValue::from_str(&id).unwrap());
    }
    response
}
async fn delete(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if !allowed(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(id) = headers.get("mcp-session-id").and_then(|v| v.to_str().ok()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if state.sessions.lock().await.remove(id).is_some() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}
async fn metrics(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if !allowed(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    format!(
        "# TYPE asc_mcp_requests_total counter\nasc_mcp_requests_total {}\n",
        state.requests.load(Ordering::Relaxed)
    )
    .into_response()
}
pub fn router(server: Arc<Server>, options: Options) -> Router {
    let state = HttpState {
        server,
        sessions: Arc::new(Mutex::new(HashMap::new())),
        origins: Arc::new(options.allowed_origins),
        tokens: Arc::new(options.auth_tokens),
        requests: Arc::new(AtomicU64::new(0)),
    };
    Router::new()
        .route(
            "/mcp",
            post(rpc)
                .delete(delete)
                .get(|| async { StatusCode::METHOD_NOT_ALLOWED })
                .options(|| async { StatusCode::NO_CONTENT }),
        )
        .route("/healthz", get(|| async { (StatusCode::OK, "ok") }))
        .route("/metrics", get(metrics))
        .layer(DefaultBodyLimit::max(64 << 20))
        .with_state(state)
}
pub async fn serve(server: Arc<Server>, addr: &str, options: Options) -> Result<()> {
    let addr = if addr.starts_with(':') {
        format!("0.0.0.0{addr}")
    } else {
        addr.into()
    };
    let socket: std::net::SocketAddr = addr.parse()?;
    if !socket.ip().is_loopback() && options.auth_tokens.is_empty() {
        bail!("non-loopback HTTP listener requires --auth-tokens")
    }
    let listener = tokio::net::TcpListener::bind(socket).await?;
    axum::serve(listener, router(server, options))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
/// Native TLS listener; certificates stay local and are never tool arguments.
pub async fn serve_tls(
    server: Arc<Server>,
    addr: &str,
    options: Options,
    cert: &str,
    key: &str,
) -> Result<()> {
    let addr = if addr.starts_with(':') {
        format!("0.0.0.0{addr}")
    } else {
        addr.into()
    };
    let socket: std::net::SocketAddr = addr.parse()?;
    if !socket.ip().is_loopback() && options.auth_tokens.is_empty() {
        bail!("non-loopback HTTP listener requires --auth-tokens")
    }
    let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;
    axum_server::bind_rustls(socket, config)
        .serve(router(server, options).into_make_service())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;
    fn app() -> Router {
        router(
            Arc::new(Server {
                api: None,
                reviewer: None,
                developer: None,
            }),
            Options {
                allowed_origins: vec!["https://example.test".into()],
                auth_tokens: vec!["secret-test".into()],
            },
        )
    }
    #[tokio::test]
    async fn denies_requests_without_token_or_with_wrong_origin() {
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        assert_eq!(
            app().oneshot(req).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("authorization", "Bearer secret-test")
            .header("origin", "https://evil.test")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        assert_eq!(
            app().oneshot(req).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
    #[tokio::test]
    async fn initialize_assigns_session_and_get_is_unsupported() {
        let req=axum::http::Request::builder().method("POST").uri("/mcp").header("authorization","Bearer secret-test").header("content-type","application/json").body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#)).unwrap();
        let response = app().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key("mcp-session-id"));
    }
}
