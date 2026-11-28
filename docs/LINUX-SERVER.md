# Run Job Harness on a Linux server

Keep the ledger on an always-on Linux machine, serve it on a Unix socket, back it
up to git every 15 minutes, and open it from the desktop app over SSH. Nothing
listens on a network port.

## Set up (on the server)

The ledger is a git repository (any remote you can push to). Then:

```sh
curl -fsSL https://raw.githubusercontent.com/machineyearningengineer/job-harness/main/scripts/get.sh | sh
git clone <ledger-git-url> "$HOME/ledger"
~/.local/bin/jh --root "$HOME/ledger" service install
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
| `job-harness.service` | `jh --root <ledger> serve --bind unix:$XDG_RUNTIME_DIR/job-harness/serve.sock` |
| `job-harness-sync.service` | `jh --root <ledger> sync` |
| `job-harness-sync.timer` | the sync every 15 minutes |

It refuses a ledger this binary cannot read, uses the absolute path of the binary,
and is safe to re-run (to change the ledger, run it again).

On a Unix socket, file permissions are the access control; there is no token.
`jh serve` makes the socket mode 0660 and replaces a stale socket, never another
file. systemd creates the socket's directory (`RuntimeDirectory=job-harness`, mode
0700) inside your runtime directory, which only you can enter, so only you, and SSH
sessions logged in as you, can call it.

It prints, but does not run, two root-only follow-ups:

- `sudo loginctl enable-linger <user>` keeps the units running after you log out.
- `sudo chattr -R +i <ledger>/config` makes the ledger's config read-only, even to
  agents running as you (undo with `chattr -R -i`).

Check it: `curl -fsS --unix-socket "$XDG_RUNTIME_DIR/job-harness/serve.sock" http://localhost/v1/health`
and `systemctl --user status job-harness`.

`jh serve` listens only on a Unix socket (`--bind unix:PATH`); there is no TCP port
and no token. Other computers reach it over SSH, below.

## Agents without file access (remote mode)

An agent that can read or write the ledger's files can edit `harness.sqlite` or
`config/` directly and skip every rule Job Harness enforces. To stop that, run the
ledger and `jh serve` as one user and the agent as another that cannot read the ledger,
and point the agent's `jh` at the server's socket:

```sh
jh configure --remote unix:/run/job-harness/ledger/serve.sock
```

From then on `jh call` and `jh mcp` send every tool call to the server's `/v1/call`
instead of opening files. The agent needs to reach the socket, not the files: put the
server in a system unit with its own group, and add the agent's user (and the user you
SSH in as) to that group. For a ledger user `ledger`, a group `jh-ledger` and a ledger
at `/srv/ledger`:

```ini
# /etc/systemd/system/jh-serve.service
[Unit]
Description=Job Harness server for one ledger
RequiresMountsFor=/srv/ledger
[Service]
User=ledger
Group=jh-ledger
UMask=0077
RuntimeDirectory=job-harness/ledger
RuntimeDirectoryMode=0750
ExecStart=/usr/local/bin/job-harness --root /srv/ledger serve --bind unix:/run/job-harness/ledger/serve.sock
Restart=on-failure
RestartSec=10
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/srv/ledger
[Install]
WantedBy=multi-user.target
```

```sh
sudo groupadd --system jh-ledger
sudo usermod -aG jh-ledger agent
sudo systemctl enable --now jh-serve.service
```

The directory is `ledger:jh-ledger` mode 0750 and the socket 0660, so members of
`jh-ledger` can call the server and nobody else can. `UMask=0077` keeps the files the
server writes readable by the ledger user alone; keep the ledger's own directory
private (mode 0700) too, so the group reaches the socket and nothing else. Anyone in
the group has the server's full tool access.

