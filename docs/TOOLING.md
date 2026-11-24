# Recommended Tooling Stack

Job Harness owns **deterministic career state, submission guards, and your desktop workbench**. Your host agent owns **reasoning, search, mail, calendar, and interactive browsing**.

Every connector layer is optional and swappable. Below is the recommended stack per facet — and how to keep scopes and sessions clean.

---

## Quick Matrix

| Facet | Recommended default | Great alternatives | Scope |
|---|---|---|---|
| **Inference & runtime** | **Codex CLI** (`codex login`) or **Claude Code** (`claude`) or **Hermes** (`hermes model`) | Cursor, OpenCode, or local OpenAI-compatible server (oMLX / Ollama / llama.cpp) | Host CLI OAuth — zero API keys stored in Job Harness |
| **Career Ledger (required)** | **`jh connect mcp`** (`job-harness` stdio MCP) | `jh` CLI directly | Shared across all agents |
| **Email & Calendar** | **Shared MCPorter** gateway (Google Workspace / Gmail + Calendar MCP) | Agent-specific Gmail / Outlook MCP | **Shared (global)** preferred so switching agents keeps mail + calendar working |
| **Web Search & Fetch** | **Built-in runtime search** (`Hermes web_search`, `Claude WebSearch`, `Codex --search`) | Exa, Brave Search, Tavily, or Firecrawl MCP for bulk role scouting | Built-in first; add Exa/Brave MCP for high-volume scouting |
| **Interactive Browser (ATS portals)** | **Host ADE preview browser** (inside T3 Code / Cursor / Antigravity) or **Hermes `browser_*`** / **Playwright MCP** | Chrome DevTools / CDP attached to a dedicated job-search profile | Dedicated job-search profile — never your personal banking browser |
| **Passwords / Portal Tokens** | **Host ADE `request_secret`** (e.g. T3 Code) or **Job Harness Ephemeral Secret Modal (`jh secret`)** | Interactive human login inside the dedicated browser profile | Ephemeral `0600` temp file, 1-use, auto-shred — never in chat or Git |

---

## Global vs. Agent-Only Connectors

In **Settings → Tools & connectors**, Job Harness evaluates every capability **relative to the currently active runtime**:

- **Shared across agents (recommended):** Registered once in `~/.mcporter/mcporter.json` (or `~/.config/mcporter/mcporter.json`). Switching between Hermes, Claude, and Codex preserves Email, Calendar, and Career Ledger automatically.
- **Agent-only:** Registered inside a single client (`~/.claude.json` or `~/.codex/config.toml`). Shown as **In Claude only** / **In Codex only** (amber) when another runtime is active so you immediately see why a tool isn't reachable.

---

## Web Search vs. Browser (The 90 / 10 Rule)

1. **Read-only research (~90% of turns):** Use native web search/fetch (`web_search`, `WebSearch`, or Exa/Brave MCP). It is fast, cheap, and carries zero session/cookie risk.
2. **Interactive state (~10% of turns):** Use a real browser only when a page needs JavaScript rendering, ATS form filling (Ashby, Greenhouse, Lever, Workday), or an authenticated account (LinkedIn, Wellfound, Welcome to the Jungle).

---

## Browser Auth & Cookies — Security North Star

**Never clone or bulk-dump your main personal browser's cookie database.**
- On macOS, Chromium cookies are encrypted with the Keychain Safe Storage key; decrypting them triggers invasive OS prompts and mirrors infostealer behavior.
- Sharing your main personal profile exposes banking, personal email, password managers, and GitHub sessions to prompt injection from untrusted job listings.

**Use least-privilege sessions instead:**
1. **Dedicated persistent job-search profile:** Launch Playwright / Chromium with its own `--user-data-dir` (e.g. `~/.local/share/job-harness/browser-profile`). Sign in **once** visibly to only the job boards you want automated. Cookies persist across runs with zero access to your personal browser.
2. **Site-scoped human handoff:** When the agent encounters MFA, OAuth, or a CAPTCHA, it pauses and lets you complete login directly in that tab, then resumes.
3. **Single-tab attach (optional):** If you prefer using an already-open tab, attach to **one explicitly chosen tab** (e.g. via Chrome DevTools / extension MCP) rather than exporting all browser cookies.
