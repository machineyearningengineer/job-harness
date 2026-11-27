# Job Harness Desktop (GPUI)

Native GPU-accelerated desktop app built with Rust and [GPUI](https://gpui.rs), linking `job-harness-core` directly in-process with zero WebView overhead.

## Features

- **4-Column Draggable Kanban (`Saved` · `Applied` · `Screening` · `Interviews`):** Optimistic guarded drag-and-drop (`expected_sha256`), 5-second `⌘Z` Undo, instant non-LLM search (`⌘K`), and hidden Archive toggle.
- **Left Expandable Chat Rail (`⌘\`):** Streams directly from your existing host agent session (Hermes, Claude Code, or Codex) with mid-turn Stop, follow-up queueing, and automatic clickable workspace citations. Hermes uses its active profile unless `JOB_HARNESS_HERMES_PROFILE` names one. Opened with `jh open --ssh`, chat runs the server's Hermes over SSH (`--hermes-profile NAME`).
- **Right Slide-Over Drawer:** Tabbed role drilldown (`Overview` · `Brief` · `Packet` · `Notes`) with inline citation previews, plus a **CVs Master → Children tree** (`⌘D`) with Open, Reveal in Finder, Copy Path, and native PDF import.
- **Actionable Controls:** Click `0 / 25` to view and adjust daily channel splits (`Portals`, `Direct email`, `Outreach`), click the subtle health status dot for live diagnostics, and toggle the 5 fail-closed permissions in Settings.
- **Local & Remote VPS Support:** Runs in-process locally via SQLite WAL + `notify`, or connects over SSH/Tailscale to a headless VPS running `JOB_HARNESS_SERVE_TOKEN_FILE=~/.config/job-harness/serve-token jh serve --bind 127.0.0.1:7420 --root ~/Career/workspace`.

## Run

```sh
# Local workspace
cargo run --release --manifest-path apps/desktop/Cargo.toml -- --root "$HOME/Career/workspace"

# Remote VPS (over SSH tunnel or Tailscale)
cargo run --release --manifest-path apps/desktop/Cargo.toml -- --remote http://127.0.0.1:7420 --token-file ~/.config/job-harness/server-token
```

`jh serve` always needs its bearer token. The app takes the first of `--token-file`, `JOB_HARNESS_SERVE_TOKEN`, and the file named by `JOB_HARNESS_SERVE_TOKEN_FILE`, and stops with an error if that file can't be read. Without arguments it opens what `jh configure` saved, including a `--remote` server and its token file.
