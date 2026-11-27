//! Zero-competition streaming chat bridge to the user's real agent runtime
//! (Hermes / Claude Code / Codex). Streams live chunks, supports mid-turn cancel,
//! and never maintains a duplicate competing chat database.
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{
    ffi::OsString,
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
    Hermes {
        binary: PathBuf,
        /// `-p` profile; unset uses Hermes's own active profile.
        profile: Option<String>,
    },
    /// An ACP agent on the server, through its `jh agent-host` (see `acp`).
    Remote,
    ClaudeCode {
        binary: PathBuf,
    },
    Codex {
        binary: PathBuf,
    },
    Pi {
        binary: PathBuf,
    },
    Custom {
        name: String,
        command: String,
        models: Vec<String>,
    },
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
    /// The agent asks before a tool runs: options are (id, label).
    Permission {
        request: String,
        title: String,
        options: Vec<(String, String)>,
    },
    /// The permission request was answered (here, elsewhere or by timeout).
    PermissionDone,
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
#[serde(default)]
pub struct AgentOptions {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub reasoning: Option<String>,
    pub fast_mode: bool,
    /// When true on macOS, spawns `caffeinate -di -w <pid>` only for the duration of an active turn.
    pub keep_awake: bool,
    /// Ephemeral per-chat session key rotated on New Chat so fresh threads start clean.
    #[serde(skip)]
    pub session_name: Option<String>,
    /// True only when explicitly resuming an existing host-runtime session by its native ID.
    #[serde(skip)]
    pub resume_existing: bool,
}

/// Read-only handle to a recent session owned by the host agent runtime (zero Job Harness chat DB).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostSession {
    pub id: String,
    pub title: String,
    pub age_label: String,
    pub message_count: usize,
}

/// A selectable model preset discovered from the runtime's own configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelPreset {
    pub label: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub provider_label: Option<String>,
}

/// Scope of a discovered MCP server: shared across runtimes (e.g. MCPorter) vs pinned to one agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpScope {
    Global,
    AgentOnly(&'static str),
}

/// Read-only view of a configured MCP server (names and scope only — never reads secrets).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredMcpServer {
    pub name: String,
    pub source: String,
    pub scope: McpScope,
}

impl DiscoveredMcpServer {
    /// True when the given active runtime kind (`"hermes"`, `"claude"`, `"codex"`, ...) can see this MCP.
    pub fn visible_to(&self, runtime_kind: &str) -> bool {
        match self.scope {
            McpScope::Global => true,
            McpScope::AgentOnly(owner) => owner == runtime_kind,
        }
    }
}

/// High-level tool capability status relative to the currently selected agent runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapabilityAccess {
    Active { server: String, global: bool },
    BuiltIn,
    OtherRuntime { server: String, owner: String },
    NotConnected,
}

pub fn evaluate_capability(
    servers: &[DiscoveredMcpServer],
    active_runtime_kind: &str,
    keywords: &[&str],
) -> CapabilityAccess {
    let matches_kw = |s: &DiscoveredMcpServer| {
        let low = s.name.to_ascii_lowercase();
        keywords.iter().any(|kw| low.contains(kw))
    };
    if let Some(s) = servers
        .iter()
        .find(|s| matches_kw(s) && s.visible_to(active_runtime_kind))
    {
        return CapabilityAccess::Active {
            server: s.name.clone(),
            global: matches!(s.scope, McpScope::Global),
        };
    }
    if let Some(s) = servers.iter().find(|s| matches_kw(s)) {
        return CapabilityAccess::OtherRuntime {
            server: s.name.clone(),
            owner: s.source.clone(),
        };
    }
    CapabilityAccess::NotConnected
}

