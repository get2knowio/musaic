# AGENTS.md

Guidance for coding agents lives in [CLAUDE.md](CLAUDE.md): the crate map, commands, test
data tiers, and the architectural rules with the files that implement each.

The short version:

- Rust commands run from `rust/` on the toolchain in `rust/rust-toolchain.toml`
  (1.99.1). Build the UI (`cd frontend && npm ci && npm run build`) before building or
  testing the whole workspace.
- Before you finish: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and
  for UI changes `npm run lint && npm run typecheck && npm run typecheck:test && npm test`
  in `frontend/`.
- Mutations publish events; they never call the fan-out directly. Resolve dialogs
  re-derive from a preview endpoint. Compare files with same-file checks, not path
  strings. Use the shared album-field accessors. Never delete an album row from a scan.
- Never run against `~/.musaic`, the OS keyring, or a real music library: use a temp
  config dir with `--dev-memory-secrets` (desktop) or `--verification-memory-secrets`
  (server), and a copy of a library.
- Do not commit audio, cover art, recorded third-party API answers, or anything derived
  from a real music library. Such test data belongs in the private tier (see CLAUDE.md).
