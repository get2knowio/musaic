# Quickstart: verifying spec 006

Prerequisites: `(cd frontend && npm ci && npm run build)`; the synthetic library (CLAUDE.md, "The synthetic audio library"). Never use `~/.musaic` or a real library; every run below uses a scratch config dir, in-memory keys and `--network offline`.

## Checks

```bash
cd rust
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p musaic-cli -- catalog --check        # 23 tools, events with batchId, skill 1.1.0
cd ../frontend && npm run lint && npm run typecheck && npm run typecheck:test && npm test && npm run build
```

Targeted:

```bash
cargo test -p musaic-db --test v24_migration
cargo test -p musaic-db --test delegated_work claim_many
cargo test -p musaic-db --test merge_album_rows
cargo test -p musaic-app --test ops_fixes
cargo test -p musaic-app --test ops_jobs
cargo test -p musaic-api --test operations_inventory --test thin_handlers --test stream_equivalence
cargo test -p musaic-cli --test no_db_owner
```

## By hand

Setup, once per scenario:

```bash
SCRATCH="$(mktemp -d)"; mkdir -p "$SCRATCH/config"
cp -R rust/testdata/synthetic-library/library "$SCRATCH/library"
cd rust
cargo run -p musaic-desktop -- --config-dir "$SCRATCH/config" --dev-memory-secrets --network offline
```

Point Musaic at `$SCRATCH/library`, scan, and let the checks run. For an assistant: Settings → Agents → **Add an assistant**, then drive the tools by hand with
`cargo run -p musaic-cli -- call --config-dir "$SCRATCH/config" --dev-credentials [--pairing <id>] <tool-cli-name> --input '<json>'` (`cargo run -p musaic-cli -- tools` lists the CLI names). Remove `$SCRATCH` afterwards.

### US1 — Fix sends what the resolver cannot settle

1. Open a "no cover art" finding (offline, the resolver finds nothing). Choose **Fix**.
2. Expect: the hint dialog opens; the tooltip says MusicBrainz and the Cover Art Archive are tried first and the rest goes to the assistant.
3. Enter a hint, confirm. Expect "Research requested. You can follow it in Review."; the finding shows research pending; the request is in Review and in Activity under **Research requests**.
4. Repeat for unidentified album, tags out of sync, track gaps. Fix again on the same finding: no second request.
5. Revoke the assistant and repeat on another album: the request shows **Waiting for an assistant**, never failed.

### US2 — Fix all makes one batch

1. Open **Fix all** for "Unidentified album". Expect the checkbox "Send what the resolver can't settle to my assistant" on (an assistant was added) and the number of findings in scope. With no assistant added, it starts off.
2. Run it. Expect the summary "Settled A. Sent B for research (one batch)…", one entry "B research requests from Fix all · Unidentified album" in Review and in Activity, with counts by state; opening it lists the albums.
3. `work-list --input '{"batchId":"<id>"}'` lists only the batch; `work-claim-many --input '{"batchId":"<id>","max":25,"idempotencyKey":"manual-claim-0001"}'` returns up to 25 claims, each with its own lease. Repeat the same call: same claims. `work-renew-many` with those leases renews them.
4. **Cancel** the batch in Review. Expect waiting and held items cancelled; any item awaiting review stays.
5. Run Fix all with the checkbox off: no batch, no request. Run it with one album's folder moved away (Missing after a rescan): no request for that album.

### US3 — No provider to configure

Walk Settings, Settings → Agents, Activity, the album panel, Review and the finding buttons. Expect no model-provider, model, parallelism, queue-pause, model-key or FireCrawl setting; no "not available in this build", "AI job", "AI backend", "Resume queue"; Settings → Agents reads Added / Connected / Listening for work / Last request picked up; the album panel always offers **Request research** for a present album with no unresolved request.

### US4 — Upgrade keeps history

```bash
SCRATCH="$(mktemp -d)"; mkdir -p "$SCRATCH/config"
cp rust/testdata/db/v22-jobs.db "$SCRATCH/config/library.db"   # also v20-jobs.db, v23-jobs.db, rust/testdata/golden/persistence/old-schemas/v16.db
```

Before starting, note the old rows: `sqlite3 "$SCRATCH/config/library.db" "SELECT id,job_type,status,acted_on_action,hint,created_at FROM ai_jobs"`. Start the app on that config dir. Expect a `library-<tag>-v22.db` snapshot in the config dir; `ai_jobs` gone; every old row in Activity history with the same title, hint, times and outcome, the `lyrics` row as history only (no Retry, no review); the old completed result reviewable, with its cover preview. Quit and start again: `sqlite3 … .dump` before and after the second start is identical. Try to merge or convert a set whose member has an unresolved request: refused, naming the request.

### US5 — Keys removed once

Start on a scratch dir whose database (copy of a v16 fixture) has `ai_api_key` and `firecrawl_api_key` rows (the in-memory key store cannot be seeded by hand; the keyring half is covered by the startup test of slice 4). After start: neither row in `settings`, `GET /api/settings` has neither `_set` field, `server.log` names neither value. Start again: no keyring delete is attempted (`retired_keys_removed` present).
