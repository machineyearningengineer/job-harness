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

/// Checkpoint the SQLite WAL and commit any change under the exclusive coordinator
/// lock, so no writer can change the ledger between fold and commit; push after
/// releasing it.
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
    let changed = {
        let _lock = WorkspaceLock::acquire(&root, true)?;
        state::checkpoint(&root)?;
        run("sync_failed", &mut git(&["add", "-A"]))?;
        let changed = !git(&["diff", "--cached", "--quiet"])
            .status()
            .map_err(|err| fail("sync_failed", err.to_string()))?
            .success();
        if changed {
            let mut commit = git(&[]);
            // Keep the owner's identity; fill in only the part that is not configured.
            for (key, fallback) in [
                ("user.name", "Job Harness"),
                ("user.email", "job-harness@localhost"),
            ] {
                if run("sync_failed", &mut git(&["config", key])).is_err() {
                    commit.args(["-c", &format!("{key}={fallback}")]);
                }
            }
            run(
                "sync_failed",
                commit.args(["commit", "-q", "-m", "Sync ledger"]),
            )?;
        }
        changed
    };
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

pub fn units(binary: &Path, root: &Path, port: u16, token: &Path) -> [(&'static str, String); 3] {
    let (bin, root, token) = (unit_word(binary), unit_word(root), unit_word(token));
    [
        (
            UNITS[0],
            format!(
                "[Unit]\nDescription=Job Harness ledger server on 127.0.0.1:{port}\n\n[Service]\nEnvironment=JOB_HARNESS_SERVE_TOKEN_FILE={token}\nExecStart={bin} --root {root} serve --bind 127.0.0.1:{port}\nRestart=on-failure\nRestartSec=10\n\n[Install]\nWantedBy=default.target\n"
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

fn config_dir() -> anyhow::Result<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| fail("service_failed", "HOME is not set"))
}
/// Keep an existing token so clients stay configured; otherwise write a new owner-only one.
fn ensure_token(path: &Path) -> anyhow::Result<()> {
    if path.is_file() {
        return Ok(());
    }
    use std::io::{Read, Write};
    let mut bytes = [0u8; 24];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)?
        .write_all(format!("{token}\n").as_bytes())?;
    Ok(())
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
    let dir = config_dir()?.join("systemd/user");
    match action {
        ServiceCommand::Install { port } => {
            let root = root()?.canonicalize()?;
            // Refuse a ledger this binary cannot serve before anything is written.
            state::load_for_read(Engine::open(&root)?.root())?;
            let binary = std::env::current_exe()?.canonicalize()?;
            let token = config_dir()?.join("job-harness/serve-token");
            ensure_token(&token)?;
            std::fs::create_dir_all(&dir)?;
            for (name, text) in units(&binary, &root, port, &token) {
                std::fs::write(dir.join(name), text)?;
            }
            systemctl(&["daemon-reload"])?;
            systemctl(&["enable", UNITS[0], UNITS[2]])?;
            // Restart so a re-run picks up a new binary, port or ledger.
            systemctl(&["restart", UNITS[0], UNITS[2]])?;
            let user = std::env::var("USER").unwrap_or_else(|_| "$USER".into());
            Ok(json!({
                "units": dir, "workspace": root, "url": format!("http://127.0.0.1:{port}"), "token_file": token,
                "next": [
                    format!("Check: curl -fsS -H \"Authorization: Bearer $(cat {})\" http://127.0.0.1:{port}/v1/health", token.display()),
                    format!("Point agents on this server at it, without file access: jh configure --remote http://127.0.0.1:{port} --token-file {}", token.display()),
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
/// The token reaches the app as a file path, never as an argument or a logged value.
/// Which Hermes on the server the desktop app's chat runs.
pub struct Hermes<'a> {
    pub profile: Option<&'a str>,
    pub path: Option<&'a str>,
}

/// Desktop arguments after `--remote URL`: the token file and, so chat runs on
/// the server, the SSH target, identity and Hermes.
fn app_args(
    ssh: &str,
    identity: Option<&Path>,
    token_file: Option<&Path>,
    hermes: &Hermes,
) -> anyhow::Result<Vec<std::ffi::OsString>> {
    let mut args: Vec<std::ffi::OsString> = vec![];
    if let Some(file) = token_file {
        args.extend(["--token-file".into(), file.canonicalize()?.into()]);
    }
    args.extend(["--ssh".into(), ssh.into()]);
    if let Some(identity) = identity {
        args.extend(["--identity".into(), std::path::absolute(identity)?.into()]);
    }
    if let Some(profile) = hermes.profile {
        args.extend(["--hermes-profile".into(), profile.into()]);
    }
    if let Some(path) = hermes.path {
        args.extend(["--hermes-path".into(), path.into()]);
    }
    Ok(args)
}

pub fn open(
    ssh: &str,
    port: u16,
    identity: Option<&Path>,
    token_file: Option<&Path>,
    hermes: Hermes,
) -> anyhow::Result<Value> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let tunnel = !listening(addr);
    if tunnel {
        let mut command = Command::new("ssh");
        command.args(["-f", "-N", "-o", "ExitOnForwardFailure=yes", "-L"]);
        command.arg(format!("127.0.0.1:{port}:127.0.0.1:{port}"));
        if let Some(identity) = identity {
            command
                .args(["-o", "IdentitiesOnly=yes", "-i"])
                .arg(identity);
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
    // Run the desktop binary itself, so it inherits this environment (such as
    // JOB_HARNESS_SERVE_TOKEN) and two servers can be open at once. Prefer one
    // next to this binary, then the macOS app bundle, then PATH.
    let sibling = std::env::current_exe()?.with_file_name("job-harness-desktop");
    let bundle = PathBuf::from("/Applications/Job Harness.app/Contents/MacOS/job-harness-desktop");
    let binary = [sibling, bundle]
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("job-harness-desktop"));
    let mut app = Command::new(binary);
    app.args(["--remote", &url]);
    app.args(app_args(ssh, identity, token_file, &hermes)?);
    app.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if app.spawn().is_err() {
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
            Path::new("/home/u/.config/job-harness/serve-token"),
        );
        assert!(serve.1.contains(
            "Environment=JOB_HARNESS_SERVE_TOKEN_FILE=\"/home/u/.config/job-harness/serve-token\"\n"
        ));
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

    /// A pre-push hook waits until the test takes the exclusive lock, which only
    /// succeeds if sync released it before pushing.
    #[cfg(unix)]
    #[test]
    fn sync_releases_the_lock_before_pushing() {
        use std::os::unix::fs::PermissionsExt;
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
        let (pushing, release) = (t.path().join("pushing"), t.path().join("release"));
        let hook = ledger.join(".git/hooks/pre-push");
        std::fs::write(
            &hook,
            format!(
                "#!/bin/sh\ntouch '{}'\nwhile [ ! -f '{}' ]; do sleep 0.1; done\n",
                pushing.display(),
                release.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

        let syncing = std::thread::spawn({
            let ledger = ledger.clone();
            move || sync(&ledger).map(|_| ()).map_err(|err| err.to_string())
        });
        while !pushing.exists() {
            std::thread::sleep(Duration::from_millis(50));
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn({
            let ledger = ledger.clone();
            move || {
                let lock = WorkspaceLock::acquire(&ledger, true);
                let _ = sender.send(lock.is_ok());
            }
        });
        let locked = receiver.recv_timeout(Duration::from_secs(10));
        std::fs::write(&release, b"").unwrap();
        assert_eq!(
            locked,
            Ok(true),
            "sync held the ledger lock during git push"
        );
        syncing.join().unwrap().unwrap();
    }

    #[test]
    fn open_passes_the_server_and_its_hermes_to_the_app() {
        let none = Hermes {
            profile: None,
            path: None,
        };
        assert_eq!(
            app_args("bot@server", None, None, &none).unwrap(),
            ["--ssh", "bot@server"]
        );
        let named = Hermes {
            profile: Some("bot"),
            path: Some("~/.local/bin/hermes"),
        };
        let id = std::path::absolute("/keys/id").unwrap();
        let args = app_args("bot@server", Some(&id), None, &named).unwrap();
        assert_eq!(
            args,
            [
                "--ssh",
                "bot@server",
                "--identity",
                id.to_str().unwrap(),
                "--hermes-profile",
                "bot",
                "--hermes-path",
                "~/.local/bin/hermes"
            ]
        );
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
