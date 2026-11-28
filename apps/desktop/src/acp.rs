//! Chat with the ACP agents (Agent Client Protocol) a server's `jh agent-host` keeps
//! running, through the tunnel's forwarded socket. The host owns each turn, so a turn
//! goes on while the link is down and this client re-attaches from the last event it
//! saw. What an agent can do (images, session list, load, fast mode, models) comes from
//! its own `initialize` and session config options; nothing here knows which agent it is.
use crate::chat_bridge::{AgentOptions, HostSession, ModelPreset, StreamEvent};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    time::Duration,
};

#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;
#[cfg(not(unix))]
type Stream = std::net::TcpStream;

/// The agent this window chats with, as its host described it.
#[derive(Clone, Debug, Default)]
struct Agent {
    name: String,
    title: String,
    image: bool,
    list: bool,
    load: bool,
    resume: bool,
}

#[derive(Default)]
struct State {
    agent: Option<Agent>,
    /// Why chat is unavailable, when it is.
    problem: Option<String>,
    /// The app's chat key (or a listed session id) -> the agent's session id.
    sessions: HashMap<String, String>,
    /// A session made ahead of the first turn, so it starts sooner.
    spare: Option<String>,
    /// The latest session config options (`configOptions`), for fast mode and models.
    options: Vec<Value>,
}

static SOCKET: OnceLock<PathBuf> = OnceLock::new();
static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    f(STATE.lock().unwrap().get_or_insert_with(State::default))
}

/// Chat through the agent host at `socket` (the tunnel's local end).
pub fn open(socket: PathBuf) {
    let _ = SOCKET.set(socket);
    with(|state| state.problem = Some("Connecting to the server's agent host…".into()));
    std::thread::spawn(discover);
}

pub fn enabled() -> bool {
    SOCKET.get().is_some()
}

fn connected() -> bool {
    crate::ssh::tunnel().is_none_or(|t| t.link().0 == crate::ssh::Link::Connected)
}

