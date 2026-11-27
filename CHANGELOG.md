# Changelog

## Unreleased

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
