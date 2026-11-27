//! Headless workspace server (`jh serve`) for remote VPS GPUI clients over SSH/Tailscale.
//! Exposes typed `Engine::call` (`POST /v1/call`), health (`GET /v1/health`), and a live
//! `notify`-driven SSE change stream (`GET /v1/events`). Never stores credentials or UI HTML.
//! Every request needs the bearer token, so another local user cannot reach the ledger
//! through its loopback port. [`Client`] is the matching remote mode for `jh call` and `jh mcp`.
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use futures_util::stream::{self, Stream};
use harness_core::Engine;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::sync::broadcast;

#[derive(Clone)]
struct AppState {
    engine: Arc<Engine>,
    revision: Arc<AtomicU64>,
    tx: broadcast::Sender<u64>,
    token: String,
    /// `user@hostname` of the serving process, so clients can say where they are connected.
    host: String,
}

fn server_host() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "user".into());
    let host = std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "server".into());
    format!("{user}@{host}")
}

#[derive(Deserialize)]
struct CallRequest {
    tool: String,
    #[serde(default)]
    arguments: Value,
}

fn authorized(headers: &HeaderMap, expected: &str) -> bool {
    let Some(raw) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some(supplied) = raw.strip_prefix("Bearer ") else {
        return false;
    };
    supplied.as_bytes().ct_eq(expected.as_bytes()).into()
}

async fn health(State(state): State<AppState>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
    if !authorized(&headers, &state.token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"ok":false,"error":"unauthorized"})),
        );
    }
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "version": env!("CARGO_PKG_VERSION"),
            "workspace": state.engine.root(),
            "name": state.engine.root().file_name().and_then(|name| name.to_str()),
            "host": state.host,
            "revision": state.revision.load(Ordering::Relaxed)
        })),
    )
}

async fn call_tool(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CallRequest>,
) -> (StatusCode, Json<Value>) {
    if !authorized(&headers, &state.token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "schema_version": 2,
                "ok": false,
                "data": null,
                "error": {"code":"unauthorized","message":"Missing or invalid bearer token","retryable":false,"recovery":"Pass Authorization: Bearer <token>, the token in the server's JOB_HARNESS_SERVE_TOKEN_FILE."},
                "next": []
            })),
        );
    }
    let engine = state.engine.clone();
    let tool = req.tool;
    let args = if req.arguments.is_null() {
        json!({})
    } else {
        req.arguments
    };
    let out = tokio::task::spawn_blocking(move || engine.call(&tool, &args))
        .await
        .unwrap_or_else(|_| {
            json!({
                "schema_version": 2,
                "ok": false,
                "data": null,
                "error": {"code":"internal_error","message":"Worker task panicked","retryable":false,"recovery":"Inspect server logs."},
                "next": []
            })
        });
    let status = if out["ok"] == true {
        StatusCode::OK
    } else {
        StatusCode::BAD_REQUEST
    };
    (status, Json(out))
}

async fn events(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    if !authorized(&headers, &state.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let initial = state.revision.load(Ordering::Relaxed);
    let rx = state.tx.subscribe();
    let init_event = stream::once(async move {
        Ok::<_, Infallible>(
            Event::default()
                .event("ready")
                .data(json!({"revision": initial}).to_string()),
        )
    });
    let updates = stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(rev) => {
                    let ev = Event::default()
                        .event("changed")
                        .data(json!({"revision": rev}).to_string());
                    return Some((Ok::<_, Infallible>(ev), rx));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Ok(
        Sse::new(futures_util::StreamExt::chain(init_event, updates))
            .keep_alive(KeepAlive::new().interval(Duration::from_secs(15))),
    )
}

fn spawn_watcher(
    root: PathBuf,
    revision: Arc<AtomicU64>,
    tx: broadcast::Sender<u64>,
) -> Option<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res
            && !matches!(ev.kind, notify::EventKind::Access(_))
        {
            let rev = revision.fetch_add(1, Ordering::Relaxed) + 1;
            let _ = tx.send(rev);
        }
    })
    .ok()?;
    for sub in ["applications", "config", "output"] {
        let dir = root.join(sub);
        if dir.exists() {
            let _ = watcher.watch(&dir, RecursiveMode::Recursive);
        }
    }
    Some(watcher)
}

pub fn router(root: &Path, token: String) -> anyhow::Result<(Router, Option<RecommendedWatcher>)> {
    let engine = Arc::new(Engine::open(root)?);
    // Refuse to serve a ledger whose coordinator this binary cannot read.
    harness_core::state::load_for_read(engine.root())?;
    let revision = Arc::new(AtomicU64::new(1));
    let (tx, _) = broadcast::channel(64);
    let watcher = spawn_watcher(engine.root().to_path_buf(), revision.clone(), tx.clone());
    let state = AppState {
        engine,
        revision,
        tx,
        token,
        host: server_host(),
    };
    let app = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/call", post(call_tool))
        .route("/v1/events", get(events))
        .with_state(state);
    Ok((app, watcher))
}

