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

For a brand-new ledger with no history yet, create it and push once by hand so
`jh sync` has an upstream branch to push to:

```sh
~/.local/bin/jh --root "$HOME/ledger" init
git -C "$HOME/ledger" init -b main
git -C "$HOME/ledger" remote add origin <ledger-git-url>
git -C "$HOME/ledger" add -A && git -C "$HOME/ledger" commit -m "New ledger"
git -C "$HOME/ledger" push -u origin main
```

`jh service install` writes three systemd **user** units to `~/.config/systemd/user/`
and enables them:

| Unit | Runs |
|---|---|
| `job-harness.service` | `jh --root <ledger> serve --bind 127.0.0.1:<port>` |
| `job-harness-sync.service` | `jh --root <ledger> sync` |
| `job-harness-sync.timer` | the sync every 15 minutes |

It refuses a ledger this binary cannot read, uses the absolute path of the binary,
and is safe to re-run (to change the port or ledger, run it again).

`jh serve` always requires a bearer token, even on loopback, so another user on the
server cannot call your port. `install` writes a random one to
`~/.config/job-harness/serve-token` (mode 0600) the first time, keeps it on re-runs,
and points the unit at it with `JOB_HARNESS_SERVE_TOKEN_FILE`. A hand-written unit sets
`JOB_HARNESS_SERVE_TOKEN_FILE` (or `JOB_HARNESS_SERVE_TOKEN`) itself; without one,
`jh serve` refuses to start.

It prints, but does not run, two root-only follow-ups:

- `sudo loginctl enable-linger <user>` keeps the units running after you log out.
- `sudo chattr -R +i <ledger>/config` makes the ledger's config read-only, even to
  agents running as you (undo with `chattr -R -i`).

Check it: `curl -fsS -H "Authorization: Bearer $(cat ~/.config/job-harness/serve-token)" http://127.0.0.1:7420/v1/health`
and `systemctl --user status job-harness`.

## Agents without file access (remote mode)

An agent that can read or write the ledger's files can edit `harness.sqlite` or
`config/` directly and skip every rule Job Harness enforces. To stop that, run the
ledger and `jh serve` as one user and the agent as another that cannot read the ledger,
and point the agent's `jh` at the server:

```sh
jh configure --remote http://127.0.0.1:7420 --token-file /path/to/serve-token
```

The agent user must be able to read that token file. From then on `jh call` and `jh mcp`
send every tool call to the server's `/v1/call` instead of opening files. Use the
environment instead of host settings with `JOB_HARNESS_REMOTE_URL` and
`JOB_HARNESS_SERVE_TOKEN_FILE`. An explicit `--root` still opens a local ledger. Paths in
tool arguments, such as `cv_path` or `vacancy_text_path`, are paths in the server's
ledger. An agent that can't write there saves a job ad, answers file or screenshot with
`save_artifact` (base64, at most 5 MB) and passes the returned path on. It reads a frozen
CV to upload with `read_artifact`. Neither tool can overwrite a file, or reach `config/`
or the coordinator. If the server cannot be reached, the call returns `remote_unavailable`; repeat a
write with the same `idempotency_key` to read its recorded result.

## Sync

`jh sync` checkpoints the SQLite coordinator (`applications/operations/harness.sqlite`)
through Job Harness itself, so no `sqlite3` CLI is needed, then runs `git add -A`,
commits only if something changed, and pushes. It holds the coordinator's exclusive
lock from the checkpoint through the commit, so no write lands between them, and
releases it before the push. If another process (the desktop app, an agent) is
still reading the database so the checkpoint cannot fold everything in, it fails
with `checkpoint_incomplete` instead of committing a stale `harness.sqlite`; the
next run retries. It exits non-zero with the failing step if anything goes wrong;
see `journalctl --user -u job-harness-sync`. The commit uses your git
`user.name` and `user.email`, falling back to `Job Harness` and
`job-harness@localhost` for whichever is not set.

## Open it from your computer

With `jh` and the desktop app installed locally (macOS or Linux):

```sh
jh open --ssh user@server --port 7420 --token-file ~/.config/job-harness/server-token
```

Copy the server's `serve-token` to that local file first (mode 0600). Add
`--identity ~/.ssh/key` if needed. The app reads the token from the file; it never
appears in a command line.

This starts `ssh -N -L 127.0.0.1:7420:127.0.0.1:7420 user@server` in the background
unless something already listens on that local port, then launches the desktop app
with `--remote http://127.0.0.1:7420 --token-file PATH` (on macOS `Job Harness.app`; on Linux
`job-harness-desktop` next to `jh` or on `PATH`).

## Update

Re-run the installer on the server. It replaces the binary in place and, when
`job-harness.service` is running, restarts it:

```sh
curl -fsSL https://raw.githubusercontent.com/machineyearningengineer/job-harness/main/scripts/get.sh | sh
```

Pin a version with `JOB_HARNESS_VERSION=v0.4.4` before `sh`. If a ledger and the
binary disagree on format, every command says which version the ledger needs and
how to install it; there is no automatic conversion. Ledgers that still keep
`applications/operations/state.json` need job-harness 0.2.x.

## Remove

`jh service uninstall` stops and removes the three units. The ledger and its git
history are left as they are.
