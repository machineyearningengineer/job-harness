<div align="center">
  <img src="docs/assets/icon.svg" width="76" height="76" alt="Job Harness">
  <h1>Job Harness</h1>
  <p><strong>The local-first career state, safety &amp; workflow engine for any AI agent.</strong><br>
  Hand over results, not tasks. Keep your data. Swap agents anytime.</p>
  <p>
    <img alt="Local first" src="https://img.shields.io/badge/local--first-4965ae?style=flat-square">
    <img alt="Rust + SQLite WAL" src="https://img.shields.io/badge/core-Rust%20%2B%20SQLite%20WAL-384655?style=flat-square">
    <img alt="MCP + CLI" src="https://img.shields.io/badge/interface-MCP%20%2B%20CLI-2d6a4f?style=flat-square">
    <img alt="Agent agnostic" src="https://img.shields.io/badge/agents-Hermes%20%7C%20Claude%20%7C%20Codex%20%7C%20Cursor-1f2937?style=flat-square">
    <a href="LICENSE"><img alt="MIT license" src="https://img.shields.io/badge/license-MIT-66717b?style=flat-square"></a>
  </p>
  <p>
    <a href="#quick-start-copy-paste-for-your-agent">Quick Start</a> ·
    <a href="#where-job-harness-fits-in-the-agent-stack">Architecture</a> ·
    <a href="#install-for-your-agent-or-ide">Install</a> ·
    <a href="#the-north-star-loop">North Star Loop</a> ·
    <a href="#defaults--safety-policy">Defaults &amp; Policy</a> ·
    <a href="#documentation">Docs</a>
  </p>
</div>

![Your workspace, any capable agent. Shared tools and private career history stay separate.](docs/assets/hero.svg)

---

## Quick Start (Copy-Paste for Your Agent)

Already using **Hermes, Claude Code, Codex, Cursor, Windsurf, or OpenCode**? Paste this one prompt into your agent:

```text
Install Job Harness (`jh`), initialize a separate private career workspace at ~/Career/workspace (never inside the software repo), wire Job Harness to my agent via `jh connect mcp`, and walk me through the 3-minute onboarding checklist (my CV PDF, target role families, timezone, and core application defaults in applications/approved-answers.md). Keep all external action permissions false until I explicitly approve them.
```

Or initialize directly from your terminal in 10 seconds:

```sh
jh setup --workspace "$HOME/Career/workspace"
jh status
jh connect mcp
```

---

## Where Job Harness Fits in the Agent Stack

Job Harness is **not** another monolithic agent framework — it is the **deterministic state, safety, and tool-contract layer** that makes *any* AI agent safe, fast, and reliable for job searching:

| Layer | Responsibility | Provided by |
|---|---|---|
| **1. State, Invariants & Tool Contracts** | SQLite WAL operational DB (`harness.sqlite`), deduplication, immutable SHA-256 submission packets, atomic total + per-channel daily caps, fail-closed permissions, authoritative clock, typed CLI & stdio MCP | **Job Harness (`jh`)** *(single Rust binary, zero runtime deps)* |
| **2. Private Human Knowledge** | Master CV + child tracks (`content/`), verified career stories (`context/profile/story-bank.md`), voice & anti-patterns (`communication-style.md`), approved answers, and 2-layer company/round briefs (`applications/briefs/`) | **Your private workspace** *(curated Markdown + JSON in Git)* |
| **3. Reasoning & Scheduling** | Reading briefs, scouting, filling forms, drafting replies, running scheduled desks | **Any agent runtime** *(Hermes, Claude Code, Codex, Cursor, OpenCode, or OS cron)* |
| **4. External Connectors** | Browser automation (`browser_exec`, Playwright MCP, Browserbase), pluggable search/ingest (Exa, Parallel, Context.dev, employer boards), mail/calendar (Gmail/Outlook MCP), meeting transcripts (Granola, Wispr Flow) | **Your host’s connectors** *(mapped to abstract capability contracts)* |

Switch models, switch browser drivers, or move from your laptop to a Linux VPS — your career history, packets, and safety rails come with you intact.

---

## Install for Your Agent or IDE

Job Harness ships as a **single native Rust binary (`jh`)** — no Node runtime, no Python virtualenv, no Docker, no background database server, and zero telemetry.

