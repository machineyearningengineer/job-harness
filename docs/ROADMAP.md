# Product boundaries

Job Harness ships as a native Rust CLI, focused stdio MCP server, SQLite WAL coordinator (`harness.sqlite`), derived attention engine (`review_queue`), source-preserving intake, evidence-backed CV builder, private encrypted-ready transfers, and optional local web/desktop views. Optional local model runtimes and the desktop window install separately.

The priority is completing useful job-search work through agents: fewer repeated questions, exact packets, clear next actions, interrupted-work recovery, and portable private context. Add a capability only when it removes a real manual step and has a bounded contract, source identity, and recovery rule.

## Next milestones

- **Zero-friction distribution:** Homebrew tap (`brew install`), signed macOS/Windows/Linux release archives, and a thin `npx` binary-launcher package (downloading the prebuilt Rust release binary, never rewriting the core in Node).
- **Native desktop acceptance** across macOS, Windows, and Linux beyond compilation checks.
- **Connector templates** for common MCP ecosystems (Gmail/Outlook, Google/Outlook Calendar, Exa/Parallel search, Granola/Wispr meeting transcripts) that pass canonical observations into Job Harness without becoming a second state store.

## Separate future scope

- Multi-host sync (beyond single-machine Git + SQLite WAL backup) requires explicit per-user auth and conflict resolution.
- Hosted cloud / multi-tenant deployments are out of scope for the local-first core.