/// Ask the host for its agents until one answers, then make a spare session.
fn discover() {
    loop {
        if connected() {
            match first_agent() {
                Ok(agent) => {
                    let spare = new_session(&agent).ok();
                    // Ready once the spare session told us the agent's options.
                    with(|state| {
                        state.agent = Some(agent);
                        state.spare = spare;
                        state.problem = None;
                    });
                    return;
                }
                Err(err) => with(|state| state.problem = Some(err.to_string())),
            }
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

fn first_agent() -> Result<Agent> {
    let no_host = || {
        anyhow!(
            "No agent host is running on the server, so chat is unavailable; the board works as usual. Start one there with `jh service install-agents --agent NAME=COMMAND`."
        )
    };
    let reply = request(&json!({"op":"agents"})).map_err(|_| no_host())?;
    let agents = reply["agents"].as_array().cloned().unwrap_or_default();
    let mut errors = Vec::new();
    for entry in &agents {
        let name = entry["name"].as_str().unwrap_or_default().to_owned();
        let Some(init) = entry.get("init") else {
            errors.push(format!(
                "{name}: {}",
                entry["error"].as_str().unwrap_or("did not start")
            ));
            continue;
        };
        let caps = &init["agentCapabilities"];
        let session = &caps["sessionCapabilities"];
        return Ok(Agent {
            title: init["agentInfo"]["title"]
                .as_str()
                .or(init["agentInfo"]["name"].as_str())
                .unwrap_or(&name)
                .to_owned(),
            name,
            image: caps["promptCapabilities"]["image"] == true,
            list: session.get("list").is_some(),
            load: caps["loadSession"] == true,
            resume: session.get("resume").is_some(),
        });
    }
    if errors.is_empty() {
        return Err(no_host());
    }
    bail!("The server's agents did not start: {}", errors.join("; "))
}

fn connect() -> std::io::Result<Stream> {
    let socket = SOCKET.get().ok_or(std::io::ErrorKind::NotFound)?;
    #[cfg(unix)]
    return Stream::connect(socket);
    #[cfg(not(unix))]
    return Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("{} needs Unix sockets", socket.display()),
    ));
}

/// How long a request may wait for the host, which gives an agent 120 s to start.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(150);
/// How long attaching may wait for the host's answer, which it gives at once.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(15);

/// One request on its own connection; the host's `result`, or its error.
fn request(body: &Value) -> Result<Value> {
    let mut stream = connect()?;
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    let mut body = body.clone();
    body["id"] = json!(1);
    writeln!(stream, "{body}")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let reply: Value =
        serde_json::from_str(&line).context("The agent host closed the connection")?;
    match reply.get("error") {
        Some(error) => bail!("{}", error["message"].as_str().unwrap_or("Agent error")),
        None => Ok(reply["result"].clone()),
    }
}

fn call(agent: &Agent, method: &str, params: Value) -> Result<Value> {
    request(&json!({"op":"call","agent":agent.name,"method":method,"params":params}))
}

fn send(body: Value) {
    if let Ok(mut stream) = connect() {
        let _ = writeln!(stream, "{body}");
    }
}

fn keep_options(result: &Value) {
    if let Some(options) = result["configOptions"].as_array() {
        with(|state| state.options = options.clone());
    }
}

fn new_session(agent: &Agent) -> Result<String> {
    let result = call(agent, "session/new", json!({}))?;
    keep_options(&result);
    result["sessionId"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("{} did not start a session", agent.title))
}

fn agent() -> Result<Agent> {
    with(|state| {
        state.agent.clone().ok_or_else(|| {
            anyhow!(
                state
                    .problem
                    .clone()
                    .unwrap_or_else(|| "Chat is not connected yet.".into())
            )
        })
    })
}

/// A config option offering a "fast" value: (id, fast value, other value, current).
fn fast_option(options: &[Value]) -> Option<(String, String, String, String)> {
    options.iter().find_map(|option| {
        let values: Vec<&str> = option["options"]
            .as_array()?
            .iter()
            .filter_map(|choice| choice["value"].as_str())
            .collect();
        let fast = values.iter().find(|v| v.eq_ignore_ascii_case("fast"))?;
        let other = values.iter().find(|v| !v.eq_ignore_ascii_case("fast"))?;
        Some((
            option["id"].as_str()?.to_owned(),
            fast.to_string(),
            other.to_string(),
            option["currentValue"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        ))
    })
}

fn model_option(options: &[Value]) -> Option<&Value> {
    options.iter().find(|option| option["category"] == "model")
}

pub fn label() -> String {
    let target = crate::ssh::tunnel().map_or("the server", |t| t.target());
    match with(|state| state.agent.clone()) {
        Some(agent) => format!("{} on {target}", agent.title),
        None => format!("Agent on {target}"),
    }
}

/// Why chat cannot be used right now, if it cannot.
pub fn problem() -> Option<String> {
    with(|state| state.problem.clone())
}

pub fn supports_fast_mode() -> bool {
    with(|state| fast_option(&state.options).is_some())
}

pub fn supports_sessions() -> bool {
    with(|state| {
        state
            .agent
            .as_ref()
            .is_some_and(|a| a.list && (a.load || a.resume))
    })
}

pub fn model_presets() -> Vec<ModelPreset> {
    with(|state| {
        let Some(option) = model_option(&state.options) else {
            return Vec::new();
        };
        let mut presets: Vec<ModelPreset> = option["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|choice| {
                Some(ModelPreset {
                    label: choice["name"].as_str()?.to_owned(),
                    model: Some(choice["value"].as_str()?.to_owned()),
                    provider: None,
                    provider_label: None,
                })
            })
            .collect();
        // The agent's current model first: the app treats the first as the default.
        let current = option["currentValue"].as_str();
        if let Some(at) = presets.iter().position(|p| p.model.as_deref() == current) {
            presets[..=at].rotate_right(1);
        }
        presets
    })
}

pub fn list_sessions() -> Vec<HostSession> {
    let Ok(agent) = agent() else {
        return Vec::new();
    };
    let Ok(result) = call(&agent, "session/list", json!({})) else {
        return Vec::new();
    };
    let now = chrono::Utc::now();
    result["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .take(8)
        .filter_map(|session| {
            let age = session["updatedAt"]
                .as_str()
                .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                .map(|at| {
                    let mins = (now - at.with_timezone(&chrono::Utc)).num_minutes().max(0);
                    match mins {
                        0 => "just now".to_owned(),
                        1..60 => format!("{mins}m ago"),
                        60..1440 => format!("{}h ago", mins / 60),
                        _ => format!("{}d ago", mins / 1440),
                    }
                })
                .unwrap_or_default();
            Some(HostSession {
                id: session["sessionId"].as_str()?.to_owned(),
                title: session["title"].as_str().unwrap_or("Untitled").to_owned(),
                age_label: age,
                message_count: 0,
            })
        })
        .collect()
}

/// Answer a permission request shown in the chat panel.
pub fn respond(request: &str, option: Option<&str>) {
    let Ok(request) = serde_json::from_str::<Value>(request) else {
        return;
    };
    let outcome = match option {
        Some(id) => json!({"outcome":"selected","optionId":id}),
        None => json!({"outcome":"cancelled"}),
    };
    send(
        json!({"op":"respond","agent":request["agent"],"request":request["id"],
        "result":{"outcome":outcome}}),
    );
}

/// The agent's session for this chat: made, loaded or resumed as needed.
fn session(agent: &Agent, opts: &AgentOptions) -> Result<String> {
    let key = opts.session_name.clone().unwrap_or_default();
    if let Some(id) = with(|state| state.sessions.get(&key).cloned()) {
        return Ok(id);
    }
    let id = if opts.resume_existing {
        let method = if agent.resume {
            "session/resume"
        } else if agent.load {
            "session/load"
        } else {
            bail!("{} cannot reopen a session", agent.title);
        };
        keep_options(&call(agent, method, json!({"sessionId": key}))?);
        key.clone()
    } else {
        match with(|state| state.spare.take()) {
            Some(spare) => spare,
            None => new_session(agent)?,
        }
    };
    with(|state| state.sessions.insert(key, id.clone()));
    Ok(id)
}

fn set_option(agent: &Agent, session: &str, id: &str, value: &str) -> Result<()> {
    let params = json!({"sessionId":session,"configId":id,"value":value});
    keep_options(&call(agent, "session/set_config_option", params)?);
    Ok(())
}

/// Text plus each attachment as an image block, where the agent takes images.
fn prompt_blocks(agent: &Agent, prompt: &str, attachments: &[PathBuf]) -> Result<Vec<Value>> {
    let mut blocks = vec![json!({"type":"text","text":prompt})];
    for path in attachments {
        let mime = image_mime(path).filter(|_| agent.image).ok_or_else(|| {
            anyhow!(
                "{} takes {} here; paste the text of {} into the message instead.",
                agent.title,
                if agent.image {
                    "images only"
                } else {
                    "text only"
                },
                path.display()
            )
        })?;
        let data = std::fs::read(path).with_context(|| format!("Read {}", path.display()))?;
        let data = base64::engine::general_purpose::STANDARD.encode(data);
        blocks.push(json!({"type":"image","mimeType":mime,"data":data}));
    }
    Ok(blocks)
}

fn image_mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => return None,
    })
}

