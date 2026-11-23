# Architecture

Job Harness is a deterministic local state, safety, and workflow engine. It never embeds an agent runtime or stores external credentials.

## Layers

1. **Core engine (`crates/harness-core`):** SQLite WAL coordinator (`applications/operations/harness.sqlite`), exact deduplication, immutable SHA-256 submission packets, atomic total + per-channel daily caps, derived attention (`review_queue`), and typed v2 JSON schemas (`resources/schemas/tools.json`).
2. **Transports (`crates/harness-protocol`):** Focused/full stdio MCP server (`rmcp`) and optional loopback web UI (`jh gui`).
3. **CLI (`crates/harness-cli`):** Single static binary (`job-harness`, aliased as `jh`) with embedded skills, guides, and schemas.
4. **Private workspace (user-owned):** Canonical Markdown/JSON career facts, CVs, 2-layer company/round briefs, and `harness.sqlite` stored outside this repository.

## Key invariants

- **One-shot submission boundary:** `prepare_application` → `begin_submission` → single external submit → `finish_submission`. Unclear outcomes become `outcome_unknown` and block accidental retries until externally reconciled.
- **Derived vs. persisted state:** Urgency (`urgent_now` / `upcoming` / `background`), `needs_attention_now`, `daily_budget`, and CV `track_fit` are computed fresh on read — never stored as drift-prone flags. Only immutable submission facts, canonical records, and suppression metadata persist.
- **Fail-closed defaults:** All 5 external action switches in `config/career-ledger.json:permissions` default to `false`.
- **No external credentials:** Browser sessions, mail/calendar OAuth, and model keys live strictly in the host runtime.

See [resources/guides/job-harness-contract.md](../resources/guides/job-harness-contract.md) for the exact v2 tool contract.
