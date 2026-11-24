//! Ephemeral, owner-only (`0600`), self-destructing secret handoff vault.
//!
//! Secrets NEVER live inside the git repository, workspace folder, chat transcript,
//! or SQLite ledger. They are stored in a per-user `0700` OS temp directory with
//! strict TTL expiry, single-use auto-shred (zero-overwrite + unlink), and instant
//! user revocation — usable from the GPUI desktop modal, CLI, or any host agent.

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretFieldSpec {
    pub name: String,
    pub label: String,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default = "default_secret_true")]
    pub secret: bool,
}

fn default_secret_true() -> bool {
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretStatus {
    Pending,
    Ready,
    Consumed,
    Revoked,
    Expired,
}

/// Non-secret metadata persisted in `req-<id>.meta.json` (`0600`).
/// Secret values are NEVER stored in this struct or serialized into metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRequest {
    pub id: String,
    pub label: String,
    pub reason: String,
    #[serde(default)]
    pub url: Option<String>,
    pub fields: Vec<SecretFieldSpec>,
    pub created_at_unix: u64,
    pub expires_at_unix: u64,
    pub max_uses: u32,
    pub uses_remaining: u32,
    pub status: SecretStatus,
    #[serde(default)]
    pub consumed_at_unix: Option<u64>,
    #[serde(default)]
    pub consumed_fields: Vec<String>,
}

impl SecretRequest {
    pub fn remaining_secs(&self, now: u64) -> u64 {
        self.expires_at_unix.saturating_sub(now)
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Returns the owner-only (`0700`) ephemeral vault directory in OS temp storage.
pub fn vault_dir() -> std::io::Result<PathBuf> {
    if let Ok(custom) = std::env::var("JOB_HARNESS_SECRET_VAULT")
        && !custom.trim().is_empty()
    {
        return ensure_private_dir(PathBuf::from(custom));
    }
    let user_tag = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "local".to_owned());
    let clean_tag: String = user_tag
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    let dir = std::env::temp_dir().join(format!(
        "job-harness-secrets-{}",
        if clean_tag.is_empty() {
            "user"
        } else {
            &clean_tag
        }
    ));
    ensure_private_dir(dir)
}

fn ensure_private_dir(dir: PathBuf) -> std::io::Result<PathBuf> {
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

fn write_private_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let mut f = fs::File::create(path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    }
}

/// Overwrites file contents with zeros before unlinking so raw bytes don't linger.
fn shred_file(path: &Path) {
    if !path.is_file() {
        return;
    }
    if let Ok(meta) = fs::metadata(path) {
        let len = meta.len().min(1024 * 1024) as usize;
        if len > 0
            && let Ok(mut f) = fs::OpenOptions::new().write(true).open(path)
        {
            let zeros = vec![0u8; len];
            let _ = f.write_all(&zeros);
            let _ = f.sync_all();
        }
    }
    let _ = fs::remove_file(path);
}

fn sanitize_id(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(48)
        .collect()
}

fn random_hex_id() -> String {
    let mut buf = [0u8; 8];
    let filled = fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
        .is_ok();
    if !filled {
        let t = now_unix().to_le_bytes();
        let pid = std::process::id().to_le_bytes();
        for i in 0..8 {
            buf[i] = t[i] ^ pid[i % 4].wrapping_mul(31);
        }
    }
    let mut s = String::with_capacity(16);
    for b in buf {
        use std::fmt::Write;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

fn meta_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("req-{}.meta.json", sanitize_id(id)))
}

fn payload_path(dir: &Path, id: &str, field: &str) -> PathBuf {
    dir.join(format!(
        "val-{}-{}.secret",
        sanitize_id(id),
        sanitize_id(field)
    ))
}

fn shred_all_payloads(dir: &Path, req: &SecretRequest) {
    for f in &req.fields {
        shred_file(&payload_path(dir, &req.id, &f.name));
    }
}

fn save_meta(dir: &Path, req: &SecretRequest) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(req)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write_private_bytes(&meta_path(dir, &req.id), &bytes)
}

