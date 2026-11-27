//! Headless workspace server (`jh serve`) for remote VPS GPUI clients over SSH/Tailscale.
//! Exposes typed `Engine::call` (`POST /v1/call`), health (`GET /v1/health`), and a live
//! `notify`-driven SSE change stream (`GET /v1/events`). Never stores credentials or UI HTML.
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
    token: Option<String>,
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

fn authorized(headers: &HeaderMap, expected: Option<&str>) -> bool {
    let Some(expected) = expected else {
        return true;
    };
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
    if !authorized(&headers, state.token.as_deref()) {
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
    if !authorized(&headers, state.token.as_deref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "schema_version": 2,
                "ok": false,
                "data": null,
                "error": {"code":"unauthorized","message":"Missing or invalid bearer token","retryable":false,"recovery":"Pass Authorization: Bearer <JOB_HARNESS_SERVE_TOKEN>."},
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
    if !authorized(&headers, state.token.as_deref()) {
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

pub fn router(
    root: &Path,
    token: Option<String>,
) -> anyhow::Result<(Router, Option<RecommendedWatcher>)> {
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
    let token = std::env::var("JOB_HARNESS_SERVE_TOKEN")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    if !addr.ip().is_loopback() && token.is_none() {
        anyhow::bail!(
            "Refusing non-loopback bind {addr} without JOB_HARNESS_SERVE_TOKEN. Bind 127.0.0.1:7420 over SSH tunnel, or set JOB_HARNESS_SERVE_TOKEN over Tailscale."
        );
    }
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
        let (app, _w) = router(dir.path(), Some("secret-token".into())).unwrap();

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
}
