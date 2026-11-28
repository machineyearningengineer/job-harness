//! The app's own SSH connection to a server, as T3 Code and Hermes keep theirs: one
//! OpenSSH ControlMaster that this process starts, watches and restarts. The server's
//! Unix sockets (the ledger's, the agent host's) are forwarded to private local sockets
//! over it, and commands run through it without logging in again. OpenSSH brings `~/.ssh/config`, the agent,
//! ProxyJump and known_hosts with it.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::{Duration, Instant},
};

/// What the app shows: Connected, or for a short while Reconnecting, then Offline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Connected,
    Reconnecting,
    Offline,
}

/// Reconnecting for longer than this is Offline (still retried in the background).
const OFFLINE_AFTER: Duration = Duration::from_secs(30);
/// A control command (`-O check`, a forward, a path lookup) that takes longer failed.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Tunnel {
    target: String,
    identity: Option<PathBuf>,
    /// The server sockets to forward, in order; one with a `$` is expanded by the
    /// server's shell once.
    sockets: Mutex<Vec<String>>,
    /// Private (0700) folder for the control socket and the forwarded sockets, short
    /// enough for macOS's 104-byte socket path limit.
    dir: PathBuf,
    state: Mutex<State>,
    wake: Condvar,
}

struct State {
    link: Link,
    since: Instant,
    retry_now: bool,
    error: Option<String>,
}

static TUNNEL: OnceLock<Arc<Tunnel>> = OnceLock::new();

/// The app's tunnel, when it was opened with `--ssh`.
pub fn tunnel() -> Option<&'static Arc<Tunnel>> {
    TUNNEL.get()
}

/// The server user's runtime folder, the one part of a socket path the server's
/// shell expands.
const RUNTIME_DIR: &str = "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}";
/// The `jh service install` ledger socket on the server.
pub const LEDGER_SOCKET: &str = "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/job-harness/serve.sock";
/// The `jh service install-agents` agent host socket on the server.
pub const AGENTS_SOCKET: &str = "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/job-harness-agents.sock";

/// Start the one supervised connection for this process, forwarding each of the
/// server's `sockets` to [`Tunnel::local_socket`] of the same index.
pub fn open(
    target: String,
    identity: Option<PathBuf>,
    sockets: Vec<String>,
) -> std::io::Result<&'static Arc<Tunnel>> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("jh-{}", std::process::id()));
    private_dir(&dir)?;
    let tunnel = Arc::new(Tunnel {
        target,
        identity,
        sockets: Mutex::new(sockets),
        dir,
        state: Mutex::new(State {
            link: Link::Reconnecting,
            since: Instant::now(),
            retry_now: false,
            error: None,
        }),
        wake: Condvar::new(),
    });
    let tunnel = TUNNEL.get_or_init(|| tunnel);
    let supervised = tunnel.clone();
    std::thread::spawn(move || supervised.supervise());
    Ok(tunnel)
}

/// Make `dir` 0700 in one step, or fail: never use one someone else prepared. One left
/// by an earlier process with this pid is ours to replace.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    if std::fs::remove_dir_all(dir).is_err() && std::fs::symlink_metadata(dir).is_ok() {
        return Err(std::io::Error::other(format!(
            "{} exists and is not ours to replace",
            dir.display()
        )));
    }
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Reconnecting for longer than [`OFFLINE_AFTER`] shows as Offline; it is still
/// retried until a connection succeeds.
fn shown(link: Link, down_for: Duration) -> Link {
    if link == Link::Reconnecting && down_for > OFFLINE_AFTER {
        Link::Offline
    } else {
        link
    }
}

/// Exponential backoff from 1s to 60s, each wait jittered down to half, as T3 Code's
/// supervisor.
pub fn backoff(attempt: u32, random: u128) -> Duration {
    let base = 1000u64.saturating_mul(1 << attempt.min(6)).min(60_000);
    let jitter = (random % 1001) as u64; // 0..=1000 per mille
    Duration::from_millis(base / 2 + base / 2 * jitter / 1000)
}

impl Tunnel {
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The link as the UI shows it, and for how long it has been up or down.
    pub fn link(&self) -> (Link, Duration) {
        let state = self.state.lock().unwrap();
        let how_long = state.since.elapsed();
        (shown(state.link, how_long), how_long)
    }

    pub fn last_error(&self) -> Option<String> {
        self.state.lock().unwrap().error.clone()
    }

    /// Try again now instead of waiting out the backoff.
    pub fn retry(&self) {
        self.state.lock().unwrap().retry_now = true;
        self.wake.notify_all();
    }

