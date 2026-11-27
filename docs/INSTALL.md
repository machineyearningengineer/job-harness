# Install

## One-line install (prebuilt binary + SHA-256 check)

- **macOS / Linux:**
  ```sh
  curl -fsSL https://raw.githubusercontent.com/machineyearningengineer/job-harness/main/scripts/get.sh | sh
  ```
- **Windows (PowerShell):**
  ```powershell
  irm https://raw.githubusercontent.com/machineyearningengineer/job-harness/main/scripts/get.ps1 | iex
  ```
- **Rust Cargo (1.88+):**
  ```sh
  cargo install --git https://github.com/machineyearningengineer/job-harness job-harness
  ```
- **From a cloned checkout:** `./scripts/install.sh` *(Windows: `./scripts/install.ps1`)*

Manual archives and `SHA256SUMS` for macOS (`aarch64`/`x86_64`), Linux (`aarch64`/`x86_64`), and Windows (`x86_64`) live on [GitHub Releases](https://github.com/machineyearningengineer/job-harness/releases).

## Update

Re-run the same one-line installer; it replaces the binary in place. To run a ledger on an always-on Linux machine, see [LINUX-SERVER.md](LINUX-SERVER.md).

## Workspace setup

Keep your private career workspace outside any software checkout:

```sh
jh setup --workspace "$HOME/Career/workspace"
jh --root "$HOME/Career/workspace" status
jh --root "$HOME/Career/workspace" connect mcp
```

- **Selection order:** `--root PATH` → `JOB_HARNESS_WORKSPACE` → saved default (`~/.config/job-harness/settings.json`).
- **Agent setup:** See [docs/AGENT-SETUP.md](AGENT-SETUP.md) for Claude Code, Codex, Hermes, Cursor, Windsurf, and OpenCode wiring.
- **Optional components:** Native GPUI desktop app ([apps/desktop/README.md](../apps/desktop/README.md)), headless remote server (`jh serve`), local model provider ([docs/MODELS.md](MODELS.md)), and Typst CV builder ([docs/CV.md](CV.md)).
- **Backups & migration:** `jh workspace export` / `jh workspace import` ([docs/BUNDLES.md](BUNDLES.md)).
