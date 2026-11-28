# Job Harness Desktop (GPUI)

Native GPU-accelerated desktop app built with Rust and [GPUI](https://gpui.rs), linking `job-harness-core` directly in-process with zero WebView overhead.

## Features

- **4-Column Draggable Kanban (`Saved` · `Applied` · `Screening` · `Interviews`):** Optimistic guarded drag-and-drop (`expected_sha256`), 5-second `⌘Z` Undo, instant non-LLM search (`⌘K`), and hidden Archive toggle.
- **Left Expandable Chat Rail (`⌘\`):** Streams directly from your existing host agent session (Hermes, Claude Code, or Codex) with mid-turn Stop, follow-up queueing, and automatic clickable workspace citations. Hermes uses its active profile unless `JOB_HARNESS_HERMES_PROFILE` names one. Opened with `jh open --ssh`, chat talks to the ACP agents of the server's `jh agent-host` (see [Linux server](../../docs/LINUX-SERVER.md#chat-with-an-agent-on-the-server)); a turn keeps running while the app is offline and resumes where it left off.
- **Right Slide-Over Drawer:** Tabbed role drilldown (`Overview` · `Brief` · `Packet` · `Notes`) with inline citation previews, plus a **CVs Master → Children tree** (`⌘D`) with Open, Reveal in Finder, Copy Path, and native PDF import.
- **Actionable Controls:** Click `0 / 25` to view and adjust daily channel splits (`Portals`, `Direct email`, `Outreach`), click the subtle health status dot for live diagnostics, and toggle the 5 fail-closed permissions in Settings.
- **Local & Remote VPS Support:** Runs in-process locally via SQLite WAL + `notify`, or opens its own supervised SSH connection to a server running `jh serve` on a Unix socket (`jh service install`), with a Reconnecting and Offline banner and a read-only board while it is down. See [LINUX-SERVER.md](../../docs/LINUX-SERVER.md#open-it-from-your-computer).

## Run

```sh
# Local workspace
cargo run --release --manifest-path apps/desktop/Cargo.toml -- --root "$HOME/Career/workspace"

# A server, over the app's own SSH connection
cargo run --release --manifest-path apps/desktop/Cargo.toml -- --ssh user@server [--identity KEY] [--socket PATH]
```

`jh serve` listens only on a Unix socket, whose file permissions decide who may call; there is no token. `--remote unix:PATH` opens a socket on this computer. Without arguments it opens what `jh configure` saved, including a `--remote` socket.
