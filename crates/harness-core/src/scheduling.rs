//! Interview scheduling preferences and time-zone-safe slot ranking.
//!
//! Preferred windows are wall-clock times in the workspace time zone, so a
//! 10:00 window stays at 10:00 local across daylight-saving changes. Every
//! instant crossing the API boundary carries an explicit UTC offset; naive
//! times are rejected rather than guessed.
use crate::{Error, Result};
use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Meeting kinds, lightest to heaviest, plus self-scheduled assessment blocks.
pub const KINDS: [&str; 4] = ["screen", "interview", "deep", "assessment"];
const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
const STEP_MINUTES: i64 = 15;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// Monday first, matching `Weekday::num_days_from_monday`.
    pub days: [bool; 7],
    pub start: NaiveTime,
    pub end: NaiveTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KindPolicy {
    pub windows: Vec<Window>,
    pub daily_limit: u64,
    pub buffer_minutes: i64,
    pub min_notice_hours: i64,
    pub deadline_margin_hours: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub earliest: NaiveTime,
    pub latest: NaiveTime,
    pub max_per_day: u64,
    pub kinds: BTreeMap<&'static str, KindPolicy>,
}

pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        "screen" => "screen",
        "interview" => "interview",
        "deep" => "technical or final",
        _ => "assessment",
    }
}

fn hm(text: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(text.trim(), "%H:%M")
        .map_err(|_| Error::validation(format!("Use 24-hour HH:MM times, not {text:?}")))
}

fn day_index(text: &str) -> Result<usize> {
    let t = text.trim().to_ascii_lowercase();
    DAYS.iter()
        .position(|d| t.starts_with(d) && d.len() <= t.len())
        .ok_or_else(|| Error::validation(format!("Unknown day {text:?}; use mon to sun")))
}

fn parse_days(text: &str) -> Result<[bool; 7]> {
    let mut days = [false; 7];
    for item in text.split(',').map(str::trim).filter(|x| !x.is_empty()) {
        match item.to_ascii_lowercase().as_str() {
            "weekdays" => days[..5].iter_mut().for_each(|d| *d = true),
            "weekends" => days[5..].iter_mut().for_each(|d| *d = true),
            "daily" | "every day" => days = [true; 7],
            _ => match item.split_once('-') {
                Some((a, b)) => {
                    let (mut i, end) = (day_index(a)?, day_index(b)?);
                    loop {
                        days[i] = true;
                        if i == end {
                            break;
                        }
                        i = (i + 1) % 7;
                    }
                }
                None => days[day_index(item)?] = true,
            },
        }
    }
    if !days.contains(&true) {
        return Err(Error::validation("Name at least one day, e.g. Mon-Fri"));
    }
    Ok(days)
}

fn window(days: [bool; 7], start: NaiveTime, end: NaiveTime) -> Result<Window> {
    if end <= start {
        return Err(Error::validation(format!(
            "A window must end after it starts ({}-{})",
            start.format("%H:%M"),
            end.format("%H:%M")
        )));
    }
    Ok(Window { days, start, end })
}

/// Parse "Mon-Fri 09:30-10:00, 12:00-13:00; Tue-Thu 10:00-12:00".
pub fn parse_windows(text: &str) -> Result<Vec<Window>> {
    let normal = text.replace(['–', '—'], "-");
    let mut out = Vec::new();
    for segment in normal.split([';', '\n']).map(str::trim) {
        if segment.is_empty() {
            continue;
        }
        let split = segment
            .find(|c: char| c.is_ascii_digit())
            .ok_or_else(|| Error::validation(format!("Add times to {segment:?}")))?;
        let days = parse_days(segment[..split].trim())?;
        for range in segment[split..].split(',').map(str::trim) {
            let (a, b) = range
                .split_once('-')
                .ok_or_else(|| Error::validation(format!("Write {range:?} as HH:MM-HH:MM")))?;
            out.push(window(days, hm(a)?, hm(b)?)?);
        }
    }
    if out.len() > 20 {
        return Err(Error::validation("Use at most 20 windows per meeting kind"));
    }
    Ok(out)
}

