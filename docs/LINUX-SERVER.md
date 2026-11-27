# Run Job Harness on a Linux server

Keep the ledger on an always-on Linux machine, serve it on loopback, back it up to
git every 15 minutes, and open it from the desktop app over SSH. Nothing listens
on a public port.

## Set up (on the server)

The ledger is a git repository (any remote you can push to). Then:

```sh
curl -fsSL https://raw.githubusercontent.com/machineyearningengineer/job-harness/main/scripts/get.sh | sh
git clone <ledger-git-url> "$HOME/ledger"
~/.local/bin/jh --root "$HOME/ledger" service install --port 7420
```

`jh service install` writes three systemd **user** units to `~/.config/systemd/user/`
and enables them:

| Unit | Runs |
|---|---|
| `job-harness.service` | `jh --root <ledger> serve --bind 127.0.0.1:<port>` |
| `job-harness-sync.service` | `jh --root <ledger> sync` |
| `job-harness-sync.timer` | the sync every 15 minutes |

It refuses a ledger this binary cannot read, uses the absolute path of the binary,
and is safe to re-run (to change the port or ledger, run it again). It prints, but
does not run, two root-only follow-ups:

- `sudo loginctl enable-linger <user>` keeps the units running after you log out.
- `sudo chattr -R +i <ledger>/config` makes the ledger's config read-only, even to
  agents running as you (undo with `chattr -R -i`).

Check it: `curl -fsS http://127.0.0.1:7420/v1/health` and
`systemctl --user status job-harness`.

## Sync

`jh sync` checkpoints the SQLite coordinator (`applications/operations/harness.sqlite`)
through Job Harness itself, so no `sqlite3` CLI is needed, then runs `git add -A`,
commits only if something changed, and pushes. It holds the coordinator's read lock
while it does, so a write never lands half-committed. It exits non-zero with the
failing git step if anything goes wrong; see `journalctl --user -u job-harness-sync`.
The commit uses your git identity, or `Job Harness <job-harness@localhost>` if none
is set.

## Open it from your computer

With `jh` and the desktop app installed locally (macOS or Linux):

```sh
jh open --ssh user@server --port 7420            # add --identity ~/.ssh/key if needed
```

This starts `ssh -N -L 127.0.0.1:7420:127.0.0.1:7420 user@server` in the background
unless something already listens on that local port, then launches the desktop app
with `--remote http://127.0.0.1:7420` (on macOS `Job Harness.app`; on Linux
`job-harness-desktop` next to `jh` or on `PATH`).

## Update

Re-run the installer on the server. It replaces the binary in place and, when
`job-harness.service` is running, restarts it:

```sh
curl -fsSL https://raw.githubusercontent.com/machineyearningengineer/job-harness/main/scripts/get.sh | sh
```

Pin a version with `JOB_HARNESS_VERSION=v0.4.0` before `sh`. If a ledger and the
binary disagree on format, every command says which version the ledger needs and
how to install it; there is no automatic conversion. Ledgers that still keep
`applications/operations/state.json` need job-harness 0.2.x.

## Remove

`jh service uninstall` stops and removes the three units. The ledger and its git
history are left as they are.
