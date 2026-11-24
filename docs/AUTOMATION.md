# Background Automation & Delegation Modes

Job Harness is designed to stay **lightweight and battery-friendly** — it does **not** run a heavy always-on background LLM daemon.

---

## How Background Runs Work (Zero-Daemon Pattern)

1. **OS Scheduler or Host Cron triggers a `$0`-token check:**
   - **macOS (`launchd`) / Linux (`systemd` timers)** or your host runtime's scheduler (`hermes cron`, T3 Code scheduled tasks) runs `jh daily check --json` on your schedule (e.g. every 15 minutes during work hours, plus morning/evening desks).
   - That check is pure local Rust/SQLite (~15 ms, **0 LLM tokens**).
2. **Wake the agent only when something actually changed:**
   - If an urgent interview is within 90 minutes, an inbound email needs triage, or a scheduled window opens, the scheduler invokes your chosen agent runtime (Hermes, Claude Code, Codex, or Pi) for one bounded run.
3. **What happens when your laptop sleeps:**
   - **Sleeping laptop (default):** macOS `launchd` automatically runs missed scheduled jobs as soon as you open your lid — no work is lost, and overnight battery stays untouched.
   - **Keep Mac awake during active runs (toggleable in Settings):** When turned **On**, Job Harness wraps active desktop agent turns with macOS `caffeinate -di` so long research or batch turns never get interrupted mid-flight by idle sleep, then immediately allows normal sleep once the turn finishes.
   - **24/7 always-on option:** Run `jh serve` + your agent scheduler on an inexpensive VPS over Tailscale, and point the Desktop App at the remote URL (`Remote` mode).

---

## Delegation Modes (Settings → Automation Mode)

Every workspace starts in **Review only**. Switching modes sets the underlying fail-closed flags in `config/career-ledger.json` transparently:

| Mode | Portal submits | First-contact emails | Recruiter replies | Inbox archive |
|---|---|---|---|---|
| **Review only** *(Default)* | Draft / prep only | Draft only | Draft only | Off |
| **Assisted apply** | Auto within daily cap | Draft only | Draft only | Optional |
| **Autonomous** | Auto within daily cap | Auto (0 prior Sent check) | Auto (live Sent check) | Optional |

### Always Human-Confirmed (In Every Mode)
- Picking or changing interview times
- Taking live interviews and negotiating or accepting offers
- Solving CAPTCHAs / MFA / portal login walls (in interactive mode the agent hands you the tab; in unattended runs it stops and records `outcome_unknown` — never retries blindly)
- Passing temporary credentials (via the focused 1-use Secret Modal or `jh secret`)