    /// The local end of the server socket given at `index` to [`open`].
    pub fn local_socket(&self, index: usize) -> PathBuf {
        self.dir.join(format!("{index}.sock"))
    }

    fn control(&self) -> PathBuf {
        self.dir.join("m")
    }

    /// `ssh` with the options every call shares, before the destination.
    fn ssh(&self) -> Command {
        let mut command = Command::new("ssh");
        command.arg("-S").arg(self.control());
        command.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
        if let Some(identity) = &self.identity {
            command
                .args(["-o", "IdentitiesOnly=yes", "-i"])
                .arg(identity);
        }
        command.stdin(Stdio::null());
        command
    }

    /// A command run on the server through the master: `ssh ... TARGET -- WORDS`.
    fn exec(&self, remote: &str) -> Command {
        let mut command = self.ssh();
        command.args(["-T", &self.target, "--", remote]);
        command
    }

    /// Forward `local` (removed first if stale) to the server's Unix socket.
    fn forward(&self, local: &Path, remote: &str) -> Result<(), String> {
        let _ = std::fs::remove_file(local);
        let mut spec = OsString::from(local);
        spec.push(":");
        spec.push(remote);
        let mut command = self.ssh();
        command
            .args(["-O", "forward", "-L"])
            .arg(spec)
            .arg(&self.target);
        quiet(&mut command).map(|_| ())
    }

    fn check(&self) -> bool {
        let mut command = self.ssh();
        command.args(["-O", "check", &self.target]);
        quiet(&mut command).is_ok()
    }

    /// Where the master writes its messages; a pipe nobody reads would fill and stop it.
    fn master_log(&self) -> PathBuf {
        self.dir.join("ssh.log")
    }

    /// Stop the master this process started, e.g. when the app quits.
    pub fn shutdown(&self) {
        let mut command = self.ssh();
        command.args(["-O", "exit", &self.target]);
        let _ = quiet(&mut command);
        let _ = std::fs::remove_dir_all(&self.dir);
    }

    fn start_master(&self) -> Result<Child, String> {
        let _ = std::fs::remove_file(self.control());
        let mut command = self.ssh();
        command.args(["-M", "-N", "-o", "ControlPersist=no"]);
        command.args([
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
        ]);
        command.args([
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "StreamLocalBindUnlink=yes",
        ]);
        command.arg(&self.target);
        let log = std::fs::File::create(self.master_log())
            .map_err(|err| format!("Could not write {}: {err}", self.master_log().display()))?;
        let mut child = command
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .map_err(|err| format!("Could not run ssh: {err}"))?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                let text = std::fs::read_to_string(self.master_log()).unwrap_or_default();
                let text = text.trim();
                return Err(if text.is_empty() {
                    format!("ssh {} exited with {status}", self.target)
                } else {
                    text.to_owned()
                });
            }
            if self.control().exists() && self.check() {
                return Ok(child);
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("ssh {} did not connect in time", self.target));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The server path of socket `index`, expanded by the server's shell if needed.
    fn remote_socket(&self, index: usize) -> Result<String, String> {
        let path = self.sockets.lock().unwrap()[index].clone();
        let Some(printf) = expand_command(&path) else {
            return Ok(path);
        };
        let output = quiet(&mut self.exec(&printf));
        let expanded = String::from_utf8_lossy(&output.unwrap_or_default())
            .trim()
            .to_owned();
        if expanded.is_empty() {
            return Err(format!(
                "Could not find {path} on the server; pass its path."
            ));
        }
        self.sockets.lock().unwrap()[index] = expanded.clone();
        Ok(expanded)
    }

    fn connect(&self) -> Result<Child, String> {
        let mut master = self.start_master()?;
        let count = self.sockets.lock().unwrap().len();
        let forwarded = (0..count).try_for_each(|index| {
            let remote = self.remote_socket(index)?;
            self.forward(&self.local_socket(index), &remote)
        });
        if let Err(err) = forwarded {
            let _ = master.kill();
            let _ = master.wait();
            return Err(err);
        }
        Ok(master)
    }

    fn set(&self, link: Link, error: Option<String>) {
        let mut state = self.state.lock().unwrap();
        if state.link != link {
            state.since = Instant::now();
        }
        state.link = link;
        state.error = error;
    }

