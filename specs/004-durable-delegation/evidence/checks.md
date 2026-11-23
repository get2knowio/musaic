# T008 conformance evidence

Run on macOS (Darwin), 2026-11-23, from the `004-durable-delegation` worktree.

| Check | Result |
|---|---|
| `cargo fmt --all --check` | clean (rustfmt applied once to agent/change-finishing code) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace --no-fail-fast` | 1765 passed, 0 failed, 10 ignored |
| `musaic-cli catalog --check` | in sync (`docs/agent-tools.{json,md}`) |
| frontend `lint`, `typecheck`, `typecheck:test` | clean |
| frontend `npm test` | 501 passed |
| frontend `npm run build` | built |

| `cargo test -p musaic-desktop --test boot -- --ignored` | 2 passed (boot, quit routes), schema v22, macOS |

## Fix made while checking

`api_offline_capture_replays` failed: `research_library_changed` publishes two
empty `AgentStateChanged` events (`work` and `operations`), but the replay's
`research_epoch` delta filtered only `work`. The delta now filters both.

## Limitations (not verified)

- Only macOS was run; Linux and Windows are left to CI.
- The 10 ignored tests were not run, except the desktop boot test (below).
- Private test data was not available, so tests that need it printed "skipped" and passed.
- No real external-agent end-to-end run and no host/version matrix (the release
  criterion in the Notion parent page) was done. This evidence covers the
  repository's own fixtures and transcripts only.
