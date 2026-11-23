//! Local derived attention, deterministic urgency tiers, and packet previews. No inbox access or submission authority.
use super::*;

fn parse_checked_days_ago(checked: Option<&str>, today: chrono::NaiveDate) -> Option<i64> {
    let raw = checked?.trim();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some((today - dt.date_naive()).num_days().max(0));
    }
    let prefix = raw.get(..10)?;
    let date = chrono::NaiveDate::parse_from_str(prefix, "%Y-%m-%d").ok()?;
    Some((today - date).num_days().max(0))
}

fn parse_minutes_until(
    scheduled_start: Option<&str>,
    now_utc: chrono::DateTime<chrono::Utc>,
) -> Option<i64> {
    let raw = scheduled_start?.trim();
    let dt = chrono::DateTime::parse_from_rfc3339(raw).ok()?;
    Some((dt.with_timezone(&chrono::Utc) - now_utc).num_minutes())
}

fn default_followup_days(stage: &str) -> i64 {
    match stage {
        "offer" => 2,
        "introduction" | "screening" | "final" => 5,
        "technical" => 7,
        _ => 7,
    }
}

impl Engine {
    pub(super) fn review_queue(&self, args: &Value, state: &Value) -> Result<Value> {
        let settings = self.settings()?;
        let tz: chrono_tz::Tz = s(&settings, "timezone")?
            .parse()
            .map_err(|_| Error::new("invalid_arguments", "Configure an IANA timezone"))?;
        let now_utc = chrono::Utc::now();
        let today = now_utc.with_timezone(&tz).date_naive();
        let reminders = settings.get("time_and_reminders");
        let pre_reminder_mins = reminders
            .and_then(|r| r.get("pre_interview_reminder_minutes"))
            .and_then(Value::as_i64)
            .unwrap_or(90);
        let applied_ghost_days = reminders
            .and_then(|r| r.get("applied_ghost_days"))
            .and_then(Value::as_i64)
            .unwrap_or(30);
        let stage_followup = reminders.and_then(|r| r.get("stage_followup_days"));

        let built = pipeline::build(&self.root)?;
        let records: BTreeMap<String, Value> = built
            .records
            .iter()
            .map(|row| {
                (
                    format!(
                        "applications/{}",
                        row["record"].as_str().unwrap_or_default()
                    ),
                    row.clone(),
                )
            })
            .collect();
        let mut items = vec![];
        let mut counts = json!({"unknown_submissions":0,"reserved_attempts":0,"prepared_applications":0,"interviews":0,"supplied_mail":0,"pending_note_reviews":0});
        for attempt in state["attempts"].as_object().unwrap().values() {
            let status = SubmissionState::parse(&attempt["status"])?;
            if !status.unresolved() {
                continue;
            }
            let kind = if status == SubmissionState::Reserved {
                "reserved_attempt"
            } else {
                "unknown_submission"
            };
            let count = if status == SubmissionState::Reserved {
                "reserved_attempts"
            } else {
                "unknown_submissions"
            };
            counts[count] = json!(counts[count].as_u64().unwrap_or(0) + 1);
            let (urgency, urgency_reason) = if status == SubmissionState::Reserved {
                (
                    "urgent_now",
                    "Prepared application reserved — review, submit, or cancel",
                )
            } else {
                (
                    "background",
                    "Unresolved submission outcome — audit externally before any retry",
                )
            };
            let mut item = json!({"kind":kind,"urgency":urgency,"urgency_reason":urgency_reason,"attempt_id":attempt["id"],"run_id":attempt["run_id"],"record_path":attempt["record"],"company":attempt["company"],"status":attempt["status"],"next_action":if status==SubmissionState::Reserved{"Review the prepared form before the separate begin boundary"}else{"Inspect the external outcome before resolving or retrying"},"reason":"Persisted coordination state; elapsed time never clears this attempt."});
            if let Some(record) = records.get(s(attempt, "record")?) {
                for key in [
                    "title",
                    "stage",
                    "checked",
                    "schedule_state",
                    "scheduled_start",
                ] {
                    if let Some(value) = record.get(key) {
                        item[key] = value.clone();
                    }
                }
            }
            items.push(item);
        }
        let mut next_upcoming_interview: Option<(i64, Value)> = None;
        let mut quiet_applied_count: u64 = 0;

        // Deterministic CV track-fit gap check across recent active/saved/applied roles
        let default_track_keywords = |name: &str| -> Vec<String> {
            let lower = name.to_ascii_lowercase();
            let mut kws = vec![lower.clone()];
            match lower.as_str() {
                "backend" => kws.extend(
                    [
                        "back-end",
                        "platform",
                        "systems",
                        "distributed",
                        "api",
                        "server",
                        "infrastructure",
                    ]
                    .into_iter()
                    .map(str::to_owned),
                ),
                "product" => kws.extend(
                    [
                        "full-stack",
                        "fullstack",
                        "full stack",
                        "frontend",
                        "front-end",
                        "web",
                        "ui",
                    ]
                    .into_iter()
                    .map(str::to_owned),
                ),
                "ai" => kws.extend(
                    [
                        "ml",
                        "llm",
                        "agent",
                        "machine learning",
                        "genai",
                        "applied ai",
                        "artificial intelligence",
                    ]
                    .into_iter()
                    .map(str::to_owned),
                ),
                _ => {}
            }
            kws
        };
        let configured_tracks: Vec<(String, Vec<String>)> = match settings.get("cv_tracks") {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(Value::as_str)
                .map(|name| (name.to_owned(), default_track_keywords(name)))
                .collect(),
            Some(Value::Object(map)) => map
                .iter()
                .map(|(name, cfg)| {
                    let mut kws = default_track_keywords(name);
                    if let Some(arr) = cfg.get("keywords").and_then(Value::as_array) {
                        for v in arr {
                            if let Some(kw) = v.as_str() {
                                kws.push(kw.to_ascii_lowercase());
                            }
                        }
                    } else if let Some(focus) = cfg.get("focus").and_then(Value::as_str) {
                        for tok in focus.split(|c: char| !c.is_alphanumeric()) {
                            let t = tok.to_ascii_lowercase();
                            if t.len() >= 3 {
                                kws.push(t);
                            }
                        }
                    }
                    (name.clone(), kws)
                })
                .collect(),
            _ => vec![],
        };
        let mut matched_by_track: BTreeMap<String, u64> = configured_tracks
            .iter()
            .map(|(name, _)| (name.clone(), 0))
            .collect();
        let mut unmatched_recent_titles: Vec<String> = Vec::new();
        let mut unmatched_recent_count: u64 = 0;

        for row in &built.records {
            let stage = row["stage"].as_str().unwrap_or_default();
            let days_ago =
                parse_checked_days_ago(row.get("checked").and_then(Value::as_str), today);
            if stage == "applied" && days_ago.is_some_and(|d| d >= applied_ghost_days) {
                quiet_applied_count += 1;
            }
            if !configured_tracks.is_empty()
                && matches!(
                    stage,
                    "saved"
                        | "prepared"
                        | "applied"
                        | "introduction"
                        | "screening"
                        | "technical"
                        | "final"
                        | "offer"
                )
                && days_ago.is_some_and(|d| d <= 30)
            {
                let hay = format!(
                    "{} {}",
                    row.get("title").and_then(Value::as_str).unwrap_or_default(),
                    row.get("next_action")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                )
                .to_ascii_lowercase();
                let mut matched = false;
                for (tname, kws) in &configured_tracks {
                    if kws.iter().any(|kw| hay.contains(kw.as_str())) {
                        *matched_by_track.entry(tname.clone()).or_insert(0) += 1;
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    unmatched_recent_count += 1;
                    if unmatched_recent_titles.len() < 5
                        && let Some(title) = row.get("title").and_then(Value::as_str)
                    {
                        unmatched_recent_titles.push(title.to_owned());
                    }
                }
            }

            let (kind, count) = match stage {
                "prepared" => ("prepared_application", "prepared_applications"),
                "introduction" | "screening" | "technical" | "final" | "offer" => {
                    ("interview", "interviews")
                }
                _ => continue,
            };
            counts[count] = json!(counts[count].as_u64().unwrap_or(0) + 1);
            let mut item = domain::record(row);
            item["kind"] = json!(kind);
            for key in [
                "schedule_state",
                "scheduled_start",
                "proposed_time",
                "contact",
                "brief",
            ] {
                if let Some(value) = row.get(key) {
                    item[key] = value.clone();
                }
            }
            if let Some(d) = days_ago {
                item["days_since_checked"] = json!(d);
            }
            let mins_until =
                parse_minutes_until(row.get("scheduled_start").and_then(Value::as_str), now_utc);
            if let Some(m) = mins_until {
                item["minutes_until_start"] = json!(m);
            }
            let (urgency, urgency_reason) = if kind == "prepared_application" {
                (
                    "background".to_owned(),
                    "Prepared application ready for bounded submission".to_owned(),
                )
            } else if stage == "offer" {
                (
                    "urgent_now".to_owned(),
                    "Active offer requires review or decision".to_owned(),
                )
            } else if row.get("schedule_state").and_then(Value::as_str) == Some("proposed") {
                (
                    "urgent_now".to_owned(),
                    "Proposed interview time awaiting confirmation".to_owned(),
                )
            } else if let Some(mins) = mins_until {
                if (-120..=24 * 60).contains(&mins) {
                    let msg = if mins < 0 {
                        "Interview just ended — confirm held and capture debrief".to_owned()
                    } else if mins <= pre_reminder_mins {
                        format!("Interview starts in {mins}m — review 10-line cheatsheet")
                    } else {
                        "Interview scheduled within 24 hours".to_owned()
                    };
                    ("urgent_now".to_owned(), msg)
                } else if mins > 24 * 60 && mins <= 7 * 24 * 60 {
                    (
                        "upcoming".to_owned(),
                        "Interview within 7 days — prepare or refresh brief".to_owned(),
                    )
                } else if mins > 7 * 24 * 60 {
                    (
                        "background".to_owned(),
                        "Interview scheduled more than 7 days out".to_owned(),
                    )
                } else {
                    let threshold = stage_followup
                        .and_then(|m| m.get(stage))
                        .and_then(Value::as_i64)
                        .unwrap_or_else(|| default_followup_days(stage));
                    if days_ago.is_some_and(|d| d >= threshold) {
                        (
                            "upcoming".to_owned(),
                            format!(
                                "Past scheduled call with no update for {}d — debrief or follow up",
                                days_ago.unwrap()
                            ),
                        )
                    } else {
                        (
                            "background".to_owned(),
                            "Past scheduled call awaiting outcome".to_owned(),
                        )
                    }
                }
            } else {
                let threshold = stage_followup
                    .and_then(|m| m.get(stage))
                    .and_then(Value::as_i64)
                    .unwrap_or_else(|| default_followup_days(stage));
                if let Some(d) = days_ago {
                    if d >= threshold {
                        (
                            "upcoming".to_owned(),
                            format!(
                                "Active {stage} stage quiet for {d}d (follow-up threshold {threshold}d)"
                            ),
                        )
                    } else {
                        (
                            "background".to_owned(),
                            format!("Active {stage} stage waiting quietly (checked {d}d ago)"),
                        )
                    }
                } else {
                    (
                        "background".to_owned(),
                        format!("Active {stage} stage with no scheduled time"),
                    )
                }
            };
            if kind == "interview"
                && let Some(mins) = mins_until
                && mins >= 0
                && next_upcoming_interview
                    .as_ref()
                    .is_none_or(|(best, _)| mins < *best)
            {
                next_upcoming_interview = Some((
                    mins,
                    json!({
                        "company": item.get("company").cloned().unwrap_or(Value::Null),
                        "title": item.get("title").cloned().unwrap_or(Value::Null),
                        "stage": stage,
                        "scheduled_start": item.get("scheduled_start").cloned().unwrap_or(Value::Null),
                        "minutes_until_start": mins,
                        "record_path": item.get("record_path").cloned().unwrap_or(Value::Null),
                        "brief": item.get("brief").cloned().unwrap_or(Value::Null),
                    }),
                ));
            }
            item["urgency"] = json!(urgency);
            item["urgency_reason"] = json!(urgency_reason);
            item["reason"] = json!(
                "Canonical tracking and its recorded next action; current provider state is not inferred."
            );
            items.push(item);
        }
        let mut mail_source = Value::Null;
        if let Some(path) = args.get("mail_observations_path").and_then(Value::as_str) {
            let source = self.safe_path(path, &["context", "applications"], &["json"])?;
            let bytes = fs::read(source)?;
            if bytes.len() > 1_000_000 {
                return Err(Error::new(
                    "invalid_arguments",
                    "Mail observations exceed one MB; supply a bounded observation file",
                ));
            }
            let value: Value = serde_json::from_slice(&bytes)?;
            let rows = value
                .get("items")
                .or_else(|| value.get("messages"))
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    Error::new(
                        "invalid_arguments",
                        "Mail observation JSON requires items or messages",
                    )
                })?;
            if rows.len() > 1000 {
                return Err(Error::new(
                    "invalid_arguments",
                    "Supply at most 1000 bounded mail observations",
                ));
            }
            mail_source = json!({"path":path,"sha256":domain::digest(&bytes),"observed_at":value.get("observed_at").or_else(||value.get("checked_at")).cloned().unwrap_or(Value::Null)});
            for row in rows {
                if !row.is_object() {
                    return Err(Error::new(
                        "invalid_arguments",
                        "Every supplied mail observation must be an object",
                    ));
                }
                let needs_reply = row
                    .get("needs_reply")
                    .or_else(|| row.get("needs_attention"))
                    .and_then(Value::as_bool);
                let attribution = row.get("attribution").and_then(Value::as_str);
                let uncertain = needs_reply.is_none()
                    || row.get("needs_review") == Some(&Value::Bool(true))
                    || attribution.is_some_and(|value| value != "proposed_exact_identity");
                if needs_reply == Some(false) && !uncertain {
                    continue;
                }
                let mut item = json!({"kind":"supplied_mail","urgency":"urgent_now","urgency_reason":"Recruiter or employer email awaiting review or reply","needs_reply":needs_reply,"needs_review":uncertain,"source_path":path,"source_sha256":mail_source["sha256"],"next_action":"Review the supplied observation and latest full mail before replying","reason":"Supplied mail observation, not a fresh inbox scan. Unknown request state and ambiguous attribution remain visible."});
                for key in [
                    "id",
                    "subject",
                    "company",
                    "thread_url",
                    "summary",
                    "observed_at",
                    "attribution",
                    "latest_message_id",
                ] {
                    if let Some(value) = row.get(key)
                        && value.is_string()
                    {
                        item[key] = value.clone();
                    }
                }
                if let Some(ids) = row.get("request_message_ids").filter(|value| {
                    value
                        .as_array()
                        .is_some_and(|ids| ids.iter().all(Value::is_string))
                }) {
                    item["request_message_ids"] = ids.clone();
                }
                items.push(item);
                counts["supplied_mail"] = json!(counts["supplied_mail"].as_u64().unwrap_or(0) + 1);
            }
        }
        let notes = crate::intake::list(&self.root, &json!({"limit":100}))?.data;
        let note_reviews_complete = notes["page"]["has_more"] != true;
        for note in notes["notes"].as_array().into_iter().flatten() {
            let id = s(note, "note_id")?;
            let review = crate::intake::review(&self.root, &json!({"note_id":id}))?.data;
            let accepted: BTreeSet<_> = review["accepted_reviews"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|row| row["proposal_id"].as_str())
                .collect();
            let proposals = review["proposals"]
                .as_array()
                .ok_or_else(|| Error::new("invalid_state", "Intake review lacks proposals"))?;
            let pending: Vec<_> = proposals
                .iter()
                .filter(|proposal| {
                    proposal["proposal_id"]
                        .as_str()
                        .is_some_and(|id| !accepted.contains(id))
                })
                .collect();
            if proposals.is_empty() {
                items.push(json!({"kind":"pending_note_review","urgency":"background","urgency_reason":"Unreviewed intake note","note_id":id,"source_path":note["original_path"],"source_sha256":note["source_sha256"],"next_action":"Review the supplied original and propose any separately cited additions","reason":"This unarchived intake has no reviewed proposal; no facts or CV content are inferred."}));
                counts["pending_note_reviews"] =
                    json!(counts["pending_note_reviews"].as_u64().unwrap_or(0) + 1);
            }
            for proposal in pending {
                items.push(json!({"kind":"pending_note_review","urgency":"background","urgency_reason":"Unreviewed intake proposal","note_id":id,"proposal_id":proposal["proposal_id"],"source_path":proposal["path"],"source_sha256":proposal["sha256"],"next_action":"Review citations and explicitly select proposal item IDs before accepting","reason":"This proposal has no explicit accepted review. Proposed text is not a confirmed career fact."}));
                counts["pending_note_reviews"] =
                    json!(counts["pending_note_reviews"].as_u64().unwrap_or(0) + 1);
            }
        }
        counts["pending_mail_actions"] = json!(pending_mail_actions(state).len());
        for row in pending_mail_actions(state) {
            items.push(json!({"kind":"mail_archive_action","urgency":"urgent_now","urgency_reason":"Pending durable mail archive action","action_id":row["action_id"],"plan_id":row["plan_id"],"status":row["status"],"next_action":"Inspect queued host work or reconcile observed mailbox state; never blindly retry"}));
        }
        let mut urgency_counts = json!({"urgent_now":0,"upcoming":0,"background":0});
        for item in &items {
            let bucket = match item["urgency"].as_str().unwrap_or("background") {
                "urgent_now" => "urgent_now",
                "upcoming" => "upcoming",
                _ => "background",
            };
            urgency_counts[bucket] = json!(urgency_counts[bucket].as_u64().unwrap_or(0) + 1);
        }
        let needs_attention_now = urgency_counts["urgent_now"].as_u64().unwrap_or(0) > 0;
        items.sort_by(|a, b| {
            let urgency_rank =
                |value: &Value| match value["urgency"].as_str().unwrap_or("background") {
                    "urgent_now" => 0,
                    "upcoming" => 1,
                    _ => 2,
                };
            let kind_rank = |value: &Value| match value["kind"].as_str().unwrap_or_default() {
                "interview" => 0,
                "supplied_mail" => 1,
                "reserved_attempt" => 2,
                "mail_archive_action" => 3,
                "unknown_submission" => 4,
                "prepared_application" => 5,
                "pending_note_review" => 6,
                _ => 7,
            };
            urgency_rank(a)
                .cmp(&urgency_rank(b))
                .then_with(|| {
                    a["scheduled_start"]
                        .as_str()
                        .unwrap_or("9999")
                        .cmp(b["scheduled_start"].as_str().unwrap_or("9999"))
                })
                .then_with(|| kind_rank(a).cmp(&kind_rank(b)))
                .then_with(|| {
                    a["record_path"]
                        .as_str()
                        .unwrap_or_default()
                        .cmp(b["record_path"].as_str().unwrap_or_default())
                })
        });
        let track_fit = if configured_tracks.is_empty() {
            Value::Null
        } else {
            json!({
                "configured_tracks": configured_tracks.iter().map(|(n, _)| n).collect::<Vec<_>>(),
                "matched_by_track": matched_by_track,
                "unmatched_recent_count": unmatched_recent_count,
                "sample_unmatched_titles": unmatched_recent_titles,
                "suggest_new_track": unmatched_recent_count >= 3
            })
        };
        let (items, page) = domain::page(&items, n(args, "offset", 0), n(args, "limit", 10));
        Ok(json!({
            "items": items,
            "counts": counts,
            "needs_attention_now": needs_attention_now,
            "urgency_counts": urgency_counts,
            "next_upcoming_interview": next_upcoming_interview.map(|(_, v)| v).unwrap_or(Value::Null),
            "quiet_applied_count": quiet_applied_count,
            "track_fit": track_fit,
            "page": page,
            "note_reviews_complete": note_reviews_complete,
            "mail_source": mail_source,
            "authority": "recorded_local_state",
            "live_mail_checked": false
        }))
    }
    pub(super) fn preview_packet(&self, args: &Value, state: &Value) -> Result<Value> {
        let id = s(args, "packet_id")?;
        if id.len() != 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::new("invalid_arguments", "Invalid packet ID"));
        }
        let relative = format!("applications/operations/packets/{id}.json");
        let path = self.safe_path(&relative, &["applications"], &["json"])?;
        let mut checks = vec![];
        let mut packet = Value::Null;
        if path.is_file() {
            let bytes = fs::read(path)?;
            let parsed: Value = serde_json::from_slice(&bytes)?;
            let actual = domain::digest(&domain::encoded(&parsed)?);
            checks.push(json!({"kind":"packet_metadata","path":relative,"expected_sha256":id,"actual_sha256":actual,"changed":actual!=id}));
            for field in ["cv", "answers", "source_record"] {
                let artifact = parsed.get(field).ok_or_else(|| {
                    Error::new(
                        "invalid_state",
                        "Packet is missing frozen artifact metadata",
                    )
                })?;
                let expected = s(artifact, "sha256")?;
                for (original, path) in [
                    (false, s(artifact, "path")?),
                    (true, s(artifact, "original_path")?),
                ] {
                    let target =
                        self.safe_path(path, ARTIFACT_ROOTS, &["md", "json", "pdf", "txt"])?;
                    let actual = if target.is_file() {
                        Some(domain::digest(&fs::read(target)?))
                    } else {
                        None
                    };
                    checks.push(json!({"kind":if original{"original_artifact"}else{"frozen_artifact"},"path":path,"expected_sha256":expected,"actual_sha256":actual,"changed":actual.as_deref()!=Some(expected)}));
                }
            }
            let sources = parsed["answer_source_sha256"].as_object().ok_or_else(|| {
                Error::new(
                    "invalid_state",
                    "Packet lacks canonical answer-source hashes",
                )
            })?;
            for (source, expected) in sources {
                let target = self.text_path(source)?;
                let actual = if target.is_file() {
                    Some(domain::digest(&fs::read(target)?))
                } else {
                    None
                };
                checks.push(json!({"kind":"answer_source","path":source,"expected_sha256":expected,"actual_sha256":actual,"changed":actual.as_ref().map(|x|json!(x))!=Some(expected.clone())}));
            }
            let settings = self.settings()?;
            let current: BTreeSet<_> = settings["answer_sources"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            let frozen: BTreeSet<_> = sources.keys().map(String::as_str).collect();
            if current != frozen {
                checks.push(json!({"kind":"answer_configuration","path":"config/career-ledger.json","expected_sha256":domain::digest(&domain::encoded(&json!(frozen))?),"actual_sha256":domain::digest(&domain::encoded(&json!(current))?),"changed":true}));
            }
            packet = domain::packet(&parsed);
        } else {
            checks.push(json!({"kind":"packet_metadata","path":relative,"expected_sha256":id,"actual_sha256":null,"changed":true}));
        }
        let attempts: Vec<_> = state["attempts"]
            .as_object()
            .unwrap()
            .values()
            .filter(|attempt| attempt["packet_id"] == id)
            .map(domain::attempt)
            .collect();
        let (attempts, page) = domain::page(&attempts, 0, 50);
        let stale = checks.iter().any(|check| check["changed"] == true);
        Ok(
            json!({"packet_id":id,"packet":packet,"stale":stale,"checks":checks,"attempts":attempts,"page":page,"authority":"preview_only","instruction":"A preview never begins, retries or grants submission authority. Inspect changes and the existing attempt before acting."}),
        )
    }
}
