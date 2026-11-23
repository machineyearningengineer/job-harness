//! Unified local + remote VPS workspace transport over Job Harness.
//! Local mode calls `job-harness-core::Engine` directly in-process (zero HTTP overhead).
//! Remote mode connects over SSH tunnel / Tailscale to `jh serve` with live SSE events.
use anyhow::{Context, Result, bail};
use job_harness_core::{Engine, storage};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub enum WorkspaceTarget {
    Local(PathBuf),
    Remote { url: String, token: Option<String> },
}

impl WorkspaceTarget {
    pub fn label(&self) -> String {
        match self {
            Self::Local(p) => p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("workspace")
                .to_owned(),
            Self::Remote { url, .. } => format!(
                "VPS · {}",
                url.trim_start_matches("http://")
                    .trim_start_matches("https://")
            ),
        }
    }
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }
    pub fn local_root(&self) -> Option<&Path> {
        match self {
            Self::Local(p) => Some(p.as_path()),
            Self::Remote { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RoleCard {
    pub record_path: String,
    pub company: String,
    pub title: String,
    pub stage: String,
    pub column: &'static str,
    pub urgency: String,
    pub urgency_reason: String,
    pub note_line: Option<String>,
    pub checked: String,
    pub scheduled_start: Option<String>,
    pub next_action: String,
    pub brief_path: Option<String>,
    pub cv_track: Option<String>,
    pub packet_hash: Option<String>,
    pub source_url: Option<String>,
    pub search_blob: String,
}

#[derive(Clone, Debug)]
pub struct AttentionEntry {
    pub urgency: String,
    pub company: String,
    pub detail: String,
    pub record_path: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct DailyBudget {
    pub limit: u64,
    pub used: u64,
    pub portal_limit: u64,
    pub portal_used: u64,
    pub email_limit: u64,
    pub email_used: u64,
    pub outreach_limit: u64,
    pub outreach_used: u64,
}

#[derive(Clone, Debug)]
pub struct PermissionsState {
    pub auto_apply_portal: bool,
    pub auto_send_first_contact_email: bool,
    pub auto_reply_threads: bool,
    pub auto_book_calendar: bool,
    pub auto_archive_mail: bool,
    pub brief_lead_time_minutes: i64,
}

impl Default for PermissionsState {
    fn default() -> Self {
        Self {
            auto_apply_portal: false,
            auto_send_first_contact_email: false,
            auto_reply_threads: false,
            auto_book_calendar: false,
            auto_archive_mail: false,
            brief_lead_time_minutes: 90,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CvNode {
    pub name: String,
    pub relative_path: String,
    pub full_path: Option<PathBuf>,
    pub is_master: bool,
    pub matched_roles: Option<u64>,
    pub exists: bool,
}

#[derive(Clone, Debug, Default)]
pub struct WorkspaceSnapshot {
    pub total_records: usize,
    pub closed_count: usize,
    pub active_cards: Vec<RoleCard>,
    pub archived_cards: Vec<RoleCard>,
    pub urgent_count: u64,
    pub upcoming_count: u64,
    pub attention_items: Vec<AttentionEntry>,
    pub daily_budget: DailyBudget,
    pub permissions: PermissionsState,
    pub cv_nodes: Vec<CvNode>,
    pub suggest_new_track: bool,
    pub unmatched_track_titles: Vec<String>,
    pub unfinished_runs: u64,
    pub unresolved_attempts: u64,
    pub healthy: bool,
    pub status_note: String,
}

pub fn stage_to_column(stage: &str) -> &'static str {
    match stage {
        "saved" | "prepared" | "needs-review" => "Saved",
        "applied" => "Applied",
        "introduction" | "screening" => "Screening",
        "technical" | "final" | "offer" => "Interviews",
        _ => "Archive",
    }
}

pub fn column_to_stage(column: &str) -> &'static str {
    match column {
        "Saved" => "saved",
        "Applied" => "applied",
        "Screening" => "screening",
        "Interviews" => "technical",
        _ => "closed",
    }
}

pub struct WorkspaceClient {
    pub target: WorkspaceTarget,
    http: reqwest::blocking::Client,
}

impl WorkspaceClient {
    pub fn new(target: WorkspaceTarget) -> Self {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("http client");
        Self { target, http }
    }

    pub fn call(&self, tool: &str, arguments: Value) -> Result<Value> {
        match &self.target {
            WorkspaceTarget::Local(root) => {
                let engine = Engine::open(root)?;
                Ok(engine.call(tool, &arguments))
            }
            WorkspaceTarget::Remote { url, token } => {
                let endpoint = format!("{}/v1/call", url.trim_end_matches('/'));
                let mut req = self
                    .http
                    .post(endpoint)
                    .json(&json!({"tool": tool, "arguments": arguments}));
                if let Some(tok) = token {
                    req = req.bearer_auth(tok);
                }
                let resp = req.send().context("Remote workspace request failed")?;
                let parsed: Value = resp.json().context("Invalid remote JSON envelope")?;
                Ok(parsed)
            }
        }
    }

    pub fn load_snapshot(&self) -> Result<WorkspaceSnapshot> {
        let batch = self.call(
            "query_batch",
            json!({
                "steps": [
                    {"id": "boot", "tool": "bootstrap", "arguments": {"operation": "daily_check", "capabilities": []}},
                    {"id": "queue", "tool": "review_queue", "arguments": {"limit": 50}}
                ]
            }),
        )?;
        if batch["ok"] != true {
            let msg = batch["error"]["message"]
                .as_str()
                .unwrap_or("Failed to load workspace");
            bail!("{msg}");
        }
        let boot = batch_step_data(&batch, "boot").unwrap_or(&Value::Null);
        let queue = batch_step_data(&batch, "queue").unwrap_or(&Value::Null);

        let settings = &boot["settings"];
        let health = &boot["health"];
        let budget_json = &boot["daily_budget"];

        let daily_budget = DailyBudget {
            limit: budget_json["limit"].as_u64().unwrap_or(3),
            used: budget_json["used"].as_u64().unwrap_or(0),
            portal_limit: budget_json["channels"]["portal"]["limit"]
                .as_u64()
                .unwrap_or_else(|| budget_json["limit"].as_u64().unwrap_or(3)),
            portal_used: budget_json["channels"]["portal"]["used"]
                .as_u64()
                .unwrap_or(0),
            email_limit: budget_json["channels"]["direct_email"]["limit"]
                .as_u64()
                .unwrap_or(0),
            email_used: budget_json["channels"]["direct_email"]["used"]
                .as_u64()
                .unwrap_or(0),
            outreach_limit: budget_json["channels"]["recruiter_outreach"]["limit"]
                .as_u64()
                .unwrap_or(0),
            outreach_used: budget_json["channels"]["recruiter_outreach"]["used"]
                .as_u64()
                .unwrap_or(0),
        };

        let perms = settings.get("permissions");
        let reminders = settings.get("time_and_reminders");
        let permissions = PermissionsState {
            auto_apply_portal: perms
                .and_then(|p| p["auto_apply_portal"].as_bool())
                .unwrap_or(false),
            auto_send_first_contact_email: perms
                .and_then(|p| p["auto_send_first_contact_email"].as_bool())
                .unwrap_or(false),
            auto_reply_threads: perms
                .and_then(|p| p["auto_reply_threads"].as_bool())
                .unwrap_or(false),
            auto_book_calendar: perms
                .and_then(|p| p["auto_book_calendar"].as_bool())
                .unwrap_or(false),
            auto_archive_mail: perms
                .and_then(|p| p["auto_archive_mail"].as_bool())
                .unwrap_or(false),
            brief_lead_time_minutes: reminders
                .and_then(|r| r["brief_lead_time_minutes"].as_i64())
                .unwrap_or(90),
        };

        // Map derived urgency by record_path from review_queue
        let mut urgency_by_path: std::collections::HashMap<String, (String, String)> =
            std::collections::HashMap::new();
        let mut attention_items = Vec::new();
        for item in queue["items"].as_array().into_iter().flatten() {
            let urg = item["urgency"].as_str().unwrap_or("background").to_owned();
            let reason = item["urgency_reason"].as_str().unwrap_or("").to_owned();
            let rpath = item["record_path"].as_str().map(str::to_owned);
            if let Some(p) = &rpath {
                urgency_by_path.insert(p.clone(), (urg.clone(), reason.clone()));
            }
            if urg == "urgent_now" || urg == "upcoming" {
                attention_items.push(AttentionEntry {
                    urgency: urg,
                    company: item["company"]
                        .as_str()
                        .or_else(|| item["title"].as_str())
                        .unwrap_or("Role")
                        .to_owned(),
                    detail: reason,
                    record_path: rpath,
                });
            }
        }

        // Load pipeline records cleanly (local fast path or via find_application)
        let records = self.load_records()?;
        let total_records = records.len();
        let mut active_cards = Vec::new();
        let mut archived_cards = Vec::new();

        for row in records {
            let rec_name = row["record"].as_str().unwrap_or_default();
            let record_path = if rec_name.starts_with("applications/") {
                rec_name.to_owned()
            } else {
                format!("applications/{rec_name}")
            };
            let title_full = row["title"].as_str().unwrap_or("Untitled Role");
            let (company, role_title) = split_company_role(row["company"].as_str(), title_full);
            let stage = row["stage"].as_str().unwrap_or("saved").to_owned();
            let column = stage_to_column(&stage);
            let (urgency, urgency_reason) = urgency_by_path
                .get(&record_path)
                .cloned()
                .unwrap_or_else(|| ("background".into(), String::new()));

            let scheduled_start = row["scheduled_start"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let checked = row["checked"].as_str().unwrap_or("").to_owned();
            let next_action = row["next_action"].as_str().unwrap_or("").to_owned();
            let brief_path = row["brief"].as_str().filter(|s| !s.is_empty()).map(|b| {
                if b.starts_with("applications/") {
                    b.to_owned()
                } else {
                    format!("applications/{b}")
                }
            });
            let source_url = row["vacancy_url"]
                .as_str()
                .or_else(|| row["url"].as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_owned);

            let note_line = if urgency == "urgent_now" {
                scheduled_start
                    .as_deref()
                    .map(short_time_label)
                    .or_else(|| Some("Action needed".into()))
            } else if urgency == "upcoming" && urgency_reason.contains("quiet for") {
                extract_quiet_badge(&urgency_reason)
            } else if stage == "technical" {
                Some("Technical".into())
            } else if stage == "final" {
                Some("Final round".into())
            } else if stage == "offer" {
                Some("Offer".into())
            } else {
                None
            };

            let search_blob = format!(
                "{} {} {} {} {}",
                company, role_title, stage, next_action, checked
            )
            .to_ascii_lowercase();

            let card = RoleCard {
                record_path,
                company,
                title: role_title,
                stage,
                column,
                urgency,
                urgency_reason,
                note_line,
                checked,
                scheduled_start,
                next_action,
                brief_path,
                cv_track: None,
                packet_hash: None,
                source_url,
                search_blob,
            };
            if column == "Archive" {
                archived_cards.push(card);
            } else {
                active_cards.push(card);
            }
        }

        // Prioritize urgent -> upcoming -> newest checked inside each column
        active_cards.sort_by(|a, b| {
            let ur = |u: &str| match u {
                "urgent_now" => 0,
                "upcoming" => 1,
                _ => 2,
            };
            ur(&a.urgency)
                .cmp(&ur(&b.urgency))
                .then_with(|| b.checked.cmp(&a.checked))
                .then_with(|| a.company.cmp(&b.company))
        });

        let cv_nodes = build_cv_tree(self.target.local_root(), settings, &queue["track_fit"]);
        let suggest_new_track = queue["track_fit"]["suggest_new_track"]
            .as_bool()
            .unwrap_or(false);
        let unmatched_track_titles = queue["track_fit"]["sample_unmatched_titles"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();

        let unfinished_runs = health["unfinished_run_count"].as_u64().unwrap_or(0);
        let unresolved_attempts = health["unresolved_attempt_count"].as_u64().unwrap_or(0);
        let closed_count = archived_cards.len();

        Ok(WorkspaceSnapshot {
            total_records,
            closed_count,
            active_cards,
            archived_cards,
            urgent_count: queue["urgency_counts"]["urgent_now"].as_u64().unwrap_or(0),
            upcoming_count: queue["urgency_counts"]["upcoming"].as_u64().unwrap_or(0),
            attention_items,
            daily_budget,
            permissions,
            cv_nodes,
            suggest_new_track,
            unmatched_track_titles,
            unfinished_runs,
            unresolved_attempts,
            healthy: unfinished_runs == 0,
            status_note: if unfinished_runs == 0 {
                format!("SQLite WAL · {total_records} roles")
            } else {
                format!("{unfinished_runs} unfinished run(s) need reconcile")
            },
        })
    }

    fn load_records(&self) -> Result<Vec<Value>> {
        if let Some(root) = self.target.local_root() {
            let pjson = root.join("applications/pipeline.json");
            if pjson.is_file()
                && let Ok(bytes) = fs::read(&pjson)
                && let Ok(parsed) = serde_json::from_slice::<Value>(&bytes)
                && let Some(rows) = parsed["records"].as_array()
            {
                return Ok(rows.clone());
            }
            let built = job_harness_core::pipeline::build(root)?;
            return Ok(built.records);
        }
        let res = self.call("find_application", json!({"query": "", "limit": 200}))?;
        Ok(res["data"]["matches"]
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    /// Guarded optimistic Kanban move using an atomic `execute_batch`:
    /// `start_run` -> `update_application` (with `expected_record_sha256` + attribution) -> `finish_run`.
    /// Never fabricates receipts, submission attempts, or `scheduled_start` timestamps.
    pub fn move_role_stage(
        &self,
        record_path: &str,
        from_stage: &str,
        target_stage: &str,
        next_action: &str,
        has_scheduled_start: bool,
    ) -> Result<String> {
        let current = self.call("read_record", json!({"path": record_path}))?;
        if current["ok"] != true {
            bail!(
                "{}",
                current["error"]["message"]
                    .as_str()
                    .unwrap_or("Could not read record before stage update")
            );
        }
        let expected_sha = current["data"]["sha256"]
            .as_str()
            .context("Record missing sha256")?;
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let op_id = Uuid::new_v4();

        // Deterministic invariant-safe next_action without fabricating external facts
        let safe_next_action = match target_stage {
            "applied" if from_stage == "saved" || from_stage == "prepared" => {
                "Manual board move to applied — verify receipt on next reconcile"
            }
            "technical" if !has_scheduled_start => {
                "Confirm interview time & calendar invite on next reconcile"
            }
            _ if next_action.trim().is_empty() => "Updated manually on desktop board",
            _ => next_action,
        };
        let summary = format!(
            "Desktop board drag ({today}): {from_stage} -> {target_stage} (manual; verify receipts/calendar on next reconcile)"
        );

        let mut update_args = json!({
            "run_id": { "$ref": "start.data.run_id" },
            "record": record_path,
            "expected_record_sha256": expected_sha,
            "tracking": {
                "stage": target_stage,
                "checked": today,
                "next_action": safe_next_action
            },
            "summary": summary,
            "evidence": [record_path],
            "idempotency_key": format!("gui-move:{op_id}")
        });
        if is_stage_regression(from_stage, target_stage) {
            update_args["correction_reason"] = json!(format!(
                "User manually moved card on desktop board ({from_stage} -> {target_stage}); prior history preserved."
            ));
        }

        let batch = self.call(
            "execute_batch",
            json!({
                "steps": [
                    {
                        "id": "start",
                        "tool": "start_run",
                        "arguments": {
                            "operation": "daily_update",
                            "scope": { "record": record_path },
                            "user_request": summary,
                            "idempotency_key": format!("gui-start:{op_id}")
                        }
                    },
                    {
                        "id": "update",
                        "tool": "update_application",
                        "arguments": update_args
                    },
                    {
                        "id": "finish",
                        "tool": "finish_run",
                        "arguments": {
                            "run_id": { "$ref": "start.data.run_id" },
                            "status": "completed",
                            "summary": summary,
                            "evidence": [record_path],
                            "idempotency_key": format!("gui-finish:{op_id}")
                        }
                    }
                ]
            }),
        )?;
        if batch["ok"] != true {
            let msg = batch_step_error(&batch, "update")
                .or_else(|| batch_step_error(&batch, "finish"))
                .or_else(|| batch["error"]["message"].as_str())
                .unwrap_or("Stage update refused by workspace guard");
            bail!("{msg}");
        }
        Ok(format!("Moved to {target_stage}"))
    }

    pub fn read_text_preview(&self, rel_path: &str) -> Result<String> {
        let res = self.call("read_record", json!({"path": rel_path}))?;
        if res["ok"] != true {
            bail!(
                "{}",
                res["error"]["message"]
                    .as_str()
                    .unwrap_or("Could not read cited file")
            );
        }
        let content = res["data"]["content"].as_str().unwrap_or("");
        let trimmed: String = content.chars().take(1400).collect();
        Ok(trimmed)
    }

    pub fn update_ledger_config(&self, mutate: impl FnOnce(&mut Value)) -> Result<()> {
        let Some(root) = self.target.local_root() else {
            bail!("Editing permissions/caps directly requires a local workspace; ask Chat on VPS.");
        };
        let _guard = storage::WorkspaceLock::acquire(root, true)?;
        let path = root.join("config/career-ledger.json");
        let mut cfg: Value = serde_json::from_slice(&fs::read(&path)?)?;
        mutate(&mut cfg);
        job_harness_core::contracts::validate(job_harness_core::contracts::ledger_schema(), &cfg)?;
        let encoded = serde_json::to_vec_pretty(&cfg)?;
        storage::atomic_write(&path, &encoded)?;
        Ok(())
    }

    pub fn import_primary_cv(&self, source_pdf: &Path) -> Result<String> {
        let Some(root) = self.target.local_root() else {
            bail!("CV import requires a local workspace.");
        };
        let imported = job_harness_core::presentation::import_source(root, source_pdf)?;
        let imported_path = imported["path"]
            .as_str()
            .context("Import did not return path")?
            .to_owned();
        self.update_ledger_config(|cfg| {
            cfg["primary_cv"] = json!(imported_path.clone());
        })?;
        Ok(imported_path)
    }
}

pub fn spawn_change_listener(
    target: WorkspaceTarget,
    tx: Sender<()>,
) -> Option<RecommendedWatcher> {
    match target {
        WorkspaceTarget::Local(root) => {
            let mut watcher =
                notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                    if let Ok(ev) = res
                        && !matches!(ev.kind, notify::EventKind::Access(_))
                    {
                        let _ = tx.send(());
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
        WorkspaceTarget::Remote { url, token } => {
            std::thread::spawn(move || {
                let client = reqwest::blocking::Client::builder().build().ok();
                let Some(client) = client else { return };
                loop {
                    let endpoint = format!("{}/v1/events", url.trim_end_matches('/'));
                    let mut req = client.get(&endpoint);
                    if let Some(tok) = &token {
                        req = req.bearer_auth(tok);
                    }
                    if let Ok(resp) = req.send() {
                        let reader = BufReader::new(resp);
                        for line in reader.lines().map_while(Result::ok) {
                            if line.starts_with("event: changed") {
                                let _ = tx.send(());
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_secs(3));
                }
            });
            None
        }
    }
}

fn batch_step_data<'a>(batch: &'a Value, id: &str) -> Option<&'a Value> {
    batch["data"]["results"]
        .as_array()?
        .iter()
        .find(|row| row["id"].as_str() == Some(id))
        .map(|row| &row["result"]["data"])
}

fn batch_step_error<'a>(batch: &'a Value, id: &str) -> Option<&'a str> {
    batch["data"]["results"]
        .as_array()?
        .iter()
        .find(|row| row["id"].as_str() == Some(id))
        .and_then(|row| row["result"]["error"]["message"].as_str())
}

fn is_stage_regression(from: &str, to: &str) -> bool {
    const LADDER: &[&str] = &[
        "saved",
        "prepared",
        "applied",
        "introduction",
        "screening",
        "technical",
        "final",
        "offer",
    ];
    if from == "closed" && to != "closed" {
        return true;
    }
    match (
        LADDER.iter().position(|s| *s == from),
        LADDER.iter().position(|s| *s == to),
    ) {
        (Some(a), Some(b)) => b < a,
        _ => false,
    }
}

fn split_company_role(company: Option<&str>, full_title: &str) -> (String, String) {
    for sep in [" — ", " – ", " - "] {
        if let Some((left, right)) = full_title.split_once(sep) {
            return (
                company
                    .filter(|s| !s.is_empty())
                    .unwrap_or(left)
                    .trim()
                    .to_owned(),
                right.trim().to_owned(),
            );
        }
    }
    (
        company
            .filter(|s| !s.is_empty())
            .unwrap_or(full_title)
            .trim()
            .to_owned(),
        full_title.trim().to_owned(),
    )
}

fn short_time_label(iso: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(iso) {
        let local = dt.with_timezone(&chrono::Local);
        let today = chrono::Local::now().date_naive();
        let d = local.date_naive();
        if d == today {
            return format!("Today {}", local.format("%H:%M"));
        }
        if d == today.succ_opt().unwrap_or(today) {
            return format!("Tomorrow {}", local.format("%H:%M"));
        }
        return local.format("%a %H:%M").to_string();
    }
    iso.chars().take(16).collect()
}

fn extract_quiet_badge(reason: &str) -> Option<String> {
    let idx = reason.find("quiet for ")?;
    let rest = &reason[idx + 10..];
    let days = rest.split_whitespace().next()?;
    Some(format!("Quiet {days}"))
}

fn build_cv_tree(local_root: Option<&Path>, settings: &Value, track_fit: &Value) -> Vec<CvNode> {
    let mut nodes = Vec::new();
    let primary = settings["primary_cv"]
        .as_str()
        .unwrap_or("output/primary-cv.pdf");
    let primary_full = local_root.map(|r| r.join(primary));
    nodes.push(CvNode {
        name: "Master CV".into(),
        relative_path: primary.to_owned(),
        exists: primary_full.as_ref().is_some_and(|p| p.is_file()),
        full_path: primary_full,
        is_master: true,
        matched_roles: None,
    });

    let matched = track_fit.get("matched_by_track").and_then(Value::as_object);
    let tracks: Vec<String> = match settings.get("cv_tracks") {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(Value::Object(map)) => map.keys().cloned().collect(),
        _ => vec![],
    };
    let stem = Path::new(primary)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("cv.pdf");
    for track in tracks {
        let rel = format!("output/tracks/{track}/{stem}");
        let full = local_root.map(|r| r.join(&rel));
        nodes.push(CvNode {
            name: track.clone(),
            relative_path: rel,
            exists: full.as_ref().is_some_and(|p| p.is_file()),
            full_path: full,
            is_master: false,
            matched_roles: matched.and_then(|m| m.get(&track)).and_then(Value::as_u64),
        });
    }
    nodes
}

pub fn open_in_system(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(path)
        .spawn();
}

pub fn reveal_in_file_manager(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open")
        .args(["-R"])
        .arg(path)
        .spawn();
    #[cfg(target_os = "linux")]
    if let Some(parent) = path.parent() {
        let _ = std::process::Command::new("xdg-open").arg(parent).spawn();
    }
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_stage_mapping_uses_valid_core_stages() {
        assert_eq!(stage_to_column("saved"), "Saved");
        assert_eq!(stage_to_column("applied"), "Applied");
        assert_eq!(stage_to_column("screening"), "Screening");
        assert_eq!(stage_to_column("technical"), "Interviews");
        assert_eq!(stage_to_column("closed"), "Archive");
        assert_eq!(column_to_stage("Saved"), "saved");
        assert_eq!(column_to_stage("Applied"), "applied");
        assert_eq!(column_to_stage("Screening"), "screening");
        assert_eq!(column_to_stage("Interviews"), "technical");
    }

    #[test]
    fn manual_kanban_move_is_atomic_and_leaves_no_open_runs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        job_harness_core::workspace::initialize(root).unwrap();
        let rec = root.join("applications/2026-11-23-acme-engineer.md");
        fs::write(
            &rec,
            "# Acme — Backend Engineer\n\n<!-- tracking\n{\"stage\":\"saved\",\"checked\":\"2026-11-23\",\"next_action\":\"Review role\"}\n-->\n",
        )
        .unwrap();

        let client = WorkspaceClient::new(WorkspaceTarget::Local(root.to_path_buf()));
        let res = client
            .move_role_stage(
                "applications/2026-11-23-acme-engineer.md",
                "saved",
                "applied",
                "Review role",
                false,
            )
            .unwrap();
        assert!(res.contains("applied"));

        let snap = client.load_snapshot().unwrap();
        assert_eq!(snap.unfinished_runs, 0);
        let moved = snap
            .active_cards
            .iter()
            .find(|c| c.record_path == "applications/2026-11-23-acme-engineer.md")
            .expect("card exists");
        assert_eq!(moved.stage, "applied");
        assert_eq!(moved.column, "Applied");
        assert!(moved.next_action.contains("reconcile"));
    }
}
