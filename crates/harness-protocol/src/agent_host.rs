//! `jh agent-host`: keeps a user's ACP agents (Agent Client Protocol: JSON-RPC over
//! stdio) running so a chat turn outlives the app's connection, as T3 Code's server
//! does. It runs as the agent's user, listens on a 0600 Unix socket, and speaks JSON
//! lines to the app:
//!
//! - `{"id":1,"op":"agents"}` starts each configured agent and answers
//!   `{"result":{"agents":[{"name","init"|"error"}]}}`; `init` is the agent's own
//!   `initialize` result, so the app can see what it supports.
//! - `{"id":2,"op":"call","agent","method","params"}` sends one ACP request and answers
//!   with its `result` or `error`. A `session/prompt` also ends its turn in the
//!   session's event log, so a client that was away still sees how it ended.
//! - `{"op":"notify","agent","method","params"}` sends an ACP notification, such as
//!   `session/cancel`.
//! - `{"op":"respond","agent","request","result"}` answers the agent's
//!   `session/request_permission` (the first answer wins).
//! - `{"id":3,"op":"attach","agent","session","after":N}` answers
//!   `{"result":{"gap","latest"}}` and then streams
//!   `{"agent","session","seq", "msg"|"end"|"resolved"}` for every event after `N`:
//!   first from a bounded replay buffer, then live. Leave `after` out to start from now.
//!   `gap` means some events after `N` are gone: reload the session with `session/load`.
//!
//! Nothing here knows which agent it runs.
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command},
    sync::{broadcast, mpsc, oneshot},
};

/// Each session keeps at most this many events for replay...
pub const REPLAY_EVENTS: usize = 1000;
/// ...and at most this many bytes of them.
pub const REPLAY_BYTES: usize = 4 << 20;
/// A permission request nobody answers is cancelled after this; the turn goes on.
pub const PERMISSION_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// An agent with no client, no request in flight and no output for this long is
/// stopped. The next call starts it again; its sessions come back with `session/load`.
pub const AGENT_IDLE_EXIT: Duration = Duration::from_secs(30 * 60);

type Line = Arc<str>;

/// One session's numbered events: the replay buffer and the live feed.
pub struct Log {
    seq: u64,
    events: VecDeque<(u64, Line)>,
    bytes: usize,
    live: broadcast::Sender<(u64, Line)>,
}

impl Default for Log {
    fn default() -> Self {
        Self {
            seq: 0,
            events: VecDeque::new(),
            bytes: 0,
            live: broadcast::channel(1024).0,
        }
    }
}

impl Log {
    /// Number `body` and keep the buffer within [`REPLAY_EVENTS`] and [`REPLAY_BYTES`].
    pub fn push(&mut self, mut body: Value) -> u64 {
        self.seq += 1;
        body["seq"] = json!(self.seq);
        let line: Line = body.to_string().into();
        self.bytes += line.len();
        self.events.push_back((self.seq, line.clone()));
        while self.events.len() > REPLAY_EVENTS || self.bytes > REPLAY_BYTES {
            let Some((_, old)) = self.events.pop_front() else {
                break;
            };
            self.bytes -= old.len();
        }
        let _ = self.live.send((self.seq, line));
        self.seq
    }

    /// The events after `after` (all new ones when `None`), whether some of them were
    /// already dropped, the latest seq, and a live feed from here on.
    #[allow(clippy::type_complexity)]
    pub fn since(
        &self,
        after: Option<u64>,
    ) -> (
        Vec<(u64, Line)>,
        bool,
        u64,
        broadcast::Receiver<(u64, Line)>,
    ) {
        let after = after.unwrap_or(self.seq);
        let oldest = self.events.front().map_or(self.seq + 1, |(seq, _)| *seq);
        let gap = after + 1 < oldest && after < self.seq;
        let events = self
            .events
            .iter()
            .filter(|(seq, _)| *seq > after)
            .cloned()
            .collect();
        (events, gap, self.seq, self.live.subscribe())
    }
}

/// A permission request's session and its timeout task.
type Waiting = (String, tokio::task::JoinHandle<()>);
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

