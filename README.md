<div align="center">
  <img src="docs/assets/icon.svg" width="76" height="76" alt="Job Harness">
  <h1>Job Harness</h1>
  <p><strong>Local career ledger, safety guardrails, and native desktop workbench for any AI agent.</strong><br>
  Scouts roles, tracks every application and CV track, preps 2-layer interview briefs, and drafts replies in your voice — stored locally on your machine.</p>
  <p>
    <a href="https://github.com/machineyearningengineer/job-harness/releases"><img alt="Releases" src="https://img.shields.io/badge/releases-GitHub-4965ae?style=flat-square&logo=github"></a>
    <img alt="Claude Code" src="https://img.shields.io/badge/Claude_Code-18181b?style=flat-square&logo=anthropic&logoColor=white">
    <img alt="Codex" src="https://img.shields.io/badge/Codex-18181b?style=flat-square&logo=openai&logoColor=white">
    <img alt="Cursor" src="https://img.shields.io/badge/Cursor-18181b?style=flat-square&logo=cursor&logoColor=white">
    <img alt="Hermes" src="https://img.shields.io/badge/Hermes-2d6a4f?style=flat-square">
    <a href="LICENSE"><img alt="MIT License" src="https://img.shields.io/badge/license-MIT-66717b?style=flat-square"></a>
  </p>
  <p>
    <a href="#showcase">Showcase</a> ·
    <a href="#quick-start">Quick Start</a> ·
    <a href="#what-it-does">What It Does</a> ·
    <a href="#permissions--control">Permissions</a> ·
    <a href="#daily-workflow">Daily Workflow</a> ·
    <a href="#install">Install</a> ·
    <a href="#for-agents--developers">Docs</a>
  </p>
</div>

<br>

<p align="center">
  <img src="docs/assets/showcase-brief.png" alt="Job Harness desktop workbench — AI Chief-of-Staff chat, local Kanban board, and 2-layer interview brief side by side" width="100%">
</p>
<p align="center">
  <sub><em>Synthetic demo workspace:</em> Live agent triage with clickable workspace citations (left), deterministic SQLite WAL pipeline board (center), and 2-layer interview cheatsheet (right).</sub>
</p>

---

## Showcase

### Product Walkthrough

<p align="center">
  <img src="docs/assets/demo.gif" alt="Job Harness animated walkthrough showing AI triage, 2-layer interview briefs, interactive Sankey funnel flow, review-only guardrails, and command bar" width="100%">
</p>

### Interactive Funnel Flow (Board ⇄ Sankey)
Swap between your Kanban board and a full-canvas interactive **Sankey flow** built natively in GPUI. Hover any stage to illuminate its path, tap to drill straight into matching roles, and keep **Rejected**, **Withdrawn**, and **Archived** outcomes cleanly distinct — never guessed from silence.

<p align="center">
  <img src="docs/assets/showcase-flow.png" alt="Interactive GPUI Sankey funnel view showing pipeline conversion and explicit rejection/withdrawal outcomes" width="100%">
</p>

### Connections & Review-Only Guardrails
See every connected agent runtime and MCP server at a glance alongside your four external action switches — all `false` by default — plus daily submission caps and reminder timing.

<p align="center">
  <img src="docs/assets/showcase-guardrails.png" alt="Settings drawer showing connected runtimes, MCP connectors, and review-only automation guardrails" width="100%">
</p>

<details>
<summary><strong>More views — Full Pipeline Board &amp; Keyboard Command Bar (⌘K)</strong></summary>
<br>
<p align="center">
  <img src="docs/assets/showcase-hero.png" alt="Full pipeline board with AI chief-of-staff triage and clickable workspace citations" width="100%">
</p>
<p align="center">
  <img src="docs/assets/showcase-command.png" alt="Keyboard-first Cmd+K command bar for jumping to roles, actions, or chat" width="100%">
</p>
</details>

---

## Quick Start

### Option 1 — Paste into your AI agent *(recommended)*

Using **Claude Code, Codex, Hermes, Cursor, Windsurf, or OpenCode**? Paste this one prompt — your agent reads the setup guide, installs the binary, connects MCP/CLI, and walks you through the 3-minute onboarding:

```text
Read https://raw.githubusercontent.com/machineyearningengineer/job-harness/main/docs/AGENT-SETUP.md and set up Job Harness for me at ~/Career/workspace.
```

### Option 2 — One-line install in your terminal

**macOS & Linux:**
```sh
curl -fsSL https://raw.githubusercontent.com/machineyearningengineer/job-harness/main/scripts/get.sh | sh
```

**Windows (PowerShell):**
```powershell
irm https://raw.githubusercontent.com/machineyearningengineer/job-harness/main/scripts/get.ps1 | iex
```

Then initialize your private workspace and print the MCP config for your agent:
```sh
jh setup --workspace "$HOME/Career/workspace"
jh --root "$HOME/Career/workspace" connect mcp
```

### Try the Desktop App with Synthetic Demo Data
Want to explore the native GPUI desktop workbench without touching real personal data? Seed a 100% synthetic 18-role workspace in 2 seconds:

```sh
python3 scripts/seed_demo.py /tmp/job-harness-demo
cargo run --release --manifest-path apps/desktop/Cargo.toml -- --root /tmp/job-harness-demo
```

---

## What It Does

