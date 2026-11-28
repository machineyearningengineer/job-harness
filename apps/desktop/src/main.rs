//! Native GPUI desktop binary for Job Harness.
mod acp;
mod app;
mod chat_bridge;
mod demo_seed;
mod icons;
mod macos_notify;
mod markdown;
mod sankey;
mod ssh;
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
    let remote = |url: &str| {
        Ok(WorkspaceTarget::Remote {
            url: url.to_owned(),
        })
    };
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match (arg.as_str(), iter.next()) {
            ("--remote", Some(url)) => return remote(url),
            ("--root", Some(path)) => return Ok(WorkspaceTarget::Local(PathBuf::from(path))),
            _ => {}
        }
    }
    if let Some(url) = set(env("JOB_HARNESS_REMOTE_URL")) {
        return remote(&url);
    }
    if let Some(root) = set(env("JOB_HARNESS_WORKSPACE")) {
        return Ok(WorkspaceTarget::Local(PathBuf::from(root)));
    }
    let saved = settings(env);
    if let Some(url) = saved["remote"]["url"].as_str() {
        return remote(url);
    }
    if let Some(ws) = saved["workspace"].as_str() {
        return Ok(WorkspaceTarget::Local(PathBuf::from(ws)));
    }
    Ok(WorkspaceTarget::Local(set(env("HOME")).map_or_else(
        || PathBuf::from("."),
        |home| PathBuf::from(home).join("Career/workspace"),
    )))
}

/// With `--ssh` (and no `--remote`), the app opens its own supervised SSH connection
/// and reaches the server's `jh serve` and `jh agent-host` sockets through it.
fn ssh_target(args: &[String]) -> Option<std::io::Result<WorkspaceTarget>> {
    if flag(args, "--remote").is_some() {
        return None;
    }
    let owned = |name: &str| set(flag(args, name).map(str::to_owned));
    let target = owned("--ssh")?;
    Some(
        ssh::open(
            target,
            owned("--identity").map(PathBuf::from),
            vec![
                owned("--socket").unwrap_or_else(|| ssh::LEDGER_SOCKET.into()),
                owned("--agents-socket").unwrap_or_else(|| ssh::AGENTS_SOCKET.into()),
            ],
        )
        .map(|tunnel| {
            acp::open(tunnel.local_socket(1));
            WorkspaceTarget::Remote {
                url: format!("unix:{}", tunnel.local_socket(0).display()),
            }
        }),
    )
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
                "Job Harness desktop\n\nUsage: job-harness-desktop [--root PATH | --ssh USER@HOST [--identity KEY] [--socket PATH] [--agents-socket PATH] | --remote unix:PATH]\n\nWith --ssh, the app keeps its own SSH connection to that server, reaches its jh serve socket (--socket, default the one jh service install makes) and chats with the ACP agents of its jh agent-host (--agents-socket, default the one jh service install-agents makes); jh open passes these. With --remote unix:PATH, it opens a jh serve socket on this computer. Without arguments it opens what `jh configure` saved."
            );
            return;
        }
        _ => {}
    }
    let args: Vec<String> = std::env::args().collect();
    let target = match ssh_target(&args) {
        Some(Ok(target)) => Ok(target),
        Some(Err(err)) => Err(err.to_string()),
        None => resolve_target(&args, &|key| std::env::var(key).ok()),
    };
    let target = match target {
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
        // Stop the SSH master this process started.
        cx.on_app_quit(|_| async {
            if let Some(tunnel) = ssh::tunnel() {
                tunnel.shutdown();
            }
        })
        .detach();
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

    #[test]
    fn saved_settings_open_the_configured_remote_under_xdg_config_home() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("job-harness")).unwrap();
        std::fs::write(
            dir.path().join("job-harness/settings.json"),
            serde_json::json!({"schema_version":1,"remote":{"url":"unix:/run/jh/serve.sock"}})
                .to_string(),
        )
        .unwrap();
        let env = env(&[
            ("XDG_CONFIG_HOME", dir.path().to_str().unwrap()),
            ("HOME", "/nonexistent-home"),
        ]);
        match resolve_target(&args(&[]), &env).unwrap() {
            WorkspaceTarget::Remote { url } => assert_eq!(url, "unix:/run/jh/serve.sock"),
            other => panic!("expected the saved remote, got {:?}", other.label()),
        }
        assert!(matches!(
            resolve_target(&args(&["--root", "/ledger"]), &env).unwrap(),
            WorkspaceTarget::Local(_)
        ));
    }
}
