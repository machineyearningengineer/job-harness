# Examples

Every example uses fictional data and never contacts an employer, mailbox, or calendar.

## Try the 30-second demo

```sh
jh demo --workspace "$HOME/Career/demo"
jh --root "$HOME/Career/demo" status
jh --root "$HOME/Career/demo" call review_queue --input '{"limit":10}'
jh --root "$HOME/Career/demo" gui
```

## Connect an AI agent

```sh
jh --root "$HOME/Career/demo" connect mcp
```

Paste the returned stdio MCP entry into Claude Code, Codex, Hermes, Cursor, Windsurf, or OpenCode.

## Walkthroughs

| Example | What it shows |
|---|---|
| [Agent & CLI batching](agent/README.md) | Connecting MCP (`focused` vs `full`) and piping bounded `query_batch` calls |
| [Daily review](daily-review/README.md) | Reviewing live mail observations and approving exact archive batches |
| [Evidence-backed CV](cv/README.md) | Validating and building a one-page Typst CV from [selection.json](cv/selection.json) |
| [Optional local models](models/README.md) | Running optional local retrieval/decision probes without external APIs |
| `./scripts/demo.sh` | End-to-end disposable shell walkthrough (init, batch, run, export, restore) |