| Capability | What happens in practice |
|---|---|
| **Daily triage & check-ins** | Ask *"what needs my attention today?"* anytime. Surfaces today's interviews, overdue follow-ups, and recruiter emails needing a reply — with clickable in-app citations to every brief and role record. |
| **Reply drafts in your voice** | Drafts follow-ups and recruiter replies matching how you write. Re-checks live Sent mail before sending so it never double-replies if you already answered from your phone. |
| **Role discovery from any source** | Pulls leads from employer boards, founder posts, HN, or any search MCP you connect (Exa, Parallel, etc.) and deduplicates against every role you have already touched. |
| **Exact application records & duplicate guards** | Saves the exact CV PDF, screening answers, and receipt for every role. Enforces your daily submission cap and blocks duplicate retries when an attempt's outcome is still unconfirmed. |
| **2-layer interview briefs & debriefs** | Maintains one canonical Layer 1 company brief per employer plus Layer 2 round cheatsheets 90 minutes before interviews. Captures post-call debriefs and feeds verified takeaways back into later rounds. |
| **Truthful CV tracks** | Keeps one verified master CV and generates role-focused tracks (e.g. Backend, Product, AI) without inventing experience or metrics. Flags when new target roles need another track. |
| **Honest outcome tracking** | Distinguishes explicit rejections (`outcome: rejected`), candidate withdrawals (`outcome: withdrawn`), and silent/other closes in both analytics and the interactive Sankey flow. |

---

## Permissions & Control

Every new workspace starts **review-only**: all four external action switches in `config/career-ledger.json` default to `false`.

| Switch | Default | When turned `true` |
|---|---|---|
| `auto_apply_portal` | `false` | Submits eligible ATS portal applications within your daily cap (`max_daily_submissions` / `daily_channel_limits`) |
| `auto_send_first_contact_email` | `false` | Sends initial direct-email applications (only after verifying zero prior Sent emails to that contact) |
| `auto_reply_threads` | `false` | Replies to existing recruiter threads without asking first |
| `auto_archive_mail` | `false` | Archives verified receipts and rejections |

- **Default (all `false`):** Scouts roles, deduplicates, prepares briefs, and drafts replies/applications for your approval.
- **Always human-confirmed:** Scheduling new interview times (checks live calendar + buffers, suggests 2–3 slots with rationale, then finishes booking & accepts the invite once you pick one), live interviews, and offers.
- **Local & portable:** Your career history stays in one private folder on your disk — zero cloud lock-in, zero telemetry. Switch between Claude Code, Codex, Cursor, or Hermes without losing history.

---

## Daily Workflow

```text
08:00       Morning desk      Interviews, deadlines, top priorities & reply drafts ready to approve
Every 15m   Inbox watch       $0-token probe; wakes the agent only when an urgent thread arrives
Daytime     Job scout         Finds fresh roles from your configured sources and deduplicates
Windows     Application desk  Submits within your daily cap (off by default)
-90 min     Interview prep    Refreshes company/round brief + 10-line pre-call cheatsheet
17:30       Evening desk      Receipts, post-interview debriefs & tomorrow's preview
Sunday      Weekly reconcile  Audits unknown outcomes against Gmail, flags quiet stages & CV track gaps
```

---

## Install

Single native binary (`job-harness`, aliased as `jh`) with zero runtime dependencies.

| Method | Command |
|---|---|
| **macOS / Linux (1 line)** | `curl -fsSL https://raw.githubusercontent.com/machineyearningengineer/job-harness/main/scripts/get.sh \| sh` |
| **Windows PowerShell (1 line)** | `irm https://raw.githubusercontent.com/machineyearningengineer/job-harness/main/scripts/get.ps1 \| iex` |
| **Rust Cargo** | `cargo install --git https://github.com/machineyearningengineer/job-harness job-harness` |
| **Prebuilt archives** | [GitHub Releases](https://github.com/machineyearningengineer/job-harness/releases) *(macOS, Linux, Windows + SHA256SUMS)* |
| **From cloned checkout** | `./scripts/install.sh` *(Windows: `./scripts/install.ps1`)* |
| **Native GPUI desktop app** | `cargo run --release --manifest-path apps/desktop/Cargo.toml` *(see [apps/desktop/README.md](apps/desktop/README.md))* |
| **Remote VPS sync server** | `jh serve --bind 127.0.0.1:7420` *(HTTP + SSE for remote desktop clients)* |

---

## For Agents & Developers

<details>
<summary><strong>Agent setup playbook, architecture &amp; tool contracts</strong></summary>

Job Harness is the deterministic state, safety, and derived-attention layer (Rust + SQLite WAL `harness.sqlite` + typed CLI & focused stdio MCP). Your agent provides reasoning, browser, search, mail, and calendar connectors.

| Area | Guide |
|---|---|
| **Agent setup & install** | [docs/AGENT-SETUP.md](docs/AGENT-SETUP.md) · [docs/INSTALL.md](docs/INSTALL.md) |
| **Onboarding & daily ops** | [resources/guides/job-harness-onboarding.md](resources/guides/job-harness-onboarding.md) · [resources/guides/career-operations.md](resources/guides/career-operations.md) |
| **Architecture & v2 contracts** | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) · [resources/guides/job-harness-contract.md](resources/guides/job-harness-contract.md) |
| **Daily review, CV & backups** | [resources/guides/daily-review.md](resources/guides/daily-review.md) · [docs/CV.md](docs/CV.md) · [docs/BUNDLES.md](docs/BUNDLES.md) |
| **Contributing & development** | [CONTRIBUTING.md](CONTRIBUTING.md) · [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) · [CHANGELOG.md](CHANGELOG.md) |

</details>

---

<div align="center">
  <sub>MIT Licensed</sub>
</div>