/// Discovers configured MCP servers from standard local agent registries without reading secrets.
pub fn discover_mcp_servers() -> Vec<DiscoveredMcpServer> {
    let mut out: Vec<DiscoveredMcpServer> = Vec::new();
    let mut seen: std::collections::HashSet<(String, &'static str)> =
        std::collections::HashSet::new();

    if let Ok(custom) = std::env::var("JOB_HARNESS_MCP_CONFIG")
        && !custom.trim().is_empty()
    {
        read_json_mcp_names(
            Path::new(&custom),
            "Shared",
            McpScope::Global,
            &mut out,
            &mut seen,
        );
        return out;
    }

    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return out;
    };
    read_json_mcp_names(
        &home.join(".mcporter/mcporter.json"),
        "MCPorter",
        McpScope::Global,
        &mut out,
        &mut seen,
    );
    read_json_mcp_names(
        &home.join(".config/mcporter/mcporter.json"),
        "MCPorter",
        McpScope::Global,
        &mut out,
        &mut seen,
    );
    read_json_mcp_names(
        &home.join(".claude.json"),
        "Claude",
        McpScope::AgentOnly("claude"),
        &mut out,
        &mut seen,
    );
    read_json_mcp_names(
        &home.join(".pi/agent/mcp.json"),
        "Pi",
        McpScope::AgentOnly("pi"),
        &mut out,
        &mut seen,
    );
    read_hermes_yaml_mcp_names(&home.join(".hermes/config.yaml"), &mut out, &mut seen);
    read_hermes_yaml_mcp_names(
        &hermes_home(hermes_profile().as_deref()).join("config.yaml"),
        &mut out,
        &mut seen,
    );
    if let Ok(toml_txt) = std::fs::read_to_string(home.join(".codex/config.toml")) {
        for line in toml_txt.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("[mcp_servers.")
                && let Some(name) = rest.strip_suffix(']')
            {
                let clean = name.trim_matches('"').trim();
                if !clean.is_empty()
                    && !clean.contains('.')
                    && seen.insert((clean.to_owned(), "codex"))
                {
                    out.push(DiscoveredMcpServer {
                        name: clean.to_owned(),
                        source: "Codex".into(),
                        scope: McpScope::AgentOnly("codex"),
                    });
                }
            }
        }
    }
    out
}

fn read_hermes_yaml_mcp_names(
    path: &Path,
    out: &mut Vec<DiscoveredMcpServer>,
    seen: &mut std::collections::HashSet<(String, &'static str)>,
) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut in_mcp = false;
    let mut child_indent: Option<usize> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            in_mcp = trimmed == "mcp_servers:" || trimmed.starts_with("mcp_servers:");
            child_indent = None;
            continue;
        }
        if !in_mcp {
            continue;
        }
        let target = *child_indent.get_or_insert(indent);
        if indent == target
            && let Some((raw_key, _)) = trimmed.split_once(':')
        {
            let name = raw_key.trim().trim_matches('"').trim_matches('\'');
            if !name.is_empty() && seen.insert((name.to_owned(), "hermes")) {
                out.push(DiscoveredMcpServer {
                    name: name.to_owned(),
                    source: "Hermes".into(),
                    scope: McpScope::AgentOnly("hermes"),
                });
            }
        }
    }
}

fn read_json_mcp_names(
    path: &Path,
    source: &'static str,
    scope: McpScope,
    out: &mut Vec<DiscoveredMcpServer>,
    seen: &mut std::collections::HashSet<(String, &'static str)>,
) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let Ok(val) = serde_json::from_slice::<Value>(&bytes) else {
        return;
    };
    let scope_key = match scope {
        McpScope::Global => "global",
        McpScope::AgentOnly(k) => k,
    };
    for key in ["mcpServers", "servers"] {
        if let Some(obj) = val.get(key).and_then(Value::as_object) {
            for name in obj.keys() {
                if seen.insert((name.clone(), scope_key)) {
                    out.push(DiscoveredMcpServer {
                        name: name.clone(),
                        source: source.to_owned(),
                        scope,
                    });
                }
            }
        }
    }
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

fn hermes_profile() -> Option<String> {
    std::env::var("JOB_HARNESS_HERMES_PROFILE")
        .ok()
        .filter(|p| !p.trim().is_empty())
}

/// The profile Hermes uses: the one given, else its sticky `active_profile`.
fn hermes_profile_name(profile: Option<&str>) -> String {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    profile
        .map(str::to_owned)
        .or_else(|| std::fs::read_to_string(home.join(".hermes/active_profile")).ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "default".into())
}

/// A local Hermes profile's folder; the default profile is `~/.hermes` itself.
fn hermes_home(profile: Option<&str>) -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".hermes");
    match hermes_profile_name(profile).as_str() {
        "default" => home,
        name => home.join("profiles").join(name),
    }
}

