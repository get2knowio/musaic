# Data model: External agents only

Schema v23 → v24, one migration (`v23_to_v24` in `rust/crates/musaic-db/src/migrate.rs`, SQL in a new `research_v24.sql`). Times on delegated work are milliseconds since the epoch, as today; times on history records are the original ISO-8601 text, copied verbatim.

`delegated_work.sql` is **not edited**: the v21→v22 step includes it, and an older file migrating through v22 must get the v22 schema. Everything v24 adds is in its own file.

## Where each piece of an old job lives after v24

| Old `ai_jobs` column | New home | Notes |
| --- | --- | --- |
| `id` | `research_history.id`; `delegated_work.legacy_job_id` on its requests | link already written by v22 |
| `album_id` | `research_history.album_id` (and each request's `album_id`) | moved on rename, adoption, merge |
| `album_path`, `album_title` | `research_history` | title shown in history |
| `job_type` | `research_history.job_type` | unknown types kept as text |
| `hint` | `research_history.hint` | native requests keep the hint in `input_json` (`WorkContext.hint`) as today |
| `status`, `acted_on_action` | `research_history`; the request's `state` already mapped by v22 | history view shows the request state where one exists, else the old status |
| `error` | `research_history.error`; request `last_error` (v22) | |
| `result_json` | `research_history.result_json` | read by unconverted-result conversion, cover preview, no-cover rule |
| `created_at`, `started_at`, `completed_at`, `acted_on_at` | `research_history`, verbatim | |
| `attach_cover` | `research_history.attach_cover` | |
| combined job link | requests share `group_id` and `legacy_job_id` | unchanged from v22 |

A **history-only record** is a `research_history` row with no `delegated_work` row pointing at it (an unknown type, `legacy_job_id` unmatched). It is never listed to an agent (agents read only `delegated_work`), never retryable or reviewable, and blocks nothing.

## New tables

### `research_history`

| Column | Type | Notes |
| --- | --- | --- |
| `id` | TEXT PRIMARY KEY | the old job id |
| `album_id` | TEXT NOT NULL | may name an album that no longer exists (shown as an unknown album) |
| `album_path` | TEXT NOT NULL | |
| `album_title` | TEXT | |
| `job_type` | TEXT NOT NULL | `metadata`, `cover`, `metadata+cover`, or any old text |
| `hint` | TEXT NOT NULL DEFAULT '' | |
| `status` | TEXT NOT NULL | old status text, unvalidated |
| `acted_on_action` | TEXT | |
| `error` | TEXT | |
| `result_json` | TEXT | unvalidated: may be unreadable (shown as an unsupported old result) |
| `created_at` | TEXT NOT NULL | |
| `started_at`, `completed_at`, `acted_on_at` | TEXT | |
| `attach_cover` | INTEGER NOT NULL DEFAULT 1 | |
| `migrated_at` | INTEGER NOT NULL | ms, the v24 step's clock |

Indexes: `research_history_album (album_id)`, `research_history_created (created_at, id)`.

Never updated except `album_id` (id moves) and deleted by Clear / delete.

### `work_batches`

| Column | Type | Notes |
| --- | --- | --- |
| `id` | TEXT PRIMARY KEY | opaque UUID, the batch reference |
| `library_epoch` | TEXT NOT NULL | epoch at creation |
| `check_id` | TEXT NOT NULL | origin check |
| `started_at` | INTEGER NOT NULL | when the run started (ms) |
| `finished_at` | INTEGER | set only when the loop reaches its end; a run stopped part-way (by the listener, a lost stream, a quit or a crash) leaves it NULL, and NULL with no run in progress means "unfinished run" |

Index: `work_batches_epoch (library_epoch, started_at)`.

Created in the transaction of the run's first request. Deleted when Clear leaves it no request. A library-epoch rotation (restore, root switch) does not delete it: its unresolved items are invalidated like any other work by `rotate_in`, and the batch is listed with them in that state (spec edge case). Batches are listed whatever their epoch, newest first.

## Changed tables

- `delegated_work`: `ADD COLUMN batch_id TEXT REFERENCES work_batches(id)` (NULL default, so SQLite allows it in `ALTER TABLE`). Set at insert, never changed. Index `delegated_work_batch ON delegated_work(batch_id, state, id) WHERE batch_id IS NOT NULL`.
- `delegation_events`: `ADD COLUMN batch_id TEXT`, written by `delegation_events::record` from the work row in the same transaction.
- `work_replays`: rebuilt (create `work_replays_v24`, copy, drop, rename) so `operation IN ('claim','complete','claim_many')`. No table references it.
- `settings`: rows `ai_provider_id`, `ai_model_id`, `ai_max_parallel`, `ai_queue_paused` deleted. `ai_api_key` and `firecrawl_api_key` rows are left to the key scrub, which runs after open (it must also reach snapshots and backups).

## Removed

- Table `ai_jobs` and its indexes `idx_ai_jobs_status`, `idx_ai_jobs_album`, `idx_ai_jobs_created`.

## The v24 step, in order (one IMMEDIATE transaction)

1. Re-read `schema_version`; do nothing unless it is 23.
2. `CREATE TABLE research_history …`, `CREATE TABLE work_batches …`, their indexes.
3. `ALTER TABLE delegated_work ADD COLUMN batch_id …`; create `delegated_work_batch`.
4. `ALTER TABLE delegation_events ADD COLUMN batch_id TEXT`.
5. Rebuild `work_replays` with the widened check.
6. `INSERT OR IGNORE INTO research_history SELECT …, <now> FROM ai_jobs`.
7. Back-fill: for each `ai_jobs` row of a known type with no `delegated_work` row whose `legacy_job_id` is its id, insert its requests with the v22 mapping (shared function `delegated_work::link_job(tx, epoch, &JobRow)`, extracted in place from `migrate_legacy`; v24 passes the existing epoch). Test harnesses that seed old jobs use `research_history::import_legacy_job` (`#[doc(hidden)]`), which runs steps 6 and 7 for one row. Only builds with a test provider wrote such rows after v22; shipped files have none, but the step does not assume it.
8. Delete the four retired settings rows.
9. `DROP INDEX IF EXISTS` the three job indexes; `DROP TABLE IF EXISTS ai_jobs`.
10. `UPDATE schema_version SET version = 24`; commit. `migrate::upgrade` gains `if found <= 23 { v23_to_v24 }`, so a fresh database (created at v16, then upgraded) gets v24 too.

Before this step exists, v22→v23 is changed to write the literal `23`.

**Idempotence**: the version check makes a second run a no-op; within the step, `OR IGNORE` and `IF EXISTS` make the body safe to re-run on a hand-replayed copy. Interruption is tested with a trigger that aborts the version update to 24: nothing is committed, and the next open completes the step.

**Album rows**: the step reads `albums` nowhere and deletes no album row (rule 6). Rows whose album no longer exists are copied like any other.

## Reads that move to the single store

| Reader | Today (`ai_jobs`) | After v24 |
| --- | --- | --- |
| history projection (`desktop_research::group`, `children`, `jobs`) | title, hint, times, `historical_result` from `ai_jobs`; unlinked rows via `list_unmapped` | `research_history` joined by `legacy_job_id`; history-only records listed from `research_history` with no link |
| legacy hydration and conversion (`desktop_research::hydrate`, `research/legacy.rs`) | `ai_jobs::get_job` | `research_history::get` (same fields) |
| cover preview of an old result (`ops/apply.rs:394-408`) | `ai_jobs.result_json` | `research_history.result_json` |
| merge refusal and repoint (`merge.rs`) | active statuses; `UPDATE ai_jobs` | `delegated_work` unresolved states; repoint `delegated_work` and `research_history` |
| conversion blockers (`multi_disc.rs` `active_jobs`, `set_blockers`) | `ai_jobs` | `delegated_work::unresolved_for_albums` (album id, `group_id`, kind, state, job-style status `queued`/`running`/`completed`; built on the existing `UNRESOLVED` state list) |
| id moves (`renames.rs`) | `ai_jobs` in `move_album_rows` and `ALBUM_ID_HOLDERS` | `delegated_work`, `research_history` |
| database info (`backup.rs:71`) | `COUNT(*) FROM ai_jobs` | requests (all epochs) + history-only records; wire key unchanged |
| queue counts (`research_jobs::queue_counts`) | unlinked `ai_jobs` + groups | groups only |
| no-cover-art suppression (`no_cover_art.rs:37-52`) | failed, or acted on with empty `$.cover.cover_url`, within the window | a **native** `cover.research` request (`legacy_job_id IS NULL`) of the album created within the window that is `failed`, or `applied`/`discarded` with no cover recommendation in its result (outcome `no_match`/unresolved included); or a `research_history` record of type `cover`/`metadata+cover` that is `failed`, or acted on with empty `$.cover.cover_url` (the old rule, on the old data). A request linked to an old job is judged only by its history record: it has no `work_results` row, so the native rule would wrongly read every applied old cover job as "no candidate" |
| sweep inputs digest (`inputs.rs:167-174`) | job rows | `delegated_work` rows (`id, kind, state, result_id, legacy_job_id, created_at`; not `revision` or `updated_at`, which every lease renewal and progress report bumps) and `research_history` rows (`id, job_type, status, acted_on_at`); `INPUTS_VERSION` → `check-inputs/3` (every check reruns once after upgrade) |
| job log (`ai.rs::job_transitions`) | `ai_jobs` | removed with the worker |

## Derived values

- **Batch counts by state**: `SELECT state, count(*) FROM delegated_work WHERE batch_id=? GROUP BY state`.
- **Batch size**: total of the counts.
- **Unfinished run**: `finished_at IS NULL` and the batch is not the one the running quick-fix-all holds (in memory, `App`).
- **Clear count** (`queue_state.clearable`, and what `delete_many(['acted_on'])` returns): `count(delegated_work WHERE state IN ('applied','discarded'))` (all epochs) + `count(research_history with no linked request WHERE status='acted_on')` (history-only records; `acted_on` with any action other than `applied` is discarded, the v22 mapping). History records whose linked requests are all removed by the same Clear are removed too, uncounted (research D13).

## Validation rules

- `batch_id` must name a batch of the current epoch at insert.
- Claim-many `max` 1..=25; renew-many 1..=25 leases; `musaic_work_list` page size stays at most 100.
- At most one unresolved request per album, kind and input digest (the existing `delegated_work_unresolved` index); a bulk run that meets an existing one counts it as already requested and leaves its batch membership unchanged.

## State transitions

Unchanged from spec 004 (`specs/004-durable-delegation/data-model.md`). Batch cancel applies the existing desktop cancel (`queued`, `claimed`, `running` → `cancelled`) to each item; `awaiting_review` and finished items are untouched.
