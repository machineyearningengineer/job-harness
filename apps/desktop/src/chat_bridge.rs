//! Zero-competition streaming chat bridge to the user's real agent runtime
//! (Hermes / Claude Code / Codex). Streams live chunks, supports mid-turn cancel,
//! and never maintains a duplicate competing chat database.
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentRuntime {
    Hermes { binary: PathBuf, profile: String },
    ClaudeCode { binary: PathBuf },
    Codex { binary: PathBuf },
    Custom { name: String, command: String },
    Unconfigured,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamEvent {
    Status(String),
    ToolStart {
        id: String,
        name: String,
        detail: String,
    },
    ToolFinish {
        id: String,
        ok: bool,
        duration_ms: Option<u64>,
        output: String,
    },
    Thinking(String),
    Delta(String),
    Usage {
        total_tokens: u64,
    },
    Done,
    Error(String),
}

pub fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .is_some_and(|ext| matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif"))
}

/// Per-turn model controls forwarded to the host agent CLI (never stored in the ledger).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentOptions {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub reasoning: Option<String>,
}

/// A selectable model preset discovered from the runtime's own configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelPreset {
    pub label: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub provider_label: Option<String>,
}

fn find_hermes() -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    for bin in [
        PathBuf::from(&home).join(".local/bin/hermes"),
        PathBuf::from("/usr/local/bin/hermes"),
        PathBuf::from("/opt/homebrew/bin/hermes"),
    ] {
        if bin.is_file() {
            return Some(bin);
        }
    }
    which_in_path("hermes")
}

fn hermes_profile() -> String {
    std::env::var("JOB_HARNESS_HERMES_PROFILE").unwrap_or_else(|_| "career-chief".into())
}

impl AgentRuntime {
    /// Every host agent CLI available on this machine, in preference order.
    pub fn detect_all() -> Vec<Self> {
        let mut out = Vec::new();
        if let Some(binary) = find_hermes() {
            out.push(Self::Hermes {
                binary,
                profile: hermes_profile(),
            });
        }
        if let Some(binary) = which_in_path("claude") {
            out.push(Self::ClaudeCode { binary });
        }
        if let Some(binary) = which_in_path("codex") {
            out.push(Self::Codex { binary });
        }
        if let Ok(cmd) = std::env::var("JOB_HARNESS_AGENT_CMD")
            && !cmd.trim().is_empty()
        {
            let name = std::env::var("JOB_HARNESS_AGENT_NAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "Custom".into());
            out.push(Self::Custom {
                name,
                command: cmd.trim().to_owned(),
            });
        }
        out
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hermes { .. } => "hermes",
            Self::ClaudeCode { .. } => "claude",
            Self::Codex { .. } => "codex",
            Self::Custom { .. } => "custom",
            Self::Unconfigured => "none",
        }
    }

    pub fn short_name(&self) -> &str {
        match self {
            Self::Hermes { .. } => "Hermes",
            Self::ClaudeCode { .. } => "Claude",
            Self::Codex { .. } => "Codex",
            Self::Custom { name, .. } => name.as_str(),
            Self::Unconfigured => "No agent",
        }
    }

    pub fn supports_reasoning(&self) -> bool {
        matches!(self, Self::Hermes { .. })
    }

