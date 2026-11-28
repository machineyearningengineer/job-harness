# Changelog

## 0.5.1 — Chat and the SSH link no longer hang

- **Every chat turn ends:** `jh agent-host` logs a turn's end even when its prompt fails before reaching the agent (one that cannot start, for example). Each end names its turn, so a cancelled turn's late end no longer ends the next one. An attach that resumes past the host's latest event, or from another run of the host (`boot` in the attach answer), reports a gap, so the app stops waiting for a turn that a restart lost. A client that falls too far behind is disconnected and reattaches from its last event, instead of waiting on a feed that had stopped; each client's queue is bounded.
- **Sessions come back after an agent restart:** when an agent was stopped while idle or exited, the host reopens the session a call names (`session/resume`, else `session/load`) before sending it, and does not log the history that reopening replays. Logs of an agent stopped while idle are freed, and attaching to an agent the host does not run is refused.
- **The SSH connection cannot freeze:** the app's `ssh` master writes its messages to a file in its private folder instead of a pipe nobody read, which once full stopped the master; `-O check`, forwards and the socket path lookup give up after 15 s. Agent host requests time out after 150 s and attaching after 15 s. Answering a permission prompt while offline says so and keeps the prompt.
- **Private from the first instant:** `jh serve` and `jh agent-host` create their sockets under a umask, so they are never open to more users than their final mode (0660, 0600) allows, not even before it is set; the units `jh service install` and `install-agents` write set `UMask=0077`. The app's tunnel folder is created 0700 in one step and refused if it exists and is not its own. The server's shell expands only the leading `${XDG_RUNTIME_DIR:-/run/user/$(id -u)}` of a socket path; the rest is taken as written.
- **`get.sh` replaces the binary atomically:** it installs beside the old one and renames over it, so `jh` is never missing during an update.

## 0.5.0 — A server over SSH, chat that survives disconnects

