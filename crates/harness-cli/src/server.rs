//! Run a ledger on a server: `jh sync`, `jh service` (systemd user units) and `jh open`.
use clap::Subcommand;
use job_harness_core::{Engine, HarnessError, state, storage::WorkspaceLock};
use serde_json::{Value, json};
use std::{
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const UNITS: [&str; 3] = [
    "job-harness.service",
    "job-harness-sync.service",
    "job-harness-sync.timer",
];

#[derive(Subcommand)]
pub enum ServiceCommand {
    /// Write and enable systemd user units: `jh serve` on 127.0.0.1:PORT and a 15-minute `jh sync`.
    Install {
        #[arg(long, default_value_t = 7420)]
        port: u16,
    },
    /// Stop, disable and remove the units written by `install`; the ledger is not touched.
    Uninstall,
}

fn fail(code: &str, message: impl Into<String>) -> anyhow::Error {
    HarnessError::new(code, message).into()
}

fn run(code: &str, command: &mut Command) -> anyhow::Result<String> {
    let shown = format!("{command:?}");
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|err| fail(code, format!("Could not run {shown}: {err}")))?;
    if !output.status.success() {
        return Err(fail(
            code,
            format!(
                "{shown} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Checkpoint the SQLite WAL, commit any change and push, holding the coordinator read lock.
pub fn sync(root: &Path) -> anyhow::Result<Value> {
    let root = root.canonicalize()?;
    let git = |args: &[&str]| {
        let mut command = Command::new("git");
        command.arg("-C").arg(&root).args(args);
        command
    };
    run(
        "sync_failed",
        &mut git(&["rev-parse", "--is-inside-work-tree"]),
    )
    .map_err(|_| {
        fail(
            "sync_failed",
            format!(
                "{} is not a git repository; clone the ledger with git first.",
                root.display()
            ),
        )
    })?;
    let _lock = WorkspaceLock::acquire(&root, false)?;
    state::checkpoint(&root)?;
    run("sync_failed", &mut git(&["add", "-A"]))?;
    let changed = !git(&["diff", "--cached", "--quiet"])
        .status()
        .map_err(|err| fail("sync_failed", err.to_string()))?
        .success();
    if changed {
        let mut commit = git(&[]);
        // Keep the owner's identity; fall back only where none is configured.
        if run("sync_failed", &mut git(&["config", "user.email"])).is_err() {
            commit.args([
                "-c",
                "user.name=Job Harness",
                "-c",
                "user.email=job-harness@localhost",
            ]);
        }
        run(
            "sync_failed",
            commit.args(["commit", "-q", "-m", "Sync ledger"]),
        )?;
    }
    run("sync_failed", &mut git(&["push", "-q"]))?;
    let head = run("sync_failed", &mut git(&["rev-parse", "HEAD"]))?;
    Ok(json!({"workspace": root, "committed": changed, "pushed": true, "head": head}))
}

/// systemd `ExecStart` word: quoted, with `%`, `$`, `\` and `"` escaped.
fn unit_word(path: &Path) -> String {
    let text = path.display().to_string();
    let escaped = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    format!("\"{escaped}\"")
}

pub fn units(binary: &Path, root: &Path, port: u16) -> [(&'static str, String); 3] {
    let (bin, root) = (unit_word(binary), unit_word(root));
    [
        (
            UNITS[0],
            format!(
                "[Unit]\nDescription=Job Harness ledger server on 127.0.0.1:{port}\n\n[Service]\nExecStart={bin} --root {root} serve --bind 127.0.0.1:{port}\nRestart=on-failure\nRestartSec=10\n\n[Install]\nWantedBy=default.target\n"
            ),
        ),
        (
            UNITS[1],
            format!(
                "[Unit]\nDescription=Commit and push the Job Harness ledger\n\n[Service]\nType=oneshot\nExecStart={bin} --root {root} sync\n"
            ),
        ),
        (
            UNITS[2],
            "[Unit]\nDescription=Sync the Job Harness ledger every 15 minutes\n\n[Timer]\nOnCalendar=*:0/15\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n".to_owned(),
        ),
    ]
}

fn unit_dir() -> anyhow::Result<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| fail("service_failed", "HOME is not set"))?;
    Ok(config.join("systemd/user"))
}

fn systemctl(args: &[&str]) -> anyhow::Result<String> {
    run(
        "service_failed",
        Command::new("systemctl").arg("--user").args(args),
    )
}

/// `root` is resolved only for `install`, so `uninstall` works without a workspace.
pub fn service(
    root: impl FnOnce() -> anyhow::Result<PathBuf>,
    action: ServiceCommand,
) -> anyhow::Result<Value> {
    if !cfg!(target_os = "linux") {
        return Err(fail(
            "service_failed",
            "jh service manages systemd user units and runs on Linux only.",
        ));
    }
    let dir = unit_dir()?;
    match action {
        ServiceCommand::Install { port } => {
            let root = root()?.canonicalize()?;
            // Refuse a ledger this binary cannot serve before anything is written.
            state::load_for_read(Engine::open(&root)?.root())?;
            let binary = std::env::current_exe()?.canonicalize()?;
            std::fs::create_dir_all(&dir)?;
            for (name, text) in units(&binary, &root, port) {
                std::fs::write(dir.join(name), text)?;
            }
            systemctl(&["daemon-reload"])?;
            systemctl(&["enable", UNITS[0], UNITS[2]])?;
            // Restart so a re-run picks up a new binary, port or ledger.
            systemctl(&["restart", UNITS[0], UNITS[2]])?;
            let user = std::env::var("USER").unwrap_or_else(|_| "$USER".into());
            Ok(json!({
                "units": dir, "workspace": root, "url": format!("http://127.0.0.1:{port}"),
                "next": [
                    format!("Check: curl -fsS http://127.0.0.1:{port}/v1/health"),
                    format!("Keep services running after logout (needs root): sudo loginctl enable-linger {user}"),
                    format!("Optional, make ledger config read-only (needs root; undo with -i): sudo chattr -R +i {}", root.join("config").display()),
                ]
            }))
        }
        ServiceCommand::Uninstall => {
            // Units may already be gone; removal below is what makes a re-run safe.
            let _ = systemctl(&["disable", "--now", UNITS[0], UNITS[2]]);
            let _ = systemctl(&["stop", UNITS[1]]);
            let mut removed = Vec::new();
            for name in UNITS {
                if std::fs::remove_file(dir.join(name)).is_ok() {
                    removed.push(name);
                }
            }
            systemctl(&["daemon-reload"])?;
            Ok(json!({"removed": removed}))
        }
    }
}

fn listening(addr: SocketAddr) -> bool {
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

/// Tunnel 127.0.0.1:PORT to the same port on an SSH host (unless already open) and start the desktop app.
pub fn open(ssh: &str, port: u16, identity: Option<&Path>) -> anyhow::Result<Value> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let tunnel = !listening(addr);
    if tunnel {
        let mut command = Command::new("ssh");
        command.args(["-f", "-N", "-o", "ExitOnForwardFailure=yes", "-L"]);
        command.arg(format!("127.0.0.1:{port}:127.0.0.1:{port}"));
        if let Some(identity) = identity {
            command.arg("-i").arg(identity);
        }
        let status = command
            .arg(ssh)
            .status()
            .map_err(|err| fail("open_failed", format!("Could not run ssh: {err}")))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while status.success() && !listening(addr) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
        if !status.success() || !listening(addr) {
            return Err(fail(
                "open_failed",
                format!(
                    "Could not open an SSH tunnel to {ssh} on port {port}. Check `ssh {ssh}` works and `jh serve` is running there."
                ),
            ));
        }
    }
    let url = format!("http://127.0.0.1:{port}");
    let mut app = if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        command.args(["-a", "Job Harness", "--args", "--remote", &url]);
        command
    } else {
        // Prefer a desktop binary next to this one, then PATH.
        let sibling = std::env::current_exe()?.with_file_name("job-harness-desktop");
        let mut command = Command::new(if sibling.is_file() {
            sibling
        } else {
            PathBuf::from("job-harness-desktop")
        });
        command.args(["--remote", &url]);
        command
    };
    app.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // `open` returns once the app is launched (or not found); the Linux binary keeps running.
    let started = if cfg!(target_os = "macos") {
        app.status().map(|status| status.success())
    } else {
        app.spawn().map(|_| true)
    };
    if !matches!(started, Ok(true)) {
        return Err(fail(
            "open_failed",
            format!(
                "Tunnel is up at {url}, but the desktop app did not start. Install the Job Harness desktop app, or run it with --remote {url}."
            ),
        ));
    }
    Ok(json!({"url": url, "tunnel_started": tunnel}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_quote_paths_and_bind_loopback() {
        let [serve, sync, timer] = units(
            Path::new("/opt/jh bin/job-harness"),
            Path::new("/data/100% $ledger"),
            7555,
        );
        assert_eq!(serve.0, "job-harness.service");
        assert!(serve.1.contains(
            "ExecStart=\"/opt/jh bin/job-harness\" --root \"/data/100%% $$ledger\" serve --bind 127.0.0.1:7555\n"
        ));
        assert!(serve.1.contains("WantedBy=default.target"));
        assert!(sync.1.contains("Type=oneshot\nExecStart=\"/opt/jh bin/job-harness\" --root \"/data/100%% $$ledger\" sync\n"));
        assert_eq!(timer.0, "job-harness-sync.timer");
        assert!(timer.1.contains("OnCalendar=*:0/15\n"));
        assert!(timer.1.contains("WantedBy=timers.target"));
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn sync_checkpoints_commits_only_changes_and_pushes() {
        let t = tempfile::tempdir().unwrap();
        let (remote, ledger) = (t.path().join("remote.git"), t.path().join("ledger"));
        git(
            t.path(),
            &["init", "-q", "--bare", remote.to_str().unwrap()],
        );
        crate::onboarding::demo(&ledger).unwrap();
        git(&ledger, &["init", "-q"]);
        git(
            &ledger,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(
            &ledger,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "start",
            ],
        );
        git(&ledger, &["push", "-q", "-u", "origin", "HEAD"]);

        let first = sync(&ledger).unwrap();
        assert_eq!(first["committed"], true);
        // The WAL was folded into the committed database file.
        let wal = ledger.join("applications/operations/harness.sqlite-wal");
        assert!(!wal.exists() || std::fs::metadata(&wal).unwrap().len() == 0);
        let pushed = run(
            "t",
            Command::new("git")
                .arg("-C")
                .arg(&remote)
                .args(["rev-parse", "HEAD"]),
        )
        .unwrap();
        assert_eq!(first["head"], pushed);
        let listed = run(
            "t",
            Command::new("git").arg("-C").arg(&remote).args([
                "ls-tree",
                "-r",
                "--name-only",
                "HEAD",
            ]),
        )
        .unwrap();
        assert!(listed.lines().any(|path| path == state::DB_REL_PATH));

        let second = sync(&ledger).unwrap();
        assert_eq!(second["committed"], false);
        assert_eq!(second["head"], first["head"]);
    }

    #[test]
    fn sync_outside_git_fails_clearly() {
        let t = tempfile::tempdir().unwrap();
        let err = sync(t.path()).unwrap_err();
        let err = err.downcast_ref::<HarnessError>().unwrap();
        assert_eq!(err.code, "sync_failed");
        assert!(err.message.contains("not a git repository"));
    }
}