    /// Connect, watch, and reconnect with backoff, forever.
    fn supervise(self: Arc<Self>) {
        let mut attempt = 0u32;
        loop {
            match self.connect() {
                Ok(mut master) => {
                    attempt = 0;
                    self.set(Link::Connected, None);
                    self.watch(&mut master);
                    let _ = master.kill();
                    let _ = master.wait();
                    self.set(Link::Reconnecting, None);
                }
                Err(err) => {
                    self.set(Link::Reconnecting, Some(err));
                    let delay = backoff(attempt, uuid::Uuid::new_v4().as_u128());
                    attempt = attempt.saturating_add(1);
                    let state = self.state.lock().unwrap();
                    let (mut state, _) = self
                        .wake
                        .wait_timeout_while(state, delay, |s| !s.retry_now)
                        .unwrap();
                    state.retry_now = false;
                }
            }
        }
    }

    /// Return when the master exits or stops answering `-O check`.
    fn watch(&self, master: &mut Child) {
        let mut last_check = Instant::now();
        loop {
            if !matches!(master.try_wait(), Ok(None)) {
                return;
            }
            if last_check.elapsed() > Duration::from_secs(15) {
                if !self.check() {
                    return;
                }
                last_check = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

/// The server command that prints `path` with its leading [`RUNTIME_DIR`] expanded,
/// the rest single-quoted so the shell takes it as written; `None` if nothing expands.
fn expand_command(path: &str) -> Option<String> {
    let rest = path.strip_prefix(RUNTIME_DIR)?;
    Some(format!(
        "printf %s \"{RUNTIME_DIR}\"'{}'",
        rest.replace('\'', "'\\''")
    ))
}

/// Run a short `ssh` control command for at most [`CONTROL_TIMEOUT`]; its stderr is
/// the error.
fn quiet(command: &mut Command) -> Result<Vec<u8>, String> {
    quiet_within(command, CONTROL_TIMEOUT)
}

fn quiet_within(command: &mut Command, timeout: Duration) -> Result<Vec<u8>, String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("Could not run ssh: {err}"))?;
    let deadline = Instant::now() + timeout;
    while matches!(child.try_wait(), Ok(None)) {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("ssh did not answer in {} s", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("Could not run ssh: {err}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnecting_shows_as_offline_after_thirty_seconds() {
        let secs = Duration::from_secs;
        assert_eq!(shown(Link::Connected, secs(99)), Link::Connected);
        assert_eq!(shown(Link::Reconnecting, secs(29)), Link::Reconnecting);
        assert_eq!(shown(Link::Reconnecting, secs(31)), Link::Offline);
    }

    #[test]
    fn only_the_runtime_folder_is_expanded_by_the_servers_shell() {
        assert_eq!(expand_command("/run/user/1000/x.sock"), None);
        assert_eq!(expand_command("$HOME/x.sock"), None);
        assert_eq!(
            expand_command(AGENTS_SOCKET).unwrap(),
            r#"printf %s "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"'/job-harness-agents.sock'"#
        );
        assert!(LEDGER_SOCKET.starts_with(RUNTIME_DIR));
    }

    #[cfg(unix)]
    #[test]
    fn the_rest_of_a_socket_path_stays_literal_on_the_server() {
        let odd = format!("{RUNTIME_DIR}/a'$(touch x)\"b");
        let printf = expand_command(&odd)
            .unwrap()
            .replace(RUNTIME_DIR, "${XDG_RUNTIME_DIR:-/r}");
        let out = Command::new("sh")
            .args(["-c", &printf])
            .env_remove("XDG_RUNTIME_DIR")
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), "/r/a'$(touch x)\"b");
    }

    #[cfg(unix)]
    #[test]
    fn a_control_command_that_hangs_is_stopped() {
        let started = Instant::now();
        let mut sleep = Command::new("sleep");
        sleep.arg("30");
        assert!(quiet_within(&mut sleep, Duration::from_millis(300)).is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
        let mut echo = Command::new("echo");
        echo.arg("ok");
        assert_eq!(quiet_within(&mut echo, CONTROL_TIMEOUT).unwrap(), b"ok\n");
    }

    #[cfg(unix)]
    #[test]
    fn the_tunnel_folder_is_made_private_replacing_a_stale_one() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("jh-1");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::fs::write(dir.join("0.sock"), "stale").unwrap();
        private_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        assert!(!dir.join("0.sock").exists());
    }

    #[test]
    fn backoff_doubles_to_a_minute_with_jitter() {
        assert_eq!(backoff(0, 0), Duration::from_millis(500));
        assert_eq!(backoff(0, 1000), Duration::from_millis(1000));
        assert_eq!(backoff(3, 500), Duration::from_millis(6000));
        assert_eq!(backoff(9, 0), Duration::from_millis(30_000));
        assert_eq!(backoff(40, 1000), Duration::from_millis(60_000));
    }
}