const LOST: &str =
    "Part of this reply was lost while offline; reopen the session from the list to see all of it.";

/// Follow one session's events from the host, from `after` (or from now).
struct Feed {
    reader: BufReader<Stream>,
    line: Vec<u8>,
    last: u64,
    /// The host's run; another one means it restarted and this turn is gone.
    boot: Value,
}

impl Feed {
    fn attach(agent: &Agent, session: &str, after: Option<u64>) -> Result<Self> {
        let mut stream = connect()?;
        let body = json!({"id":1,"op":"attach","agent":agent.name,"session":session,"after":after});
        writeln!(stream, "{body}")?;
        stream.set_read_timeout(Some(ATTACH_TIMEOUT))?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let reply: Value =
            serde_json::from_str(&line).context("The agent host closed the connection")?;
        if reply["result"]["gap"] == true {
            bail!(LOST);
        }
        reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(250)))?;
        Ok(Self {
            reader,
            line: Vec::new(),
            last: after.unwrap_or_else(|| reply["result"]["latest"].as_u64().unwrap_or(0)),
            boot: reply["result"]["boot"].clone(),
        })
    }

    /// The next event; `Ok(None)` when none arrived in time, `Err` when the link broke.
    fn next(&mut self) -> std::io::Result<Option<Value>> {
        match self.reader.read_until(b'\n', &mut self.line) {
            Ok(0) => Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(_) if self.line.ends_with(b"\n") => {
                let event: Value = serde_json::from_slice(&self.line).unwrap_or_default();
                self.line.clear();
                self.last = event["seq"].as_u64().unwrap_or(self.last);
                Ok(Some(event))
            }
            Ok(_) => Ok(None),
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }
}

