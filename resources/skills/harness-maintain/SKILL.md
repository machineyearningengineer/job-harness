---
name: harness-maintain
description: Improve Job Harness capabilities, protocols, documentation or focused skills using synthetic reproduction and compatibility checks. Use for toolkit development, not job applications or candidate fact updates.
---

# Improve the harness

Read the reusable repository's AGENTS.md and docs/ARCHITECTURE.md. Select the narrow capability, adapter or resource responsible for observed friction. Keep candidate workspaces and host credentials outside the checkout.

Reproduce the problem with a synthetic workspace and observable behavior. Keep the versioned envelope and schema contracts consistent across CLI, MCP and embedding. Batch references are exact data lookups, never executable expressions. The model host is optional and provider-independent; external connector access belongs to its host.

Use source development selection (cargo run or a CLI invoked in the checkout),
which ignores live workspace defaults and has separate host settings. Initialize
its sibling test workspace once; explicit --root arguments must name synthetic
data. The installed app/MCP keeps the person's real workspace outside the code
checkout. See docs/DEVELOPMENT.md for the exact paths and precedence.

Preserve schema-1 state, exact packet bytes, sourced answer identity and unknown submission guards. Use the canonical journal for operational state and the rebuildable index for search; a cache failure cannot imply a failed canonical action. Generic guidance belongs under resources/, candidate facts in the private workspace, and credentials in the host store.

Run targeted Rust tests while developing, then make check. Run make check-parity for identity, coordinator, schema or transfer changes, and make check-package for install/resource changes. Report what was tested locally, on CI and through an actual installed client. Review graphics when changed.

Without Make, use the portable `cargo xtask repository`, `cargo xtask contracts --check` and `cargo xtask package --binary PATH` commands alongside Rust formatting, Clippy and tests. Compatibility tests retain native vectors after the completed language migration; do not restore the retired runtime as a test dependency. Optional desktop/model components have separate manifests and validation.

Finish a focused reviewable change with clear commits and evidence. Do not publish packages, expose services, change visibility or send real employer messages as part of a synthetic check.