| Surface | Setup |
|---|---|
| **Hermes Agent** *(recommended for 24/7 routines)* | Install `jh` on `PATH`, run `jh connect mcp`, and sync portable routines from `config/career-routines.json` (one Career Bot, scoped least-privilege toolsets per routine). |
| **Claude Code / Codex / OpenCode** | Run `jh connect mcp` and add the printed stdio MCP entry to your agent’s MCP config. |
| **Cursor / Windsurf / VS Code** | Add the stdio MCP command from `jh connect mcp` in MCP settings and open your private workspace folder. |
| **Terminal / Headless VPS** | Download a [release binary](https://github.com/machineyearningengineer/job-harness/releases) or build with `cargo build --release --bin jh`. Run `jh --root "$PWD" call <tool> --input '{...}'`. |
| **Optional Desktop Window** | Install the optional desktop package to view the exact same SQLite WAL workspace visually ([App setup](docs/INSTALL.md)). |

> **Why a native Rust binary instead of `npx`?** `jh` enforces file locks, SQLite WAL transactions, SHA-256 artifact immutability, and strict schema validation in milliseconds with zero network fetches at startup and zero `node_modules` supply-chain surface.

---

## Core Features

- **SQLite WAL Operational Engine (`harness.sqlite`):** Sub-millisecond reads/writes, `PRAGMA journal_mode=WAL` + `busy_timeout=5000`, OS-level shared/exclusive workspace locking, safe for concurrent local agents and GUI reads.
- **Exact Deduplication & Immutable Packets:** Checks normalized URL, exact posting ID, and employer history across all records and attempts. Every application freezes an immutable SHA-256 packet (`packets/<sha256>.json` + exact submitted CV PDF bytes + exact answers + receipt) so you always know **which CV and answers went to which job**.
- **Per-Channel Atomic Daily Caps:** Configure overall `max_daily_submissions` plus separate `daily_channel_limits` (`portal`, `direct_email`, `recruiter_outreach`) enforced atomically at `begin_submission` in your configured IANA timezone.
- **One-Shot Submission Safety:** `prepare_application` $\rightarrow$ `begin_submission` $\rightarrow$ **one external Submit** $\rightarrow$ `finish_submission`. Interrupted or unclear attempts become `outcome_unknown` and permanently block accidental retries until externally reconciled.
- **Pluggable Discovery Ingest:** Configure defaults (`official_employer_ats_boards`, `founder_and_engineering_posts`, `hn_who_is_hiring`) or plug in **any** search/MCP provider (Exa, Parallel, Context.dev, Firecrawl, Cord, Jack & Jill) — `jh` validates the normalized record regardless of how it was found.
- **2-Layer Interview Briefs, Cheatsheets & Transcript Debriefs:** One canonical file per company holds **Layer 1 (Company Brief — product, stage/archetype, posted vs. estimated salary, role fit, AI stance)** + **Layer 2 (Round Brief + integrated 10-line Pre-Call Cheatsheet)**. After each call, verifies live Calendar/Gmail for reschedules, pulls verified matching transcripts from connected note tools (Granola, Wispr Flow, Fireflies, or manual paste), and feeds learnings back into Layer 1.
- **Time Awareness & Anti-Spam Policy:** `jh call bootstrap` injects an authoritative `clock` (`utc`, `local`, `today_local`, `timezone`). Built-in `time_and_reminders` enforces pre-call lead times, async question cutoffs (`no_answer` without freezing independent work), 24h reminder cooldowns, per-stage follow-up thresholds, and a clean give-up notice after `max_reminders_per_item`.
- **Voice Mirroring & Anti-Patterns:** Built-in `communication` defaults mirror your own recent Sent emails and ban AI-slop filler, invented metrics, and guessed founder addresses (SMTP bounces log immediately as `not_submitted`).
- **Master CV + Child Tracks (Typst):** One master `content/baseline.json`, emphasis-only child tracks (e.g. `backend`, `product`, `ai`), deterministic parsing/link/term-coverage checks, and automatic suggestions when repeated roles fall outside existing tracks.

---

## The North Star Loop

**Principle: Hand over results, not tasks.** Your agent handles the repetitive grind autonomously (within your permissions), prepares ready-to-send drafts where you only say *"yes"*, and leaves the genuinely human moments (the interview itself, offer negotiation, missing declarations) to you.

```text
1. Morning Desk (08:00)      Live Gmail + Calendar brief, top 3 priorities, ready-to-send reply drafts
2. Inbox Watch (*/15m)       Deterministic $0-token signal probe; wakes agent only on new urgent threads
3. Job Scout (3x/weekday)    Pluggable ingest (ATS boards, HN, Exa/Parallel/MCP) -> deduplicated `saved` leads
4. Application Desk          2-3 concurrent batched browser sessions -> freeze -> begin -> 1 Submit -> retire tab
   (portal + direct email)   Email verification-code self-heal first; async SMS ask with cutoff as last resort
5. Interview Prep & Debrief  Pre-call 10-line cheatsheet (30m prior) -> verify held -> transcript debrief -> update Layer 1
6. Evening Desk (17:30)      Read-only closeout: receipts vs. unknowns, debrief prompts, tomorrow's priorities
7. Weekly Reconcile (Sun)    Audit `outcome_unknown` vs. live Gmail, check quiet stages/ghosts, detect CV track gaps, optimize WAL
```

---

## Defaults & Safety Policy

Every new workspace initialized by `jh setup` starts **fail-closed** in `config/career-ledger.json`:

- **Permissions (all `false` by default):** `auto_apply_portal`, `auto_send_first_contact_email`, `auto_reply_threads`, `auto_book_calendar`, `auto_archive_mail`.
- **Daily caps (safe starter defaults):** `max_daily_submissions: 3` (`portal: 3`, `direct_email: 0`, `recruiter_outreach: 0`) — scale up (e.g. `25–30/day`) once verified.
- **No silent destructive actions:** Never deletes emails or files; Gmail archival requires a frozen human-selected message list, fresh readback, and one-shot `begin_mail_archive` permit.
- **Credentials stay in your host:** Zero API keys, cookies, passwords, or OTPs are ever stored in the workspace or exported bundles.

---

## Documentation

| Use your workspace | Build & extend |
|---|---|
| [Onboarding guide](resources/guides/job-harness-onboarding.md) · [Career operations](resources/guides/career-operations.md) | [Architecture](docs/ARCHITECTURE.md) · [Tool contracts (v2)](resources/guides/job-harness-contract.md) |
| [Daily review & mail cleanup](resources/guides/daily-review.md) · [CV workflow](docs/CV.md) | [Connect an agent (MCP / CLI)](docs/AGENTS.md) · [Install & platforms](docs/INSTALL.md) |
| [Backup & restore](docs/BUNDLES.md) · [Notes & evidence](docs/INTAKE.md) | [Contributing](CONTRIBUTING.md) · [Development](docs/DEVELOPMENT.md) · [Validation](docs/VALIDATION.md) |

MIT licensed. Your private career workspace always stays outside this software repository.