    /// Model presets read from the runtime's own config (Hermes profile) or CLI aliases.
    pub fn model_presets(&self) -> Vec<ModelPreset> {
        match self {
            Self::Hermes { profile, .. } => hermes_presets(profile),
            Self::ClaudeCode { .. } => ["sonnet", "opus", "haiku"]
                .into_iter()
                .map(|m| ModelPreset {
                    label: m.to_owned(),
                    model: Some(m.to_owned()),
                    provider: None,
                    provider_label: Some("Anthropic".into()),
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The model the runtime uses when no override is set (if discoverable).
    pub fn default_model(&self) -> Option<String> {
        match self {
            Self::Hermes { profile, .. } => hermes_presets(profile)
                .into_iter()
                .next()
                .and_then(|p| p.model),
            _ => None,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Hermes { profile, .. } => format!("Hermes ({profile})"),
            Self::ClaudeCode { .. } => "Claude Code".into(),
            Self::Codex { .. } => "Codex".into(),
            Self::Custom { name, .. } => format!("{name} (custom)"),
            Self::Unconfigured => "Not connected".into(),
        }
    }

    pub fn is_ready(&self) -> bool {
        !matches!(self, Self::Unconfigured)
    }

    /// Streams one turn directly from the host agent runtime with cancellation and media support.
    pub fn stream_turn(
        &self,
        workspace: Option<PathBuf>,
        prompt: String,
        attachments: Vec<PathBuf>,
        opts: AgentOptions,
        tx: Sender<StreamEvent>,
        cancel: Arc<AtomicBool>,
    ) {
        let runtime = self.clone();
        std::thread::spawn(move || {
            if let Err(err) = run_streaming(
                &runtime,
                workspace.as_deref(),
                &prompt,
                &attachments,
                &opts,
                &tx,
                &cancel,
            ) {
                let _ = tx.send(StreamEvent::Error(err.to_string()));
            }
            let _ = tx.send(StreamEvent::Done);
        });
    }
}

fn run_streaming(
    runtime: &AgentRuntime,
    workspace: Option<&Path>,
    prompt: &str,
    attachments: &[PathBuf],
    opts: &AgentOptions,
    tx: &Sender<StreamEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<()> {
    let mut final_prompt = prompt.trim().to_owned();
    if final_prompt.is_empty() && attachments.is_empty() {
        bail!("Empty message");
    }
    let mut non_native_files = Vec::new();
    let (mut cmd, json_stream) = match runtime {
        AgentRuntime::Hermes { binary, profile } => {
            let mut c = Command::new(binary);
            c.args([
                "-p",
                profile,
                "chat",
                "--query-file",
                "-",
                "--oneshot",
                "-Q",
                "--format",
                "stream-json",
                "--continue",
                "career-gui",
                "--create-if-missing",
            ]);
            if let Some(dir) = workspace {
                c.arg("--in").arg(dir);
            }
            if let Some(m) = opts.model.as_deref().filter(|m| !m.is_empty()) {
                c.args(["-m", m]);
            }
            if let Some(p) = opts.provider.as_deref().filter(|p| !p.is_empty()) {
                c.args(["--provider", p]);
            }
            if let Some(r) = opts.reasoning.as_deref().filter(|r| !r.is_empty()) {
                c.args(["--reasoning", r]);
            }
            for att in attachments {
                if is_image_path(att) {
                    c.arg("--image").arg(att);
                } else {
                    non_native_files.push(att.clone());
                }
            }
            (c, true)
        }
        AgentRuntime::ClaudeCode { binary } => {
            let mut c = Command::new(binary);
            c.args([
                "-p",
                "--continue",
                "--verbose",
                "--output-format",
                "stream-json",
                "--include-partial-messages",
            ]);
            if let Some(m) = opts.model.as_deref().filter(|m| !m.is_empty()) {
                c.args(["--model", m]);
            }
            if let Some(dir) = workspace {
                c.current_dir(dir);
            }
            non_native_files.extend(attachments.iter().cloned());
            (c, true)
        }
        AgentRuntime::Codex { binary } => {
            let mut c = Command::new(binary);
            c.args(["exec", "--json"]);
            if let Some(m) = opts.model.as_deref().filter(|m| !m.is_empty()) {
                c.args(["-m", m]);
            }
            c.arg("-");
            if let Some(dir) = workspace {
                c.current_dir(dir);
            }
            non_native_files.extend(attachments.iter().cloned());
            (c, true)
        }
        AgentRuntime::Custom { command, .. } => {
            let mut c = Command::new("sh");
            c.args(["-lc", command]);
            if let Some(dir) = workspace {
                c.current_dir(dir);
            }
            if !attachments.is_empty() {
                let joined = attachments
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(":");
                c.env("JOB_HARNESS_ATTACHMENTS", joined);
            }
            non_native_files.extend(attachments.iter().cloned());
            (c, true)
        }
        AgentRuntime::Unconfigured => {
            bail!(
                "No host agent runtime detected (Hermes, Claude Code, Codex, or JOB_HARNESS_AGENT_CMD). Install one or run setup from your agent."
            );
        }
    };

    if !non_native_files.is_empty() {
        let lines = non_native_files
            .iter()
            .map(|p| format!("[Attached file: {}]", p.display()))
            .collect::<Vec<_>>()
            .join("\n");
        if final_prompt.is_empty() {
            final_prompt = format!("Inspect the attached file(s):\n{lines}");
        } else {
            final_prompt = format!("{final_prompt}\n\n{lines}");
        }
    } else if final_prompt.is_empty() {
        final_prompt = "Inspect the attached image(s).".into();
    }

    let _ = tx.send(StreamEvent::Status("Thinking…".into()));
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to launch agent runtime")?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(final_prompt.as_bytes());
    }

    let stdout = child.stdout.take().context("Missing stdout pipe")?;
    let reader = BufReader::new(stdout);
    let mut emitted_any = false;
    let mut saw_incremental_delta = false;

    for line in reader.lines() {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = tx.send(StreamEvent::Status("Stopped.".into()));
            return Ok(());
        }
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if json_stream && let Ok(v) = serde_json::from_str::<Value>(&line) {
            for ev in parse_stream_json_value(&v, &mut saw_incremental_delta) {
                if matches!(ev, StreamEvent::Delta(_)) {
                    emitted_any = true;
                }
                let _ = tx.send(ev);
            }
            continue;
        }
        emitted_any = true;
        let _ = tx.send(StreamEvent::Delta(format!("{line}\n")));
    }

    let status = child.wait()?;
    if !status.success() && !cancel.load(Ordering::Relaxed) {
        let mut err_buf = String::new();
        if let Some(mut se) = child.stderr.take() {
            let _ = std::io::Read::read_to_string(&mut se, &mut err_buf);
        }
        let msg = err_buf.trim();
        if !msg.is_empty() {
            bail!("{msg}");
        }
        if !emitted_any {
            bail!("Agent exited with status {status}");
        }
    }
    Ok(())
}

/// Parses one NDJSON line from Hermes `stream-json`, Claude Code `stream-json`,
/// Codex `--json`, or OpenAI-style delta streams into typed live UI events.
fn parse_stream_json_value(v: &Value, saw_incremental_delta: &mut bool) -> Vec<StreamEvent> {
    let mut out = Vec::new();
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("");

    match kind {
        // Hermes & Anthropic tool invocations
        "tool_use" => {
            let name = v
                .get("name")
                .or_else(|| v.get("tool"))
                .or_else(|| v.get("tool_name"))
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_owned();
            let id = v
                .get("tool_call_id")
                .or_else(|| v.get("id"))
                .and_then(Value::as_str)
                .unwrap_or(&name)
                .to_owned();
            let detail = summarize_tool_input(&name, v.get("input").or_else(|| v.get("arguments")));
            out.push(StreamEvent::Status(format!("Running {name}…")));
            out.push(StreamEvent::ToolStart { id, name, detail });
            return out;
        }
        "tool_result" => {
            let id = v
                .get("tool_call_id")
                .or_else(|| v.get("id"))
                .or_else(|| v.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_owned();
            let ok = !v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
            let duration_ms = v.get("duration_ms").and_then(Value::as_u64);
            let output = extract_tool_output(v.get("output").or_else(|| v.get("content")));
            out.push(StreamEvent::ToolFinish {
                id,
                ok,
                duration_ms,
                output,
            });
            out.push(StreamEvent::Status("Thinking…".into()));
            return out;
        }
        // Hermes live text chunks (`{"type": "text", "text": "..."}`)
        "text" => {
            if let Some(s) = v.get("text").and_then(Value::as_str)
                && !s.is_empty()
            {
                *saw_incremental_delta = true;
                out.push(StreamEvent::Delta(s.to_owned()));
            }
            return out;
        }
        "thinking" | "reasoning" => {
            if let Some(s) = v
                .get("text")
                .or_else(|| v.get("thinking"))
                .or_else(|| v.get("content"))
                .and_then(Value::as_str)
                && !s.is_empty()
            {
                out.push(StreamEvent::Thinking(s.to_owned()));
            }
            return out;
        }
        // Final summary object: emit token usage if reported, and text only if no deltas streamed
        "result" => {
            if let Some(total) = extract_total_tokens(v) {
                out.push(StreamEvent::Usage {
                    total_tokens: total,
                });
            }
            if !*saw_incremental_delta
                && let Some(s) = v
                    .get("text")
                    .or_else(|| v.get("result"))
                    .or_else(|| v.get("response"))
                    .and_then(Value::as_str)
                && !s.is_empty()
            {
                out.push(StreamEvent::Delta(s.to_owned()));
            }
            return out;
        }
        // Claude Code `--include-partial-messages` wrapper
        "stream_event" => {
            if let Some(ev) = v.get("event") {
                if let Some(s) = ev.pointer("/delta/text").and_then(Value::as_str)
                    && !s.is_empty()
                {
                    *saw_incremental_delta = true;
                    out.push(StreamEvent::Delta(s.to_owned()));
                } else if let Some(s) = ev.pointer("/delta/thinking").and_then(Value::as_str)
                    && !s.is_empty()
                {
                    out.push(StreamEvent::Thinking(s.to_owned()));
                } else if ev.get("type").and_then(Value::as_str) == Some("content_block_start")
                    && ev.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use")
                {
                    let name = ev
                        .pointer("/content_block/name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_owned();
                    let id = ev
                        .pointer("/content_block/id")
                        .and_then(Value::as_str)
                        .unwrap_or(&name)
                        .to_owned();
                    out.push(StreamEvent::Status(format!("Running {name}…")));
                    out.push(StreamEvent::ToolStart {
                        id,
                        name,
                        detail: String::new(),
                    });
                }
            }
            return out;
        }
        // Claude Code assistant message block (fallback when partial chunks were absent)
        "assistant" => {
            if let Some(arr) = v.pointer("/message/content").and_then(Value::as_array) {
                for block in arr {
                    match block.get("type").and_then(Value::as_str) {
                        Some("tool_use") => {
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .to_owned();
                            let id = block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or(&name)
                                .to_owned();
                            let detail = summarize_tool_input(&name, block.get("input"));
                            out.push(StreamEvent::Status(format!("Running {name}…")));
                            out.push(StreamEvent::ToolStart { id, name, detail });
                        }
                        Some("text") if !*saw_incremental_delta => {
                            if let Some(s) = block.get("text").and_then(Value::as_str)
                                && !s.is_empty()
                            {
                                *saw_incremental_delta = true;
                                out.push(StreamEvent::Delta(s.to_owned()));
                            }
                        }
                        _ => {}
                    }
                }
            }
            return out;
        }
        // Codex `--json` events (`item.started`, `item.completed`)
        "item.started" | "item.completed" => {
            if let Some(item) = v.get("item") {
                let itype = item.get("type").and_then(Value::as_str).unwrap_or("");
                let id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(itype)
                    .to_owned();
                if itype == "command_execution" {
                    let cmd = item
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    if kind == "item.started" {
                        out.push(StreamEvent::Status("Running command…".into()));
                        out.push(StreamEvent::ToolStart {
                            id,
                            name: "shell".into(),
                            detail: cmd,
                        });
                    } else {
                        let ok = item.get("exit_code").and_then(Value::as_i64).unwrap_or(0) == 0;
                        let output = extract_tool_output(
                            item.get("aggregated_output").or_else(|| item.get("output")),
                        );
                        out.push(StreamEvent::ToolFinish {
                            id,
                            ok,
                            duration_ms: None,
                            output,
                        });
                    }
                } else if itype == "reasoning"
                    && let Some(t) = item.get("text").and_then(Value::as_str)
                {
                    out.push(StreamEvent::Thinking(t.to_owned()));
                } else if itype == "agent_message"
                    && kind == "item.completed"
                    && let Some(t) = item.get("text").and_then(Value::as_str)
                    && !t.is_empty()
                {
                    *saw_incremental_delta = true;
                    out.push(StreamEvent::Delta(t.to_owned()));
                }
            }
            return out;
        }
        "turn.completed" => {
            if let Some(total) = extract_total_tokens(v) {
                out.push(StreamEvent::Usage {
                    total_tokens: total,
                });
            }
            return out;
        }
        "system" | "thread.started" | "turn.started" => return out,
        _ => {}
    }

    // Generic fallback for OpenAI-compatible or custom JSON streams
    if let Some(s) = v.get("delta").and_then(Value::as_str).or_else(|| {
        v.pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
    }) && !s.is_empty()
    {
        *saw_incremental_delta = true;
        out.push(StreamEvent::Delta(s.to_owned()));
    } else if let Some(tool) = v
        .get("tool")
        .or_else(|| v.get("tool_name"))
        .and_then(Value::as_str)
    {
        out.push(StreamEvent::Status(format!("Running {tool}…")));
        out.push(StreamEvent::ToolStart {
            id: tool.to_owned(),
            name: tool.to_owned(),
            detail: summarize_tool_input(tool, v.get("input").or_else(|| v.get("arguments"))),
        });
    } else if !*saw_incremental_delta {
        for key in ["response", "message", "content", "text"] {
            if let Some(s) = v.get(key).and_then(Value::as_str)
                && !s.is_empty()
            {
                out.push(StreamEvent::Delta(s.to_owned()));
                break;
            }
        }
    }
    out
}

fn extract_tool_output(val: Option<&Value>) -> String {
    let Some(v) = val else {
        return String::new();
    };
    // Unwrap Hermes JSON-encoded terminal wrapper: `{"output": "...", "exit_code": 0}`
    // Note: Hermes often pre-truncates `tool_result` previews mid-string, so handle both
    // complete JSON AND truncated `{"output": "{\"...` envelopes cleanly!
    let mut unwrapped = if let Some(s) = v.as_str() {
        if let Ok(inner) = serde_json::from_str::<Value>(s) {
            if let Some(out) = inner.get("output").and_then(Value::as_str) {
                out.to_owned()
            } else {
                s.to_owned()
            }
        } else if let Some(rest) = s.trim_start().strip_prefix("{\"output\":") {
            let body = rest.trim_start().strip_prefix('"').unwrap_or(rest);
            let body = body.strip_suffix("\"}").unwrap_or(body);
            unescape_partial_json_str(body)
        } else {
            s.to_owned()
        }
    } else if let Some(out) = v.get("output").and_then(Value::as_str) {
        out.to_owned()
    } else {
        v.to_string()
    };

    // Unwrap a second JSON layer if the command itself printed a job-harness v2 envelope
    if let Ok(parsed) = serde_json::from_str::<Value>(unwrapped.trim()) {
        if let Some(summary) = summarize_json_tool_result(&parsed) {
            return summary;
        }
        if let Ok(pretty) = serde_json::to_string_pretty(&parsed) {
            unwrapped = pretty;
        }
    } else if unwrapped.trim_start().starts_with('{') || unwrapped.trim_start().starts_with('[') {
        // Even on truncated JSON previews from Hermes, extract clean readable formatting!
        if let Some(summary) = summarize_partial_batch_json(&unwrapped) {
            return summary;
        }
        unwrapped = pretty_format_partial_json(unwrapped.trim());
    }

    let trimmed = unwrapped.trim();
    if trimmed.chars().count() <= 1200 {
        trimmed.to_owned()
    } else {
        let head: String = trimmed.chars().take(1200).collect();
        format!("{head}\n…")
    }
}

fn unescape_partial_json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('n') => out.push('\n'),
                Some('r') => {}
                Some('t') => out.push_str("  "),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => break,
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn summarize_partial_batch_json(src: &str) -> Option<String> {
    if !src.contains("\"schema_version\":2") || !src.contains("\"results\"") {
        return None;
    }
    let mut tools: Vec<String> = Vec::new();
    let mut rest = src;
    while let Some(idx) = rest.find("\"tool\":\"") {
        let after = &rest[idx + 8..];
        if let Some(end) = after.find('"') {
            let name = &after[..end];
            if !name.is_empty() && !tools.iter().any(|t| t == name) {
                tools.push(name.to_owned());
            }
            rest = &after[end + 1..];
        } else {
            break;
        }
    }
    if tools.is_empty() {
        return None;
    }
    let mut lines: Vec<String> = tools.into_iter().map(|t| format!("✓ {t}")).collect();
    if let Some(idx) = src.find("\"today_local\":\"") {
        let after = &src[idx + 15..];
        if let Some(end) = after.find('"') {
            lines.push(format!("Ledger date: {}", &after[..end]));
        }
    }
    Some(lines.join("\n"))
}

fn pretty_format_partial_json(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + 64);
    let mut indent = 0usize;
    let mut in_str = false;
    let mut esc = false;
    for ch in src.chars() {
        if in_str {
            out.push(ch);
            if esc {
                esc = false;
            } else if ch == '\\' {
                esc = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => {
                in_str = true;
                out.push('"');
            }
            '{' | '[' => {
                out.push(ch);
                indent += 2;
                out.push('\n');
                out.push_str(&" ".repeat(indent));
            }
            '}' | ']' => {
                indent = indent.saturating_sub(2);
                out.push('\n');
                out.push_str(&" ".repeat(indent));
                out.push(ch);
            }
            ',' => {
                out.push(',');
                out.push('\n');
                out.push_str(&" ".repeat(indent));
            }
            ':' => out.push_str(": "),
            c if c.is_ascii_whitespace() => {}
            other => out.push(other),
        }
    }
    out
}

fn summarize_json_tool_result(v: &Value) -> Option<String> {
    // Job Harness v2 query_batch / execute_batch response
    if let Some(results) = v.pointer("/data/results").and_then(Value::as_array) {
        let mut lines = Vec::new();
        for step in results {
            let tool = step["tool"].as_str().unwrap_or("step");
            let ok = step["result"]["ok"].as_bool().unwrap_or(true);
            let mark = if ok { "✓" } else { "✕" };
            let extra = if let Some(items) = step["result"]["data"]["items"].as_array() {
                format!(" ({} items)", items.len())
            } else if let Some(total) = step["result"]["data"]["health"]["record_count"].as_u64() {
                format!(" ({total} roles)")
            } else if let Some(msg) = step["result"]["error"]["message"].as_str() {
                format!(" — {msg}")
            } else {
                String::new()
            };
            lines.push(format!("{mark} {tool}{extra}"));
        }
        if !lines.is_empty() {
            return Some(lines.join("\n"));
        }
    }
    if let Some(err) = v.pointer("/error/message").and_then(Value::as_str) {
        return Some(format!("Error: {err}"));
    }
    None
}

fn extract_total_tokens(v: &Value) -> Option<u64> {
    if let Some(t) = v.pointer("/tokens/total").and_then(Value::as_u64) {
        return Some(t);
    }
    if let Some(u) = v.get("usage").or_else(|| v.get("tokens")) {
        if let Some(t) = u.get("total_tokens").and_then(Value::as_u64) {
            return Some(t);
        }
        let inp = u
            .get("input_tokens")
            .or_else(|| u.get("input"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let out = u
            .get("output_tokens")
            .or_else(|| u.get("output"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if inp + out > 0 {
            return Some(inp + out);
        }
    }
    None
}

fn summarize_tool_input(tool_name: &str, input: Option<&Value>) -> String {
    let Some(val) = input else {
        return String::new();
    };
    for key in [
        "command", "cmd", "path", "file", "query", "q", "url", "tool", "action", "record",
    ] {
        if let Some(s) = val.get(key).and_then(Value::as_str) {
            let one_line = s.lines().next().unwrap_or(s).trim();
            if !one_line.is_empty() {
                return humanize_command(one_line);
            }
        }
    }
    if (tool_name.contains("gmail") || tool_name.contains("calendar") || tool_name.contains("mail"))
        && let Some(obj) = val.as_object()
    {
        for (k, v) in obj {
            if let Some(s) = v.as_str()
                && !s.is_empty()
            {
                return format!("{k}: {}", s.lines().next().unwrap_or(s));
            }
        }
    }
    String::new()
}

fn humanize_command(cmd: &str) -> String {
    if let Some(pos) = cmd.find("jh ") {
        let rest = &cmd[pos..];
        let parts: Vec<&str> = rest.split_whitespace().collect();
        // Skip `jh --root <path>` to highlight the actual subcommand
        let mut i = 1;
        while i < parts.len() {
            if parts[i] == "--root" {
                i += 2;
            } else {
                return format!("jh {}", parts[i]);
            }
        }
        return "jh".into();
    }
    if cmd.contains("gmailUsers") {
        return "Gmail check".into();
    }
    if cmd.contains("calendarEvents") {
        return "Calendar check".into();
    }
    cmd.to_owned()
}

/// Automatically extracts canonical workspace citations referenced inside any agent reply.
pub fn extract_citations(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for raw in text.split_whitespace() {
        let token = raw.trim_matches(|c: char| {
            matches!(
                c,
                '`' | '(' | ')' | '[' | ']' | ',' | '.' | ';' | ':' | '"' | '\'' | '*'
            )
        });
        if (token.starts_with("applications/")
            || token.starts_with("briefs/")
            || token.starts_with("output/")
            || token.starts_with("context/"))
            && (token.ends_with(".md") || token.ends_with(".pdf") || token.ends_with(".json"))
            && !found.iter().any(|existing| existing == token)
        {
            found.push(token.to_owned());
        }
    }
    found.truncate(6);
    found
}

fn which_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_deduplicated_workspace_citations() {
        let reply = "Checked `applications/2026-11-23-acme.md` and (`briefs/acme.md`), plus output/tracks/backend/cv.pdf and applications/2026-11-23-acme.md.";
        let cites = extract_citations(reply);
        assert_eq!(
            cites,
            vec![
                "applications/2026-11-23-acme.md",
                "briefs/acme.md",
                "output/tracks/backend/cv.pdf"
            ]
        );
    }

    #[test]
    fn parses_hermes_tool_steps_and_deduplicates_final_result() {
        let mut saw_delta = false;
        let start = serde_json::json!({
            "type": "tool_use",
            "name": "terminal",
            "tool_call_id": "call_1",
            "input": {"command": "jh status"}
        });
        let evs = parse_stream_json_value(&start, &mut saw_delta);
        assert!(matches!(
            &evs[1],
            StreamEvent::ToolStart { id, name, detail }
            if id == "call_1" && name == "terminal" && detail == "jh status"
        ));

        let done = serde_json::json!({
            "type": "tool_result",
            "name": "terminal",
            "tool_call_id": "call_1",
            "duration_ms": 971,
            "is_error": false
        });
        let evs = parse_stream_json_value(&done, &mut saw_delta);
        assert!(matches!(
            &evs[0],
            StreamEvent::ToolFinish {
                id,
                ok: true,
                duration_ms: Some(971),
                ..
            } if id == "call_1"
        ));

        let chunk = serde_json::json!({"type": "text", "text": "Hello Owen"});
        let evs = parse_stream_json_value(&chunk, &mut saw_delta);
        assert_eq!(evs, vec![StreamEvent::Delta("Hello Owen".into())]);
        assert!(saw_delta);

        // Trailing final `result` must NOT duplicate text already streamed
        let summary = serde_json::json!({"type": "result", "text": "Hello Owen"});
        assert!(parse_stream_json_value(&summary, &mut saw_delta).is_empty());
    }
}

/// Minimal, dependency-free read of a Hermes profile `config.yaml`: the default
/// `model:` block first, then each `providers:` entry with its configured model.
fn hermes_presets(profile: &str) -> Vec<ModelPreset> {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = PathBuf::from(home)
        .join(".hermes/profiles")
        .join(profile)
        .join("config.yaml");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_hermes_presets(&text)
}

fn yaml_value(line: &str, key: &str) -> Option<String> {
    let rest = line.trim().strip_prefix(key)?.strip_prefix(':')?.trim();
    let v = rest.trim_matches(|c| c == '"' || c == '\'');
    (!v.is_empty()).then(|| v.to_owned())
}

pub(crate) fn parse_hermes_presets(text: &str) -> Vec<ModelPreset> {
    let mut out = Vec::new();
    let mut section = "";
    let mut default_model = None;
    let mut default_provider = None;
    let mut current_provider: Option<(String, Option<String>, Option<String>)> = None;
    let flush = |cur: &mut Option<(String, Option<String>, Option<String>)>,
                 out: &mut Vec<ModelPreset>| {
        if let Some((key, name, model)) = cur.take()
            && let Some(m) = model
        {
            let prov_display = name.unwrap_or_else(|| key.clone());
            out.push(ModelPreset {
                label: m.clone(),
                model: Some(m),
                provider: Some(key),
                provider_label: Some(prov_display),
            });
        }
    };
    for line in text.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            if section == "providers" {
                flush(&mut current_provider, &mut out);
            }
            section = match line.trim_end_matches(':').trim() {
                "model" => "model",
                "providers" => "providers",
                _ => "",
            };
            continue;
        }
        match section {
            "model" if indent == 2 => {
                if let Some(v) = yaml_value(line, "default") {
                    default_model = Some(v);
                } else if let Some(v) = yaml_value(line, "provider") {
                    default_provider = Some(v);
                }
            }
            "providers" => {
                if indent == 2 && line.trim().ends_with(':') {
                    flush(&mut current_provider, &mut out);
                    current_provider =
                        Some((line.trim().trim_end_matches(':').to_owned(), None, None));
                } else if indent == 4
                    && let Some(cur) = current_provider.as_mut()
                {
                    if let Some(v) = yaml_value(line, "name") {
                        cur.1 = Some(v);
                    } else if let Some(v) = yaml_value(line, "model") {
                        cur.2 = Some(v);
                    }
                }
            }
            _ => {}
        }
    }
    if section == "providers" {
        flush(&mut current_provider, &mut out);
    }
    if let Some(m) = default_model {
        out.insert(
            0,
            ModelPreset {
                label: m.clone(),
                model: Some(m),
                provider: default_provider.clone(),
                provider_label: default_provider,
            },
        );
    }
    out
}

#[cfg(test)]
mod preset_tests {
    use super::parse_hermes_presets;

    #[test]
    fn reads_default_and_provider_models() {
        let cfg = "model:\n  default: m-main\n  provider: cloud\nproviders:\n  local:\n    name: Local\n    model: m-local\n    models:\n      m-local:\n        context_length: 1\n";
        let presets = parse_hermes_presets(cfg);
        assert_eq!(presets.len(), 2);
        assert_eq!(presets[0].model.as_deref(), Some("m-main"));
        assert_eq!(presets[0].provider.as_deref(), Some("cloud"));
        assert_eq!(presets[1].model.as_deref(), Some("m-local"));
        assert_eq!(presets[1].provider.as_deref(), Some("local"));
    }
}
