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

fn resolve_target() -> WorkspaceTarget {
    let args: Vec<String> = std::env::args().collect();
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--remote"
            && let Some(url) = iter.next()
        {
            let token = std::env::var("JOB_HARNESS_SERVE_TOKEN").ok();
            return WorkspaceTarget::Remote {
                url: url.clone(),
                token,
            };
        }
        if arg == "--root"
            && let Some(path) = iter.next()
        {
            return WorkspaceTarget::Local(PathBuf::from(path));
        }
    }
    if let Ok(url) = std::env::var("JOB_HARNESS_REMOTE_URL")
        && !url.trim().is_empty()
    {
        return WorkspaceTarget::Remote {
            url,
            token: std::env::var("JOB_HARNESS_SERVE_TOKEN").ok(),
        };
    }
    if let Ok(root) = std::env::var("JOB_HARNESS_WORKSPACE")
        && !root.trim().is_empty()
    {
        return WorkspaceTarget::Local(PathBuf::from(root));
    }
    if let Ok(home) = std::env::var("HOME") {
        let cfg = PathBuf::from(&home).join(".config/job-harness/settings.json");
        if let Ok(bytes) = std::fs::read(&cfg)
            && let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&bytes)
            && let Some(ws) = parsed["workspace"].as_str()
        {
            return WorkspaceTarget::Local(PathBuf::from(ws));
        }
        return WorkspaceTarget::Local(PathBuf::from(&home).join("Career/workspace"));
    }
    WorkspaceTarget::Local(PathBuf::from("."))
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
    let target = resolve_target();
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