pub fn load_request(id: &str) -> std::io::Result<SecretRequest> {
    let dir = vault_dir()?;
    let bytes = fs::read(meta_path(&dir, id))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Sweeps expired requests, shredding their payloads and pruning stale terminal metadata (>15m).
pub fn sweep_expired() -> std::io::Result<Vec<SecretRequest>> {
    let dir = vault_dir()?;
    let now = now_unix();
    let mut all = Vec::new();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(all);
    };
    for ent in entries.flatten() {
        let p = ent.path();
        let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !name.starts_with("req-") || !name.ends_with(".meta.json") {
            continue;
        }
        let Ok(bytes) = fs::read(&p) else {
            continue;
        };
        let Ok(mut req) = serde_json::from_slice::<SecretRequest>(&bytes) else {
            continue;
        };
        if matches!(req.status, SecretStatus::Pending | SecretStatus::Ready)
            && now >= req.expires_at_unix
        {
            shred_all_payloads(&dir, &req);
            req.status = SecretStatus::Expired;
            req.uses_remaining = 0;
            let _ = save_meta(&dir, &req);
        }
        // Prune terminal metadata after 15 minutes so temp dir stays spotless.
        let terminal_age = now.saturating_sub(req.consumed_at_unix.unwrap_or(req.expires_at_unix));
        if matches!(
            req.status,
            SecretStatus::Consumed | SecretStatus::Revoked | SecretStatus::Expired
        ) && terminal_age > 900
        {
            shred_all_payloads(&dir, &req);
            let _ = fs::remove_file(&p);
            continue;
        }
        all.push(req);
    }
    all.sort_by_key(|a| std::cmp::Reverse(a.created_at_unix));
    Ok(all)
}

/// Creates a new pending ephemeral secret request (metadata only — zero secret values).
pub fn create_request(
    label: String,
    reason: String,
    url: Option<String>,
    fields: Vec<SecretFieldSpec>,
    ttl_seconds: u64,
    max_uses: u32,
) -> std::io::Result<SecretRequest> {
    let dir = vault_dir()?;
    let now = now_unix();
    let ttl = ttl_seconds.clamp(60, 900);
    let uses = max_uses.clamp(1, 5);
    let clean_fields = if fields.is_empty() {
        vec![SecretFieldSpec {
            name: "value".into(),
            label: "Secret value".into(),
            placeholder: Some("Enter secret…".into()),
            secret: true,
        }]
    } else {
        fields
            .into_iter()
            .map(|mut f| {
                f.name = sanitize_id(&f.name);
                if f.name.is_empty() {
                    f.name = "value".into();
                }
                f
            })
            .collect()
    };
    let req = SecretRequest {
        id: random_hex_id(),
        label,
        reason,
        url,
        fields: clean_fields,
        created_at_unix: now,
        expires_at_unix: now + ttl,
        max_uses: uses,
        uses_remaining: uses,
        status: SecretStatus::Pending,
        consumed_at_unix: None,
        consumed_fields: Vec::new(),
    };
    save_meta(&dir, &req)?;
    Ok(req)
}

/// Privately fulfills a pending request, writing one `0600` file per field and arming the TTL.
pub fn fulfill_request(
    id: &str,
    values: &BTreeMap<String, String>,
    ttl_seconds: Option<u64>,
) -> std::io::Result<SecretRequest> {
    let dir = vault_dir()?;
    let mut req = load_request(id)?;
    let now = now_unix();
    if req.status != SecretStatus::Pending {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Secret request is no longer pending.",
        ));
    }
    for f in &req.fields {
        let Some(v) = values.get(&f.name) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Missing required field '{}'.", f.name),
            ));
        };
        if v.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Field '{}' cannot be empty.", f.name),
            ));
        }
        write_private_bytes(&payload_path(&dir, &req.id, &f.name), v.as_bytes())?;
    }
    let ttl = ttl_seconds.unwrap_or(300).clamp(60, 900);
    req.expires_at_unix = now + ttl;
    req.status = SecretStatus::Ready;
    req.uses_remaining = req.max_uses;
    save_meta(&dir, &req)?;
    Ok(req)
}

