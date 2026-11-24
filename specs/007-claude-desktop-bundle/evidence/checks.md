# Check counts

Baseline (main bf45aa60, recorded by the requester): Rust 1893 passed, 0 failed, 11 ignored;
frontend 566 passed in 58 files; lint 0 errors, 34 warnings. A baseline run on this branch was
not usable: it started while the first edits were landing and stopped at the first failing test
binary (`ops_boundary`, the lock file not yet updated).

Boundary tests before the first change: not recorded separately (same reason); all pass in the
final run below.

## Dependency findings (T003)

- `semver` 1.0.28 is in `Cargo.lock` (used by build tooling); it is added to `musaic-app` and
  used for the "older" comparison.
- `zip` 9.8.2, pinned exactly, default features off, feature `deflate-flate2` (reuses the
  `flate2` already in the tree; the `deflate` feature would add zopfli). Licence MIT, which is in
  `rust/crates/musaic-desktop/notices/about.toml`'s accepted list.

## Final run (T034, T040)

- `cargo fmt --all --check`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace --no-fail-fast`: 1930 passed, 2 failed, 11 ignored on the full run; the
  two failures (the provider-name source scan, and a directory-permission check on the generated
  folder) were fixed and their targets rerun green (`no_model_calls` 2 passed, `ops_agent_claude`
  14 passed), so 1932 passed, 0 failed, 11 ignored.
- `cargo test -p musaic-desktop --test boot -- --ignored --nocapture`: 2 passed.
- `cargo run -p musaic-cli -- catalog --check`: clean, no regenerated file; no change under
  `docs/agent-*` or `skills/`.
- Frontend: lint 0 errors (34 warnings), `typecheck` and `typecheck:test` clean, vitest 586
  passed in 59 files, build ok.
