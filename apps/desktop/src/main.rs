//! Native GPUI desktop binary for Job Harness.
mod app;
mod chat_bridge;
mod theme;
mod transport;

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
    // Fall back to saved user host setting (~/.config/job-harness/settings.json)
    if let Ok(home) = std::env::var("HOME") {
        let cfg = PathBuf::from(&home).join(".config/job-harness/settings.json");
        if let Ok(bytes) = std::fs::read(&cfg)
            && let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&bytes)
            && let Some(ws) = parsed["workspace"].as_str()
        {
            return WorkspaceTarget::Local(PathBuf::from(ws));
        }
        let fallback = PathBuf::from(&home).join("Career/workspace");
        return WorkspaceTarget::Local(fallback);
    }
    WorkspaceTarget::Local(PathBuf::from("."))
}

fn main() {
    let target = resolve_target();
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1220.0), px(760.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Job Harness".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(14.0), px(14.0))),
                }),
                window_background: WindowBackgroundAppearance::Blurred,
                ..Default::default()
            },
            |window, cx| cx.new(|cx| JobHarnessApp::new(target, window, cx)),
        )
        .expect("open Job Harness GPUI window");
        cx.activate(true);
    });
}
