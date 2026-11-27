//! The app's own SSH connection to a server, as T3 Code and Hermes keep theirs: one
//! OpenSSH ControlMaster that this process starts, watches and restarts. The ledger's
//! Unix socket is forwarded to a private local socket over it, and commands run
//! through it without logging in again. OpenSSH brings `~/.ssh/config`, the agent,
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

pub struct Tunnel {
    target: String,
    identity: Option<PathBuf>,
    /// The server's `jh serve` socket; `None` until asked, for the `jh service install` one.
    remote_socket: Mutex<Option<String>>,
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

/// Start the one supervised connection for this process.
pub fn open(
    target: String,
    identity: Option<PathBuf>,
    remote_socket: Option<String>,
) -> std::io::Result<&'static Arc<Tunnel>> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("jh-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let tunnel = Arc::new(Tunnel {
        target,
        identity,
        remote_socket: Mutex::new(remote_socket),
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

    /// The forwarded ledger socket, as a `unix:` URL for the app's HTTP client.
    pub fn ledger_url(&self) -> String {
        format!("unix:{}", self.dir.join("l.sock").display())
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
    pub fn exec(&self, remote: &str) -> Command {
        let mut command = self.ssh();
        command.args(["-T", &self.target, "--", remote]);
        command
    }

    /// Forward `local` (removed first if stale) to the server's Unix socket or
    /// `127.0.0.1:PORT`.
    pub fn forward(&self, local: &Path, remote: &str) -> Result<(), String> {
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
        let mut child = command
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("Could not run ssh: {err}"))?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                let mut text = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut stderr, &mut text);
                }
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

    /// The server socket to forward: as given, else the `jh service install` one.
    fn remote_socket(&self) -> Result<String, String> {
        if let Some(path) = self.remote_socket.lock().unwrap().clone() {
            return Ok(path);
        }
        let output = self
            .exec(r#"printf %s "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/job-harness/serve.sock""#)
            .output()
            .map_err(|err| err.to_string())?;
        let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !output.status.success() || path.is_empty() {
            return Err("Could not find the server's socket; pass --socket PATH.".into());
        }
        *self.remote_socket.lock().unwrap() = Some(path.clone());
        Ok(path)
    }

    fn connect(&self) -> Result<Child, String> {
        let mut master = self.start_master()?;
        let forwarded = self
            .remote_socket()
            .and_then(|remote| self.forward(&self.dir.join("l.sock"), &remote));
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

/// Run a short `ssh` control command; its stderr is the error.
fn quiet(command: &mut Command) -> Result<Vec<u8>, String> {
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
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
    fn backoff_doubles_to_a_minute_with_jitter() {
        assert_eq!(backoff(0, 0), Duration::from_millis(500));
        assert_eq!(backoff(0, 1000), Duration::from_millis(1000));
        assert_eq!(backoff(3, 500), Duration::from_millis(6000));
        assert_eq!(backoff(9, 0), Duration::from_millis(30_000));
        assert_eq!(backoff(40, 1000), Duration::from_millis(60_000));
    }
}
