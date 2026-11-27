//! Native SQLite WAL coordinator store shared by the engine and workspace transfers.
use crate::{Error, Result, storage};
use job_harness_domain::{SubmissionLifecycle, SubmissionStatus};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

pub const DB_REL_PATH: &str = "applications/operations/harness.sqlite";

fn sqlite_err(err: rusqlite::Error) -> Error {
    Error::new("io_error", format!("SQLite coordinator error: {err}"))
}

fn db_path(root: &Path) -> Result<PathBuf> {
    let path = storage::safe_path(root, DB_REL_PATH, &["applications"], &[".sqlite"])?;
    storage::reject_symlink_ancestors(&path)?;
    for suffix in ["", "-wal", "-shm"] {
        let candidate = PathBuf::from(format!("{}{suffix}", path.display()));
        if candidate.is_symlink() {
            return Err(Error::new(
                "path_denied",
                "Coordinator SQLite paths must not be symlinks",
            ));
        }
    }
    Ok(path)
}

fn open_connection(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        storage::reject_symlink_ancestors(parent)?;
        fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    let created = !path.exists();
    let conn = Connection::open(path).map_err(sqlite_err)?;
    #[cfg(unix)]
    if created {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 5000;
         PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS meta (
             key TEXT PRIMARY KEY,
             value TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS runs (
             id TEXT PRIMARY KEY,
             operation TEXT NOT NULL,
             status TEXT NOT NULL,
             started_at TEXT NOT NULL,
             payload TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS attempts (
             id TEXT PRIMARY KEY,
             run_id TEXT NOT NULL,
             company TEXT NOT NULL,
             vacancy_url TEXT NOT NULL,
             status TEXT NOT NULL,
             channel TEXT NOT NULL DEFAULT 'portal',
             reserved_at TEXT NOT NULL,
             began_at TEXT,
             payload TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_attempts_status ON attempts(status);
         CREATE INDEX IF NOT EXISTS idx_attempts_began_at ON attempts(began_at);
         CREATE INDEX IF NOT EXISTS idx_attempts_run_id ON attempts(run_id);
         CREATE TABLE IF NOT EXISTS events (
             seq INTEGER PRIMARY KEY AUTOINCREMENT,
             idempotency_key TEXT NOT NULL,
             tool TEXT NOT NULL,
             at TEXT NOT NULL,
             payload TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_events_idempotency ON events(idempotency_key);
         CREATE INDEX IF NOT EXISTS idx_events_tool ON events(tool);
         CREATE TABLE IF NOT EXISTS pending_changes (
             seq INTEGER PRIMARY KEY AUTOINCREMENT,
             path TEXT NOT NULL UNIQUE,
             payload TEXT NOT NULL
         );",
    )
    .map_err(sqlite_err)?;
    Ok(conn)
}

pub fn init(root: &Path) -> Result<()> {
    let empty = json!({"schema_version":1,"runs":{},"attempts":{},"events":[]});
    save(root, &empty)?;
    checkpoint(root)
}

pub fn checkpoint(root: &Path) -> Result<()> {
    let path = db_path(root)?;
    if !path.is_file() {
        return Ok(());
    }
    let conn = open_connection(&path)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA optimize;")
        .map_err(sqlite_err)?;
    Ok(())
}

pub fn parse(root: &Path, value: &Value) -> Result<Value> {
    parse_owned(root, value.clone())
}

fn parse_owned(root: &Path, mut value: Value) -> Result<Value> {
    let invalid = || {
        Error::new(
            "invalid_state",
            "Malformed coordinator record; preserve and repair its exact source",
        )
    };
    if value["schema_version"] != 1
        || !value["runs"].is_object()
        || !value["attempts"].is_object()
        || !value["events"].is_array()
    {
        return Err(invalid());
    }
    for (id, run) in value["runs"].as_object().unwrap() {
        let row = run.as_object().ok_or_else(invalid)?;
        if row.get("id").and_then(Value::as_str) != Some(id.as_str())
            || !row.get("scope").is_some_and(Value::is_object)
        {
            return Err(invalid());
        }
        for key in ["operation", "status", "started_at"] {
            if !row.get(key).is_some_and(Value::is_string) {
                return Err(invalid());
            }
        }
    }
    for (id, attempt) in value["attempts"].as_object().unwrap() {
        let row = attempt.as_object().ok_or_else(invalid)?;
        if row.get("id").and_then(Value::as_str) != Some(id.as_str()) {
            return Err(invalid());
        }
        for key in [
            "run_id",
            "packet_id",
            "record",
            "company",
            "vacancy_url",
            "source",
            "reserved_at",
        ] {
            if !row.get(key).is_some_and(Value::is_string) {
                return Err(invalid());
            }
        }
        SubmissionStatus::parse(&attempt["status"])?;
    }
    for event in value["events"].as_array().unwrap() {
        let row = event.as_object().ok_or_else(invalid)?;
        for key in ["idempotency_key", "request_sha256", "tool", "at"] {
            if !row.get(key).is_some_and(Value::is_string) {
                return Err(invalid());
            }
        }
        if !row.get("result").is_some_and(Value::is_object) {
            return Err(invalid());
        }
    }
    crate::engine::validate_daily_events(&value)
        .map_err(|error| Error::new("invalid_state", error.to_string()))?;
    if let Some(pending) = value.get("pending") {
        if pending.is_null() || pending.as_array().is_some_and(Vec::is_empty) {
            value.as_object_mut().unwrap().remove("pending");
        } else {
            let mut seen = BTreeSet::new();
            for change in pending.as_array().ok_or_else(invalid)? {
                let path = change
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?;
                if !seen.insert(path) {
                    return Err(Error::new(
                        "invalid_state",
                        "Pending journal contains duplicate paths",
                    ));
                }
                journal_path(root, path)?;
                if !change.get("text").is_some_and(Value::is_string)
                    || !change
                        .get("before_sha256")
                        .is_some_and(|v| v.is_null() || v.is_string())
                {
                    return Err(invalid());
                }
            }
        }
    }
    Ok(value)
}

pub fn journal_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = storage::safe_path(
        root,
        relative,
        &["applications", "context"],
        &[".md", ".json"],
    )?;
    storage::reject_symlink_ancestors(&path)?;
    if relative.starts_with("context/intake/") {
        return Ok(path);
    }
    if path.parent() != Some(root.canonicalize()?.join("applications").as_path())
        || path.extension().and_then(|e| e.to_str()) != Some("md")
    {
        return Err(Error::new(
            "path_denied",
            "Journal paths must be canonical vacancy records or explicit intake data",
        ));
    }
    Ok(path)
}

pub fn load(root: &Path) -> Result<Value> {
    load_inner(root, true)
}

/// Fast read path using SQLite's `idx_events_tool` index to load only mail-lifecycle
/// events (skipping thousands of unrelated historical tool events on read-only queries).
pub fn load_for_read(root: &Path) -> Result<Value> {
    load_inner(root, false)
}

fn load_inner(root: &Path, include_all_events: bool) -> Result<Value> {
    let path = db_path(root)?;
    if !path.is_file() {
        if ["packets", "receipts", "conflicts"]
            .iter()
            .any(|dir| root.join("applications/operations").join(dir).exists())
        {
            return Err(Error::new(
                "invalid_state",
                "Coordinator SQLite database is missing while operational artifacts exist. Restore its exact state backup; never create a fresh coordinator over existing attempts.",
            ));
        }
        return Ok(json!({"schema_version":1,"runs":{},"attempts":{},"events":[]}));
    }
    let conn = open_connection(&path)?;
    let invalid = || {
        Error::new(
            "invalid_state",
            "Malformed SQLite coordinator record; preserve and repair its exact source",
        )
    };

    let mut obj = Map::new();
    let mut stmt = conn
        .prepare("SELECT key, value FROM meta")
        .map_err(sqlite_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(sqlite_err)?;
    for row in rows {
        let (k, v) = row.map_err(sqlite_err)?;
        let parsed: Value = serde_json::from_str(&v).map_err(|_| invalid())?;
        obj.insert(k, parsed);
    }
    if !obj.contains_key("schema_version") {
        return Err(invalid());
    }

    let mut runs = Map::new();
    let mut stmt = conn
        .prepare("SELECT id, payload FROM runs ORDER BY id")
        .map_err(sqlite_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(sqlite_err)?;
    for row in rows {
        let (id, payload) = row.map_err(sqlite_err)?;
        runs.insert(id, serde_json::from_str(&payload).map_err(|_| invalid())?);
    }
    obj.insert("runs".into(), Value::Object(runs));

    let mut attempts = Map::new();
    let mut stmt = conn
        .prepare("SELECT id, payload FROM attempts ORDER BY id")
        .map_err(sqlite_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(sqlite_err)?;
    for row in rows {
        let (id, payload) = row.map_err(sqlite_err)?;
        attempts.insert(id, serde_json::from_str(&payload).map_err(|_| invalid())?);
    }
    obj.insert("attempts".into(), Value::Object(attempts));

    let mut events = Vec::new();
    let events_sql = if include_all_events {
        "SELECT payload FROM events ORDER BY seq ASC"
    } else {
        "SELECT payload FROM events WHERE tool IN ('daily_update', 'begin_mail_archive', 'record_adapter_result') ORDER BY seq ASC"
    };
    let mut stmt = conn.prepare(events_sql).map_err(sqlite_err)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sqlite_err)?;
    for row in rows {
        let payload = row.map_err(sqlite_err)?;
        events.push(serde_json::from_str(&payload).map_err(|_| invalid())?);
    }
    obj.insert("events".into(), Value::Array(events));

    let mut pending = Vec::new();
    let mut stmt = conn
        .prepare("SELECT payload FROM pending_changes ORDER BY seq ASC")
        .map_err(sqlite_err)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sqlite_err)?;
    for row in rows {
        let payload = row.map_err(sqlite_err)?;
        pending.push(serde_json::from_str(&payload).map_err(|_| invalid())?);
    }
    if !pending.is_empty() {
        obj.insert("pending".into(), Value::Array(pending));
    }

    parse_owned(root, Value::Object(obj))
}

pub fn save(root: &Path, value: &Value) -> Result<()> {
    let normalized = parse(root, value)?;
    let path = db_path(root)?;
    let mut conn = open_connection(&path)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_err)?;

    // 1. Meta keys (schema_version + any extra top-level metadata keys)
    let obj = normalized.as_object().unwrap();
    let mut keep_meta = BTreeSet::new();
    for (k, v) in obj {
        if matches!(k.as_str(), "runs" | "attempts" | "events" | "pending") {
            continue;
        }
        keep_meta.insert(k.clone());
        let encoded = serde_json::to_string(v)?;
        tx.execute(
            "INSERT INTO meta(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value WHERE value != excluded.value",
            params![k, encoded],
        )
        .map_err(sqlite_err)?;
    }
    {
        let mut stmt = tx.prepare("SELECT key FROM meta").map_err(sqlite_err)?;
        let existing: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .map_err(sqlite_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sqlite_err)?;
        for key in existing {
            if !keep_meta.contains(&key) {
                tx.execute("DELETE FROM meta WHERE key = ?1", params![key])
                    .map_err(sqlite_err)?;
            }
        }
    }

    // 2. Runs delta upsert
    let mut existing_runs: BTreeMap<String, String> = BTreeMap::new();
    {
        let mut stmt = tx
            .prepare("SELECT id, payload FROM runs")
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(sqlite_err)?;
        for row in rows {
            let (id, payload) = row.map_err(sqlite_err)?;
            existing_runs.insert(id, payload);
        }
    }
    let runs_map = obj["runs"].as_object().unwrap();
    for (id, run) in runs_map {
        let encoded = serde_json::to_string(run)?;
        if existing_runs.get(id) != Some(&encoded) {
            tx.execute(
                "INSERT INTO runs(id, operation, status, started_at, payload)
                 VALUES(?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET
                    operation = excluded.operation,
                    status = excluded.status,
                    started_at = excluded.started_at,
                    payload = excluded.payload",
                params![
                    id,
                    run["operation"].as_str().unwrap_or_default(),
                    run["status"].as_str().unwrap_or_default(),
                    run["started_at"].as_str().unwrap_or_default(),
                    encoded
                ],
            )
            .map_err(sqlite_err)?;
        }
    }
    for id in existing_runs.keys() {
        if !runs_map.contains_key(id) {
            tx.execute("DELETE FROM runs WHERE id = ?1", params![id])
                .map_err(sqlite_err)?;
        }
    }

    // 3. Attempts delta upsert
    let mut existing_attempts: BTreeMap<String, String> = BTreeMap::new();
    {
        let mut stmt = tx
            .prepare("SELECT id, payload FROM attempts")
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(sqlite_err)?;
        for row in rows {
            let (id, payload) = row.map_err(sqlite_err)?;
            existing_attempts.insert(id, payload);
        }
    }
    let attempts_map = obj["attempts"].as_object().unwrap();
    for (id, attempt) in attempts_map {
        let encoded = serde_json::to_string(attempt)?;
        if existing_attempts.get(id) != Some(&encoded) {
            let channel = attempt
                .get("channel")
                .and_then(Value::as_str)
                .unwrap_or("portal");
            let began_at = attempt.get("began_at").and_then(Value::as_str);
            tx.execute(
                "INSERT INTO attempts(id, run_id, company, vacancy_url, status, channel, reserved_at, began_at, payload)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET
                    run_id = excluded.run_id,
                    company = excluded.company,
                    vacancy_url = excluded.vacancy_url,
                    status = excluded.status,
                    channel = excluded.channel,
                    reserved_at = excluded.reserved_at,
                    began_at = excluded.began_at,
                    payload = excluded.payload",
                params![
                    id,
                    attempt["run_id"].as_str().unwrap_or_default(),
                    attempt["company"].as_str().unwrap_or_default(),
                    attempt["vacancy_url"].as_str().unwrap_or_default(),
                    attempt["status"].as_str().unwrap_or_default(),
                    channel,
                    attempt["reserved_at"].as_str().unwrap_or_default(),
                    began_at,
                    encoded
                ],
            )
            .map_err(sqlite_err)?;
        }
    }
    for id in existing_attempts.keys() {
        if !attempts_map.contains_key(id) {
            tx.execute("DELETE FROM attempts WHERE id = ?1", params![id])
                .map_err(sqlite_err)?;
        }
    }

    // 4. Events (fast O(1) append path when existing prefix is unchanged)
    let events_arr = obj["events"].as_array().unwrap();
    let existing_count: usize = tx
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
        .map_err(sqlite_err)? as usize;
    let can_append = if existing_count == 0 {
        true
    } else if existing_count <= events_arr.len() {
        let last_payload: Option<String> = tx
            .query_row(
                "SELECT payload FROM events ORDER BY seq DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_err)?;
        let expected_last = serde_json::to_string(&events_arr[existing_count - 1])?;
        last_payload.as_deref() == Some(expected_last.as_str())
    } else {
        false
    };

    let start_idx = if can_append {
        existing_count
    } else {
        tx.execute("DELETE FROM events", []).map_err(sqlite_err)?;
        0
    };
    if start_idx < events_arr.len() {
        let mut insert_event = tx
            .prepare(
                "INSERT INTO events(idempotency_key, tool, at, payload) VALUES(?1, ?2, ?3, ?4)",
            )
            .map_err(sqlite_err)?;
        for event in &events_arr[start_idx..] {
            let encoded = serde_json::to_string(event)?;
            insert_event
                .execute(params![
                    event["idempotency_key"].as_str().unwrap_or_default(),
                    event["tool"].as_str().unwrap_or_default(),
                    event["at"].as_str().unwrap_or_default(),
                    encoded
                ])
                .map_err(sqlite_err)?;
        }
    }

    // 5. Pending journal changes
    tx.execute("DELETE FROM pending_changes", [])
        .map_err(sqlite_err)?;
    if let Some(pending) = obj.get("pending").and_then(Value::as_array) {
        let mut insert_pending = tx
            .prepare("INSERT INTO pending_changes(path, payload) VALUES(?1, ?2)")
            .map_err(sqlite_err)?;
        for change in pending {
            let encoded = serde_json::to_string(change)?;
            insert_pending
                .execute(params![
                    change["path"].as_str().unwrap_or_default(),
                    encoded
                ])
                .map_err(sqlite_err)?;
        }
    }

    tx.commit().map_err(sqlite_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_pending_is_clean_without_changing_source_bytes() {
        let root = tempfile::tempdir().unwrap();
        for pending in [Value::Null, json!([])] {
            let raw = json!({"schema_version":1,"runs":{},"attempts":{},"events":[],"pending":pending,"future":{"a":1}});
            let normalized = parse(root.path(), &raw).unwrap();
            assert!(normalized.get("pending").is_none());
            assert!(raw.get("pending").is_some());
            assert_eq!(normalized["future"], raw["future"]);
        }
        assert!(
            parse(
                root.path(),
                &json!({"schema_version":1,"runs":{},"attempts":{},"events":[],"pending":{}})
            )
            .is_err()
        );
    }
    #[test]
    fn orphaned_operational_artifacts_do_not_reset_history() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("applications/operations/packets")).unwrap();
        assert_eq!(load(root.path()).unwrap_err().code, "invalid_state");
        assert!(!root.path().join(DB_REL_PATH).exists());
    }
    #[test]
    fn sqlite_wal_roundtrip_and_delta_append() {
        let root = tempfile::tempdir().unwrap();
        init(root.path()).unwrap();
        let mut s = load(root.path()).unwrap();
        s["runs"]["r1"] = json!({"id":"r1","operation":"apply","status":"running","started_at":"2026-11-23T10:00:00Z","scope":{}});
        s["events"].as_array_mut().unwrap().push(json!({
            "idempotency_key":"k1","request_sha256":"abc","tool":"start_run","at":"2026-11-23T10:00:00Z","result":{"run_id":"r1"}
        }));
        save(root.path(), &s).unwrap();
        s["events"].as_array_mut().unwrap().push(json!({
            "idempotency_key":"k2","request_sha256":"def","tool":"finish_run","at":"2026-11-23T10:01:00Z","result":{"run_id":"r1"}
        }));
        save(root.path(), &s).unwrap();
        let reloaded = load(root.path()).unwrap();
        assert_eq!(reloaded["runs"]["r1"]["status"], "running");
        assert_eq!(reloaded["events"].as_array().unwrap().len(), 2);
    }
}
