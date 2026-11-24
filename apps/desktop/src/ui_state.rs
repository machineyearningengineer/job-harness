//! Device-local interface preferences (layout toggles, reduce motion, chosen agent
//! and model). Career facts and board data always come from the ledger; this file
//! only remembers how the window was arranged so it reopens the same way.
use crate::chat_bridge::AgentOptions;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    pub reduce_motion: bool,
    #[serde(default = "default_true")]
    pub desktop_notifications: bool,
    pub show_archive: bool,
    pub only_attention: bool,
    pub interviews_expanded: bool,
    pub collapsed_cols: Vec<String>,
    pub chat_open: bool,
    pub chat_width: Option<u32>,
    pub drawer_width: Option<u32>,
    pub agent_kind: Option<String>,
    pub agent: AgentOptions,
    pub last_seen_activity_at: Option<String>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            reduce_motion: false,
            desktop_notifications: true,
            show_archive: false,
            only_attention: false,
            interviews_expanded: false,
            collapsed_cols: Vec::new(),
            chat_open: false,
            chat_width: None,
            drawer_width: None,
            agent_kind: None,
            agent: AgentOptions::default(),
            last_seen_activity_at: None,
        }
    }
}

impl UiState {
    pub fn path() -> Option<PathBuf> {
        if let Ok(custom) = std::env::var("JOB_HARNESS_UI_STATE_PATH")
            && !custom.trim().is_empty()
        {
            return Some(PathBuf::from(custom));
        }
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        let dir = if cfg!(target_os = "macos") {
            home.join("Library/Application Support/Job Harness")
        } else if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            PathBuf::from(xdg).join("job-harness")
        } else {
            home.join(".config/job-harness")
        };
        Some(dir.join("ui-state.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Atomic write (temp file + rename) so a crash never leaves a half-written file.
    pub fn save(&self) -> Result<()> {
        let path = Self::path().context("No home directory")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UiState;

    #[test]
    fn unknown_or_missing_fields_fall_back_to_defaults() {
        let s: UiState = serde_json::from_str(r#"{"reduce_motion":true,"future":1}"#).unwrap();
        assert!(s.reduce_motion);
        assert!(!s.chat_open);
        assert!(s.agent.model.is_none());
    }
}
