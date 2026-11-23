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
    Unconfigured,
}

#[derive(Clone, Debug)]
pub enum StreamEvent {
    Status(String),
    Delta(String),
    Done,
    Error(String),
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
}

pub const HERMES_REASONING_LEVELS: [&str; 6] = ["minimal", "low", "medium", "high", "xhigh", "max"];

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
        out
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hermes { .. } => "hermes",
            Self::ClaudeCode { .. } => "claude",
            Self::Codex { .. } => "codex",
            Self::Unconfigured => "none",
        }
    }

    pub fn short_name(&self) -> &'static str {
        match self {
            Self::Hermes { .. } => "Hermes",
            Self::ClaudeCode { .. } => "Claude Code",
            Self::Codex { .. } => "Codex",
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
            Self::Unconfigured => "Not connected".into(),
        }
    }

    pub fn is_ready(&self) -> bool {
        !matches!(self, Self::Unconfigured)
    }

    /// Streams one turn directly from the host agent runtime with cancellation support.
    pub fn stream_turn(
        &self,
        workspace: Option<PathBuf>,
        prompt: String,
        opts: AgentOptions,
        tx: Sender<StreamEvent>,
        cancel: Arc<AtomicBool>,
    ) {
        let runtime = self.clone();
        std::thread::spawn(move || {
            if let Err(err) =
                run_streaming(&runtime, workspace.as_deref(), &prompt, &opts, &tx, &cancel)
            {
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
    opts: &AgentOptions,
    tx: &Sender<StreamEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<()> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        bail!("Empty message");
    }
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
            (c, true)
        }
        AgentRuntime::ClaudeCode { binary } => {
            let mut c = Command::new(binary);
            c.args(["-p", "--continue"]);
            if let Some(m) = opts.model.as_deref().filter(|m| !m.is_empty()) {
                c.args(["--model", m]);
            }
            if let Some(dir) = workspace {
                c.current_dir(dir);
            }
            (c, false)
        }
        AgentRuntime::Codex { binary } => {
            let mut c = Command::new(binary);
            c.arg("exec");
            if let Some(m) = opts.model.as_deref().filter(|m| !m.is_empty()) {
                c.args(["-m", m]);
            }
            c.arg("-");
            if let Some(dir) = workspace {
                c.current_dir(dir);
            }
            (c, false)
        }
        AgentRuntime::Unconfigured => {
            bail!(
                "No host agent runtime detected (Hermes, Claude Code, or Codex). Install one or run setup from your agent."
            );
        }
    };

    let _ = tx.send(StreamEvent::Status("Thinking…".into()));
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to launch agent runtime")?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(trimmed.as_bytes());
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
            if let Some(chunk) = extract_incremental_delta(&v) {
                emitted_any = true;
                saw_incremental_delta = true;
                let _ = tx.send(StreamEvent::Delta(chunk));
            } else if !saw_incremental_delta && let Some(final_text) = extract_final_text(&v) {
                // Only emit final summary object if the runtime didn't already stream deltas
                emitted_any = true;
                let _ = tx.send(StreamEvent::Delta(final_text));
            } else if let Some(status) = extract_stream_status(&v) {
                let _ = tx.send(StreamEvent::Status(status));
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

fn extract_incremental_delta(v: &Value) -> Option<String> {
    if let Some(s) = v.get("delta").and_then(Value::as_str)
        && !s.is_empty()
    {
        return Some(s.to_owned());
    }
    if let Some(s) = v
        .pointer("/choices/0/delta/content")
        .and_then(Value::as_str)
        && !s.is_empty()
    {
        return Some(s.to_owned());
    }
    if let Some(kind) = v.get("type").and_then(Value::as_str)
        && kind.contains("delta")
        && let Some(s) = v
            .get("text")
            .or_else(|| v.get("content"))
            .and_then(Value::as_str)
        && !s.is_empty()
    {
        return Some(s.to_owned());
    }
    None
}

fn extract_final_text(v: &Value) -> Option<String> {
    for key in ["response", "message", "content", "text"] {
        if let Some(s) = v.get(key).and_then(Value::as_str)
            && !s.is_empty()
        {
            return Some(s.to_owned());
        }
    }
    None
}

fn extract_stream_status(v: &Value) -> Option<String> {
    if let Some(tool) = v
        .get("tool")
        .or_else(|| v.get("tool_name"))
        .and_then(Value::as_str)
    {
        return Some(format!("Running {tool}…"));
    }
    if let Some(kind) = v.get("type").and_then(Value::as_str)
        && (kind.contains("tool") || kind.contains("think") || kind.contains("status"))
    {
        return Some("Working…".into());
    }
    None
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
    fn parses_stream_json_text_and_tool_status() {
        let delta = serde_json::json!({"delta": "Hello world"});
        assert_eq!(
            extract_incremental_delta(&delta).as_deref(),
            Some("Hello world")
        );
        let final_msg = serde_json::json!({"response": "Done reply"});
        assert_eq!(extract_incremental_delta(&final_msg), None);
        assert_eq!(
            extract_final_text(&final_msg).as_deref(),
            Some("Done reply")
        );
        let status = serde_json::json!({"tool": "review_queue"});
        assert_eq!(
            extract_stream_status(&status).as_deref(),
            Some("Running review_queue…")
        );
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
            out.push(ModelPreset {
                label: format!("{m} · {}", name.unwrap_or_else(|| key.clone())),
                model: Some(m),
                provider: Some(key),
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
        let label = match &default_provider {
            Some(p) => format!("{m} · {p}"),
            None => m.clone(),
        };
        out.insert(
            0,
            ModelPreset {
                label,
                model: Some(m),
                provider: default_provider,
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
