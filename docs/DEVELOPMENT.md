# Development

Requires Rust 1.88+ (pinned toolchain in `rust-toolchain.toml`).

## Common commands

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo run -p job-harness-dev --locked -- repository
cargo run -p job-harness-dev --locked -- contracts --check
```

Optional components:
```sh
cargo test --manifest-path providers/local/native/Cargo.toml --locked
cargo test --manifest-path apps/desktop/Cargo.toml --locked
```

## Workspace isolation

Source-built binaries and `jh` commands run inside this checkout automatically target a sibling development workspace (`../job-harness-development/workspace`) and ignore your real `JOB_HARNESS_WORKSPACE` or saved default. Pass `--root PATH` explicitly to operate on a real workspace.

## Cutting a release

1. Bump `version` in `Cargo.toml`, `apps/desktop/Cargo.toml`, `apps/desktop/tauri.conf.json`, and `providers/local/native/Cargo.toml`, and refresh lockfiles.
2. Add release notes in `docs/releases/vX.Y.Z.md` and update `CHANGELOG.md`.
3. Pass all checks above, commit, push `main`, and push tag `vX.Y.Z`.
4. Dispatch `.github/workflows/release.yml` with `tag=vX.Y.Z` and `confirm_full_release=true` to build all five native targets and publish checksummed archives (`SHA256SUMS`).