/// Reads a single field once and immediately shreds it on final use (self-destruct).
pub fn consume_field(id: &str, field: &str) -> std::io::Result<(String, SecretRequest)> {
    let dir = vault_dir()?;
    let mut req = load_request(id)?;
    let now = now_unix();
    if req.status != SecretStatus::Ready {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("Secret is not ready (status: {:?}).", req.status),
        ));
    }
    if now >= req.expires_at_unix {
        shred_all_payloads(&dir, &req);
        req.status = SecretStatus::Expired;
        req.uses_remaining = 0;
        let _ = save_meta(&dir, &req);
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Secret expired and was destroyed.",
        ));
    }
    let clean_field = sanitize_id(field);
    let path = payload_path(&dir, &req.id, &clean_field);
    let value = fs::read_to_string(&path)?;

    if !req.consumed_fields.contains(&clean_field) {
        req.consumed_fields.push(clean_field.clone());
    }
    let all_fields_read = req
        .fields
        .iter()
        .all(|f| req.consumed_fields.contains(&f.name));

    if req.uses_remaining <= 1 {
        shred_file(&path);
        if all_fields_read {
            shred_all_payloads(&dir, &req);
            req.uses_remaining = 0;
            req.status = SecretStatus::Consumed;
            req.consumed_at_unix = Some(now);
        }
    } else if all_fields_read {
        req.uses_remaining = req.uses_remaining.saturating_sub(1);
        req.consumed_fields.clear();
    }
    save_meta(&dir, &req)?;
    Ok((value, req))
}

/// Returns `0600` file paths for T3-style command-line `$(cat <file>)` consumption without echoing secrets.
pub fn deliver_file_paths(id: &str) -> std::io::Result<BTreeMap<String, PathBuf>> {
    let dir = vault_dir()?;
    let req = load_request(id)?;
    let now = now_unix();
    if req.status != SecretStatus::Ready || now >= req.expires_at_unix {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Secret is not ready or has expired.",
        ));
    }
    let mut out = BTreeMap::new();
    for f in &req.fields {
        let p = payload_path(&dir, &req.id, &f.name);
        if p.is_file() {
            out.insert(f.name.clone(), p);
        }
    }
    Ok(out)
}

/// Immediately zero-overwrites and deletes all secret files for a request.
pub fn revoke_request(id: &str) -> std::io::Result<SecretRequest> {
    let dir = vault_dir()?;
    let mut req = load_request(id)?;
    shred_all_payloads(&dir, &req);
    req.uses_remaining = 0;
    req.status = SecretStatus::Revoked;
    req.consumed_at_unix = Some(now_unix());
    save_meta(&dir, &req)?;
    Ok(req)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_use_secret_self_destructs_after_consume_and_never_leaks_to_meta() {
        let tmp = std::env::temp_dir().join(format!("jh-sec-test-{}", random_hex_id()));
        // SAFETY: unit test runs isolated with unique directory env var.
        unsafe {
            std::env::set_var("JOB_HARNESS_SECRET_VAULT", &tmp);
        }
        let req = create_request(
            "Portal Login".into(),
            "Submit application".into(),
            Some("https://example.com/login".into()),
            vec![
                SecretFieldSpec {
                    name: "email".into(),
                    label: "Email".into(),
                    placeholder: None,
                    secret: false,
                },
                SecretFieldSpec {
                    name: "password".into(),
                    label: "Password".into(),
                    placeholder: None,
                    secret: true,
                },
            ],
            300,
            1,
        )
        .unwrap();
        assert_eq!(req.status, SecretStatus::Pending);

        let mut vals = BTreeMap::new();
        vals.insert("email".into(), "test@example.com".into());
        vals.insert("password".into(), "super-secret-pass-42".into());
        let armed = fulfill_request(&req.id, &vals, Some(180)).unwrap();
        assert_eq!(armed.status, SecretStatus::Ready);

        // Metadata file must NEVER contain the password.
        let meta_txt = fs::read_to_string(meta_path(&tmp, &req.id)).unwrap();
        assert!(!meta_txt.contains("super-secret-pass-42"));

        let (email, after_email) = consume_field(&req.id, "email").unwrap();
        assert_eq!(email, "test@example.com");
        assert_eq!(after_email.status, SecretStatus::Ready);

        let (pass, after_pass) = consume_field(&req.id, "password").unwrap();
        assert_eq!(pass, "super-secret-pass-42");
        assert_eq!(after_pass.status, SecretStatus::Consumed);

        // Second read must fail — self-destructed.
        assert!(consume_field(&req.id, "password").is_err());
        assert!(!payload_path(&tmp, &req.id, "password").exists());

        let _ = fs::remove_dir_all(&tmp);
    }
}