/// Parse meeting hours such as "09:00-18:00".
pub fn parse_hours(text: &str) -> Result<(NaiveTime, NaiveTime)> {
    let normal = text.replace(['–', '—'], "-");
    let (a, b) = normal
        .split_once('-')
        .ok_or_else(|| Error::validation("Write meeting hours as HH:MM-HH:MM"))?;
    let w = window([true; 7], hm(a)?, hm(b)?)?;
    Ok((w.start, w.end))
}

fn format_days(days: &[bool; 7]) -> String {
    let names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (i, on) in days.iter().enumerate() {
        if !on {
            continue;
        }
        match runs.last_mut() {
            Some((_, end)) if *end + 1 == i => *end = i,
            _ => runs.push((i, i)),
        }
    }
    runs.iter()
        .map(|&(a, b)| match b - a {
            0 => names[a].to_owned(),
            1 => format!("{}, {}", names[a], names[b]),
            _ => format!("{}-{}", names[a], names[b]),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Inverse of `parse_windows`, grouping ranges that share days.
pub fn format_windows(windows: &[Window]) -> String {
    let mut groups: Vec<([bool; 7], Vec<String>)> = Vec::new();
    for w in windows {
        let range = format!("{}-{}", w.start.format("%H:%M"), w.end.format("%H:%M"));
        match groups.iter_mut().find(|(days, _)| *days == w.days) {
            Some((_, ranges)) => ranges.push(range),
            None => groups.push((w.days, vec![range])),
        }
    }
    groups
        .iter()
        .map(|(days, ranges)| format!("{} {}", format_days(days), ranges.join(", ")))
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn windows_json(windows: &[Window]) -> Value {
    json!(windows.iter().map(|w| json!({
        "days": DAYS.iter().zip(w.days).filter(|(_, on)| *on).map(|(d, _)| d).collect::<Vec<_>>(),
        "start": w.start.format("%H:%M").to_string(),
        "end": w.end.format("%H:%M").to_string(),
    })).collect::<Vec<_>>())
}

fn windows_from_json(value: &Value) -> Result<Vec<Window>> {
    let items = value
        .as_array()
        .ok_or_else(|| Error::validation("Scheduling windows must be an array"))?;
    items
        .iter()
        .map(|w| {
            let mut days = [false; 7];
            for d in w["days"].as_array().into_iter().flatten() {
                days[day_index(d.as_str().unwrap_or_default())?] = true;
            }
            if !days.contains(&true) {
                return Err(Error::validation("Each window needs at least one day"));
            }
            window(
                days,
                hm(w["start"].as_str().unwrap_or_default())?,
                hm(w["end"].as_str().unwrap_or_default())?,
            )
        })
        .collect()
}

fn default_kind(kind: &str) -> KindPolicy {
    let (windows, daily_limit, buffer_minutes, min_notice_hours) = match kind {
        "screen" => ("Mon-Fri 09:30-10:00, 12:00-13:00, 16:30-17:30", 3, 15, 2),
        "interview" => ("Mon-Fri 10:00-12:00, 13:30-15:30", 2, 15, 12),
        "deep" => ("Tue-Thu 10:00-12:00, 13:30-15:30", 1, 30, 24),
        _ => ("Mon-Fri 13:30-17:30", 1, 15, 0),
    };
    KindPolicy {
        windows: parse_windows(windows).expect("default windows parse"),
        daily_limit,
        buffer_minutes,
        min_notice_hours,
        deadline_margin_hours: if kind == "assessment" { 24 } else { 0 },
    }
}

impl Policy {
    /// Built-in defaults overlaid with the workspace's optional `scheduling` block.
    pub fn from_settings(settings: &Value) -> Result<Self> {
        let cfg = &settings["scheduling"];
        let mut policy = Policy {
            earliest: hm(cfg["hours"]["start"].as_str().unwrap_or("09:00"))?,
            latest: hm(cfg["hours"]["end"].as_str().unwrap_or("18:00"))?,
            max_per_day: cfg["max_per_day"].as_u64().unwrap_or(4),
            kinds: KINDS.iter().map(|k| (*k, default_kind(k))).collect(),
        };
        if policy.latest <= policy.earliest {
            return Err(Error::validation("Meeting hours must end after they start"));
        }
        for (kind, kp) in policy.kinds.iter_mut() {
            let o = &cfg["kinds"][*kind];
            if o.get("windows").is_some() {
                kp.windows = windows_from_json(&o["windows"])?;
            }
            kp.daily_limit = o["daily_limit"].as_u64().unwrap_or(kp.daily_limit);
            kp.buffer_minutes = o["buffer_minutes"].as_i64().unwrap_or(kp.buffer_minutes);
            kp.min_notice_hours = o["min_notice_hours"]
                .as_i64()
                .unwrap_or(kp.min_notice_hours);
            kp.deadline_margin_hours = o["deadline_margin_hours"]
                .as_i64()
                .unwrap_or(kp.deadline_margin_hours);
        }
        Ok(policy)
    }
    fn kind(&self, kind: &str) -> &KindPolicy {
        &self.kinds[KINDS
            .iter()
            .find(|k| **k == kind)
            .copied()
            .unwrap_or("screen")]
    }
}

fn instant(text: &str, field: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text.trim())
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| {
            Error::validation(format!(
                "{field} must be RFC 3339 with an offset or Z, e.g. 2026-11-30T09:45:00Z (got {text:?})"
            ))
        })
}

fn kind_arg(value: &Value, field: &str) -> Result<Option<String>> {
    match value.as_str() {
        None => Ok(None),
        Some(k) if KINDS.contains(&k) => Ok(Some(k.to_owned())),
        Some(k) => Err(Error::validation(format!(
            "{field} kind {k:?} must be one of {}",
            KINDS.join(", ")
        ))),
    }
}

struct Busy {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    kind: Option<String>,
    label: String,
}

/// Local wall-clock label, e.g. "Mon 30 Nov 2026, 09:45-10:15 GMT".
pub fn label(start: DateTime<Utc>, end: DateTime<Utc>, tz: Tz) -> String {
    let (a, b) = (start.with_timezone(&tz), end.with_timezone(&tz));
    let day = a.format("%a %-d %b %Y");
    if a.format("%Z").to_string() == b.format("%Z").to_string() && a.date_naive() == b.date_naive()
    {
        format!(
            "{day}, {}-{} {}",
            a.format("%H:%M"),
            b.format("%H:%M"),
            a.format("%Z")
        )
    } else {
        format!(
            "{day}, {} {} to {} {}",
            a.format("%H:%M"),
            a.format("%Z"),
            b.format("%a %-d %b %H:%M"),
            b.format("%Z")
        )
    }
}

/// Every whole quarter-hour of local wall-clock time between `from` and `until`.
/// Skipped (spring-forward) times are omitted; repeated (fall-back) times yield both instants.
fn local_grid(
    tz: Tz,
    from: DateTime<Utc>,
    until: DateTime<Utc>,
    earliest: NaiveTime,
    latest: NaiveTime,
) -> Vec<DateTime<Utc>> {
    let mut out = BTreeSet::new();
    let mut date: NaiveDate = from.with_timezone(&tz).date_naive();
    let last = until.with_timezone(&tz).date_naive();
    while date <= last {
        // First quarter-hour at or after `earliest`; last start before `latest`.
        let mut minute = ((earliest - NaiveTime::MIN).num_seconds() + 899) / 900 * STEP_MINUTES;
        let end_minute = (latest - NaiveTime::MIN).num_minutes();
        while minute < end_minute {
            let naive = date.and_time(NaiveTime::MIN) + Duration::minutes(minute);
            let instants = match tz.from_local_datetime(&naive) {
                LocalResult::Single(t) => vec![t],
                LocalResult::Ambiguous(a, b) => vec![a, b],
                LocalResult::None => vec![],
            };
            for t in instants {
                let t = t.with_timezone(&Utc);
                if t >= from && t <= until {
                    out.insert(t);
                }
            }
            minute += STEP_MINUTES;
        }
        date = date.succ_opt().expect("calendar date in range");
    }
    out.into_iter().collect()
}

struct Ranked {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    in_window: bool,
    score: i64,
    reasons: Vec<String>,
    rejections: Vec<String>,
}

/// Rank candidate or generated start times for one meeting kind.
pub fn suggest(args: &Value, settings: &Value, clock: DateTime<Utc>) -> Result<Value> {
    let tz_name = args["timezone"]
        .as_str()
        .or(settings["timezone"].as_str())
        .unwrap_or("UTC");
    let tz: Tz = tz_name
        .parse()
        .map_err(|_| Error::validation(format!("Unknown IANA time zone {tz_name:?}")))?;
    let policy = Policy::from_settings(settings)?;
    let kind = kind_arg(&args["kind"], "Meeting")?
        .ok_or_else(|| Error::validation("Choose a meeting kind"))?;
    let kp = policy.kind(&kind);
    let duration = Duration::minutes(args["duration_minutes"].as_i64().unwrap_or(30));
    let now = match args["now"].as_str() {
        Some(t) => instant(t, "now")?,
        None => clock,
    };
    let deadline = args["deadline"]
        .as_str()
        .map(|t| instant(t, "deadline"))
        .transpose()?;
    let allow_outside = args["allow_outside_hours"].as_bool().unwrap_or(false);
    let limit = args["limit"].as_u64().unwrap_or(3) as usize;
    let mut busy = Vec::new();
    for b in args["busy"].as_array().into_iter().flatten() {
        let (start, end) = (
            instant(b["start"].as_str().unwrap_or_default(), "busy start")?,
            instant(b["end"].as_str().unwrap_or_default(), "busy end")?,
        );
        if end <= start {
            return Err(Error::validation(
                "Each busy period must end after it starts",
            ));
        }
        busy.push(Busy {
            start,
            end,
            kind: kind_arg(&b["kind"], "Busy")?,
            label: b["label"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| label(start, end, tz)),
        });
    }

    let supplied = args.get("candidates").is_some();
    let candidates: Vec<(DateTime<Utc>, DateTime<Utc>)> = if supplied {
        let mut out = Vec::new();
        for c in args["candidates"].as_array().into_iter().flatten() {
            let start = instant(c["start"].as_str().unwrap_or_default(), "candidate start")?;
            let end = match c["end"].as_str() {
                Some(e) => instant(e, "candidate end")?,
                None => start + duration,
            };
            if end <= start {
                return Err(Error::validation("Each candidate must end after it starts"));
            }
            out.push((start, end));
        }
        out
    } else {
        let from = match args["from"].as_str() {
            Some(t) => instant(t, "from")?,
            None => now,
        };
        let until = match args["until"].as_str() {
            Some(t) => instant(t, "until")?,
            None => from + Duration::days(14),
        };
        if until <= from || until - from > Duration::days(62) {
            return Err(Error::validation(
                "Search between 1 minute and 62 days ahead",
            ));
        }
        let (earliest, latest) = if allow_outside {
            (NaiveTime::MIN, NaiveTime::from_hms_opt(23, 59, 59).unwrap())
        } else {
            (policy.earliest, policy.latest)
        };
        local_grid(tz, from, until, earliest, latest)
            .into_iter()
            .map(|s| (s, s + duration))
            .filter(|&(s, e)| {
                let (ls, le) = (s.with_timezone(&tz), e.with_timezone(&tz));
                e <= until
                    && (allow_outside
                        || (le.date_naive() == ls.date_naive() && le.time() <= policy.latest))
            })
            .collect()
    };

    let local_day = |t: DateTime<Utc>| t.with_timezone(&tz).date_naive();
    let ranked: Vec<Ranked> = candidates
        .iter()
        .map(|&(start, end)| {
            let (ls, le) = (start.with_timezone(&tz), end.with_timezone(&tz));
            let day = ls.date_naive();
            let mut rejections = Vec::new();
            let notice = Duration::hours(kp.min_notice_hours);
            if start < now {
                rejections.push("Already in the past".to_owned());
            } else if start < now + notice {
                rejections.push(format!("Less than {}h notice", kp.min_notice_hours));
            }
            if let Some(deadline) = deadline {
                let cutoff = deadline - Duration::hours(kp.deadline_margin_hours);
                if end > cutoff {
                    rejections.push(if kp.deadline_margin_hours > 0 {
                        format!(
                            "Ends less than {}h before the deadline",
                            kp.deadline_margin_hours
                        )
                    } else {
                        "Ends after the deadline".to_owned()
                    });
                }
            }
            if !allow_outside
                && (le.date_naive() != day
                    || ls.time() < policy.earliest
                    || le.time() > policy.latest)
            {
                rejections.push(format!(
                    "Outside meeting hours {}-{}",
                    policy.earliest.format("%H:%M"),
                    policy.latest.format("%H:%M")
                ));
            }
            for b in &busy {
                let buffer = Duration::minutes(
                    kp.buffer_minutes.max(
                        b.kind
                            .as_deref()
                            .map_or(0, |k| policy.kind(k).buffer_minutes),
                    ),
                );
                if start < b.end + buffer && b.start < end + buffer {
                    rejections.push(format!(
                        "Clashes with {} ({}-minute buffer)",
                        b.label,
                        buffer.num_minutes()
                    ));
                }
            }
            let same_day: Vec<&Busy> = busy.iter().filter(|b| local_day(b.start) == day).collect();
            let same_kind = same_day
                .iter()
                .filter(|b| b.kind.as_deref() == Some(&kind))
                .count() as u64;
            if same_kind >= kp.daily_limit {
                rejections.push(format!(
                    "Already {same_kind} {} booked that day (limit {})",
                    kind_label(&kind),
                    kp.daily_limit
                ));
            }
            let meetings = same_day
                .iter()
                .filter(|b| b.kind.as_deref().is_some_and(|k| k != "assessment"))
                .count() as u64;
            if kind != "assessment" && meetings >= policy.max_per_day {
                rejections.push(format!(
                    "Already {meetings} interviews that day (limit {})",
                    policy.max_per_day
                ));
            }

            let in_window = le.date_naive() == day
                && kp.windows.iter().any(|w| {
                    w.days[day.weekday().num_days_from_monday() as usize]
                        && ls.time() >= w.start
                        && le.time() <= w.end
                });
            let mut score = if in_window { 100 } else { 40 };
            let mut reasons = vec![if in_window {
                format!("Inside your {} window", kind_label(&kind))
            } else {
                format!("Outside your preferred {} windows", kind_label(&kind))
            }];
            if kind != "assessment" {
                score -= 10 * meetings as i64;
                if meetings > 0 {
                    reasons.push(match meetings {
                        1 => "1 other interview that day".to_owned(),
                        n => format!("{n} other interviews that day"),
                    });
                } else if !busy.is_empty() {
                    reasons.push("No other interviews that day".to_owned());
                }
            }
            if let Some(near) = busy.iter().find(|b| {
                let gap = if b.start >= end {
                    b.start - end
                } else {
                    start - b.end
                };
                gap >= Duration::zero() && gap < Duration::minutes(60)
            }) {
                score -= 5;
                reasons.push(format!("Within an hour of {}", near.label));
            }
            if kind != "deep"
                && kind != "assessment"
                && same_day.iter().any(|b| b.kind.as_deref() == Some("deep"))
            {
                score -= 15;
                reasons.push("Same day as a technical or final round".to_owned());
            }
            if kind == "deep" {
                if busy.iter().any(|b| {
                    b.kind.as_deref() == Some("deep")
                        && (local_day(b.start) - day).num_days().abs() == 1
                }) {
                    score -= 10;
                    reasons.push("Another technical round the day before or after".to_owned());
                }
                if start - now < Duration::hours(36) {
                    score -= 10;
                    reasons.push("Little time to prepare".to_owned());
                }
            }
            score -= (day - local_day(now)).num_days().max(0);
            Ranked {
                start,
                end,
                in_window,
                score,
                reasons,
                rejections,
            }
        })
        .collect();

    let mut open: Vec<&Ranked> = ranked.iter().filter(|r| r.rejections.is_empty()).collect();
    open.sort_by(|a, b| b.score.cmp(&a.score).then(a.start.cmp(&b.start)));
    // Offer different days first so one busy day does not crowd out the alternatives.
    let mut chosen: Vec<&Ranked> = Vec::new();
    let mut days = BTreeSet::new();
    for r in &open {
        if chosen.len() < limit && days.insert(local_day(r.start)) {
            chosen.push(r);
        }
    }
    for r in &open {
        if chosen.len() < limit && !chosen.iter().any(|c| c.start == r.start) {
            chosen.push(r);
        }
    }
    chosen.sort_by(|a, b| b.score.cmp(&a.score).then(a.start.cmp(&b.start)));

    let slot = |r: &Ranked| {
        json!({
            "start": r.start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "end": r.end.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "local_start": r.start.with_timezone(&tz).to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
            "local_end": r.end.with_timezone(&tz).to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
            "label": label(r.start, r.end, tz),
            "in_window": r.in_window,
            "score": r.score,
            "reasons": r.reasons,
        })
    };
    let mut blocking: BTreeMap<String, u64> = BTreeMap::new();
    for r in ranked.iter().filter(|r| !r.rejections.is_empty()) {
        for reason in &r.rejections {
            let key = match reason.rsplit_once(" (") {
                Some((head, tail))
                    if tail.ends_with("-minute buffer)") || tail.starts_with("limit ") =>
                {
                    head
                }
                _ => reason,
            }
            .to_owned();
            *blocking.entry(key).or_default() += 1;
        }
    }
    let mut blocking: Vec<(String, u64)> = blocking.into_iter().collect();
    blocking.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(json!({
        "timezone": tz.name(),
        "kind": kind,
        "duration_minutes": duration.num_minutes(),
        "suggestions": chosen.iter().map(|r| slot(r)).collect::<Vec<_>>(),
        "considered": ranked.len(),
        "available": open.len(),
        "rejected": if supplied {
            ranked.iter().filter(|r| !r.rejections.is_empty()).take(20).map(|r| json!({
                "start": r.start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "label": label(r.start, r.end, tz),
                "reasons": r.rejections,
            })).collect::<Vec<_>>()
        } else { vec![] },
        "blocking_reasons": blocking.iter().take(5).map(|(reason, count)| json!({"reason": reason, "count": count})).collect::<Vec<_>>(),
        "policy": {
            "windows": format_windows(&kp.windows),
            "daily_limit": kp.daily_limit,
            "buffer_minutes": kp.buffer_minutes,
            "min_notice_hours": kp.min_notice_hours,
            "deadline_margin_hours": kp.deadline_margin_hours,
            "hours": format!("{}-{}", policy.earliest.format("%H:%M"), policy.latest.format("%H:%M")),
            "max_per_day": policy.max_per_day,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn london() -> Value {
        json!({"timezone": "Europe/London"})
    }
    fn at(t: &str) -> DateTime<Utc> {
        instant(t, "test").unwrap()
    }
    fn run(args: Value, settings: Value) -> Value {
        suggest(&args, &settings, at("2026-01-01T00:00:00Z")).unwrap()
    }
    fn starts(out: &Value) -> Vec<String> {
        out["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["start"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn windows_parse_format_and_reject_bad_input() {
        let w = parse_windows("mon-fri 09:30-10:00, 12:00–13:00; Tue,Thu 14:00-15:00").unwrap();
        assert_eq!(w.len(), 3);
        assert_eq!(
            format_windows(&w),
            "Mon-Fri 09:30-10:00, 12:00-13:00; Tue, Thu 14:00-15:00"
        );
        assert_eq!(parse_windows(&format_windows(&w)).unwrap(), w);
        assert_eq!(windows_from_json(&windows_json(&w)).unwrap(), w);
        assert_eq!(format_days(&parse_days("fri-mon").unwrap()), "Mon, Fri-Sun");
        assert_eq!(
            parse_hours("09:00–18:00").unwrap().1,
            NaiveTime::from_hms_opt(18, 0, 0).unwrap()
        );
        assert!(parse_hours("18:00-09:00").is_err());
        for bad in [
            "mon 10:00-09:00",
            "mon 25:00-26:00",
            "funday 10:00-11:00",
            "mon",
            "mon 10-11",
        ] {
            assert!(parse_windows(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn local_windows_follow_daylight_saving_in_both_directions() {
        // London leaves BST on 25 Oct 2026 and enters it on 29 Mar 2026.
        for (day, utc) in [
            ("2026-10-22", "2026-10-22T09:00:00Z"), // Thu, BST: 10:00 local = 09:00Z
            ("2026-10-29", "2026-10-29T10:00:00Z"), // Thu, GMT: 10:00 local = 10:00Z
            ("2026-03-26", "2026-03-26T10:00:00Z"), // Thu, GMT
            ("2026-04-02", "2026-04-02T09:00:00Z"), // Thu, BST
        ] {
            let out = run(
                json!({"kind":"deep","duration_minutes":60,"limit":1,
                    "now":"2026-01-01T00:00:00Z","from":format!("{day}T00:00:00Z"),"until":format!("{day}T23:00:00Z")}),
                london(),
            );
            assert_eq!(starts(&out), vec![utc.to_owned()], "{day}");
            assert!(
                out["suggestions"][0]["label"]
                    .as_str()
                    .unwrap()
                    .contains("10:00-11:00")
            );
        }
    }

    #[test]
    fn grid_skips_missing_hour_and_keeps_both_repeated_hours() {
        let spring = local_grid(
            "Europe/London".parse().unwrap(),
            at("2026-03-29T00:00:00Z"),
            at("2026-03-29T03:00:00Z"),
            NaiveTime::MIN,
            NaiveTime::from_hms_opt(5, 0, 0).unwrap(),
        );
        // 01:00-01:59 local does not exist; 00:00Z..03:00Z maps to 00:00, then 02:00 BST onwards.
        let tz: Tz = "Europe/London".parse().unwrap();
        assert!(
            spring
                .iter()
                .all(|t| t.with_timezone(&tz).format("%H").to_string() != "01")
        );
        assert_eq!(spring.len(), 13); // every UTC quarter-hour 00:00Z..03:00Z, none duplicated
        let autumn = local_grid(
            "Europe/London".parse().unwrap(),
            at("2026-10-24T23:00:00Z"),
            at("2026-10-25T02:00:00Z"),
            NaiveTime::MIN,
            NaiveTime::from_hms_opt(3, 0, 0).unwrap(),
        );
        // 01:00-01:45 local happen twice; every UTC quarter-hour appears exactly once.
        assert_eq!(autumn.len(), 13);
        assert_eq!(autumn.first(), Some(&at("2026-10-24T23:00:00Z")));
        assert_eq!(autumn.last(), Some(&at("2026-10-25T02:00:00Z")));
    }

    #[test]
    fn offsets_from_other_zones_are_compared_as_instants() {
        // A Johannesburg (+02:00) invite and a Lisbon/UTC busy block describe the same instant.
        let out = run(
            json!({"kind":"screen","duration_minutes":30,"now":"2026-11-20T00:00:00Z",
                "candidates":[{"start":"2026-11-26T11:30:00+02:00"},{"start":"2026-11-26T12:30:00+00:00"}],
                "busy":[{"start":"2026-11-26T09:30:00Z","end":"2026-11-26T10:00:00Z","label":"Build Circle"}]}),
            london(),
        );
        assert_eq!(starts(&out), vec!["2026-11-26T12:30:00Z".to_owned()]);
        assert_eq!(out["rejected"][0]["start"], "2026-11-26T09:30:00Z");
        assert!(
            out["rejected"][0]["reasons"][0]
                .as_str()
                .unwrap()
                .contains("Build Circle")
        );
        assert_eq!(
            out["suggestions"][0]["local_start"],
            "2026-11-26T12:30:00+00:00"
        );
    }

    #[test]
    fn naive_times_and_unknown_zones_are_rejected() {
        let clock = at("2026-11-20T00:00:00Z");
        let naive = json!({"kind":"screen","duration_minutes":30,"candidates":[{"start":"2026-11-30T09:45:00"}]});
        assert!(
            suggest(&naive, &london(), clock)
                .unwrap_err()
                .message
                .contains("offset")
        );
        let zone = json!({"kind":"screen","duration_minutes":30,"timezone":"London"});
        assert!(suggest(&zone, &london(), clock).is_err());
    }

    #[test]
    fn buffers_daily_limits_and_hours_are_hard_rules() {
        let busy = json!([{"start":"2026-11-26T11:00:00Z","end":"2026-11-26T12:00:00Z","kind":"deep","label":"Build Circle technical"}]);
        let out = run(
            json!({"kind":"screen","duration_minutes":30,"now":"2026-11-20T00:00:00Z","busy":busy,
                "candidates":[{"start":"2026-11-26T10:15:00Z"},{"start":"2026-11-26T10:30:00Z"},{"start":"2026-11-26T12:30:00Z"},{"start":"2026-11-26T19:00:00Z"}]}),
            london(),
        );
        // 30-minute technical buffer blocks 10:30 and 12:00-12:30; 10:15 ends 10:45, inside the buffer too.
        assert_eq!(starts(&out), vec!["2026-11-26T12:30:00Z".to_owned()]);
        assert!(
            out["suggestions"][0]["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r == "Same day as a technical or final round")
        );
        assert!(out["rejected"].as_array().unwrap().iter().any(|r| {
            r["reasons"][0]
                .as_str()
                .unwrap()
                .starts_with("Outside meeting hours")
        }));
        let deep = run(
            json!({"kind":"deep","duration_minutes":60,"now":"2026-11-20T00:00:00Z","busy":busy,
                "candidates":[{"start":"2026-11-26T14:00:00Z"},{"start":"2026-11-25T10:00:00Z"}]}),
            london(),
        );
        assert_eq!(starts(&deep), vec!["2026-11-25T10:00:00Z".to_owned()]);
        assert!(
            deep["suggestions"][0]["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r.as_str().unwrap().contains("day before or after"))
        );
        assert!(
            deep["rejected"][0]["reasons"][0]
                .as_str()
                .unwrap()
                .contains("limit 1")
        );
    }

    #[test]
    fn outside_windows_is_offered_but_ranked_below_window_times() {
        let out = run(
            json!({"kind":"deep","duration_minutes":60,"now":"2026-11-20T00:00:00Z",
                "candidates":[{"start":"2026-11-30T09:00:00Z"},{"start":"2026-12-01T10:00:00Z"}]}),
            london(),
        );
        assert_eq!(
            starts(&out),
            vec![
                "2026-12-01T10:00:00Z".to_owned(),
                "2026-11-30T09:00:00Z".to_owned()
            ]
        );
        assert_eq!(out["suggestions"][1]["in_window"], false);
    }

    #[test]
    fn assessment_blocks_finish_a_margin_before_the_deadline() {
        let out = run(
            json!({"kind":"assessment","duration_minutes":45,"now":"2026-11-25T09:00:00Z",
                "deadline":"2026-11-28T06:12:00+05:00",
                "candidates":[{"start":"2026-11-26T14:00:00Z"},{"start":"2026-11-27T14:00:00Z"}]}),
            london(),
        );
        // Deadline is 01:12Z on 28 Nov; a 24h margin means finishing by 01:12Z on 27 Nov.
        assert_eq!(starts(&out), vec!["2026-11-26T14:00:00Z".to_owned()]);
        assert!(
            out["rejected"][0]["reasons"][0]
                .as_str()
                .unwrap()
                .contains("24h before the deadline")
        );
    }

    #[test]
    fn workspace_overrides_replace_defaults() {
        let settings = json!({"timezone":"America/New_York","scheduling":{"hours":{"start":"08:00","end":"20:00"},
            "kinds":{"screen":{"windows":[{"days":["sat"],"start":"08:00","end":"09:00"}],"min_notice_hours":0}}}});
        let out = run(
            json!({"kind":"screen","duration_minutes":30,"limit":1,"now":"2026-11-27T00:00:00Z","until":"2026-11-29T00:00:00Z"}),
            settings,
        );
        // Saturday 28 Nov 08:00 in New York (EST, UTC-5) is 13:00Z.
        assert_eq!(starts(&out), vec!["2026-11-28T13:00:00Z".to_owned()]);
        assert_eq!(out["policy"]["windows"], "Sat 08:00-09:00");
        assert!(
            out["suggestions"][0]["label"]
                .as_str()
                .unwrap()
                .ends_with("EST")
        );
    }
}