/// Hermes's arguments after the binary.
fn hermes_args(
    profile: Option<&str>,
    workspace: Option<&Path>,
    opts: &AgentOptions,
    images: &[PathBuf],
) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |words: &[&str]| args.extend(words.iter().map(OsString::from));
    if let Some(profile) = profile {
        push(&["-p", profile]);
    }
    push(&[
        "chat",
        "--query-file",
        "-",
        "--oneshot",
        "-Q",
        "--format",
        "stream-json",
    ]);
    let session_key = opts
        .session_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("career-gui");
    if opts.resume_existing {
        push(&["--resume", session_key]);
    } else {
        push(&["--continue", session_key, "--create-if-missing"]);
    }
    if let Some(m) = opts.model.as_deref().filter(|m| !m.is_empty()) {
        push(&["-m", m]);
    }
    if let Some(p) = opts.provider.as_deref().filter(|p| !p.is_empty()) {
        push(&["--provider", p]);
    }
    if let Some(r) = opts.reasoning.as_deref().filter(|r| !r.is_empty()) {
        push(&["--reasoning", r]);
    }
    if let Some(dir) = workspace {
        args.extend(["--in".into(), dir.into()]);
    }
    for image in images {
        args.extend(["--image".into(), image.into()]);
    }
    args
}