pub async fn serve(root: &Path, bind: &str) -> anyhow::Result<()> {
    let addr: SocketAddr = bind.parse().map_err(|_| {
        anyhow::anyhow!("Invalid bind address '{bind}' (expected IP:PORT, e.g. 127.0.0.1:7420)")
    })?;
    let token = serve_token(
        std::env::var("JOB_HARNESS_SERVE_TOKEN").ok(),
        std::env::var_os("JOB_HARNESS_SERVE_TOKEN_FILE").map(PathBuf::from),
    )?;
    let (app, _watcher) = router(root, token)?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let actual = listener.local_addr()?;
    eprintln!(
        "job-harness {} remote server listening on http://{}",
        env!("CARGO_PKG_VERSION"),
        actual
    );
    axum::serve(listener, app).await?;
    Ok(())
}

/// The server's bearer token: `JOB_HARNESS_SERVE_TOKEN`, or the file named by
/// `JOB_HARNESS_SERVE_TOKEN_FILE`. There is no unauthenticated mode, even on loopback.
pub fn serve_token(value: Option<String>, file: Option<PathBuf>) -> anyhow::Result<String> {
    let token = match (value, file) {
        (Some(value), _) => value.trim().to_owned(),
        (None, Some(file)) => read_token(&file)?,
        (None, None) => String::new(),
    };
    if token.len() < 16 {
        return Err(harness_core::HarnessError::new(
            "token_required",
            "jh serve needs a bearer token of at least 16 characters: set JOB_HARNESS_SERVE_TOKEN_FILE to a file only you can read (jh service install creates one).",
        )
        .into());
    }
    Ok(token)
}

pub fn read_token(file: &Path) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(file).map_err(|err| {
        harness_core::HarnessError::new(
            "token_required",
            format!("Cannot read token file {}: {err}", file.display()),
        )
    })?;
    Ok(text.trim().to_owned())
}

/// Remote mode: send each tool call to a `jh serve` instead of opening the ledger.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: String,
    token: String,
}

impl Client {
    pub fn new(url: &str, token: String) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            http,
            endpoint: format!("{}/v1/call", url.trim_end_matches('/')),
            token,
        })
    }

    /// The server's envelope, or `remote_unavailable` when none arrived.
    pub async fn call(&self, tool: &str, arguments: &Value) -> Value {
        let response = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .json(&json!({"tool": tool, "arguments": arguments}))
            .send()
            .await;
        match response {
            Ok(response) => response.json::<Value>().await.unwrap_or_else(|_| {
                unavailable(&format!("{} returned no envelope", self.endpoint))
            }),
            Err(err) => unavailable(&format!("Cannot reach {}: {err}", self.endpoint)),
        }
    }
}

fn unavailable(message: &str) -> Value {
    json!({"schema_version":2,"ok":false,"data":null,"error":{"code":"remote_unavailable","message":message,"retryable":true,"recovery":"Check that this workspace's jh serve is running. A write may have landed: repeat it with the same idempotency_key, which returns the recorded result instead of applying it twice."},"next":[]})
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_and_call_enforce_bearer_token_when_set() {
        let dir = tempfile::tempdir().unwrap();
        harness_core::workspace::initialize(dir.path()).unwrap();
        let (app, _w) = router(dir.path(), "secret-token".into()).unwrap();

        let unauth = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

        let ok_health = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/health")
                    .header("authorization", "Bearer secret-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok_health.status(), StatusCode::OK);
        let body = ok_health.into_body().collect().await.unwrap().to_bytes();
        let health: Value = serde_json::from_slice(&body).unwrap();
        let name = dir.path().file_name().unwrap().to_str().unwrap();
        assert_eq!(health["name"], name);
        let host = health["host"].as_str().unwrap();
        assert!(host.contains('@') && !host.ends_with('@'), "{host}");

        let call_res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/call")
                    .header("authorization", "Bearer secret-token")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"tool":"bootstrap","arguments":{"operation":"daily_check","capabilities":[]}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(call_res.status(), StatusCode::OK);
        let body = call_res.into_body().collect().await.unwrap().to_bytes();
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["ok"], true);
    }

    #[test]
    fn serve_always_needs_a_token() {
        assert!(serve_token(None, None).is_err());
        assert!(serve_token(Some("short".into()), None).is_err());
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("token");
        assert!(serve_token(None, Some(file.clone())).is_err());
        std::fs::write(&file, "0123456789abcdef0123\n").unwrap();
        assert_eq!(
            serve_token(None, Some(file)).unwrap(),
            "0123456789abcdef0123"
        );
    }

    #[tokio::test]
    async fn client_calls_through_the_server_with_its_token() {
        let dir = tempfile::tempdir().unwrap();
        harness_core::workspace::initialize(dir.path()).unwrap();
        let token = "0123456789abcdef0123".to_owned();
        let (app, _w) = router(dir.path(), token.clone()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });

        let client = Client::new(&url, token).unwrap();
        let ok = client.call("bootstrap", &json!({})).await;
        assert_eq!(ok["ok"], true, "{ok}");
        let refused = client.call("read_record", &json!({"path":"../x.md"})).await;
        assert_eq!(refused["ok"], false);
        assert_ne!(refused["error"]["code"], "remote_unavailable");

        let wrong = Client::new(&url, "not-the-token-at-all".into()).unwrap();
        assert_eq!(
            wrong.call("bootstrap", &json!({})).await["error"]["code"],
            "unauthorized"
        );
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gone = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let down = Client::new(&gone, "0123456789abcdef0123".into()).unwrap();
        assert_eq!(
            down.call("bootstrap", &json!({})).await["error"]["code"],
            "remote_unavailable"
        );
    }
}