- **Unix socket only:** `jh serve --bind unix:PATH` is the only way to serve a ledger; its file permissions decide who may call. The TCP listener, the bearer token (`JOB_HARNESS_SERVE_TOKEN`, `JOB_HARNESS_SERVE_TOKEN_FILE`), `jh configure --token-file` and the desktop's `--token-file` are gone. `jh service install` serves `$XDG_RUNTIME_DIR/job-harness/serve.sock`. **Breaking:** re-run `jh service install`, and `jh configure --remote unix:PATH` for agents in remote mode.
- **The desktop app keeps its own SSH connection:** `jh open --ssh user@server` only starts the app, which runs one supervised OpenSSH ControlMaster (keepalives, `-O check`, backoff from 1 to 60 s) and forwards the server's sockets to private local ones. When the link drops, a "Reconnecting…" banner appears after about 5 s and the board stays as last seen, read-only; after 30 s it says Offline and offers Retry. Changes and chat sends are disabled meanwhile, and nothing is queued. `--port` and the local TCP tunnel are gone.
- **Chat through `jh agent-host`:** chat on a server talks to any [ACP](https://agentclientprotocol.com) agent (`hermes acp`, `gemini --experimental-acp`, the Claude Code and Codex ACP adapters) kept running by `jh agent-host` as the agent's user (`jh service install-agents --agent NAME=COMMAND`). A turn goes on while the app is offline and is replayed from where the app left off, without loss or repeats. Session list, images, fast mode, model choice and permission prompts follow what each agent supports. `jh open --hermes-profile` and `--hermes-path` are gone; with no agent host on the server the board works and the chat panel says how to start one.

## 0.4.6 — `jh serve` no longer hangs

- **SQLite 3.53.2:** `jh serve` opens the ledger per call, and the bundled SQLite 3.51.1 could deadlock threads opening and closing it at once; every call then hung while `/health` still answered. rusqlite 0.40 bundles 3.53.2, which has the fix (SQLite 3.51.2).

## 0.4.5 — Desktop chat on the server

- **Chat runs where the agent lives:** opened with `jh open --ssh user@server`, the desktop app's chat runs that server's Hermes over SSH (`ssh -T user@server -- hermes chat ...`, prompt on stdin) instead of a local Hermes. New `jh open --hermes-profile NAME` and `--hermes-path PATH`. Over SSH, chat refuses attachments and does not list past sessions or the fast-mode setting, which live on the server.
- **No assumed Hermes profile:** without `JOB_HARNESS_HERMES_PROFILE` the app passes no `-p`, so Hermes uses its own active profile, and reads that profile's models and sessions. A hardcoded profile name that no longer existed made chat fail.
- **`jh open` runs the desktop binary directly** (next to `jh`, then `/Applications/Job Harness.app`), so the app inherits `JOB_HARNESS_SERVE_TOKEN` and two servers can be open at once. With `--identity`, ssh uses only that key.

## 0.4.4 — Remote mode, hardened

- **`jh serve` takes requests up to 8 MiB**, enough for `save_artifact`'s 5 MB; axum's 2 MiB default refused larger saves. In remote mode, a request the server rejects (HTTP 4xx without an envelope, such as 413) now returns a non-retryable `invalid_arguments` instead of `remote_unavailable`.
- **Desktop token and settings:** `--token-file` now wins over `JOB_HARNESS_SERVE_TOKEN`, empty values are ignored, and an unreadable or empty token file stops the app with the reason instead of failing later as `unauthorized`. Without arguments the app opens the remote saved by `jh configure --remote`, and reads settings from `XDG_CONFIG_HOME` when it is set.

## 0.4.3 — Fixes from an outside audit

- **`jh sync` never commits a stale database:** it takes the exclusive coordinator lock for the checkpoint, `git add` and commit, fails with `checkpoint_incomplete` when a reader keeps the WAL from folding in completely, and releases the lock before `git push`. Workspace export uses the same exclusive checkpoint. A missing `user.name` or `user.email` is filled in on its own, keeping whichever the owner set.
- **The version check no longer touches newer ledgers:** `schema_version` is read on a read-only connection first, so a ledger from a newer job-harness is refused before any pragma or table change.
- **`channel` is required** by `prepare_application` and `prepare_packet`; `begin_submission` refuses a packet without one instead of assuming portal, so an email never takes the portal permit or cap. Prepare reservations made without a channel again.
- **Filters match what people write:** `exclude_ats` entries with dots (`myworkdayjobs.com`, `jobs.lever.co`) match that host and its subdomains; `avoid_list` ignores legal suffixes (`Acme Ltd`, `Acme, Inc.`); a `job.salary` needs `min` or `max` (or `"unknown"`); empty `allowed_locations` or `allowed_work_arrangements` mean unset; duplicate roles are found across `Acme—Engineer`, `Acme: Engineer` and `Acme | Engineer`.
- **The workspace pin covers every command:** CLI root selection checks it as well as `Engine::open`, so `jh sync`, pipeline, workspace, cv, source, document and status honour it. A `JOB_HARNESS_WORKSPACE_PIN` that names a missing file now refuses instead of allowing everything.
- **Config validation follows the schema only**, so `disabled_cv_tracks` (written by the desktop app) no longer breaks export. The config docs state that `auto_reply_threads` and `auto_book_calendar` gate no tool.
- **Installers and release:** `get.sh` and `get.ps1` find the latest stable release from the `/releases/latest` redirect instead of the rate-limited API, which could also pick a prerelease. The release's Homebrew step fails on a missing tap file or an unchanged checksum and skips prereleases.
- **Smaller fixes:** `jh demo` no longer suggests the removed `gui` command; `jh configure --remote` names a missing token file; docs fixes.

## 0.4.2 — The desktop board over a server

- **Desktop shows roles in remote mode:** connected to a `jh serve`, the board was always empty because it listed roles with a blank `find_application` query, which the server refuses. It now reads the server's `applications/pipeline.json`, as the local view does.
- **`read_artifact` returns files over 750 KB:** its declared output capped `content_base64` at the default one million characters, so larger files failed with `internal_contract_error` despite the documented 10 MB limit.

## 0.4.1 — Agents that can only ask

- **Remote mode for agents:** `jh configure --remote URL --token-file PATH` (or `JOB_HARNESS_REMOTE_URL` and `JOB_HARNESS_SERVE_TOKEN_FILE`) makes `jh call` and `jh mcp` send every tool call to a `jh serve` instead of opening files, so an agent can run as a user with no access to the ledger. **`jh serve` now always requires a bearer token**, even on loopback. Set `JOB_HARNESS_SERVE_TOKEN_FILE`, or `JOB_HARNESS_SERVE_TOKEN`; `jh service install` creates one. The desktop app and `jh open` take `--token-file`. New `save_artifact` and `read_artifact` let such an agent hand over a job ad, answers or screenshot, and fetch a frozen CV to upload. Both work on immutable content-addressed files only.
- **Job terms checked at preparation:** `prepare_packet` and `prepare_application` need `job` (salary, location, work arrangement, office days per week) read from the saved ad, with `"unknown"` as an explicit value; a missing field returns `job_fields_missing`. Known values below `discovery.min_base_salary` or outside the new optional `allowed_locations`, `allowed_work_arrangements` and `max_office_days_per_week` return `excluded_by_filters`, at preparation, reservation and begin. An unknown salary passes and is noted in the packet's `job.unchecked`. Reservations made before this release must be prepared again; closed records are not rechecked.

## 0.4.0 — Native desktop, Linux server & first release since 0.2.0

- **Native GPUI desktop app and headless `jh serve`:** replaces the web/Tauri GUI; the desktop app opens a local folder or a remote `jh serve` (`--remote URL`).
- **Linux server:** `jh service install|uninstall` (systemd user units for `jh serve` plus a 15-minute `jh sync`), `jh sync` (SQLite checkpoint, commit if changed, push) and `jh open --ssh` (SSH tunnel + desktop app, titled with the server's `folder · user@host`). Guide: `docs/LINUX-SERVER.md`.
- **Version check:** ledgers in the 0.2.x `state.json` format, or with a newer coordinator schema, fail with the version they need instead of a bare error; `jh serve` refuses them at startup. Re-running `get.sh` updates and restarts a running service.
- **Release fixes:** dispatching the release without confirmation now fails instead of silently skipping; Linux desktop (`libc` pin for `xattr`), Windows and repository-audit build failures fixed. v0.3.0 was tagged but never released.
- **Safety rules enforced in code:** `begin_submission` and `begin_archive` now refuse (`permission_denied`) unless the matching config permission is `true`, as the fail-closed defaults always promised. Preparation, reservation and begin refuse excluded ATS hosts and avoided employers (`excluded_by_filters`) and a second application to the same employer and role through another URL (`duplicate_blocked`). An optional host pin (`/etc/job-harness/allowed-workspaces`) refuses every other workspace (`workspace_not_pinned`).

## 0.3.0 — SQLite WAL, derived attention & 1-line installs

- **SQLite WAL coordinator (`harness.sqlite`):** Replaced `state.json` with normalized SQLite WAL tables, indexes, and delta writes. Read-only queries use `idx_events_tool` instead of loading full event history.
- **Deterministic derived primitives:** Added `bootstrap.data.daily_budget` (total + per-channel daily caps), `review_queue` urgency tiers (`urgent_now` / `upcoming` / `background`), `needs_attention_now`, `next_upcoming_interview`, `quiet_applied_count`, and `cv_tracks` gap detection (`track_fit`).
- **Performance:** Cached pipeline regexes and schemas with `LazyLock`/`OnceLock` and removed redundant 18 MB state clones on reads.
- **Permissions, timing & voice defaults:** Added explicit 5-switch fail-closed `permissions`, `daily_channel_limits`, `time_and_reminders` (90-minute default interview prep lead time), `communication` voice/anti-pattern config, and `target_roles` / `cv_tracks`.
- **Cleaner binary & setup:** Removed unused ACP transport (`-640` lines in `Cargo.lock`, smaller binary), fixed `intake::list` ignoring non-note files, added verified 1-line installers (`scripts/get.sh`, `scripts/get.ps1`), and added `docs/AGENT-SETUP.md`.

## 0.2.0 — Native Rust engine

- Native Rust CLI, focused/full stdio MCP server, embedded guides/skills, SQLite exact search, source-preserving note intake, Typst CV builder, local web UI (`jh gui`), and optional desktop/local-model packages.

## 0.1.0 — Workspace foundation

- Initial typed career ledger, deduplication, immutable SHA-256 submission packets, daily caps, and focused agent skills.
