//! Headless workspace server (`jh serve`) for remote VPS GPUI clients over SSH/Tailscale.
//! Exposes typed `Engine::call` (`POST /v1/call`), health (`GET /v1/health`), and a live
//! `notify`-driven SSE change stream (`GET /v1/events`). Never stores credentials or UI HTML.
//! It listens only on a Unix socket (`--bind unix:PATH`), whose file permissions decide
//! who may call; remote computers reach it over SSH. [`Client`] is the matching remote
//! mode for `jh call` and `jh mcp`.
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
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
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::broadcast;

#[derive(Clone)]
struct AppState {
    engine: Arc<Engine>,
    revision: Arc<AtomicU64>,
    tx: broadcast::Sender<u64>,
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

async fn health(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
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
    Json(req): Json<CallRequest>,
) -> (StatusCode, Json<Value>) {
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
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
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
    Sse::new(futures_util::StreamExt::chain(init_event, updates))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
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

const MAX_BODY: usize = 8 * 1024 * 1024;

pub fn router(root: &Path) -> anyhow::Result<(Router, Option<RecommendedWatcher>)> {
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
        host: server_host(),
    };
    let app = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/call", post(call_tool))
        .route("/v1/events", get(events))
        // Room for save_artifact's 5 MB as base64 JSON; the CLI takes the same 8 MiB.
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(state);
    Ok((app, watcher))
}

pub async fn serve(root: &Path, bind: &str) -> anyhow::Result<()> {
    let Some(path) = bind.strip_prefix("unix:") else {
        return Err(harness_core::HarnessError::new(
            "invalid_arguments",
            format!("jh serve listens only on a Unix socket: --bind unix:PATH (got '{bind}'). Reach it from another computer over SSH."),
        )
        .into());
    };
    #[cfg(unix)]
    return serve_unix(root, Path::new(path)).await;
    #[cfg(not(unix))]
    anyhow::bail!("jh serve needs Unix sockets; run it on Linux or macOS ({root:?}, {path})");
}

/// Serve on a Unix socket: mode 0660, so its owner and group may call and nobody
/// else may. The directory (made by the service manager) decides who can reach it.
#[cfg(unix)]
async fn serve_unix(root: &Path, path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    let (app, _watcher) = router(root)?;
    // A socket left by a stopped server would make bind fail; never remove anything else.
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_socket()) {
        std::fs::remove_file(path)?;
    }
    let listener = tokio::net::UnixListener::bind(path)
        .map_err(|err| anyhow::anyhow!("Cannot listen on {}: {err}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    eprintln!(
        "job-harness {} remote server listening on unix:{}",
        env!("CARGO_PKG_VERSION"),
        path.display()
    );
    axum::serve(listener, app).await?;
    Ok(())
}

/// Remote mode: send each tool call to a `jh serve` instead of opening the ledger.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: String,
}

impl Client {
    /// `url` is `unix:PATH`; the socket's permissions decide who may call.
    pub fn new(url: &str) -> anyhow::Result<Self> {
        let Some(path) = url.strip_prefix("unix:") else {
            return Err(harness_core::HarnessError::new(
                "invalid_arguments",
                format!("A remote ledger is a jh serve socket, unix:PATH (got '{url}')."),
            )
            .into());
        };
        #[cfg(not(unix))]
        anyhow::bail!("unix:{path} needs a Unix host");
        #[cfg(unix)]
        Ok(Self {
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .unix_socket(path)
                .build()?,
            endpoint: format!("unix:{path}"),
        })
    }

    /// The server's envelope. Without one, a 4xx is the request's fault and is not
    /// retried; anything else is `remote_unavailable`.
    pub async fn call(&self, tool: &str, arguments: &Value) -> Value {
        let response = self
            .http
            .post("http://localhost/v1/call")
            .json(&json!({"tool": tool, "arguments": arguments}))
            .send()
            .await;
        match response {
            Ok(response) => {
                let status = response.status();
                match response.json::<Value>().await {
                    Ok(envelope) if envelope.get("ok").is_some() => envelope,
                    _ if status.is_client_error() => rejected(status),
                    _ => unavailable(&format!(
                        "{} returned no envelope (HTTP {status})",
                        self.endpoint
                    )),
                }
            }
            Err(err) => unavailable(&format!("Cannot reach {}: {err}", self.endpoint)),
        }
    }
}

fn rejected(status: reqwest::StatusCode) -> Value {
    let message = if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
        "The request is larger than jh serve accepts (8 MiB).".to_owned()
    } else {
        format!("jh serve refused the request (HTTP {status}).")
    };
    json!({"schema_version":2,"ok":false,"data":null,"error":{"code":"invalid_arguments","message":message,"retryable":false,"recovery":"Correct the request; repeating it unchanged fails the same way."},"next":[]})
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
    async fn health_names_the_ledger_and_its_host() {
        let dir = tempfile::tempdir().unwrap();
        harness_core::workspace::initialize(dir.path()).unwrap();
        let (app, _w) = router(dir.path()).unwrap();
        let ok_health = app
            .oneshot(
                Request::builder()
                    .uri("/v1/health")
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
    }

    #[tokio::test]
    async fn only_a_unix_socket_is_served_or_called() {
        let dir = tempfile::tempdir().unwrap();
        let err = serve(dir.path(), "127.0.0.1:7420").await.unwrap_err();
        assert!(err.to_string().contains("unix:PATH"), "{err}");
        assert!(Client::new("http://127.0.0.1:7420").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_socket_serves_without_a_token_and_replaces_only_a_stale_socket() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("ledger");
        harness_core::workspace::initialize(&ledger).unwrap();
        let socket = dir.path().join("serve.sock");
        // A stale socket from a stopped server is replaced.
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
        tokio::spawn({
            let (ledger, socket) = (ledger.clone(), socket.clone());
            async move { serve_unix(&ledger, &socket).await }
        });
        let client = Client::new(&format!("unix:{}", socket.display())).unwrap();
        let mut ok = json!(null);
        for _ in 0..50 {
            ok = client.call("bootstrap", &json!({})).await;
            if ok["ok"] == true {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(ok["ok"], true, "{ok}");
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o660);
        let refused = client.call("read_record", &json!({"path":"../x.md"})).await;
        assert_eq!(refused["ok"], false);
        assert_ne!(refused["error"]["code"], "remote_unavailable");
        // A 5 MB artifact fits the body limit; a larger request is the caller's fault, not an outage.
        let five_mb = "A".repeat(6_666_664);
        let saved = client
            .call(
                "save_artifact",
                &json!({"name":"cv.pdf","content_base64":five_mb,"idempotency_key":"big"}),
            )
            .await;
        assert_eq!(saved["ok"], true, "{}", saved["error"]);
        let too_big = client
            .call("bootstrap", &json!({"padding":"x".repeat(MAX_BODY)}))
            .await;
        assert_eq!(too_big["error"]["code"], "invalid_arguments", "{too_big}");
        assert_eq!(too_big["error"]["retryable"], false);
        let gone =
            Client::new(&format!("unix:{}", dir.path().join("gone.sock").display())).unwrap();
        assert_eq!(
            gone.call("bootstrap", &json!({})).await["error"]["code"],
            "remote_unavailable"
        );

        // Anything else at the path is left alone and the server refuses to start.
        let file = dir.path().join("not-a-socket");
        std::fs::write(&file, "keep").unwrap();
        assert!(serve_unix(&ledger, &file).await.is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep");
    }
}
