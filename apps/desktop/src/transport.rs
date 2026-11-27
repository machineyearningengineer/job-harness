//! Unified local + remote VPS workspace transport over Job Harness.
//! Local mode calls `job-harness-core::Engine` directly in-process (zero HTTP overhead).
//! Remote mode reaches `jh serve` through the app's own SSH tunnel ([`crate::ssh`]), or a
//! given URL, with live SSE events.
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
    /// A `jh serve` socket, `unix:PATH`.
    Remote {
        url: String,
    },
}

impl WorkspaceTarget {
    pub fn label(&self) -> String {
        match self {
            Self::Local(p) => p
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("workspace")
                .to_owned(),
            Self::Remote { url } => format!(
                "Remote ({})",
                crate::ssh::tunnel()
                    .map_or_else(|| url.trim_start_matches("unix:"), |t| t.target())
            ),
        }
    }
    #[allow(dead_code)]
    pub fn monogram(&self) -> String {
        let name = self.label();
        let parts: Vec<&str> = name
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|s| !s.is_empty())
            .collect();
        let raw = if parts.len() >= 2 {
            let a = parts[0].chars().next().unwrap_or('W');
            let b = parts[1].chars().next().unwrap_or('S');
            format!("{a}{b}")
        } else {
            name.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(2)
                .collect()
        };
        if raw.is_empty() {
            "WS".into()
        } else {
            raw.to_ascii_uppercase()
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
    pub furthest_stage: Option<String>,
    pub outcome: Option<String>,
    pub column: &'static str,
    pub urgency: String,
    pub urgency_reason: String,
    pub quiet_days: Option<u64>,
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

#[derive(Clone, Debug)]
pub struct ActivityDigestEntry {
    pub at: String,
    pub age_label: String,
    pub tool: String,
    pub summary: String,
    pub record_path: Option<String>,
    pub is_human: bool,
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
    pub auto_archive_mail: bool,
    pub brief_lead_time_minutes: i64,
}

impl Default for PermissionsState {
    fn default() -> Self {
        Self {
            auto_apply_portal: false,
            auto_send_first_contact_email: false,
            auto_reply_threads: false,
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
    pub enabled: bool,
    pub matched_roles: Option<u64>,
    pub exists: bool,
}

#[derive(Clone, Debug)]
pub struct ProfileDoc {
    pub group: &'static str,
    pub label: String,
    pub relative_path: String,
    pub full_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct ProfileSettings {
    pub application_email: String,
    pub sender_email: String,
    pub timezone: String,
    pub currency: String,
    pub min_base_salary: u64,
    pub target_base_salary: u64,
    pub location_preferences: String,
    pub notice_period: String,
    pub target_roles: String,
    pub discovery_sources: String,
    pub avoid_list: String,
    pub tone: String,
    pub cover_letter_mode: String,
    pub cover_letter_template: String,
    pub post_rejection_reply: String,
    pub applied_ghost_days: u64,
    /// Effective interview windows per kind (defaults merged with config), e.g. "Mon-Fri 10:00-12:00".
    pub schedule_windows: Vec<(&'static str, String)>,
    pub schedule_hours: String,
    pub schedule_max_per_day: u64,
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
    pub recent_activity: Vec<ActivityDigestEntry>,
    pub latest_activity_at: Option<String>,
    pub daily_budget: DailyBudget,
    pub permissions: PermissionsState,
    pub profile_settings: ProfileSettings,
    pub cv_nodes: Vec<CvNode>,
    pub profile_docs: Vec<ProfileDoc>,
    pub suggest_new_track: bool,
    pub unmatched_track_titles: Vec<String>,
    pub unfinished_runs: u64,
    pub unresolved_attempts: u64,
    pub healthy: bool,
    pub status_note: String,
    /// What a remote target is connected to, from `jh serve`'s health response.
    pub remote_label: Option<String>,
}

/// `ledger · alice@server` from a `jh serve` `/v1/health` body; `None` for servers that predate it.
pub fn remote_label(health: &Value) -> Option<String> {
    let host = health["host"].as_str()?;
    Some(match health["name"].as_str() {
        Some(name) => format!("{name} · {host}"),
        None => host.to_owned(),
    })
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
        "Interviews" | "Technical" => "technical",
        "Final" => "final",
        "Offer" => "offer",
        _ => "closed",
    }
}

#[derive(Clone)]
pub struct WorkspaceClient {
    pub target: WorkspaceTarget,
    http: reqwest::blocking::Client,
    base: String,
}

/// An HTTP client for `url` (`unix:PATH` or `http://...`) and the base its paths go under.
fn http_client(url: &str, timeout: Option<Duration>) -> (reqwest::blocking::Client, String) {
    let builder = reqwest::blocking::Client::builder().timeout(timeout);
    #[cfg(unix)]
    if let Some(path) = url.strip_prefix("unix:") {
        let client = builder.unix_socket(path).build().expect("http client");
        return (client, "http://localhost".to_owned());
    }
    let base = url.trim_end_matches('/').to_owned();
    (builder.build().expect("http client"), base)
}

/// Fail fast while the tunnel is down: the board stays as last seen, read-only, and
/// nothing is queued.
pub fn ensure_online() -> Result<()> {
    if let Some(tunnel) = crate::ssh::tunnel()
        && tunnel.link().0 != crate::ssh::Link::Connected
    {
        bail!(
            "Offline: {} is not reachable, so changes are disabled until it reconnects.",
            tunnel.target()
        );
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct RoleOverviewDetails {
    pub salary: Option<String>,
    pub location_mode: Option<String>,
    pub fit_summary: Option<String>,
    #[allow(dead_code)]
    pub status_detail: Option<String>,
    pub source_channel: Option<String>,
    pub contact: Option<String>,
    pub submitted_at: Option<String>,
    pub packet_cv: Option<String>,
    pub packet_answers: Option<String>,
    pub packet_sha: Option<String>,
    pub full_text: String,
}

impl WorkspaceClient {
    pub fn new(target: WorkspaceTarget) -> Self {
        let url = match &target {
            WorkspaceTarget::Remote { url, .. } => url.as_str(),
            WorkspaceTarget::Local(_) => "",
        };
        let (http, base) = http_client(url, Some(Duration::from_secs(10)));
        Self { target, http, base }
    }

    pub fn call(&self, tool: &str, arguments: Value) -> Result<Value> {
        match &self.target {
            WorkspaceTarget::Local(root) => {
                let engine = Engine::open(root)?;
                Ok(engine.call(tool, &arguments))
            }
            WorkspaceTarget::Remote { .. } => {
                ensure_online()?;
                let resp = self
                    .http
                    .post(format!("{}/v1/call", self.base))
                    .json(&json!({"tool": tool, "arguments": arguments}))
                    .send()
                    .context("Remote workspace request failed")?;
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
                    {"id": "queue", "tool": "review_queue", "arguments": {"limit": 50}},
                    {"id": "events", "tool": "list_events", "arguments": {"limit": 30}}
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
        let events_data = batch_step_data(&batch, "events").unwrap_or(&Value::Null);

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
        let disc = settings.get("discovery");
        let comm = settings.get("communication");
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
            auto_archive_mail: perms
                .and_then(|p| p["auto_archive_mail"].as_bool())
                .unwrap_or(false),
            brief_lead_time_minutes: reminders
                .and_then(|r| {
                    r["pre_interview_reminder_minutes"]
                        .as_i64()
                        .or_else(|| r["brief_lead_time_minutes"].as_i64())
                })
                .unwrap_or(90),
        };
        let join_str_array = |val: Option<&Value>| -> String {
            val.and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default()
        };
        let policy = job_harness_core::scheduling::Policy::from_settings(settings)
            .or_else(|_| job_harness_core::scheduling::Policy::from_settings(&json!({})))
            .expect("built-in scheduling defaults are valid");
        let profile_settings = ProfileSettings {
            schedule_windows: job_harness_core::scheduling::KINDS
                .iter()
                .map(|k| {
                    (
                        *k,
                        job_harness_core::scheduling::format_windows(&policy.kinds[k].windows),
                    )
                })
                .collect(),
            schedule_hours: format!(
                "{}-{}",
                policy.earliest.format("%H:%M"),
                policy.latest.format("%H:%M")
            ),
            schedule_max_per_day: policy.max_per_day,
            application_email: comm
                .and_then(|c| c["application_email"].as_str())
                .unwrap_or("")
                .to_owned(),
            sender_email: comm
                .and_then(|c| c["sender_email"].as_str())
                .unwrap_or("")
                .to_owned(),
            timezone: settings["timezone"].as_str().unwrap_or("UTC").to_owned(),
            currency: disc
                .and_then(|d| d["currency"].as_str())
                .unwrap_or("GBP")
                .to_owned(),
            min_base_salary: disc
                .and_then(|d| d["min_base_salary"].as_u64())
                .unwrap_or(0),
            target_base_salary: disc
                .and_then(|d| d["target_base_salary"].as_u64())
                .unwrap_or(0),
            location_preferences: join_str_array(disc.and_then(|d| d.get("location_preferences"))),
            notice_period: disc
                .and_then(|d| d["notice_period"].as_str())
                .unwrap_or("")
                .to_owned(),
            target_roles: join_str_array(settings.get("target_roles")),
            discovery_sources: join_str_array(disc.and_then(|d| d.get("sources"))),
            avoid_list: join_str_array(disc.and_then(|d| d.get("avoid_list"))),
            tone: comm
                .and_then(|c| c["tone"].as_str())
                .unwrap_or("")
                .to_owned(),
            cover_letter_mode: comm
                .and_then(|c| c["cover_letter_mode"].as_str())
                .unwrap_or("only_when_required")
                .to_owned(),
            cover_letter_template: comm
                .and_then(|c| c["cover_letter_template"].as_str())
                .unwrap_or("")
                .to_owned(),
            post_rejection_reply: comm
                .and_then(|c| c["post_rejection_reply"].as_str())
                .unwrap_or("off")
                .to_owned(),
            applied_ghost_days: reminders
                .and_then(|r| r["applied_ghost_days"].as_u64())
                .unwrap_or(30),
        };

        // Load pipeline records first so attention items can display exact interview countdowns
        let records = self.load_records()?;
        let scheduled_by_path: std::collections::HashMap<String, String> = records
            .iter()
            .filter_map(|row| {
                let rec = row["record"].as_str()?;
                let rpath = if rec.starts_with("applications/") {
                    rec.to_owned()
                } else {
                    format!("applications/{rec}")
                };
                let start = row["scheduled_start"]
                    .as_str()
                    .filter(|s| !s.is_empty())?
                    .to_owned();
                Some((rpath, start))
            })
            .collect();

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
                let detail = rpath
                    .as_deref()
                    .and_then(|p| scheduled_by_path.get(p))
                    .and_then(|start| format_interview_countdown(start))
                    .unwrap_or_else(|| concise_reason(&reason));
                attention_items.push(AttentionEntry {
                    urgency: urg,
                    company: item["company"]
                        .as_str()
                        .or_else(|| item["title"].as_str())
                        .unwrap_or("Role")
                        .to_owned(),
                    detail,
                    record_path: rpath,
                });
            }
        }

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

            let explicit_outcome = row["outcome"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned);
            let status_detail_low = row["status_detail"]
                .as_str()
                .unwrap_or("")
                .to_ascii_lowercase();
            // Preserve explicit tracking.outcome first; on closed rows without an outcome field,
            // classify explicit candidate withdrawals first, then explicit employer rejections
            // (never guessing silence as rejection).
            let outcome = explicit_outcome.or_else(|| {
                if column != "Archive" && stage != "closed" {
                    return None;
                }
                if status_detail_low.contains("withdrawn")
                    || status_detail_low.contains("withdrew")
                    || status_detail_low.contains("candidate declined")
                    || status_detail_low.contains("candidate withdrew")
                    || status_detail_low.contains("ended pursuit")
                    || status_detail_low.contains("pulled out")
                    || status_detail_low.contains("decided not to proceed")
                {
                    Some("withdrawn".into())
                } else if status_detail_low.contains("rejected")
                    || status_detail_low.contains("rejection")
                    || status_detail_low.contains("declined")
                    || status_detail_low.contains("declines")
                    || status_detail_low.contains("not progressing")
                    || status_detail_low.contains("not moving forward")
                    || status_detail_low.contains("unsuccessful")
                {
                    Some("rejected".into())
                } else {
                    None
                }
            });
            let note_line = if urgency == "urgent_now" {
                scheduled_start
                    .as_deref()
                    .map(short_time_label)
                    .or_else(|| Some("Action needed".into()))
            } else if urgency == "upcoming" && urgency_reason.contains("quiet for") {
                extract_quiet_badge(&urgency_reason)
            } else if column == "Archive" {
                match outcome
                    .as_deref()
                    .map(|s| s.to_ascii_lowercase())
                    .as_deref()
                {
                    Some("rejected" | "rejection") => Some("Rejected".into()),
                    Some("withdrawn" | "declined") => Some("Withdrawn".into()),
                    Some("expired") => Some("Expired".into()),
                    Some("filled") => Some("Role filled".into()),
                    Some("accepted") => Some("Accepted".into()),
                    _ => None,
                }
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

            let furthest_stage = if column == "Archive" || stage == "closed" {
                let explicit = row["furthest_stage"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().to_ascii_lowercase());
                explicit.or_else(|| {
                    if status_detail_low.contains("offer") {
                        Some("offer".into())
                    } else if status_detail_low.contains("final interview")
                        || status_detail_low.contains("final round")
                        || status_detail_low.contains("final stage")
                        || status_detail_low.contains("on-site")
                        || status_detail_low.contains("onsite")
                    {
                        Some("final".into())
                    } else if status_detail_low.contains("technical")
                        || status_detail_low.contains("interview")
                        || status_detail_low.contains("take-home")
                        || status_detail_low.contains("coding test")
                        || status_detail_low.contains("live coding")
                        || status_detail_low.contains("system design")
                    {
                        Some("technical".into())
                    } else if status_detail_low.contains("screen")
                        || status_detail_low.contains("intro call")
                        || status_detail_low.contains("recruiter call")
                        || status_detail_low.contains("phone call")
                        || brief_path.is_some()
                    {
                        Some("screening".into())
                    } else {
                        None
                    }
                })
            } else {
                Some(stage.clone())
            };
            let quiet_days = extract_quiet_days(&urgency_reason);
            let card = RoleCard {
                record_path,
                company,
                title: role_title,
                stage,
                furthest_stage,
                outcome,
                column,
                urgency,
                urgency_reason,
                quiet_days,
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

        let mut recent_activity: Vec<ActivityDigestEntry> = Vec::new();
        if let Some(arr) = events_data["events"].as_array() {
            let mut sorted_events: Vec<&Value> = arr.iter().collect();
            sorted_events.sort_by(|a, b| {
                let ta = a["at"].as_str().unwrap_or("");
                let tb = b["at"].as_str().unwrap_or("");
                tb.cmp(ta)
            });
            for ev in sorted_events {
                let raw_tool = ev["tool"].as_str().unwrap_or("");
                let raw_summary = ev["summary"].as_str().unwrap_or("").trim();
                if raw_summary.is_empty() && raw_tool == "start_run" {
                    continue;
                }
                let at = ev["at"].as_str().unwrap_or("").to_owned();
                let idem_key = ev["idempotency_key"].as_str().unwrap_or("");
                let record_path = ev["record_path"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                let tool_label = human_tool_label(raw_tool);
                let mut display_summary = if !raw_summary.is_empty() {
                    raw_summary.to_owned()
                } else {
                    match raw_tool {
                        "finish_run" => "Completed bounded run".into(),
                        "update_application" | "record_event" => "Updated role tracking".into(),
                        "finish_submission" => "Recorded submission receipt".into(),
                        "daily_update" => "Processed daily inbox & calendar digest".into(),
                        _ => format!("Recorded {tool_label}"),
                    }
                };
                if display_summary.eq_ignore_ascii_case("application receipt")
                    && let Some(rp) = &record_path
                {
                    let company = company_hint_from_record_path(rp);
                    if !company.is_empty() {
                        display_summary = format!("{company} — application receipt");
                    }
                }
                let sum_low = display_summary.to_ascii_lowercase();
                let is_human = idem_key.starts_with("gui-")
                    || idem_key.starts_with("manual-")
                    || idem_key.starts_with("user-")
                    || sum_low.contains("manual move")
                    || sum_low.contains("kanban")
                    || sum_low.contains("user confirmed")
                    || sum_low.contains("tidy");
                let age_label = short_time_label(&at);
                if let Some(prev) = recent_activity.last_mut()
                    && (prev.summary == display_summary
                        || prev.summary.starts_with(&format!("{display_summary} (")))
                    && prev.age_label == age_label
                {
                    if !prev.summary.ends_with("multiple)") {
                        prev.summary = format!("{display_summary} (multiple)");
                    }
                    continue;
                }
                recent_activity.push(ActivityDigestEntry {
                    age_label,
                    at,
                    tool: tool_label.to_owned(),
                    summary: display_summary,
                    record_path,
                    is_human,
                });
                if recent_activity.len() >= 12 {
                    break;
                }
            }
        }
        let latest_activity_at = recent_activity.first().map(|a| a.at.clone());

        let profile_docs = self.discover_profile_docs();

        Ok(WorkspaceSnapshot {
            total_records,
            closed_count,
            active_cards,
            archived_cards,
            urgent_count: queue["urgency_counts"]["urgent_now"].as_u64().unwrap_or(0),
            upcoming_count: queue["urgency_counts"]["upcoming"].as_u64().unwrap_or(0),
            attention_items,
            recent_activity,
            latest_activity_at,
            daily_budget,
            permissions,
            profile_settings,
            cv_nodes,
            profile_docs,
            suggest_new_track,
            unmatched_track_titles,
            unfinished_runs,
            unresolved_attempts,
            healthy: unfinished_runs == 0,
            status_note: if unfinished_runs == 0 {
                format!("Local ledger ({total_records} roles)")
            } else {
                format!("{unfinished_runs} unfinished run(s) need reconcile")
            },
            remote_label: self.remote_health().as_ref().and_then(remote_label),
        })
    }

    fn remote_health(&self) -> Option<Value> {
        let WorkspaceTarget::Remote { .. } = &self.target else {
            return None;
        };
        let health = self.http.get(format!("{}/v1/health", self.base));
        health.send().ok()?.json().ok()
    }

    fn discover_profile_docs(&self) -> Vec<ProfileDoc> {
        let Some(root) = self.target.local_root() else {
            return Vec::new();
        };
        let mut docs = Vec::new();
        let known: [(&'static str, &str, &str); 6] = [
            (
                "Defaults & screening",
                "Approved screening answers",
                "applications/approved-answers.md",
            ),
            (
                "Defaults & screening",
                "Application decisions & preferences",
                "context/profile/application-decisions.md",
            ),
            (
                "Career context",
                "Story bank (CARL stories)",
                "context/profile/story-bank.md",
            ),
            (
                "Career context",
                "Career learning & coaching",
                "context/profile/career-learning.md",
            ),
            (
                "Career context",
                "Baseline CV evidence",
                "context/baseline-evidence.md",
            ),
            (
                "Career context",
                "Master CV facts (JSON)",
                "content/baseline.json",
            ),
        ];
        for (group, label, rel) in known {
            let full = root.join(rel);
            if full.is_file() {
                docs.push(ProfileDoc {
                    group,
                    label: label.to_owned(),
                    relative_path: rel.to_owned(),
                    full_path: Some(full),
                });
            }
        }
        let proj_dir = root.join("context/projects");
        if let Ok(entries) = fs::read_dir(&proj_dir) {
            let mut projs: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
                .collect();
            projs.sort();
            for p in projs {
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    let stem = name.trim_end_matches(".md").replace('-', " ");
                    let mut ch = stem.chars();
                    let title = match ch.next() {
                        Some(f) => format!("{}{}", f.to_uppercase(), ch.as_str()),
                        None => stem,
                    };
                    docs.push(ProfileDoc {
                        group: "Projects & evidence",
                        label: title,
                        relative_path: format!("context/projects/{name}"),
                        full_path: Some(p),
                    });
                }
            }
        }
        docs
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
        // The server's own pipeline view, the file the local branch reads.
        use base64::Engine as _;
        let res = self.call(
            "read_artifact",
            json!({"path": "applications/pipeline.json"}),
        )?;
        if res["ok"] != true {
            bail!(
                "{}",
                res["error"]["message"]
                    .as_str()
                    .unwrap_or("Could not read the server's pipeline")
            );
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(res["data"]["content_base64"].as_str().unwrap_or_default())
            .context("Invalid pipeline encoding from server")?;
        let parsed: Value = serde_json::from_slice(&bytes).context("Invalid pipeline.json")?;
        Ok(parsed["records"].as_array().cloned().unwrap_or_default())
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

    /// Loads a role's canonical Markdown record + linked packet metadata off-thread
    /// and extracts at-a-glance Overview fields (salary, location/work mode, fit summary, CV proof).
    pub fn load_role_details(&self, record_path: &str) -> Result<RoleOverviewDetails> {
        let res = self.call("read_record", json!({"path": record_path}))?;
        if res["ok"] != true {
            bail!(
                "{}",
                res["error"]["message"]
                    .as_str()
                    .unwrap_or("Could not read role record")
            );
        }
        let raw = res["data"]["text"]
            .as_str()
            .or_else(|| res["data"]["content"].as_str())
            .unwrap_or("");
        let tracking = parse_tracking_json(raw);
        let status_detail = tracking
            .as_ref()
            .and_then(|t| t["status_detail"].as_str())
            .map(str::to_owned);
        let source_channel = tracking
            .as_ref()
            .and_then(|t| t["source"].as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let contact = tracking
            .as_ref()
            .and_then(|t| t["contact"].as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let mut packet_sha = tracking
            .as_ref()
            .and_then(|t| t["record_evidence"].as_str())
            .and_then(|ev| {
                ev.rsplit('/')
                    .next()
                    .and_then(|f| f.strip_suffix(".json"))
                    .filter(|h| h.len() == 64)
                    .map(str::to_owned)
            });
        let mut packet_cv = None;
        let mut packet_answers = None;
        let mut submitted_at = None;
        if let Some(root) = self.target.local_root()
            && let Some(ev_rel) = tracking
                .as_ref()
                .and_then(|t| t["record_evidence"].as_str())
        {
            let full_ev = if ev_rel.starts_with("applications/") {
                root.join(ev_rel)
            } else {
                root.join("applications").join(ev_rel)
            };
            if let Ok(bytes) = fs::read(&full_ev)
                && let Ok(pkt) = serde_json::from_slice::<Value>(&bytes)
            {
                packet_cv = pkt["cv"]["original_path"]
                    .as_str()
                    .or_else(|| pkt["cv"]["path"].as_str())
                    .map(str::to_owned);
                packet_answers = pkt["answers"]["original_path"]
                    .as_str()
                    .or_else(|| pkt["answers"]["path"].as_str())
                    .map(str::to_owned);
                submitted_at = pkt["prepared_at"]
                    .as_str()
                    .or_else(|| pkt["created_at"].as_str())
                    .map(str::to_owned);
                if packet_sha.is_none() {
                    packet_sha = pkt["cv"]["sha256"].as_str().map(str::to_owned);
                }
            }
        }
        let cleaned = strip_tracking_comment(raw);
        let fit_summary = extract_fit_summary(&cleaned);
        let salary = extract_salary_hint(&cleaned);
        let location_mode = extract_location_hint(&cleaned);
        let full_text: String = cleaned.trim().chars().take(2800).collect();

        Ok(RoleOverviewDetails {
            salary,
            location_mode,
            fit_summary,
            status_detail,
            source_channel,
            contact,
            submitted_at,
            packet_cv,
            packet_answers,
            packet_sha,
            full_text,
        })
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
        let raw = res["data"]["text"]
            .as_str()
            .or_else(|| res["data"]["content"].as_str())
            .unwrap_or("");
        // Strip machine tracking comment block for clean human reading (preserve full JSON & briefs!)
        let cleaned = if rel_path.ends_with(".json") {
            raw.to_owned()
        } else {
            strip_tracking_comment(raw)
        };
        let cap = if rel_path.ends_with(".json") {
            256_000
        } else {
            64_000
        };
        let trimmed: String = cleaned.trim().chars().take(cap).collect();
        if trimmed.is_empty() {
            Ok("Empty record.".into())
        } else {
            Ok(trimmed)
        }
    }

    pub fn update_ledger_config(&self, mutate: impl FnOnce(&mut Value)) -> Result<()> {
        let Some(root) = self.target.local_root() else {
            bail!(
                "Permissions and caps live in the server's config/career-ledger.json; the owner edits that file on the server. Agents cannot change config."
            );
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

    /// Safely replaces a child CV track PDF atomically (backing up any existing copy
    /// under `output/tracks/<track>/.backups/`). The agent automatically hashes and
    /// freezes the updated bytes on the next `prepare_application`.
    pub fn replace_track_cv(&self, relative_target: &str, source_pdf: &Path) -> Result<String> {
        let Some(root) = self.target.local_root() else {
            bail!("Replacing a track CV requires a local workspace.");
        };
        let bytes = fs::read(source_pdf).context("Could not read selected PDF")?;
        if !bytes.starts_with(b"%PDF-") {
            bail!("Selected file is not a valid PDF.");
        }
        let _guard = storage::WorkspaceLock::acquire(root, true)?;
        let dest = root.join(relative_target);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
            if dest.is_file() {
                let backup_dir = parent.join(".backups");
                fs::create_dir_all(&backup_dir)?;
                let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
                let stem = dest
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("cv.pdf");
                let _ = fs::copy(&dest, backup_dir.join(format!("{stamp}-{stem}")));
            }
        }
        storage::atomic_write(&dest, &bytes)?;
        Ok(relative_target.to_owned())
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
                    if let Ok(ev) = res {
                        let is_content_change = matches!(
                            ev.kind,
                            notify::EventKind::Create(_)
                                | notify::EventKind::Remove(_)
                                | notify::EventKind::Modify(
                                    notify::event::ModifyKind::Data(_)
                                        | notify::event::ModifyKind::Name(_)
                                )
                        );
                        if !is_content_change {
                            return;
                        }
                        let relevant = ev.paths.iter().any(|p| {
                            let s = p.to_string_lossy();
                            !s.ends_with(".sqlite3-wal")
                                && !s.ends_with(".sqlite3-shm")
                                && !s.ends_with(".sqlite3-journal")
                                && !s.ends_with(".tmp")
                                && !s.ends_with(".DS_Store")
                                && !s.contains("/.git/")
                        });
                        if relevant {
                            let _ = tx.send(());
                        }
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
        WorkspaceTarget::Remote { url } => {
            std::thread::spawn(move || {
                let (client, base) = http_client(&url, None);
                loop {
                    // Through a tunnel, wait for it instead of retrying blind.
                    while ensure_online().is_err() {
                        std::thread::sleep(Duration::from_millis(500));
                    }
                    if let Ok(resp) = client.get(format!("{base}/v1/events")).send() {
                        // The first event after a reconnect refreshes the board.
                        let reader = BufReader::new(resp);
                        for line in reader.lines().map_while(Result::ok) {
                            if line.starts_with("event: ") {
                                let _ = tx.send(());
                            }
                        }
                    }
                    if crate::ssh::tunnel().is_none() {
                        std::thread::sleep(Duration::from_secs(3));
                    } else {
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            });
            None
        }
    }
}

fn human_tool_label(raw: &str) -> &'static str {
    match raw {
        "record_event" | "update_application" => "Role update",
        "create_application" => "New role",
        "finish_submission" => "Submission",
        "finish_run" => "Run summary",
        "daily_update" => "Daily check",
        "save_receipt" => "Receipt saved",
        _ => "Activity",
    }
}

fn company_hint_from_record_path(path: &str) -> String {
    let stem = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".md");
    let parts: Vec<&str> = stem.split('-').collect();
    // Skip leading YYYY-MM-DD date prefix if present
    let start =
        if parts.len() > 3 && parts[0].len() == 4 && parts[0].chars().all(|c| c.is_ascii_digit()) {
            3
        } else {
            0
        };
    parts[start..]
        .iter()
        .take(2)
        .map(|w| {
            let mut ch = w.chars();
            match ch.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), ch.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
        if d == today.pred_opt().unwrap_or(today) {
            return format!("Yesterday {}", local.format("%H:%M"));
        }
        let delta_days = (today - d).num_days();
        if (2..=6).contains(&delta_days) || (-6..=-2).contains(&delta_days) {
            return local.format("%a %H:%M").to_string();
        }
        return local.format("%-d %b %H:%M").to_string();
    }
    iso.chars().take(16).collect()
}

fn extract_quiet_days(reason: &str) -> Option<u64> {
    let idx = reason.find("quiet for ")?;
    let rest = &reason[idx + 10..];
    let token = rest.split_whitespace().next()?;
    token.trim_end_matches('d').parse::<u64>().ok()
}

fn extract_quiet_badge(reason: &str) -> Option<String> {
    let days = extract_quiet_days(reason)?;
    if days >= 14 {
        Some(format!("Stale {days}d"))
    } else {
        Some(format!("Follow up {days}d"))
    }
}

fn format_interview_countdown(iso: &str) -> Option<String> {
    let dt = chrono::DateTime::parse_from_rfc3339(iso).ok()?;
    let now = chrono::Utc::now();
    let delta = dt.with_timezone(&chrono::Utc).signed_duration_since(now);
    let mins = delta.num_minutes();
    if mins < -60 {
        return None;
    }
    let local = dt.with_timezone(&chrono::Local);
    if mins <= 0 {
        return Some("Interview now".into());
    }
    if mins < 120 {
        return Some(format!("Interview in {mins}m"));
    }
    let hours = delta.num_hours();
    if hours < 24 && local.date_naive() == chrono::Local::now().date_naive() {
        return Some(format!("Today {}", local.format("%H:%M")));
    }
    let days = (local.date_naive() - chrono::Local::now().date_naive()).num_days();
    if days == 1 {
        return Some(format!("Tomorrow {}", local.format("%H:%M")));
    }
    if days > 1 {
        return Some(format!("In {days}d ({})", local.format("%a %H:%M")));
    }
    Some(format!("In {hours}h"))
}

pub fn save_default_workspace(root: &Path) -> Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("No HOME directory")?;
    let dir = home.join(".config/job-harness");
    fs::create_dir_all(&dir)?;
    let path = dir.join("settings.json");
    let mut cfg = fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    cfg["workspace"] = json!(root.display().to_string());
    let tmp = dir.join("settings.json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(&cfg)?)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

fn concise_reason(reason: &str) -> String {
    if let Some(days) = extract_quiet_days(reason) {
        let lower = reason.to_ascii_lowercase();
        let stage = if lower.contains("introduction") {
            "Intro"
        } else if lower.contains("screening") {
            "Screening"
        } else if lower.contains("technical") {
            "Technical"
        } else if lower.contains("final") {
            "Final"
        } else {
            "Active"
        };
        return format!("{stage} {days}d");
    }
    if reason.contains("within 24 hours") {
        return "Interview within 24h".into();
    }
    reason.split(" (").next().unwrap_or(reason).to_owned()
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
        enabled: true,
        matched_roles: None,
    });

    let disabled: std::collections::HashSet<String> = settings
        .get("disabled_cv_tracks")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
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
            enabled: !disabled.contains(&track),
            matched_roles: matched.and_then(|m| m.get(&track)).and_then(Value::as_u64),
        });
    }
    nodes
}

fn parse_tracking_json(raw: &str) -> Option<Value> {
    let start = raw.find("<!-- tracking")?;
    let body = &raw[start + "<!-- tracking".len()..];
    let end = body.find("-->")?;
    serde_json::from_str::<Value>(body[..end].trim()).ok()
}

fn extract_fit_summary(cleaned: &str) -> Option<String> {
    let mut in_fit = false;
    for line in cleaned.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            in_fit = trimmed.to_ascii_lowercase().contains("vacancy")
                || trimmed.to_ascii_lowercase().contains("fit")
                || trimmed.to_ascii_lowercase().contains("summary");
            continue;
        }
        if in_fit
            && !trimmed.is_empty()
            && !trimmed.starts_with("Source URL:")
            && !trimmed.starts_with("Evidence:")
        {
            return Some(trimmed.chars().take(260).collect());
        }
    }
    None
}

fn extract_salary_hint(text: &str) -> Option<String> {
    for tok in text.split_whitespace() {
        let clean = tok.trim_matches(|c: char| matches!(c, '(' | ')' | ',' | '.' | ';'));
        if (clean.starts_with('£') || clean.starts_with('$') || clean.starts_with('€'))
            && clean.chars().skip(1).any(|c| c.is_ascii_digit())
        {
            return Some(clean.to_owned());
        }
    }
    None
}

fn extract_location_hint(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let city = if lower.contains("london") {
        Some("London")
    } else if lower.contains("cambridge") {
        Some("Cambridge")
    } else if lower.contains("oxford") {
        Some("Oxford")
    } else if lower.contains("manchester") {
        Some("Manchester")
    } else if lower.contains("edinburgh") {
        Some("Edinburgh")
    } else if lower.contains("bristol") {
        Some("Bristol")
    } else if lower.contains("uk ") || lower.contains("united kingdom") {
        Some("UK")
    } else if lower.contains("eu ") || lower.contains("europe") {
        Some("Europe")
    } else if lower.contains("us ") || lower.contains("new york") || lower.contains("san francisco")
    {
        Some("US")
    } else {
        None
    };
    let mode = if lower.contains("fully remote") || lower.contains("uk remote") {
        Some("Remote")
    } else if lower.contains("two ") && lower.contains("office") {
        Some("Hybrid (2d/wk)")
    } else if lower.contains("three ") && lower.contains("office") {
        Some("Hybrid (3d/wk)")
    } else if lower.contains("hybrid") {
        Some("Hybrid")
    } else if lower.contains("remote") {
        Some("Remote")
    } else if lower.contains("onsite") || lower.contains("on-site") {
        Some("Onsite")
    } else {
        None
    };
    match (city, mode) {
        (Some(c), Some(m)) => Some(format!("{c} ({m})")),
        (Some(c), None) => Some(c.into()),
        (None, Some(m)) => Some(m.into()),
        (None, None) => None,
    }
}

fn strip_tracking_comment(raw: &str) -> String {
    let mut out = raw.to_owned();
    while let Some(start) = out.find("<!-- tracking") {
        if let Some(end_rel) = out[start..].find("-->") {
            out.replace_range(start..start + end_rel + 3, "");
        } else {
            break;
        }
    }
    out
}

pub fn open_url(url: &str) {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return;
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
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
    fn remote_label_names_the_server_not_the_tunnel() {
        let target = WorkspaceTarget::Remote {
            url: "unix:/run/jh/serve.sock".into(),
        };
        assert_eq!(target.label(), "Remote (/run/jh/serve.sock)");
        assert_eq!(
            remote_label(&json!({"ok":true,"name":"ledger","host":"alice@server"})).as_deref(),
            Some("ledger · alice@server")
        );
        assert_eq!(
            remote_label(&json!({"ok":true,"host":"alice@server"})).as_deref(),
            Some("alice@server")
        );
        assert_eq!(remote_label(&json!({"ok":true,"version":"0.3.0"})), None);
    }

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
