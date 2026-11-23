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

impl AgentRuntime {
    pub fn detect() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        for bin in [
            PathBuf::from(&home).join(".local/bin/hermes"),
            PathBuf::from("/usr/local/bin/hermes"),
            PathBuf::from("/opt/homebrew/bin/hermes"),
        ] {
            if bin.is_file() {
                let profile = std::env::var("JOB_HARNESS_HERMES_PROFILE")
                    .unwrap_or_else(|_| "career-chief".into());
                return Self::Hermes {
                    binary: bin,
                    profile,
                };
            }
        }
        if let Some(bin) = which_in_path("hermes") {
            let profile = std::env::var("JOB_HARNESS_HERMES_PROFILE")
                .unwrap_or_else(|_| "career-chief".into());
            return Self::Hermes {
                binary: bin,
                profile,
            };
        }
        if let Some(bin) = which_in_path("claude") {
            return Self::ClaudeCode { binary: bin };
        }
        if let Some(bin) = which_in_path("codex") {
            return Self::Codex { binary: bin };
        }
        Self::Unconfigured
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
        tx: Sender<StreamEvent>,
        cancel: Arc<AtomicBool>,
    ) {
        let runtime = self.clone();
        std::thread::spawn(move || {
            if let Err(err) = run_streaming(&runtime, workspace.as_deref(), &prompt, &tx, &cancel) {
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
            (c, true)
        }
        AgentRuntime::ClaudeCode { binary } => {
            let mut c = Command::new(binary);
            c.args(["-p", "--continue"]);
            if let Some(dir) = workspace {
                c.current_dir(dir);
            }
            (c, false)
        }
        AgentRuntime::Codex { binary } => {
            let mut c = Command::new(binary);
            c.args(["exec", "-"]);
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
            if let Some(text) = extract_stream_text(&v) {
                emitted_any = true;
                let _ = tx.send(StreamEvent::Delta(text));
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

fn extract_stream_text(v: &Value) -> Option<String> {
    for key in ["delta", "text", "content", "message", "response"] {
        if let Some(s) = v.get(key).and_then(Value::as_str)
            && !s.is_empty()
        {
            return Some(s.to_owned());
        }
    }
    if let Some(s) = v
        .pointer("/choices/0/delta/content")
        .and_then(Value::as_str)
        && !s.is_empty()
    {
        return Some(s.to_owned());
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
        assert_eq!(extract_stream_text(&delta).as_deref(), Some("Hello world"));
        let status = serde_json::json!({"tool": "review_queue"});
        assert_eq!(
            extract_stream_status(&status).as_deref(),
            Some("Running review_queue…")
        );
    }
}
