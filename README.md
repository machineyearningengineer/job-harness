<div align="center">
  <img src="docs/assets/icon.svg" width="76" height="76" alt="Job Harness">
  <h1>Job Harness</h1>
  <p><strong>Turn any AI agent into a calm, personal job-search assistant.</strong><br>
  Scouts roles, applies safely, preps you for every interview, and hands you ready-to-send replies — while keeping all your data private on your own machine.</p>
  <p>
    <img alt="Claude Code" src="https://img.shields.io/badge/Claude_Code-18181b?style=flat-square&logo=anthropic&logoColor=white">
    <img alt="Codex" src="https://img.shields.io/badge/Codex-18181b?style=flat-square&logo=openai&logoColor=white">
    <img alt="Cursor" src="https://img.shields.io/badge/Cursor-18181b?style=flat-square&logo=cursor&logoColor=white">
    <img alt="Hermes" src="https://img.shields.io/badge/Hermes_Agent-2d6a4f?style=flat-square">
    <img alt="Local First" src="https://img.shields.io/badge/100%25_Local-4965ae?style=flat-square">
    <a href="LICENSE"><img alt="MIT License" src="https://img.shields.io/badge/License-MIT-66717b?style=flat-square"></a>
  </p>
  <p>
    <a href="#start-in-60-seconds">Start in 60s</a> ·
    <a href="#what-it-does-for-you">What It Does</a> ·
    <a href="#you-stay-in-control">Safety &amp; Control</a> ·
    <a href="#your-daily-rhythm">Daily Rhythm</a> ·
    <a href="#for-agents--developers">For Agents &amp; Devs</a>
  </p>
</div>

![Your workspace, any capable agent. Shared tools and private career history stay separate.](docs/assets/hero.svg)

---

## Start in 60 seconds

Job Harness works with **Hermes, Claude Code, Codex, Cursor, Windsurf, OpenCode**, or straight from your terminal. You don't need to learn any commands — just paste this prompt into your AI agent and let it set everything up for you:

```text
Install Job Harness (https://github.com/machineyearningengineer/job-harness), set up my private career workspace at ~/Career/workspace, connect it to you, and walk me through the quick 3-minute setup (my CV, target roles, timezone, and core preferences). Keep all automatic sending/applying turned off until I tell you otherwise.
```

Prefer doing it yourself? Grab a release from [Releases](https://github.com/machineyearningengineer/job-harness/releases) (or run `./scripts/install.sh` on macOS/Linux) and run `jh setup --workspace ~/Career/workspace`.

---

## What it does for you

Most job-search tools dump more chores on your plate. **Job Harness hands you finished work, not to-do lists.**

- **Morning & evening briefs that respect your time** — Wake up to the 1–3 things that actually matter today (interviews, deadlines, recruiter replies) with complete reply drafts written in *your* voice so you just say *"yes"*.
- **Quietly scouts fresh roles from anywhere** — Checks company career pages, founder posts, HN, and any search tools you already use, automatically skipping anything you've already applied to or ruled out.
- **Applies accurately — and never twice** — Remembers every single role, the exact CV version sent, and every screening answer you gave. Enforces your daily limit and never fires an accidental duplicate application.
- **Walk into every interview ready** — Builds a plain-English company & round brief plus a 10-line pre-call cheatsheet 30 minutes before your interview. Afterwards, checks how it went (or pulls your meeting transcript if connected) and carries those lessons into your next round.
- **Tailored CVs without making things up** — Keeps one truthful master CV and suggests focused tracks (e.g. Backend, Product, AI) as your target roles evolve, without ever inventing experience.
- **Talk to it like a real assistant** — Ask *"I forgot what needs my attention today?"* anytime and get a clear answer. When nothing is urgent, it lets you know everything is running smoothly so you can step away and take a breather.

---

## You stay in control

Every new workspace starts with **all automatic external actions turned OFF**. Turn on only what you're comfortable with, when you're ready.

| | What happens |
|---|---|
| **Handles for you** *(when enabled)* | Scouting fresh roles, deduplicating history, watching your inbox, building interview briefs, and bounded portal applications within your daily cap |
| **Waits for a simple "yes"** | Recruiter replies drafted in your natural voice, proposed interview slots checked against your calendar, and inbox tidy-ups |
| **Always 100% you** | The interviews themselves, negotiating offers, and final decisions |

- **Your data stays yours.** Everything lives in a private folder on your machine — no cloud lock-in, no subscriptions, no telemetry.
- **Swap AI agents anytime.** Start in Cursor, move to Claude Code, or run 24/7 in Hermes — your entire job-search history, CV tracks, and guardrails come with you untouched.

---

## Your daily rhythm

```text
08:00  Morning brief     Today's interviews, top priorities & ready-to-send drafts
Every 15m  Inbox watch   Silent when nothing's new — alerts you only when it's urgent
Daytime    Job scout     Finds fresh matching roles from your chosen sources
Windows    Apply desk    Submits cleanly within your daily limit (off by default)
Before call  Prep        Company brief + 10-line cheatsheet 30 mins before interviews
17:30  Evening close     Today's results, quick post-call debrief & tomorrow's preview
Sunday     Weekly tidy   Checks quiet applications, resolves loose ends & suggests CV updates
```

---

## For agents & developers

<details>
<summary><strong>Technical stack, install guides &amp; documentation</strong> (your AI agent reads these automatically)</summary>

Job Harness is a single local Rust binary (`jh` / `job-harness`) backed by SQLite WAL (`harness.sqlite`), exposing typed CLI commands, a focused stdio MCP server, and an optional local web GUI (`jh gui`). It is the **state, safety, and workflow layer** — your agent brings the reasoning, browser, search, mail, and calendar connectors.

| Area | Guide |
|---|---|
| **Connect an agent (MCP / CLI)** | [docs/AGENTS.md](docs/AGENTS.md) · [docs/INSTALL.md](docs/INSTALL.md) |
| **Workspace & onboarding** | [resources/guides/job-harness-onboarding.md](resources/guides/job-harness-onboarding.md) · [resources/guides/career-operations.md](resources/guides/career-operations.md) |
| **Architecture & tool contracts** | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) · [resources/guides/job-harness-contract.md](resources/guides/job-harness-contract.md) |
| **Daily review, CV & backups** | [resources/guides/daily-review.md](resources/guides/daily-review.md) · [docs/CV.md](docs/CV.md) · [docs/BUNDLES.md](docs/BUNDLES.md) |
| **Contributing & validation** | [CONTRIBUTING.md](CONTRIBUTING.md) · [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) · [docs/VALIDATION.md](docs/VALIDATION.md) |

</details>

---

<div align="center">
  <sub>MIT Licensed · Built for calm, truthful job searches.</sub>
</div>
