//! Native GPUI desktop binary for Job Harness.
mod app;
mod chat_bridge;
mod demo_seed;
mod icons;
mod macos_notify;
mod markdown;
mod sankey;
mod theme;
mod transport;
mod ui_state;

use app::JobHarnessApp;
use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBackgroundAppearance,
    WindowBounds, WindowOptions, point, px, size,
};
use std::path::PathBuf;
use transport::WorkspaceTarget;

/// The value after `flag`, if given.
fn flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let i = args.iter().position(|arg| arg == flag)?;
    args.get(i + 1).map(String::as_str)
}

/// A trimmed, non-empty value.
fn set(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

fn read_token(file: &str) -> Result<String, String> {
    let text = std::fs::read_to_string(file)
        .map_err(|err| format!("Cannot read token file {file}: {err}"))?;
    set(Some(text)).ok_or_else(|| format!("Token file {file} is empty"))
}

/// The server's token, first found of: `--token-file`, `JOB_HARNESS_SERVE_TOKEN`,
/// `JOB_HARNESS_SERVE_TOKEN_FILE`, the token file saved by `jh configure --remote`.
fn serve_token(
    args: &[String],
    env: &dyn Fn(&str) -> Option<String>,
    saved: Option<&str>,
) -> Result<Option<String>, String> {
    if let Some(file) = flag(args, "--token-file") {
        return read_token(file).map(Some);
    }
    if let Some(token) = set(env("JOB_HARNESS_SERVE_TOKEN")) {
        return Ok(Some(token));
    }
    match set(env("JOB_HARNESS_SERVE_TOKEN_FILE")).or(saved.map(str::to_owned)) {
        Some(file) => read_token(&file).map(Some),
        None => Ok(None),
    }
}

/// `jh`'s settings: `$XDG_CONFIG_HOME/job-harness/settings.json`, else `~/.config/...`.
fn settings(env: &dyn Fn(&str) -> Option<String>) -> serde_json::Value {
    let base = set(env("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .or_else(|| set(env("HOME")).map(|home| PathBuf::from(home).join(".config")));
    base.and_then(|base| std::fs::read(base.join("job-harness/settings.json")).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// What to open: `--remote`/`--root`, then `JOB_HARNESS_REMOTE_URL`/`JOB_HARNESS_WORKSPACE`,
/// then the remote or workspace saved by `jh configure`, then `~/Career/workspace`.
fn resolve_target(
    args: &[String],
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<WorkspaceTarget, String> {
    let remote = |url: &str, saved: Option<&str>| {
        Ok(WorkspaceTarget::Remote {
            url: url.to_owned(),
            token: serve_token(args, env, saved)?,
        })
    };
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match (arg.as_str(), iter.next()) {
            ("--remote", Some(url)) => return remote(url, None),
            ("--root", Some(path)) => return Ok(WorkspaceTarget::Local(PathBuf::from(path))),
            _ => {}
        }
    }
    if let Some(url) = set(env("JOB_HARNESS_REMOTE_URL")) {
        return remote(&url, None);
    }
    if let Some(root) = set(env("JOB_HARNESS_WORKSPACE")) {
        return Ok(WorkspaceTarget::Local(PathBuf::from(root)));
    }
    let saved = settings(env);
    if let Some(url) = saved["remote"]["url"].as_str() {
        return remote(url, saved["remote"]["token_file"].as_str());
    }
    if let Some(ws) = saved["workspace"].as_str() {
        return Ok(WorkspaceTarget::Local(PathBuf::from(ws)));
    }
    Ok(WorkspaceTarget::Local(set(env("HOME")).map_or_else(
        || PathBuf::from("."),
        |home| PathBuf::from(home).join("Career/workspace"),
    )))
}

fn open_main_window(target: WorkspaceTarget, cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(1260.0), px(780.0)), cx);
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Job Harness".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(14.0), px(15.0))),
                }),
                window_background: WindowBackgroundAppearance::Opaque,
                ..Default::default()
            },
            |window, cx| {
                window.activate_window();
                cx.new(|cx| JobHarnessApp::new(target, window, cx))
            },
        )
        .expect("open Job Harness GPUI window");
    let _ = handle.update(cx, |_, window, _| window.activate_window());
    cx.activate(true);
}