Use the
environment instead of host settings with `JOB_HARNESS_REMOTE_URL`. An explicit `--root` still opens a local ledger. Paths in
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
jh open --ssh user@server
```

Add `--identity ~/.ssh/key` if needed, and `--socket PATH` when the ledger's socket
is not the `jh service install` one (for example `/run/job-harness/ledger/serve.sock`).
`jh open` only starts the desktop app (`job-harness-desktop` next to `jh`, then
`/Applications/Job Harness.app`, then `PATH`) with these arguments.

The app keeps its own SSH connection, as T3 Code and Hermes do: one OpenSSH
ControlMaster per window (`ServerAliveInterval=15`, `ServerAliveCountMax=3`,
`BatchMode=yes`, `ExitOnForwardFailure=yes`) with its control socket in a private
0700 folder, and forwards local Unix sockets in that folder to the server's ledger
and agent host sockets with `ssh -O forward`. Nothing listens on a local TCP port, so other users of your
computer cannot reach it. Your `~/.ssh/config`, agent, ProxyJump and known_hosts apply.

The app checks the connection with `ssh -O check` and restarts the master when it
dies, with backoff from 1 to 60 seconds:

- **Connected:** the board is live.
- **Reconnecting:** after about 5 seconds a "Reconnecting…" banner appears. The board
  shows what it last loaded, read-only.
- **Offline:** after 30 seconds the banner says so and offers **Retry**. Changes and
  chat sends are disabled, and nothing is queued for later. The app keeps retrying.

When the connection is back, the board reloads. Quitting the app stops its master.

## Chat with an agent on the server

The app's chat talks to agents **on the server**, where they and their settings
live, through `jh agent-host`. It speaks the
[Agent Client Protocol](https://agentclientprotocol.com) (ACP: JSON-RPC over
stdio), so any ACP agent works: `hermes acp`, `gemini --experimental-acp`, the ACP
adapters for Claude Code and Codex, or your own. Run it as the agent's user:

```sh
jh service install-agents --agent hermes="hermes acp"
```

That writes and starts `job-harness-agents.service`, a systemd user unit running
`jh agent-host --socket $XDG_RUNTIME_DIR/job-harness-agents.sock --agent NAME=COMMAND`
(repeat `--agent` for more; each command runs with `sh -lc`). The socket is mode
0600 in your runtime directory, so only you and SSH sessions logged in as you can
use it. The app forwards it over the same SSH connection as the ledger
(`--agents-socket PATH` for another one) and chats with the first agent that starts.

The host, not the app, owns each turn, as T3 Code's server does:

- A turn goes on while the app is offline or closed. When the app reconnects it
  re-attaches from the last event it showed, so nothing is lost or repeated.
- Each session keeps its last 1000 events (at most 4 MB) for this. An app away for
  longer is told part of the reply is gone, and can reopen the session from the list.
- If the host itself restarts, a turn in flight is lost. Agents that support
  `session/load` (Hermes does) keep the conversation, so you can reopen it.
- A permission request from the agent shows in the chat panel with its options. One
  nobody answers is cancelled after 10 minutes; the turn goes on.
- An agent with no app attached and nothing to do for 30 minutes is stopped, and
  started again on the next message.

What chat offers comes from each agent's `initialize` answer: images when it accepts
image prompts, the session list when it can list and load sessions, fast mode when
a session config option offers `fast`, and models from its model option. The host
declines the client file-system and terminal capabilities: the agent uses its own
tools on the server. With no agent host on the server, the board works as usual and
the chat panel says how to start one.

### The protocol (for other clients and agents)

Any ACP agent on stdio can be added with `--agent`. A client talks to the host with
one JSON object per line on the socket:

| Request | Answer |
|---|---|
| `{"id":1,"op":"agents"}` | `{"id":1,"result":{"agents":[{"name","init"}]}}`, `init` being the agent's `initialize` result (or `"error"`) |
| `{"id":2,"op":"call","agent","method","params"}` | the ACP result or error, e.g. for `session/new`, `session/prompt`, `session/list`, `session/load`, `session/set_config_option` |
| `{"op":"notify","agent","method","params"}` | none; e.g. `session/cancel` |
| `{"op":"respond","agent","request","result"}` | none; answers `session/request_permission` (the first answer wins) |
| `{"id":3,"op":"attach","agent","session","after":N}` | `{"id":3,"result":{"gap","latest"}}`, then each event after `N` (all new ones without `after`): `{"agent","session","seq","msg"}` for the agent's `session/update` and permission requests, `{…,"resolved"}` when one is answered, and `{…,"end"}` with the prompt's result when a turn ends |

The Hermes gateway's own WebSocket protocol could be added later as another way in;
ACP covers chat today.

## Update

Re-run the installer on the server. It replaces the binary in place and, when
`job-harness.service` is running, restarts it:

```sh
curl -fsSL https://raw.githubusercontent.com/machineyearningengineer/job-harness/main/scripts/get.sh | sh
```

Pin a version with `JOB_HARNESS_VERSION=v0.5.0` before `sh`. If a ledger and the
binary disagree on format, every command says which version the ledger needs and
how to install it; there is no automatic conversion. Ledgers that still keep
`applications/operations/state.json` need job-harness 0.2.x.

## Remove

`jh service uninstall` stops and removes the units, including the agent host's. The ledger and its git
history are left as they are.
