# Quickstart: Validating the Operations Surface

How to prove, after any slice and at the end, that the feature works and nothing changed.
Commands run from `rust/` unless stated. Details of the types are in
[data-model.md](data-model.md) and [contracts/](contracts/).

## Prerequisites

```bash
(cd frontend && npm ci && npm run build)          # the desktop crate copies frontend/dist
# the synthetic audio library, in a venv with the pinned tool versions (see CLAUDE.md)
python3 testdata/synthetic-library/gen_fixture_library.py
git checkout testdata/synthetic-library/library.manifest.json   # timestamp-only diff
```

## 1. Nothing observable changed (after every slice)

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
git diff --stat main -- testdata/golden crates/musaic-api/tests ../frontend
```

Expected: all green. The `git diff` shows **no modified** file under `testdata/golden`,
no modified pre-existing file under `crates/musaic-api/tests` (new files are allowed), and
nothing under `frontend/`.

If a golden or an existing HTTP test would have to change: stop, do not edit it, flag it.

## 2. The surface works without HTTP

```bash
cargo test -p musaic-app --test 'ops_*'
```

Expected: one passing test file per converted group. At the end this includes:

- a scan of the synthetic library, driven and observed through `ops().scan()` alone,
  whose events are `start`, then album / progress / marked-missing events, then exactly
  one terminal event;
- a mutating operation called while a restore guard is held, returning `OpError::Busy`
  with the library unchanged;
- the same operation called with an invalid input while the restore guard is held,
  returning `OpError::InvalidInput`.

## 3. The boundary holds

```bash
cargo test -p musaic-app --test ops_boundary
cargo test -p musaic-api --test thin_handlers
cargo test -p musaic-api --test operations_inventory
cargo test -p musaic-api --test stream_equivalence     # exists from the closing slice
```

Expected: pass. To see each one fail (in a scratch change, then discard it):

| Scratch change | Test that fails |
|---|---|
| add `axum` to `musaic-app/Cargo.toml` | `ops_boundary` |
| write `http::StatusCode` in a file under `musaic-app/src/ops/` | `ops_boundary` |
| add `st.begin_work()?` or `st.0.bus.publish(..)` to a converted handler | `thin_handlers` |
| register a new route without an inventory row | `operations_inventory` |

## 4. The app still behaves the same

```bash
cargo run -p musaic-desktop -- --config-dir "$(mktemp -d)/config" --dev-memory-secrets --network offline
```

Point it at a **copy** of a library (never a real one). Scan, open an album, apply a
correction, run a check, run a fix, analyze, create and restore a snapshot. Expected: as
before the work.

```bash
cargo test -p musaic-desktop --test boot -- --ignored --nocapture
(cd ../frontend && npm run lint && npm run typecheck && npm run typecheck:test && npm test)
```

## 5. At the end only

```bash
grep -rn 'HttpError' crates/ --include='*.rs'          # expected: no matches
grep -rnE 'begin_work|\.publish\(|ai_jobs_changed' crates/musaic-api/src/routes/   # expected: no matches
```

And read: the crate map in `CLAUDE.md` describes `musaic_app::ops`; `docs/desktop.md`
says the shell's HTTP server is one face over it; `docs/operations.md` holds the complete
inventory.