struct Proc {
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    next_id: AtomicU64,
    pending: Pending,
    init: Value,
    used: Arc<Mutex<Instant>>,
    _child: tokio::process::Child,
}

struct Agent {
    name: String,
    command: String,
    proc: tokio::sync::Mutex<Option<Arc<Proc>>>,
}

struct Host {
    agents: Vec<Agent>,
    logs: Mutex<HashMap<(String, String), Log>>,
    /// Permission requests waiting for an answer, with their timeout task.
    waiting: Mutex<HashMap<(String, String), Waiting>>,
    clients: AtomicU64,
}

/// `NAME=COMMAND` pairs. Each command runs with `sh -lc`, so it finds what the
/// user's login shell finds.
pub fn parse_agents(specs: &[String]) -> anyhow::Result<Vec<(String, String)>> {
    specs
        .iter()
        .map(|spec| {
            let (name, command) = spec
                .split_once('=')
                .filter(|(name, command)| !name.is_empty() && !command.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("--agent takes NAME=COMMAND, got '{spec}'"))?;
            Ok((name.to_owned(), command.to_owned()))
        })
        .collect()
}

/// An agent may stop once idle long enough with nothing in flight and no client.
pub fn idle_for_exit(idle: Duration, in_flight: usize, clients: u64) -> bool {
    idle >= AGENT_IDLE_EXIT && in_flight == 0 && clients == 0
}

fn error(message: impl std::fmt::Display) -> Value {
    json!({"error":{"code":-32000,"message":message.to_string()}})
}

async fn write_line(stdin: &tokio::sync::Mutex<ChildStdin>, msg: &Value) -> std::io::Result<()> {
    let mut stdin = stdin.lock().await;
    stdin.write_all(format!("{msg}\n").as_bytes()).await?;
    stdin.flush().await
}

impl Host {
    fn agent(&self, name: &str) -> anyhow::Result<&Agent> {
        self.agents
            .iter()
            .find(|agent| agent.name == name)
            .ok_or_else(|| anyhow::anyhow!("No agent named '{name}' on this host"))
    }

    fn log(&self, agent: &str, session: &str, mut body: Value) {
        body["agent"] = json!(agent);
        body["session"] = json!(session);
        let mut logs = self.logs.lock().unwrap();
        logs.entry((agent.to_owned(), session.to_owned()))
            .or_default()
            .push(body);
    }