fn main() {
    // Answer without opening a window, so packaging checks and scripts can run headless.
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => {
            println!("job-harness-desktop {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("--help" | "-h") => {
            println!(
                "Job Harness desktop\n\nUsage: job-harness-desktop [--root PATH | --remote URL [--token-file PATH]]\n\nWith --remote, the server's token comes from --token-file, JOB_HARNESS_SERVE_TOKEN or JOB_HARNESS_SERVE_TOKEN_FILE. Without arguments it opens what `jh configure` saved."
            );
            return;
        }
        _ => {}
    }
    let args: Vec<String> = std::env::args().collect();
    let target = match resolve_target(&args, &|key| std::env::var(key).ok()) {
        Ok(target) => target,
        Err(message) => {
            eprintln!("job-harness-desktop: {message}");
            std::process::exit(2);
        }
    };
    let reopen_target = target.clone();
    let app = Application::new().with_assets(icons::EmbeddedIcons);
    app.on_reopen(move |cx: &mut App| {
        if let Some(win) = cx.windows().into_iter().next() {
            let _ = win.update(cx, |_, window, _| window.activate_window());
            cx.activate(true);
        } else {
            open_main_window(reopen_target.clone(), cx);
        }
    });
    app.run(move |cx: &mut App| {
        open_main_window(target, cx);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn args(list: &[&str]) -> Vec<String> {
        std::iter::once("job-harness-desktop")
            .chain(list.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| vars.get(key).cloned()
    }

    fn token(target: Result<WorkspaceTarget, String>) -> Option<String> {
        match target.unwrap() {
            WorkspaceTarget::Remote { token, .. } => token,
            WorkspaceTarget::Local(path) => panic!("expected remote, got {path:?}"),
        }
    }

    #[test]
    fn an_explicit_token_file_wins_and_empty_values_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("token");
        std::fs::write(&file, "from-file-0123456789\n").unwrap();
        let file = file.to_str().unwrap();
        let env_token = env(&[("JOB_HARNESS_SERVE_TOKEN", "from-env-0123456789")]);
        let remote = args(&["--remote", "http://127.0.0.1:7420", "--token-file", file]);
        assert_eq!(
            token(resolve_target(&remote, &env_token)).as_deref(),
            Some("from-file-0123456789")
        );
        let bare = args(&["--remote", "http://127.0.0.1:7420"]);
        assert_eq!(
            token(resolve_target(&bare, &env_token)).as_deref(),
            Some("from-env-0123456789")
        );
        let blank = env(&[
            ("JOB_HARNESS_SERVE_TOKEN", "  "),
            ("JOB_HARNESS_SERVE_TOKEN_FILE", file),
        ]);
        assert_eq!(
            token(resolve_target(&bare, &blank)).as_deref(),
            Some("from-file-0123456789")
        );
    }

    #[test]
    fn an_unreadable_token_file_is_an_error_not_a_missing_token() {
        let missing = args(&["--remote", "http://x", "--token-file", "/nonexistent/token"]);
        let err = resolve_target(&missing, &env(&[])).unwrap_err();
        assert!(err.contains("/nonexistent/token"), "{err}");
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("token");
        std::fs::write(&empty, "\n").unwrap();
        let blank = args(&[
            "--remote",
            "http://x",
            "--token-file",
            empty.to_str().unwrap(),
        ]);
        assert!(
            resolve_target(&blank, &env(&[]))
                .unwrap_err()
                .contains("empty")
        );
    }

    #[test]
    fn saved_settings_open_the_configured_remote_under_xdg_config_home() {
        let dir = tempfile::tempdir().unwrap();
        let token_file = dir.path().join("token");
        std::fs::write(&token_file, "saved-0123456789abcdef").unwrap();
        std::fs::create_dir_all(dir.path().join("job-harness")).unwrap();
        std::fs::write(
            dir.path().join("job-harness/settings.json"),
            serde_json::json!({"schema_version":1,"remote":{"url":"http://127.0.0.1:7421","token_file":token_file}}).to_string(),
        )
        .unwrap();
        let env = env(&[
            ("XDG_CONFIG_HOME", dir.path().to_str().unwrap()),
            ("HOME", "/nonexistent-home"),
        ]);
        match resolve_target(&args(&[]), &env).unwrap() {
            WorkspaceTarget::Remote { url, token } => {
                assert_eq!(url, "http://127.0.0.1:7421");
                assert_eq!(token.as_deref(), Some("saved-0123456789abcdef"));
            }
            other => panic!("expected the saved remote, got {:?}", other.label()),
        }
        assert!(matches!(
            resolve_target(&args(&["--root", "/ledger"]), &env).unwrap(),
            WorkspaceTarget::Local(_)
        ));
    }
}
