# Quickstart: validating the agent bridge

Prerequisites (from the repo root):

```bash
(cd frontend && npm ci && npm run build)
# synthetic audio library (venv in a scratch dir), from rust/:
python3 testdata/synthetic-library/gen_fixture_library.py
git checkout rust/testdata/synthetic-library/library.manifest.json
```

## 1. Automated

```bash
cd rust
cargo test -p musaic-commands              # registry, schemas, unknown-field rejection
cargo run -p musaic-cli -- catalog --check # generated catalog matches
cargo test -p musaic-db                    # v21 migration, fresh-db golden
cargo test -p musaic-app --test ops_agent  # pairing, policy, lockout, proposals, approvals, operations
cargo test -p musaic-api --test agent_routes   # guard: hostile Host/Origin, cookie-only, size, content type
cargo test -p musaic-cli                   # descriptor validation, handshake, stale/missing app, setup, stdout purity
cargo test -p musaic-mcp                   # both profiles over stdio
cargo test --workspace
(cd ../frontend && npx vitest run src/pages/settings src/hooks/useReview.test.ts)
```

Expected: all pass. `ops_boundary`, `thin_handlers`, `operations_inventory`,
`ops_all_groups` and `stream_equivalence` keep passing.

## 2. Manual, with a scratch config and offline network

```bash
cd rust
CFG="$(mktemp -d)/config"
cargo run -p musaic-api -- --config-dir "$CFG" --verification-memory-secrets --network offline &
# point the library at a COPY of rust/testdata/synthetic-library/library, scan, then:
curl -s -X POST -H 'Content-Type: application/json' -H "Host: 127.0.0.1:8765" \
  -d '{"name":"test","scopes":["library.read","proposal.submit","tags.write"]}' \
  http://127.0.0.1:8765/api/agent/pairings
cargo run -p musaic-cli -- --config-dir "$CFG" --dev-credentials doctor
cargo run -p musaic-cli -- --config-dir "$CFG" --dev-credentials library-list --input '{"limit":5}'
```

Expected:
- `doctor` reports a valid descriptor, the pairing as active, and both profiles.
- `library-list` returns the fixture albums, and contains no path.

Then:
- Send `musaic_fix_preview` and then `musaic_fix_apply`. With `tags.write` set to Ask,
  `musaic_fix_apply` returns `awaiting_approval`.
- Approve through `/api/agent/approvals/{id}/approve`.
- `musaic_operation_get` returns `succeeded`, and the copy's FLAC tags have changed.
- Stop the server, then run `doctor`. It reports `APP_NOT_RUNNING`.

## 3. Not automatable in this run (report as not verified)

- A real Claude Desktop or Claude Code connection, with the transcript saved and secrets
  stripped. Claude Code has since been recorded for both profiles in
  `evidence/host-connection.md`; Claude Desktop has not.
- Installer builds on each platform with the bundled CLI at its documented location.
- macOS Keychain prompt behaviour on first CLI read.