    /// The agent's process, started and initialized first if it is not running.
    async fn proc(self: &Arc<Self>, name: &str) -> anyhow::Result<Arc<Proc>> {
        let agent = self.agent(name)?;
        let mut slot = agent.proc.lock().await;
        if let Some(proc) = slot.as_ref() {
            *proc.used.lock().unwrap() = Instant::now();
            return Ok(proc.clone());
        }
        let mut child = Command::new("sh")
            .args(["-lc", &agent.command])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| anyhow::anyhow!("Could not start '{}': {err}", agent.command))?;
        let stdin = Arc::new(tokio::sync::Mutex::new(child.stdin.take().unwrap()));
        let stdout = child.stdout.take().unwrap();
        let pending = Pending::default();
        let used = Arc::new(Mutex::new(Instant::now()));
        let (init_tx, init_rx) = oneshot::channel();
        pending.lock().unwrap().insert(0, init_tx);
        tokio::spawn(self.clone().read_agent(
            name.to_owned(),
            stdout,
            stdin.clone(),
            pending.clone(),
            used.clone(),
        ));
        // The agent works on its own host: the client offers no files or terminals.
        let init = json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
            "protocolVersion":1,
            "clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false},
            "clientInfo":{"name":"job-harness","version":env!("CARGO_PKG_VERSION")}}});
        write_line(&stdin, &init).await?;
        let reply = tokio::time::timeout(Duration::from_secs(120), init_rx)
            .await
            .map_err(|_| anyhow::anyhow!("{name} did not answer initialize"))?
            .map_err(|_| anyhow::anyhow!("{name} exited while starting"))?;
        if let Some(error) = reply.get("error") {
            anyhow::bail!("{name} could not start: {error}");
        }
        let proc = Arc::new(Proc {
            stdin,
            next_id: AtomicU64::new(1),
            pending,
            init: reply["result"].clone(),
            used,
            _child: child,
        });
        *slot = Some(proc.clone());
        Ok(proc)
    }

    /// Route the agent's output: replies to their callers, session updates and
    /// permission requests to the session's log.
    async fn read_agent(
        self: Arc<Self>,
        name: String,
        stdout: tokio::process::ChildStdout,
        stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
        pending: Pending,
        used: Arc<Mutex<Instant>>,
    ) {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            *used.lock().unwrap() = Instant::now();
            let session = msg["params"]["sessionId"].as_str().map(str::to_owned);
            match (msg.get("method").and_then(Value::as_str), msg.get("id")) {
                (None, Some(id)) => {
                    let waiter = id
                        .as_u64()
                        .and_then(|id| pending.lock().unwrap().remove(&id));
                    if let Some(waiter) = waiter {
                        let _ = waiter.send(msg);
                    }
                }
                (Some("session/request_permission"), Some(id)) if session.is_some() => {
                    let session = session.unwrap();
                    self.wait_for_answer(&name, &session, id.clone());
                    self.log(&name, &session, json!({"msg": msg}));
                }
                (Some(method), Some(id)) => {
                    // fs/* and terminal/* were declined in initialize.
                    let refusal = json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,
                        "message":format!("{method} is not offered by this client")}});
                    let _ = write_line(&stdin, &refusal).await;
                }
                (Some(_), None) => {
                    if let Some(session) = session {
                        self.log(&name, &session, json!({"msg": msg}));
                    }
                }
                (None, None) => {}
            }
        }
        // The agent exited: fail what waits on it; the next call starts it again.
        for (_, waiter) in pending.lock().unwrap().drain() {
            let _ = waiter.send(error(format!("{name} exited")));
        }
        if let Ok(agent) = self.agent(&name) {
            let mut slot = agent.proc.lock().await;
            if slot
                .as_ref()
                .is_some_and(|proc| Arc::ptr_eq(&proc.pending, &pending))
            {
                *slot = None;
            }
        }
    }

    /// Hold a permission request for an answer, cancelling it after [`PERMISSION_TIMEOUT`].
    fn wait_for_answer(self: &Arc<Self>, agent: &str, session: &str, request: Value) {
        let host = self.clone();
        let (name, id) = (agent.to_owned(), request.clone());
        let timeout = tokio::spawn(async move {
            tokio::time::sleep(PERMISSION_TIMEOUT).await;
            let cancelled = json!({"outcome":{"outcome":"cancelled"}});
            host.respond(&name, id, cancelled).await;
        });
        let key = (agent.to_owned(), request.to_string());
        self.waiting
            .lock()
            .unwrap()
            .insert(key, (session.to_owned(), timeout));
    }

    async fn respond(&self, agent: &str, request: Value, result: Value) {
        let key = (agent.to_owned(), request.to_string());
        let Some((session, timeout)) = self.waiting.lock().unwrap().remove(&key) else {
            return; // already answered, by another client or the timeout
        };
        timeout.abort();
        self.log(agent, &session, json!({"resolved": request}));
        let proc = match self.agent(agent) {
            Ok(agent) => agent.proc.lock().await.clone(),
            Err(_) => None,
        };
        if let Some(proc) = proc {
            let answer = json!({"jsonrpc":"2.0","id":request,"result":result});
            let _ = write_line(&proc.stdin, &answer).await;
        }
    }

    async fn call(self: &Arc<Self>, agent: &str, method: &str, mut params: Value) -> Value {
        let proc = match self.proc(agent).await {
            Ok(proc) => proc,
            Err(err) => return error(err),
        };
        if matches!(method, "session/new" | "session/load" | "session/resume") {
            if params.get("cwd").is_none() {
                params["cwd"] = json!(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
            }
            if params.get("mcpServers").is_none() {
                params["mcpServers"] = json!([]);
            }
        }
        let id = proc.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        proc.pending.lock().unwrap().insert(id, tx);
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let reply = match write_line(&proc.stdin, &request).await {
            Ok(()) => rx
                .await
                .unwrap_or_else(|_| error(format!("{agent} exited"))),
            Err(err) => error(err),
        };
        let outcome = match reply.get("error") {
            Some(error) => json!({"error": error}),
            None => json!({"result": reply.get("result").cloned().unwrap_or(Value::Null)}),
        };
        if method == "session/prompt"
            && let Some(session) = params["sessionId"].as_str()
        {
            self.log(agent, session, json!({"end": outcome}));
        }
        outcome
    }

    /// Stop agents that have been idle for [`AGENT_IDLE_EXIT`].
    async fn reap(&self) {
        for agent in &self.agents {
            let mut slot = agent.proc.lock().await;
            let idle = slot.as_ref().is_some_and(|proc| {
                idle_for_exit(
                    proc.used.lock().unwrap().elapsed(),
                    proc.pending.lock().unwrap().len(),
                    self.clients.load(Ordering::Relaxed),
                )
            });
            if idle {
                *slot = None; // dropping the child stops it
            }
        }
    }

    async fn client(self: Arc<Self>, stream: tokio::net::UnixStream) {
        self.clients.fetch_add(1, Ordering::Relaxed);
        let (read, mut write) = stream.into_split();
        let (out, mut outbox) = mpsc::unbounded_channel::<String>();
        let writer = tokio::spawn(async move {
            while let Some(line) = outbox.recv().await {
                if write
                    .write_all(format!("{line}\n").as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut lines = BufReader::new(read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(request) = serde_json::from_str::<Value>(&line) {
                // Requests run on: a prompt finishes, and logs its end, without its client.
                tokio::spawn(self.clone().handle(request, out.clone()));
            }
        }
        writer.abort(); // closes `out`, which ends this client's attach streams
        self.clients.fetch_sub(1, Ordering::Relaxed);
    }

    async fn handle(self: Arc<Self>, request: Value, out: mpsc::UnboundedSender<String>) {
        let agent = request["agent"].as_str().unwrap_or_default().to_owned();
        let reply = |mut body: Value| {
            body["id"] = request["id"].clone();
            let _ = out.send(body.to_string());
        };
        match request["op"].as_str().unwrap_or_default() {
            "agents" => {
                let mut agents = Vec::new();
                for agent in &self.agents {
                    agents.push(match self.proc(&agent.name).await {
                        Ok(proc) => json!({"name": agent.name, "init": proc.init}),
                        Err(err) => json!({"name": agent.name, "error": err.to_string()}),
                    });
                }
                reply(json!({"result":{"agents":agents}}));
            }
            "call" => {
                let method = request["method"].as_str().unwrap_or_default();
                reply(self.call(&agent, method, request["params"].clone()).await);
            }
            "notify" => {
                if let Ok(proc) = self.proc(&agent).await {
                    let note = json!({"jsonrpc":"2.0","method":request["method"],"params":request["params"]});
                    let _ = write_line(&proc.stdin, &note).await;
                }
            }
            "respond" => {
                self.respond(
                    &agent,
                    request["request"].clone(),
                    request["result"].clone(),
                )
                .await;
            }
            "attach" => {
                let session = request["session"].as_str().unwrap_or_default().to_owned();
                let (events, gap, latest, mut live) = {
                    let mut logs = self.logs.lock().unwrap();
                    let log = logs.entry((agent, session)).or_default();
                    log.since(request["after"].as_u64())
                };
                reply(json!({"result":{"gap":gap,"latest":latest}}));
                let mut last = latest;
                for (_, line) in events {
                    let _ = out.send(line.to_string());
                }
                loop {
                    tokio::select! {
                        event = live.recv() => match event {
                            Ok((seq, line)) if seq > last => {
                                last = seq;
                                if out.send(line.to_string()).is_err() {
                                    return;
                                }
                            }
                            Ok(_) => {}
                            // Too far behind: the client reattaches from its last seq.
                            Err(_) => return,
                        },
                        () = out.closed() => return,
                    }
                }
            }
            other => {
                reply(json!({"error":{"code":-32601,"message":format!("Unknown op '{other}'")}}))
            }
        }
    }
}

/// Serve `agents` on a 0600 socket at `path` until stopped.
pub async fn run(path: &std::path::Path, agents: Vec<(String, String)>) -> anyhow::Result<()> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_socket()) {
        std::fs::remove_file(path)?;
    }
    let listener = tokio::net::UnixListener::bind(path)
        .map_err(|err| anyhow::anyhow!("Cannot listen on {}: {err}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    eprintln!(
        "job-harness {} agent host listening on unix:{}",
        env!("CARGO_PKG_VERSION"),
        path.display()
    );
    let host = Arc::new(Host {
        agents: agents
            .into_iter()
            .map(|(name, command)| Agent {
                name,
                command,
                proc: tokio::sync::Mutex::new(None),
            })
            .collect(),
        logs: Mutex::default(),
        waiting: Mutex::default(),
        clients: AtomicU64::new(0),
    });
    let reaper = host.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            reaper.reap().await;
        }
    });
    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(host.clone().client(stream));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_replay_buffer_keeps_the_newest_events_and_reports_a_gap() {
        let mut log = Log::default();
        for i in 0..REPLAY_EVENTS + 5 {
            log.push(json!({"n": i}));
        }
        let (events, gap, latest, _) = log.since(Some(0));
        assert!(gap);
        assert_eq!(
            (events.len(), events[0].0, latest),
            (REPLAY_EVENTS, 6, 1005)
        );
        let (events, gap, _, _) = log.since(Some(1000));
        assert!(!gap);
        let seqs: Vec<u64> = events.iter().map(|event| event.0).collect();
        assert_eq!(seqs, [1001, 1002, 1003, 1004, 1005]);
        let (events, gap, _, _) = log.since(None);
        assert!(!gap && events.is_empty());

        let mut big = Log::default();
        let chunk = "x".repeat(REPLAY_BYTES / 3);
        for _ in 0..4 {
            big.push(json!({"text": chunk}));
        }
        assert!(big.bytes <= REPLAY_BYTES);
        assert_eq!(big.events.len(), 2);
    }

    #[test]
    fn limits_are_what_the_docs_say() {
        assert_eq!(REPLAY_EVENTS, 1000);
        assert_eq!(REPLAY_BYTES, 4 * 1024 * 1024);
        assert_eq!(PERMISSION_TIMEOUT, Duration::from_secs(600));
        assert!(!idle_for_exit(Duration::from_secs(29 * 60), 0, 0));
        assert!(idle_for_exit(AGENT_IDLE_EXIT, 0, 0));
        assert!(!idle_for_exit(AGENT_IDLE_EXIT, 1, 0));
        assert!(!idle_for_exit(AGENT_IDLE_EXIT, 0, 1));
    }

    #[test]
    fn agents_are_name_equals_command() {
        let parsed = parse_agents(&["bot=agent --acp".into()]).unwrap();
        assert_eq!(parsed, [("bot".to_owned(), "agent --acp".to_owned())]);
        assert!(parse_agents(&["nameonly".into()]).is_err());
        assert!(parse_agents(&["=cmd".into()]).is_err());
    }

    /// A stand-in ACP agent: one session; a prompt streams five chunks, asks for
    /// permission, then streams the answer and ends the turn.
    const FAKE_AGENT: &str = r#"
import json, sys, time
def send(m): print(json.dumps(m), flush=True)
def chunk(t): send({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":t}}}})
prompt = None
for line in sys.stdin:
    m = json.loads(line)
    if m.get("method") == "initialize":
        assert m["params"]["clientCapabilities"]["terminal"] is False
        send({"jsonrpc":"2.0","id":m["id"],"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":True}}})
    elif m.get("method") == "session/new":
        send({"jsonrpc":"2.0","id":m["id"],"result":{"sessionId":"s1"}})
    elif m.get("method") == "session/prompt":
        prompt = m["id"]
        for i in range(5):
            chunk(f"c{i} "); time.sleep(0.1)
        send({"jsonrpc":"2.0","id":"p1","method":"session/request_permission","params":{"sessionId":"s1","toolCall":{"toolCallId":"t"},"options":[{"optionId":"ok","name":"Allow","kind":"allow_once"}]}})
    elif m.get("id") == "p1":
        chunk(m["result"]["outcome"]["optionId"])
        send({"jsonrpc":"2.0","id":prompt,"result":{"stopReason":"end_turn"}})
"#;

    #[tokio::test]
    async fn a_turn_continues_while_no_client_is_attached_and_replays_without_gaps() {
        use tokio::{io::AsyncBufReadExt, net::UnixStream};
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("agent.py");
        std::fs::write(&script, FAKE_AGENT).unwrap();
        let socket = dir.path().join("a.sock");
        let agents = vec![("fake".to_owned(), format!("python3 {}", script.display()))];
        tokio::spawn({
            let socket = socket.clone();
            async move { run(&socket, agents).await.unwrap() }
        });
        while !socket.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        type Lines = tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>;
        async fn open(socket: &std::path::Path) -> (Lines, tokio::net::unix::OwnedWriteHalf) {
            let (read, write) = UnixStream::connect(socket).await.unwrap().into_split();
            (BufReader::new(read).lines(), write)
        }
        async fn send(write: &mut tokio::net::unix::OwnedWriteHalf, msg: Value) {
            write
                .write_all(format!("{msg}\n").as_bytes())
                .await
                .unwrap();
        }
        async fn next(lines: &mut Lines) -> Value {
            let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line());
            serde_json::from_str(&line.await.unwrap().unwrap().unwrap()).unwrap()
        }

        let (mut lines, mut write) = open(&socket).await;
        send(&mut write, json!({"id":1,"op":"agents"})).await;
        let agents = next(&mut lines).await;
        assert_eq!(
            agents["result"]["agents"][0]["init"]["agentCapabilities"]["loadSession"],
            true
        );
        send(
            &mut write,
            json!({"id":2,"op":"call","agent":"fake","method":"session/new","params":{}}),
        )
        .await;
        assert_eq!(next(&mut lines).await["result"]["sessionId"], "s1");
        send(
            &mut write,
            json!({"id":3,"op":"attach","agent":"fake","session":"s1"}),
        )
        .await;
        assert_eq!(next(&mut lines).await["result"]["latest"], 0);
        send(&mut write, json!({"id":4,"op":"call","agent":"fake","method":"session/prompt","params":{"sessionId":"s1","prompt":[]}})).await;
        let first = next(&mut lines).await;
        assert_eq!(first["seq"], 1);
        // The client goes away mid-reply; the turn goes on without it.
        drop((lines, write));
        tokio::time::sleep(Duration::from_millis(700)).await;

        let (mut lines, mut write) = open(&socket).await;
        send(
            &mut write,
            json!({"id":5,"op":"attach","agent":"fake","session":"s1","after":1}),
        )
        .await;
        let attached = next(&mut lines).await;
        assert_eq!(attached["result"]["gap"], false);
        // Replayed then live, with no gap or repeat, up to the permission request.
        let mut seqs = vec![1];
        let mut request = Value::Null;
        while request.is_null() {
            let event = next(&mut lines).await;
            seqs.push(event["seq"].as_u64().unwrap());
            if event["msg"]["method"] == "session/request_permission" {
                request = event["msg"]["id"].clone();
            }
        }
        assert_eq!(seqs, [1, 2, 3, 4, 5, 6]);
        let answer = json!({"outcome":{"outcome":"selected","optionId":"ok"}});
        send(
            &mut write,
            json!({"op":"respond","agent":"fake","request":request,"result":answer}),
        )
        .await;
        let resolved = next(&mut lines).await;
        assert_eq!(
            (resolved["seq"].as_u64(), &resolved["resolved"]),
            (Some(7), &json!("p1"))
        );
        let chunk = next(&mut lines).await;
        assert_eq!(chunk["msg"]["params"]["update"]["content"]["text"], "ok");
        let end = next(&mut lines).await;
        assert_eq!(
            (end["seq"].as_u64(), &end["end"]["result"]["stopReason"]),
            (Some(9), &json!("end_turn"))
        );
    }
}