/// One turn: the agent's updates become the panel's stream events.
pub fn stream_turn(
    prompt: &str,
    attachments: &[PathBuf],
    opts: &AgentOptions,
    tx: &Sender<StreamEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<()> {
    let agent = agent()?;
    let blocks = prompt_blocks(&agent, prompt, attachments)?;
    let session = session(&agent, opts)?;
    let options = with(|state| state.options.clone());
    if let Some((id, fast, other, current)) = fast_option(&options) {
        let want = if opts.fast_mode { fast } else { other };
        if !current.eq_ignore_ascii_case(&want)
            && (opts.fast_mode || current.eq_ignore_ascii_case("fast"))
        {
            set_option(&agent, &session, &id, &want)?;
        }
    }
    // A model this agent offers; one saved for another agent is left alone.
    if let (Some(option), Some(model)) = (model_option(&options), opts.model.as_deref())
        && option["currentValue"].as_str() != Some(model)
        && option["options"]
            .as_array()
            .is_some_and(|choices| choices.iter().any(|c| c["value"] == model))
        && let Some(id) = option["id"].as_str()
    {
        set_option(&agent, &session, id, model)?;
    }
    let mut feed = Feed::attach(&agent, &session, None)?;
    // The host runs the prompt; its end arrives on the feed, even after a reconnect,
    // named with this turn so a cancelled turn's late end is not taken for this one's.
    let turn = json!(uuid::Uuid::new_v4().to_string());
    let mut prompt_stream = connect()?;
    let body = json!({"id":1,"op":"call","agent":agent.name,"method":"session/prompt",
        "turn":turn,"params":{"sessionId":session,"prompt":blocks}});
    writeln!(prompt_stream, "{body}")?;
    std::thread::spawn(move || {
        let mut ignored = String::new();
        let _ = BufReader::new(prompt_stream).read_line(&mut ignored);
    });
    loop {
        if cancel.load(Ordering::Relaxed) {
            send(
                json!({"op":"notify","agent":agent.name,"method":"session/cancel",
                "params":{"sessionId":session}}),
            );
            return Ok(());
        }
        match feed.next() {
            Ok(Some(event)) if !of_turn(&event, &turn) => {}
            Ok(Some(event)) => {
                for out in events(&event) {
                    let _ = tx.send(out);
                }
                if event.get("end").is_some() {
                    return Ok(());
                }
            }
            Ok(None) => {}
            Err(_) => {
                let _ = tx.send(StreamEvent::Status("Reconnecting…".into()));
                feed = loop {
                    if cancel.load(Ordering::Relaxed) {
                        break feed;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                    if !connected() {
                        continue;
                    }
                    match Feed::attach(&agent, &session, Some(feed.last)) {
                        Ok(next) if next.boot != feed.boot => bail!(LOST),
                        Ok(next) => {
                            let _ = tx.send(StreamEvent::Status("Thinking…".into()));
                            break next;
                        }
                        Err(err) if err.to_string() == LOST => return Err(err),
                        Err(_) => {}
                    }
                };
            }
        }
    }
}

/// Whether `event` belongs to `turn`: only an end names its turn (a host before 0.5.1
/// names none).
fn of_turn(event: &Value, turn: &Value) -> bool {
    event.get("end").is_none() || event["turn"].is_null() || event["turn"] == *turn
}

fn text_of(content: &Value) -> String {
    match content {
        Value::Array(items) => items.iter().map(text_of).collect::<Vec<_>>().join("\n"),
        Value::Object(_) if content.get("content").is_some() => text_of(&content["content"]),
        _ => content["text"].as_str().unwrap_or_default().to_owned(),
    }
}

/// What one host event means for the panel.
fn events(event: &Value) -> Vec<StreamEvent> {
    if let Some(end) = event.get("end") {
        if let Some(error) = end.get("error") {
            let message = error["message"].as_str().unwrap_or("The agent failed");
            return vec![StreamEvent::Error(message.to_owned())];
        }
        return end["result"]["usage"]["totalTokens"]
            .as_u64()
            .map(|total_tokens| StreamEvent::Usage { total_tokens })
            .into_iter()
            .collect();
    }
    if event.get("resolved").is_some() {
        return vec![StreamEvent::PermissionDone];
    }
    let msg = &event["msg"];
    if msg["method"] == "session/request_permission" {
        let params = &msg["params"];
        let options = params["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|o| {
                Some((
                    o["optionId"].as_str()?.to_owned(),
                    o["name"].as_str()?.to_owned(),
                ))
            })
            .collect();
        return vec![StreamEvent::Permission {
            request: json!({"agent":event["agent"],"id":msg["id"]}).to_string(),
            title: params["toolCall"]["title"]
                .as_str()
                .unwrap_or("The agent asks for permission")
                .to_owned(),
            options,
        }];
    }
    let update = &msg["params"]["update"];
    let id = update["toolCallId"].as_str().unwrap_or_default().to_owned();
    match update["sessionUpdate"].as_str().unwrap_or_default() {
        "agent_message_chunk" => vec![StreamEvent::Delta(text_of(&update["content"]))],
        "agent_thought_chunk" => vec![StreamEvent::Thinking(text_of(&update["content"]))],
        "tool_call" => vec![StreamEvent::ToolStart {
            id,
            name: update["title"].as_str().unwrap_or("Tool").to_owned(),
            detail: text_of(&update["content"]),
        }],
        "tool_call_update" => match update["status"].as_str() {
            Some(status @ ("completed" | "failed")) => vec![StreamEvent::ToolFinish {
                id,
                ok: status == "completed",
                duration_ms: None,
                output: text_of(&update["content"]),
            }],
            _ => Vec::new(),
        },
        "config_option_update" => {
            keep_options(update);
            Vec::new()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_events_become_panel_events() {
        let chunk = json!({"seq":1,"msg":{"method":"session/update","params":{"update":{
            "sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}}}});
        assert_eq!(events(&chunk), [StreamEvent::Delta("hi".into())]);
        let tool = json!({"msg":{"params":{"update":{"sessionUpdate":"tool_call_update",
            "toolCallId":"t","status":"completed","content":[{"type":"content","content":{"type":"text","text":"ok"}}]}}}});
        assert_eq!(
            events(&tool),
            [StreamEvent::ToolFinish {
                id: "t".into(),
                ok: true,
                duration_ms: None,
                output: "ok".into()
            }]
        );
        let ask = json!({"agent":"bot","msg":{"id":7,"method":"session/request_permission","params":{
            "toolCall":{"title":"Run date"},"options":[{"optionId":"y","name":"Allow"}]}}});
        assert_eq!(
            events(&ask),
            [StreamEvent::Permission {
                request: r#"{"agent":"bot","id":7}"#.into(),
                title: "Run date".into(),
                options: vec![("y".into(), "Allow".into())],
            }]
        );
        let end = json!({"end":{"result":{"stopReason":"end_turn","usage":{"totalTokens":12}}}});
        assert_eq!(events(&end), [StreamEvent::Usage { total_tokens: 12 }]);
        let failed = json!({"end":{"error":{"message":"boom"}}});
        assert_eq!(events(&failed), [StreamEvent::Error("boom".into())]);
    }

    #[test]
    fn a_cancelled_turns_late_end_does_not_end_the_next_turn() {
        let turn = json!("b");
        let end = |turn: Value| json!({"end":{"result":{"stopReason":"end_turn"}},"turn":turn});
        assert!(!of_turn(&end(json!("a")), &turn));
        assert!(of_turn(&end(json!("b")), &turn));
        assert!(of_turn(&end(Value::Null), &turn));
        assert!(of_turn(&json!({"seq":1,"msg":{}}), &turn));
    }

    #[test]
    fn fast_mode_is_any_option_offering_fast() {
        let options = [
            json!({"id":"service_tier","currentValue":"normal","options":[
            {"value":"normal"},{"value":"fast"}]}),
        ];
        assert_eq!(
            fast_option(&options),
            Some((
                "service_tier".into(),
                "fast".into(),
                "normal".into(),
                "normal".into()
            ))
        );
        assert_eq!(
            fast_option(&[json!({"id":"x","options":[{"value":"a"}]})]),
            None
        );
    }
}
