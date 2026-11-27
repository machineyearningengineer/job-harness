//! Explicit host settings hold paths only, never candidate facts or credentials.
use job_harness_core::{Engine, HarnessError};
use job_harness_protocol::remote::{Client, read_token};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

fn user_settings_base() -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(path) = std::env::var_os("APPDATA") {
        return Some(PathBuf::from(path));
    }
    std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config"))
}
pub fn settings_path() -> anyhow::Result<PathBuf> {
    if let Some(development) = super::development::current() {
        return Ok(development.settings);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(user_settings_base)
        .ok_or_else(|| {
            HarnessError::new(
                "invalid_arguments",
                "Select --root explicitly or configure a user-owned host settings directory (XDG_CONFIG_HOME, APPDATA on Windows, or HOME).",
            )
        })?;
    Ok(base.join("job-harness/settings.json"))
}

/// The selected workspace, refused unless the host's workspace pins list it.
pub fn resolve(explicit: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let selected = if let Some(path) = explicit {
        path
    } else if let Some(development) = super::development::current() {
        development_selection(&development)?
    } else if let Some(path) = std::env::var_os("JOB_HARNESS_WORKSPACE") {
        PathBuf::from(path)
    } else {
        read_selection(&settings_path()?, None)?
    };
    Ok(job_harness_core::engine::pinned_root(&selected)?)
}

/// Source CLI and desktop selection share this boundary before any live env fallback.
pub fn development_workspace() -> Option<PathBuf> {
    super::development::current().map(|development| development.workspace)
}

fn development_selection(development: &super::development::Development) -> anyhow::Result<PathBuf> {
    read_selection(&development.settings, Some(&development.workspace))
}

fn read_selection(path: &Path, fallback: Option<&Path>) -> anyhow::Result<PathBuf> {
    if !path.is_file() {
        if let Some(path) = fallback {
            return Ok(path.to_owned());
        }
        return Err(HarnessError::new(
            "invalid_arguments",
            "Select --root PATH or run job-harness configure --workspace PATH.",
        )
        .into());
    }
    let data: Value = serde_json::from_slice(&fs::read(path)?)?;
    if data["schema_version"] == 1 && data["remote"].is_object() && data["workspace"].is_null() {
        return Err(HarnessError::new(
            "invalid_arguments",
            "This host reaches its workspace through jh serve, which only jh call and jh mcp use. Pass --root PATH for local commands.",
        )
        .into());
    }
    if data["schema_version"] != 1 || !data["workspace"].is_string() {
        return Err(HarnessError::new(
            "invalid_arguments",
            "Invalid host settings; supply --root or configure your workspace again.",
        )
        .into());
    }
    Ok(PathBuf::from(data["workspace"].as_str().unwrap()))
}

/// Remote mode for `jh call` and `jh mcp`: a `jh serve` URL and the file holding its
/// token, from `JOB_HARNESS_REMOTE_URL` and `JOB_HARNESS_SERVE_TOKEN_FILE` or from host
/// settings. Source builds read only their development settings.
pub fn remote() -> anyhow::Result<Option<Client>> {
    let env = super::development::current()
        .is_none()
        .then(|| std::env::var("JOB_HARNESS_REMOTE_URL").ok())
        .flatten()
        .map(|url| (url, std::env::var_os("JOB_HARNESS_SERVE_TOKEN_FILE")));
    let (url, token_file) = match env {
        Some((url, file)) => (url, file.map(PathBuf::from)),
        None => {
            let path = settings_path()?;
            let Ok(bytes) = fs::read(&path) else {
                return Ok(None);
            };
            let data: Value = serde_json::from_slice(&bytes)?;
            let Some(url) = data["remote"]["url"].as_str() else {
                return Ok(None);
            };
            let file = data["remote"]["token_file"].as_str().map(PathBuf::from);
            (url.to_owned(), file)
        }
    };
    let token_file = token_file.ok_or_else(|| {
        HarnessError::new(
            "invalid_arguments",
            "Remote mode needs the server's token file: set JOB_HARNESS_SERVE_TOKEN_FILE or run jh configure --remote URL --token-file PATH.",
        )
    })?;
    let token = read_token(&token_file)?;
    Ok(Some(Client::new(&url, token)?))
}

pub async fn configure_remote(url: &str, token_file: &Path) -> anyhow::Result<Value> {
    // read_token names the file when it is missing.
    let token = read_token(token_file)?;
    let token_file = token_file.canonicalize()?;
    let client = Client::new(url, token)?;
    let ready = client.call("bootstrap", &json!({})).await;
    if ready["ok"] != true {
        return Err(HarnessError::new(
            "invalid_arguments",
            format!(
                "The server at {url} did not answer bootstrap: {}",
                ready["error"]
            ),
        )
        .into());
    }
    save(json!({"schema_version":1,"remote":{"url":url,"token_file":token_file}}))?;
    Ok(json!({"remote":url,"token_file":token_file,"settings":settings_path()?}))
}

pub fn configure(root: &Path) -> anyhow::Result<Value> {
    let root = root.canonicalize()?;
    let ready = Engine::open(&root)?.call("bootstrap", &json!({}));
    if ready["ok"] != true {
        return Err(HarnessError::new(
            "invalid_arguments",
            "Choose an initialized readable workspace.",
        )
        .into());
    }
    let path = save(json!({"schema_version":1,"workspace":root}))?;
    Ok(
        json!({"workspace":root,"settings":path,"note":"Host-local path only. Credentials stay in the host's secure store."}),
    )
}

fn save(settings: Value) -> anyhow::Result<PathBuf> {
    let path = settings_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Invalid settings destination."))?;
    fs::create_dir_all(parent)?;
    if fs::symlink_metadata(parent)?.file_type().is_symlink() || path.is_symlink() {
        return Err(
            HarnessError::new("invalid_arguments", "Host settings may not use symlinks.").into(),
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(serde_json::to_string_pretty(&settings)?.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(&path)
        .map_err(|_| HarnessError::new("io_error", "Could not save host settings."))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_development_settings_choose_only_the_development_workspace() {
        let root = tempfile::tempdir().unwrap();
        let development = super::super::development::Development {
            workspace: root.path().join("development/workspace"),
            settings: root.path().join("development/host-settings/settings.json"),
        };
        assert_eq!(
            development_selection(&development).unwrap(),
            development.workspace
        );
        assert!(!development.workspace.exists());
    }

    #[test]
    fn invalid_development_settings_fail_without_falling_back_to_live_settings() {
        let root = tempfile::tempdir().unwrap();
        let development = super::super::development::Development {
            workspace: root.path().join("development/workspace"),
            settings: root.path().join("development-settings.json"),
        };
        fs::write(
            &development.settings,
            r#"{"schema_version":1,"workspace":null}"#,
        )
        .unwrap();
        assert!(development_selection(&development).is_err());
        assert!(read_selection(&root.path().join("missing.json"), None).is_err());
    }
}
