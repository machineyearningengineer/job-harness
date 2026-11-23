# Connecting Any AI Agent

Job Harness provides durable local state (`applications/operations/harness.sqlite`), derived attention (`review_queue`), immutable submission packets, and typed tools. Your agent brings reasoning, browser automation, search, mail, and calendar connectors.

## Connect over MCP *(recommended)*

```sh
jh setup --workspace "$HOME/Career/workspace"
jh --root "$HOME/Career/workspace" connect mcp
```

Paste the returned `command` and `args` into your agent's MCP config.

- **`--tool-profile focused` (default):** Advertises 12 core tools (~78% smaller schema overhead) and discovers specialized tools on demand via `inspect_tools`.
- **`--tool-profile full`:** Advertises all 40 tools directly.

## Or call directly via CLI

```sh
jh --root "$HOME/Career/workspace" call bootstrap --input '{"operation":"daily_check","capabilities":[]}'
jh --root "$HOME/Career/workspace" call review_queue --input '{"limit":10}'
jh --root "$HOME/Career/workspace" call query_batch --input examples/agent/inspect.json
```

All CLI calls return compact v2 JSON (`schema_version`, `ok`, `data`, `error`, `next`). See [docs/AGENT-SETUP.md](../../docs/AGENT-SETUP.md) and [resources/guides/job-harness-contract.md](../../resources/guides/job-harness-contract.md) for the full contract.
