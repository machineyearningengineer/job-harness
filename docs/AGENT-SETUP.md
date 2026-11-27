# Agent Setup Playbook — Job Harness

> **Instructions for AI agents (Claude Code, Codex, Hermes, Cursor, Windsurf, OpenCode):**
> Follow these steps in order when a user asks you to install and set up Job Harness. Keep all external action permissions `false` until the user explicitly turns them on. Never put private career data, CVs, or credentials inside the `job-harness` software repo.

---

## Step 1 — Install the binary (`job-harness` / `jh`)

Choose the user's OS:

- **macOS / Linux (prebuilt binary with SHA-256 check):**
  ```sh
  curl -fsSL https://raw.githubusercontent.com/machineyearningengineer/job-harness/main/scripts/get.sh | sh
  ```
- **Windows (PowerShell):**
  ```powershell
  irm https://raw.githubusercontent.com/machineyearningengineer/job-harness/main/scripts/get.ps1 | iex
  ```
- **From source (if Rust 1.88+ is installed or prebuilt arch is unavailable):**
  ```sh
  cargo install --git https://github.com/machineyearningengineer/job-harness job-harness
  ln -sf "$HOME/.cargo/bin/job-harness" "$HOME/.cargo/bin/jh" 2>/dev/null || true
  ```

Verify the binary runs:
```sh
jh --help
```

---

## Step 2 — Initialize the private workspace

Pick the workspace path the user requested (default: `$HOME/Career/workspace`). Never initialize inside a checkout of `job-harness`.

```sh
jh setup --workspace "$HOME/Career/workspace" --json
```

This creates:
- `config/career-ledger.json` (all external action permissions `false`, starter daily cap `3`)
- `applications/operations/harness.sqlite` (SQLite WAL operational store)
- `applications/approved-answers.md`, `context/`, `content/`, and `output/`

---

## Step 3 — Connect Job Harness to your agent runtime

Print the exact stdio MCP config (defaults to `--tool-profile focused`, which advertises 12 core tools and saves ~78% schema tokens; specialized tools load on demand via `inspect_tools`):

```sh
jh --root "$HOME/Career/workspace" connect mcp
```

Wire it to the user's runtime:

| Agent / IDE | How to wire |
|---|---|
| **Claude Code** | `claude mcp add job-harness -- "$(which jh)" --root "$HOME/Career/workspace" mcp --tool-profile focused` |
| **Codex CLI** | Add `[mcp_servers.career]` with `command` and `args` from `jh connect mcp` in `~/.codex/config.toml` |
| **Cursor / Windsurf** | Add the `command` and `args` from `jh connect mcp` under MCP Servers in settings |
| **OpenCode** | Add the stdio MCP command under `mcp` in `opencode.json` |
| **Hermes Agent** | Use `jh --root "$PWD" call <tool> --input '{...}'` directly via terminal (0 persistent MCP schema tokens) and schedule routines from `config/career-routines.json`. When the agent runs as a user without file access to the ledger, drop `--root`: `jh call` then goes to the configured `jh serve` ([remote mode](LINUX-SERVER.md#agents-without-file-access-remote-mode)) |
| **Any custom CLI agent** | Register yourself in `~/.config/job-harness/runtimes.json` (`[{"name":"MyAgent","command":"my-agent --stdin","models":["model-id"]}]`) so you appear in the desktop app's Agent selector and in-app chat automatically |

---

## Step 4 — Run the 3-minute user onboarding

Ask the user for the minimum required details (do not invent anything):

1. **CV PDF path** (imported/copied into the workspace and saved as `primary_cv` in `config/career-ledger.json`).
2. **Target role families** (`target_roles`) and any initial CV tracks (`cv_tracks`, e.g. `["backend", "product", "ai"]`).
3. **Timezone** (IANA name, e.g. `Europe/London`, `America/New_York`) and **search preferences** (`location_preferences`, `min_base_salary`, `currency`, `exclude_ats`, and optionally `allowed_locations`, `allowed_work_arrangements`, `max_office_days_per_week`, which preparation enforces).
4. **Core screening defaults** saved in `applications/approved-answers.md` (work authorisation, notice period, salary expectations, preferred work model, links).
5. **Writing style & anti-patterns** in `config/career-ledger.json:communication` (locale, tone, sign-off).

Confirm all 5 `permissions` flags in `config/career-ledger.json` remain `false` until the user explicitly enables one:
- `auto_apply_portal`
- `auto_send_first_contact_email`
- `auto_reply_threads`
- `auto_book_calendar`
- `auto_archive_mail`

---

## Step 5 — Verify in 1 turn (`query_batch`)

Always batch startup reads into a single turn instead of making separate calls, and **never read `applications/pipeline.md` whole into context**:

```sh
jh --root "$HOME/Career/workspace" call query_batch --input '{
  "steps": [
    {"id": "boot", "tool": "bootstrap", "arguments": {"operation": "daily_check", "capabilities": []}},
    {"id": "queue", "tool": "review_queue", "arguments": {"limit": 10}}
  ]
}'
```

Confirm:
- `boot.data.clock` matches the user's timezone
- `boot.data.daily_budget` reports their configured daily limit
- `queue.data.needs_attention_now` and `queue.data.urgency_counts` return cleanly

Report the finished setup clearly to the user and ask what they want to tackle first (e.g., scouting roles, reviewing their inbox, or preparing for an upcoming interview).