impl AgentRuntime {
    /// Every host agent CLI available on this machine, in preference order.
    pub fn detect_all() -> Vec<Self> {
        let mut out = Vec::new();
        // In remote mode the agents live on the server, beside the ledger.
        if crate::acp::enabled() {
            out.push(Self::Remote);
        } else if let Some(binary) = find_hermes() {
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
        if let Some(binary) = which_in_path("pi") {
            out.push(Self::Pi { binary });
        }
        if let Some(binary) = which_in_path("opencode") {
            out.push(Self::Custom {
                name: "OpenCode".into(),
                command: format!("{} run", binary.display()),
                models: Vec::new(),
            });
        }
        if let Some(binary) = which_in_path("agy") {
            out.push(Self::Custom {
                name: "Antigravity".into(),
                command: format!("{} chat", binary.display()),
                models: Vec::new(),
            });
        }
        for rt in load_registered_custom_runtimes() {
            if !out
                .iter()
                .any(|existing| existing.short_name().eq_ignore_ascii_case(rt.short_name()))
            {
                out.push(rt);
            }
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
                models: Vec::new(),
            });
        }
        out
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hermes { .. } => "hermes",
            Self::Remote => "remote",
            Self::ClaudeCode { .. } => "claude",
            Self::Codex { .. } => "codex",
            Self::Pi { .. } => "pi",
            Self::Custom { .. } => "custom",
            Self::Unconfigured => "none",
        }
    }

    pub fn short_name(&self) -> &str {
        match self {
            Self::Hermes { .. } => "Hermes",
            Self::Remote => "Agent",
            Self::ClaudeCode { .. } => "Claude",
            Self::Codex { .. } => "Codex",
            Self::Pi { .. } => "Pi",
            Self::Custom { name, .. } => name.as_str(),
            Self::Unconfigured => "No agent",
        }
    }

    /// Exact command or config hint to connect this runtime to the Job Harness MCP server.
    pub fn mcp_connect_command(&self, root: Option<&Path>) -> String {
        let root_flag = root
            .map(|r| format!(" --root {}", r.display()))
            .unwrap_or_default();
        match self {
            Self::ClaudeCode { .. } => {
                format!("claude mcp add job-harness -- jh{root_flag} mcp --tool-profile focused")
            }
            Self::Codex { .. } => {
                format!("codex mcp add job-harness -- jh{root_flag} mcp --tool-profile focused")
            }
            Self::Hermes { .. } => format!("jh{root_flag} connect mcp --client hermes"),
            Self::Pi { .. } => format!("jh{root_flag} connect mcp"),
            Self::Remote | Self::Custom { .. } | Self::Unconfigured => {
                format!("jh{root_flag} connect mcp")
            }
        }
    }

    pub fn supports_reasoning(&self) -> bool {
        matches!(
            self,
            Self::Hermes { .. } | Self::ClaudeCode { .. } | Self::Codex { .. }
        )
    }

    /// True when the selected runtime + model + provider supports priority / fast-mode tiering.
    pub fn supports_fast_mode(&self, opts: &AgentOptions) -> bool {
        match self {
            Self::Hermes { .. } => {
                let presets = self.model_presets();
                let default_preset = presets.first();
                let model = opts
                    .model
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| default_preset.and_then(|p| p.model.as_deref()))
                    .unwrap_or("");
                let provider = opts
                    .provider
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| default_preset.and_then(|p| p.provider.as_deref()))
                    .unwrap_or("");
                model_route_supports_fast_mode(model, provider)
            }
            Self::Codex { .. } => true,
            Self::Remote => crate::acp::supports_fast_mode(),
            Self::ClaudeCode { .. } => {
                let m = opts
                    .model
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                m.is_empty() || m.contains("opus")
            }
            Self::Pi { .. } | Self::Custom { .. } | Self::Unconfigured => false,
        }
    }

    /// Reads whether Hermes's profile config currently has a fast/priority `service_tier`.
    pub fn read_configured_fast_mode(&self) -> Option<bool> {
        let Self::Hermes { profile, .. } = self else {
            return None;
        };
        let text =
            std::fs::read_to_string(hermes_home(profile.as_deref()).join("config.yaml")).ok()?;
        for line in text.lines() {
            if let Some(v) = yaml_value(line, "service_tier") {
                let low = v.to_ascii_lowercase();
                return Some(matches!(
                    low.as_str(),
                    "fast" | "priority" | "ultrafast" | "auto"
                ));
            }
        }
        Some(false)
    }

    /// Persists Hermes's `agent.service_tier` asynchronously when toggled from the UI.
    pub fn sync_fast_mode(&self, enabled: bool) {
        if let Self::Hermes { binary, profile } = self {
            let bin = binary.clone();
            let prof = profile.clone();
            let tier = if enabled { "priority" } else { "normal" };
            std::thread::spawn(move || {
                let mut cmd = Command::new(bin);
                if let Some(prof) = &prof {
                    cmd.args(["-p", prof]);
                }
                cmd.args(["config", "set", "agent.service_tier", tier])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                let _ = cmd.status();
            });
        }
    }

    /// Model presets read from the runtime's own config (Hermes, Codex, Pi, Custom) or CLI aliases.
    pub fn model_presets(&self) -> Vec<ModelPreset> {
        match self {
            Self::Hermes { profile, .. } => hermes_presets(profile.as_deref()),
            Self::Remote => crate::acp::model_presets(),
            Self::ClaudeCode { .. } => ["sonnet", "opus", "haiku"]
                .into_iter()
                .map(|m| ModelPreset {
                    label: m.to_owned(),
                    model: Some(m.to_owned()),
                    provider: None,
                    provider_label: Some("Anthropic".into()),
                })
                .collect(),
            Self::Codex { .. } => codex_presets(),
            Self::Pi { .. } => pi_presets(),
            Self::Custom { name, models, .. } => models
                .iter()
                .map(|m| ModelPreset {
                    label: m.clone(),
                    model: Some(m.clone()),
                    provider: None,
                    provider_label: Some(name.clone()),
                })
                .collect(),
            Self::Unconfigured => Vec::new(),
        }
    }

    /// The model the runtime uses when no override is set (if discoverable).
    pub fn default_model(&self) -> Option<String> {
        self.model_presets()
            .into_iter()
            .next()
            .and_then(|p| p.model)
    }

    pub fn label(&self) -> String {
        match self {
            Self::Hermes { profile, .. } => {
                format!("Hermes ({})", hermes_profile_name(profile.as_deref()))
            }
            Self::Remote => crate::acp::label(),
            Self::ClaudeCode { .. } => "Claude Code".into(),
            Self::Codex { .. } => "Codex".into(),
            Self::Pi { .. } => "Pi".into(),
            Self::Custom { name, .. } => format!("{name} (custom)"),
            Self::Unconfigured => "Not connected".into(),
        }
    }

    /// True when this runtime maintains a native session store we can list read-only and resume.
    pub fn supports_sessions(&self) -> bool {
        match self {
            Self::Remote => crate::acp::supports_sessions(),
            _ => matches!(
                self,
                Self::Hermes { .. } | Self::ClaudeCode { .. } | Self::Codex { .. }
            ),
        }
    }

    /// Reads the last ~8 recent sessions directly from the host runtime's own store (zero Job Harness DB).
    pub fn list_recent_sessions(&self, workspace: Option<&Path>) -> Vec<HostSession> {
        match self {
            Self::Hermes { profile, .. } => list_hermes_sessions(profile.as_deref()),
            Self::Remote => crate::acp::list_sessions(),
            Self::ClaudeCode { .. } => list_claude_sessions(workspace),
            Self::Codex { .. } => list_codex_sessions(),
            Self::Pi { .. } | Self::Custom { .. } | Self::Unconfigured => Vec::new(),
        }
    }

    pub fn is_ready(&self) -> bool {
        !matches!(self, Self::Unconfigured)
    }

    pub fn has_builtin_web_search(&self) -> bool {
        matches!(
            self,
            Self::Hermes { .. } | Self::ClaudeCode { .. } | Self::Codex { .. }
        )
    }

    pub fn has_builtin_browser(&self) -> bool {
        matches!(self, Self::Hermes { .. })
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
            let result = if runtime == Self::Remote {
                crate::acp::stream_turn(&prompt, &attachments, &opts, &tx, &cancel)
            } else {
                run_streaming(
                    &runtime,
                    workspace.as_deref(),
                    &prompt,
                    &attachments,
                    &opts,
                    &tx,
                    &cancel,
                )
            };
            if let Err(err) = result {
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
            let mut images = Vec::new();
            for att in attachments {
                if is_image_path(att) {
                    images.push(att.clone());
                } else {
                    non_native_files.push(att.clone());
                }
            }
            let mut c = Command::new(binary);
            c.args(hermes_args(profile.as_deref(), workspace, opts, &images));
            (c, true)
        }
        AgentRuntime::ClaudeCode { binary } => {
            let mut c = Command::new(binary);
            c.args([
                "-p",
                "--verbose",
                "--output-format",
                "stream-json",
                "--include-partial-messages",
            ]);
            if opts.resume_existing
                && let Some(sid) = opts.session_name.as_deref().filter(|s| !s.is_empty())
            {
                c.args(["--resume", sid]);
            } else {
                c.arg("--continue");
            }
            if let Some(m) = opts.model.as_deref().filter(|m| !m.is_empty()) {
                c.args(["--model", m]);
            }
            if let Some(r) = opts.reasoning.as_deref().filter(|r| !r.is_empty()) {
                c.args(["--effort", r]);
            }
            if opts.fast_mode && runtime.supports_fast_mode(opts) {
                c.args(["--settings", r#"{"fastMode":true}"#]);
            }
            if let Some(dir) = workspace {
                c.current_dir(dir);
            }
            non_native_files.extend(attachments.iter().cloned());
            (c, true)
        }
        AgentRuntime::Codex { binary } => {
            let mut c = Command::new(binary);
            if opts.resume_existing
                && let Some(sid) = opts.session_name.as_deref().filter(|s| !s.is_empty())
            {
                c.args(["exec", "resume", sid, "--json"]);
            } else {
                c.args(["exec", "--json"]);
            }
            if let Some(m) = opts.model.as_deref().filter(|m| !m.is_empty()) {
                c.args(["-m", m]);
            }
            if let Some(r) = opts.reasoning.as_deref().filter(|r| !r.is_empty()) {
                c.args(["-c", &format!("model_reasoning_effort=\"{r}\"")]);
            }
            if opts.fast_mode {
                c.args(["-c", "service_tier=\"fast\""]);
            }
            c.arg("-");
            if let Some(dir) = workspace {
                c.current_dir(dir);
            }
            non_native_files.extend(attachments.iter().cloned());
            (c, true)
        }
        AgentRuntime::Pi { binary } => {
            let mut c = Command::new(binary);
            c.arg("-p");
            if opts.resume_existing
                && let Some(sid) = opts.session_name.as_deref().filter(|s| !s.is_empty())
            {
                c.args(["--session", sid]);
            } else {
                c.arg("--no-session");
            }
            if let Some(m) = opts.model.as_deref().filter(|m| !m.is_empty()) {
                c.args(["--model", m]);
            }
            if let Some(p) = opts.provider.as_deref().filter(|p| !p.is_empty()) {
                c.args(["--provider", p]);
            }
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
            if let Some(m) = opts.model.as_deref().filter(|m| !m.is_empty()) {
                c.env("JOB_HARNESS_MODEL", m);
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
        AgentRuntime::Remote => unreachable!("remote turns go through acp::stream_turn"),
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

    // macOS .app bundles launched via Finder/LaunchServices receive a stripped PATH;
    // prepend standard local tool dirs so agent CLIs, mcporter, node, and jh always resolve.
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let mut dirs: Vec<PathBuf> = vec![
            home.join(".local/bin"),
            home.join(".local/share/mise/shims"),
            home.join(".cargo/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ];
        if let Some(existing) = std::env::var_os("PATH") {
            dirs.extend(std::env::split_paths(&existing));
        }
        if let Ok(joined) = std::env::join_paths(dirs) {
            cmd.env("PATH", joined);
        }
    }

    let _ = tx.send(StreamEvent::Status("Thinking…".into()));
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to launch agent runtime")?;

    // Immediate cancel watchdog: kills the child within 50ms even while `reader.lines()`
    // is blocked waiting for stdout (fixes Steer/Stop leaving zombie agent processes).
    let child_pid = child.id();
    #[cfg(target_os = "macos")]
    if opts.keep_awake {
        let _ = Command::new("/usr/bin/caffeinate")
            .args(["-di", "-w", &child_pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    let watchdog_done = Arc::new(AtomicBool::new(false));
    {
        let cancel_w = cancel.clone();
        let done_w = watchdog_done.clone();
        std::thread::spawn(move || {
            while !done_w.load(Ordering::Relaxed) {
                if cancel_w.load(Ordering::Relaxed) {
                    let pid_s = child_pid.to_string();
                    let _ = Command::new("kill")
                        .args(["-TERM", &pid_s])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    if !done_w.load(Ordering::Relaxed) {
                        let _ = Command::new("kill")
                            .args(["-KILL", &pid_s])
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status();
                    }
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        });
    }

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
            let _ = child.wait();
            watchdog_done.store(true, Ordering::Relaxed);
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
    watchdog_done.store(true, Ordering::Relaxed);
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
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for dir in [
            home.join(".local/bin"),
            home.join(".local/share/mise/shims"),
            home.join(".cargo/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
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
fn hermes_presets(profile: Option<&str>) -> Vec<ModelPreset> {
    let Ok(text) = std::fs::read_to_string(hermes_home(profile).join("config.yaml")) else {
        return Vec::new();
    };
    parse_hermes_presets(&text)
}

/// Discovers Codex CLI configured models from `~/.codex/config.toml` plus standard aliases.
fn codex_presets() -> Vec<ModelPreset> {
    let mut out: Vec<ModelPreset> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push = |m: &str, prov: &str| {
        let clean = m.trim().trim_matches('"').trim_matches('\'');
        if !clean.is_empty() && seen.insert(clean.to_ascii_lowercase()) {
            out.push(ModelPreset {
                label: clean.to_owned(),
                model: Some(clean.to_owned()),
                provider: None,
                provider_label: Some(prov.to_owned()),
            });
        }
    };
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && let Ok(text) = std::fs::read_to_string(home.join(".codex/config.toml"))
    {
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with('#') {
                continue;
            }
            for key in ["model =", "review_model ="] {
                if let Some(rest) = t.strip_prefix(key) {
                    push(rest, "Codex");
                }
            }
        }
    }
    for fallback in ["o3", "gpt-4.1", "codex-mini-latest"] {
        push(fallback, "OpenAI");
    }
    out
}

/// Discovers Pi agent models from `~/.pi/agent/settings.json` and `~/.pi/agent/models.json`.
fn pi_presets() -> Vec<ModelPreset> {
    let mut out: Vec<ModelPreset> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return out;
    };
    let mut push = |model: &str, provider: Option<&str>| {
        let m = model.trim();
        let p = provider.unwrap_or("").trim();
        if !m.is_empty() && seen.insert((m.to_ascii_lowercase(), p.to_ascii_lowercase())) {
            out.push(ModelPreset {
                label: m.to_owned(),
                model: Some(m.to_owned()),
                provider: (!p.is_empty()).then(|| p.to_owned()),
                provider_label: if p.is_empty() {
                    Some("Pi".into())
                } else {
                    Some(p.to_owned())
                },
            });
        }
    };
    if let Ok(bytes) = std::fs::read(home.join(".pi/agent/settings.json"))
        && let Ok(val) = serde_json::from_slice::<Value>(&bytes)
        && let Some(dm) = val["defaultModel"].as_str()
    {
        push(dm, val["defaultProvider"].as_str());
    }
    if let Ok(bytes) = std::fs::read(home.join(".pi/agent/models.json"))
        && let Ok(val) = serde_json::from_slice::<Value>(&bytes)
        && let Some(providers) = val["providers"].as_object()
    {
        for (prov_name, prov_cfg) in providers {
            if let Some(models) = prov_cfg["models"].as_array() {
                for m in models {
                    let id = m["id"]
                        .as_str()
                        .or_else(|| m["name"].as_str())
                        .or_else(|| m.as_str());
                    if let Some(id) = id {
                        push(id, Some(prov_name));
                    }
                }
            }
        }
    }
    out
}

/// Loads user- or agent-registered custom runtimes from `~/.config/job-harness/runtimes.json`.
/// Format: `[{"name": "MyAgent", "command": "my-agent --prompt-stdin", "models": ["model-a"]}]`
fn load_registered_custom_runtimes() -> Vec<AgentRuntime> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let path = home.join(".config/job-harness/runtimes.json");
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let Ok(val) = serde_json::from_slice::<Value>(&bytes) else {
        return Vec::new();
    };
    let Some(arr) = val.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            let name = item["name"].as_str()?.trim();
            let command = item["command"].as_str()?.trim();
            if name.is_empty() || command.is_empty() {
                return None;
            }
            let models = item["models"]
                .as_array()
                .map(|m| {
                    m.iter()
                        .filter_map(Value::as_str)
                        .map(|s| s.trim().to_owned())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            Some(AgentRuntime::Custom {
                name: name.to_owned(),
                command: command.to_owned(),
                models,
            })
        })
        .collect()
}

fn yaml_value(line: &str, key: &str) -> Option<String> {
    let rest = line.trim().strip_prefix(key)?.strip_prefix(':')?.trim();
    let v = rest.trim_matches(|c| c == '"' || c == '\'');
    (!v.is_empty()).then(|| v.to_owned())
}

fn model_route_supports_fast_mode(model_id: &str, provider: &str) -> bool {
    let raw = model_id.trim().to_ascii_lowercase();
    let base = raw.split('/').next_back().unwrap_or(&raw);
    let base = base.split(':').next().unwrap_or(base);
    if base.is_empty() {
        return false;
    }
    let prov = provider.trim().to_ascii_lowercase();
    let is_first_party = prov.is_empty()
        || matches!(
            prov.as_str(),
            "openai" | "openai-api" | "openai-codex" | "anthropic" | "xai" | "auto"
        );
    if !is_first_party {
        return false;
    }
    let is_openai_fast = !base.contains("codex")
        && (base.starts_with("gpt-6")
            || base.starts_with("gpt-5")
            || base.starts_with("gpt-4.1")
            || base.starts_with("o3")
            || base.starts_with("o4"));
    let is_anthropic_fast = base == "opus"
        || base.starts_with("claude-opus-4-6")
        || base.starts_with("claude-opus-4-8")
        || base.starts_with("claude-opus-5");
    let is_xai_fast = base.starts_with("grok-4.6") || base.starts_with("grok-4-6");
    is_openai_fast || is_anthropic_fast || is_xai_fast
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

fn format_relative_age(epoch_secs: f64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(epoch_secs);
    let diff = (now - epoch_secs).max(0.0) as u64;
    if diff < 60 {
        "just now".into()
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86_400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86_400)
    }
}

fn clean_session_title(raw: &str) -> String {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("Untitled session");
    let chars: Vec<char> = line.chars().collect();
    if chars.len() > 54 {
        format!("{}…", chars[..53].iter().collect::<String>())
    } else {
        line.to_owned()
    }
}

fn list_hermes_sessions(profile: Option<&str>) -> Vec<HostSession> {
    let db = hermes_home(profile).join("state.db");
    if !db.is_file() {
        return Vec::new();
    }
    let sql = "SELECT s.id, COALESCE(NULLIF((SELECT m.content FROM messages m WHERE m.session_id = s.id AND m.role = 'user' ORDER BY m.id ASC LIMIT 1), ''), s.title, s.id) AS label, s.started_at, COALESCE(s.message_count, 0) AS mc FROM sessions s WHERE COALESCE(s.message_count, 0) > 0 AND COALESCE(s.title, '') NOT LIKE '[bot:%' AND COALESCE(s.title, '') NOT LIKE '[Cronjob%' AND COALESCE((SELECT m.content FROM messages m WHERE m.session_id = s.id AND m.role = 'user' ORDER BY m.id ASC LIMIT 1), '') NOT LIKE '[Cronjob%' ORDER BY COALESCE(s.last_activity_at, s.started_at) DESC LIMIT 8;";
    let Ok(out) = Command::new("sqlite3")
        .arg("-readonly")
        .arg("-json")
        .arg(&db)
        .arg(sql)
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let Ok(rows) = serde_json::from_slice::<Vec<Value>>(&out.stdout) else {
        return Vec::new();
    };
    rows.into_iter()
        .filter_map(|r| {
            let id = r["id"].as_str()?.to_owned();
            let title = clean_session_title(r["label"].as_str().unwrap_or(&id));
            let started = r["started_at"].as_f64().unwrap_or(0.0);
            let mc = r["mc"].as_u64().unwrap_or(0) as usize;
            Some(HostSession {
                id,
                title,
                age_label: format_relative_age(started),
                message_count: mc,
            })
        })
        .collect()
}

fn list_claude_sessions(workspace: Option<&Path>) -> Vec<HostSession> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let projects_dir = home.join(".claude/projects");
    let target_dir = if let Some(ws) = workspace {
        let encoded = ws.to_string_lossy().replace('/', "-");
        let candidate = projects_dir.join(encoded);
        if candidate.is_dir() {
            candidate
        } else {
            return Vec::new();
        }
    } else {
        return Vec::new();
    };
    collect_jsonl_sessions(&target_dir, 8)
}

fn list_codex_sessions() -> Vec<HostSession> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let root = home.join(".codex/sessions");
    if !root.is_dir() {
        return Vec::new();
    }
    collect_jsonl_sessions(&root, 8)
}

fn collect_jsonl_sessions(dir: &Path, limit: usize) -> Vec<HostSession> {
    let mut files: Vec<(f64, PathBuf)> = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&cur) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("jsonl")
                && let Ok(meta) = entry.metadata()
                && let Ok(mtime) = meta.modified()
                && let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH)
            {
                files.push((dur.as_secs_f64(), p));
            }
        }
    }
    files.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    files
        .into_iter()
        .take(limit)
        .filter_map(|(ts, path)| {
            let id = path.file_stem()?.to_str()?.to_owned();
            let text = std::fs::read_to_string(&path).ok()?;
            let mut preview = None;
            let mut count = 0usize;
            for line in text.lines().take(60) {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    let is_user = v["type"].as_str() == Some("user")
                        || v["role"].as_str() == Some("user")
                        || v.pointer("/payload/role").and_then(Value::as_str) == Some("user");
                    if is_user {
                        count += 1;
                        if preview.is_none() {
                            let msg = v
                                .pointer("/message/content")
                                .or_else(|| v.get("content"))
                                .or_else(|| v.pointer("/payload/content"))
                                .and_then(|c| {
                                    c.as_str().or_else(|| {
                                        c.as_array()
                                            .and_then(|arr| arr.first())
                                            .and_then(|x| x["text"].as_str())
                                    })
                                });
                            if let Some(m) = msg.filter(|s| !s.trim().is_empty()) {
                                preview = Some(clean_session_title(m));
                            }
                        }
                    }
                }
            }
            Some(HostSession {
                title: preview.unwrap_or_else(|| id.clone()),
                id,
                age_label: format_relative_age(ts),
                message_count: count,
            })
        })
        .collect()
}

#[cfg(test)]
mod hermes_command_tests {
    use super::*;

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_profile_is_passed_only_when_one_is_set() {
        let opts = AgentOptions::default();
        let plain = strings(&hermes_args(None, Some(Path::new("/ws")), &opts, &[]));
        assert_eq!(plain[0], "chat");
        assert!(!plain.contains(&"-p".to_owned()));
        assert!(plain.ends_with(&["--in".into(), "/ws".into()]));
        let named = strings(&hermes_args(Some("bot"), None, &opts, &[]));
        assert_eq!(&named[..3], ["-p", "bot", "chat"]);
        assert!(!named.contains(&"--in".to_owned()));
    }
}
