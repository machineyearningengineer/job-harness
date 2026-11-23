<div align="center">
  <img src="docs/assets/icon.svg" width="76" height="76" alt="Job Harness">
  <h1>Job Harness</h1>
  <p><strong>Turn any AI agent into a calm, personal job-search assistant.</strong><br>
  Finds fresh roles, applies safely within your limits, preps every interview, and drafts replies in your voice — while every byte of your career history stays private on your machine.</p>
  <p>
    <a href="https://github.com/machineyearningengineer/job-harness/releases"><img alt="Release" src="https://img.shields.io/badge/release-v0.2.0-4965ae?style=flat-square"></a>
    <img alt="Claude Code" src="https://img.shields.io/badge/Claude_Code-18181b?style=flat-square&logo=anthropic&logoColor=white">
    <img alt="Codex" src="https://img.shields.io/badge/Codex-18181b?style=flat-square&logo=openai&logoColor=white">
    <img alt="Cursor" src="https://img.shields.io/badge/Cursor-18181b?style=flat-square&logo=cursor&logoColor=white">
    <img alt="Hermes" src="https://img.shields.io/badge/Hermes-2d6a4f?style=flat-square">
    <a href="LICENSE"><img alt="MIT License" src="https://img.shields.io/badge/license-MIT-66717b?style=flat-square"></a>
  </p>
  <p>
    <a href="#quick-start">Quick Start</a> ·
    <a href="#why-job-harness">Why</a> ·
    <a href="#safety--control">Safety</a> ·
    <a href="#your-daily-rhythm">Daily Rhythm</a> ·
    <a href="#install">Install</a> ·
    <a href="#for-agents--developers">Docs</a>
  </p>
</div>

<br>

![Your workspace, any capable agent. Shared tools and private career history stay separate.](docs/assets/hero.svg)

---

## Quick Start

### Option 1 — Let your AI agent set it up *(recommended, 60 seconds)*

Using **Claude Code, Codex, Hermes, Cursor, Windsurf, or OpenCode**? Paste this one prompt — your agent handles the install, wiring, and setup:

```text
Install Job Harness (https://github.com/machineyearningengineer/job-harness), create my private career workspace at ~/Career/workspace, connect it to you, and walk me through the 3-minute setup (my CV, target roles, timezone, and core preferences). Keep all automatic sending and applying turned off until I say otherwise.
```

### Option 2 — Terminal

```sh
# Install from source with Cargo (or grab a prebuilt binary from Releases)
cargo install --git https://github.com/machineyearningengineer/job-harness job-harness

# Create your private career workspace outside any repo
job-harness setup --workspace "$HOME/Career/workspace"
job-harness status
```

---

## Why Job Harness

Most job-search tools either spam low-quality applications or hand you another spreadsheet to babysit. **Job Harness hands you finished work, not chores.**

| What you get | How it feels day to day |
|---|---|
| **Calm daily check-ins** | Ask *"what needs my attention today?"* anytime. Get the 1–3 things that actually matter right now — or a clear *"all quiet, go take a breather"* when nothing is urgent. |
| **Ready-to-send replies in your voice** | Drafts follow-ups and recruiter replies matching how *you* actually write. You just say *"yes"* to send. |
| **Quiet role discovery** | Scouts company career pages, founder posts, HN, and any search tools you connect — skipping anything you've already applied to or ruled out. |
| **Zero accidental double-applies** | Records the exact CV PDF and screening answers sent to every single job, enforces your daily limit, and never fires a reckless retry. |
| **Walk into every call prepared** | Builds a plain-English company & round brief plus a 10-line cheatsheet 30 minutes before your interview, then captures a quick debrief afterwards (pulling meeting notes/transcripts if connected). |
| **Truthful CV tracks** | Keeps one verified master CV and suggests focused tracks (e.g. Backend, Product, AI) as your target roles evolve — never inventing metrics or experience. |

---

## Safety & Control

Every new workspace starts with **all automatic external actions turned OFF**. Turn on only what you trust, when you're ready.

- **Runs in the background** *(only once you enable it)* — scouting roles, deduplicating history, watching your inbox, refreshing interview briefs, and bounded portal applications within your daily cap.
- **Waits for a simple "yes"** — recruiter replies drafted in your voice, proposed interview slots checked against your calendar, and inbox cleanup.
- **Always 100% human** — the interviews themselves, offer negotiations, and final decisions.
- **Private & portable** — everything lives locally in your chosen folder with zero cloud lock-in or telemetry. Switch from Cursor to Claude Code or Hermes anytime and keep your entire history intact.

---

## Your Daily Rhythm

```text
08:00       Morning brief     Today's interviews, top priorities & ready-to-send reply drafts
Every 15m   Inbox watch       Silent when nothing's new — alerts only when something is urgent
Daytime     Job scout         Finds fresh matching roles from your chosen sources
Windows     Apply desk        Submits cleanly within your daily cap (paused by default)
Before call Interview prep    Company brief + 10-line cheatsheet 30 mins before your call
17:30       Evening close     Today's results, post-call debrief & tomorrow's preview
Sunday      Weekly tidy       Reconciles loose ends, flags quiet stages & checks CV track fit
```

---

## Install

Job Harness ships as a **single native binary** (`job-harness`, aliased as `jh`) with zero runtime dependencies (no Python virtualenv, no Docker, no background database server).

| Method | Command / Link |
|---|---|
| **Prebuilt binary** *(macOS, Linux, Windows)* | Download from [GitHub Releases](https://github.com/machineyearningengineer/job-harness/releases) |
| **Cargo** *(Rust 1.88+)* | `cargo install --git https://github.com/machineyearningengineer/job-harness job-harness` |
| **Clone & local install script** | `git clone https://github.com/machineyearningengineer/job-harness && cd job-harness && ./scripts/install.sh` *(PowerShell: `./scripts/install.ps1`)* |
| **Connect to any MCP agent** | Run `jh connect mcp` and paste the printed stdio block into Claude Code, Codex, Cursor, Windsurf, OpenCode, or Hermes |
| **Optional local web view** | Run `jh gui` to browse the exact same workspace in your browser |

---

## For Agents & Developers

<details>
<summary><strong>Architecture, tool contracts, schemas &amp; contributor docs</strong> (agents read these automatically)</summary>

Job Harness is the **deterministic state, safety, and derived-attention layer** (Rust + SQLite WAL `harness.sqlite` + typed CLI & focused stdio MCP). Your agent brings the reasoning, browser, search, mail, and calendar connectors.

| Area | Guide |
|---|---|
| **Agent integration & install** | [docs/AGENTS.md](docs/AGENTS.md) · [docs/INSTALL.md](docs/INSTALL.md) |
| **Onboarding & daily ops** | [resources/guides/job-harness-onboarding.md](resources/guides/job-harness-onboarding.md) · [resources/guides/career-operations.md](resources/guides/career-operations.md) |
| **Architecture & v2 contracts** | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) · [resources/guides/job-harness-contract.md](resources/guides/job-harness-contract.md) |
| **Daily review, CV & backups** | [resources/guides/daily-review.md](resources/guides/daily-review.md) · [docs/CV.md](docs/CV.md) · [docs/BUNDLES.md](docs/BUNDLES.md) |
| **Contributing & validation** | [CONTRIBUTING.md](CONTRIBUTING.md) · [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) · [docs/VALIDATION.md](docs/VALIDATION.md) |

</details>

---

<div align="center">
  <sub>MIT Licensed · Built for calm, truthful job searches.</sub>
</div>
