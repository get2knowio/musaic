# The operations layer

Every thing the application does is an **operation**: a blocking function on
`musaic_app::ops`, reached from an `App` (`app.ops().<group>().<operation>(...)`). The HTTP
face in `musaic-api` is a thin adapter over it: parse the request, call one operation,
render the result. A second face (an MCP server, a native UI) calls the same operations and
gets the same guard, the same validation order, the same events and the same follow-up
notifications, because those live in the operation and not in a handler. The rules a face
and a handler must follow are in `specs/001-operations-without-http/contracts/`
(`operations-surface.md`, `errors.md`, `events.md`); this file holds the placement decision
and the per-route inventory.

Groups: `system`, `library`, `scan`, `settings`, `database`, `tags`, `analyze`, `apply`,
`checks`, `fixes`, `jobs`, `agent`.

## Placement decision

The surface lives in `musaic-app`, as the module tree `musaic_app::ops` with one submodule
per group, reached from an `App` handle (`app.ops()`).

- Every precondition an operation must own is held by `App`: the work gate, the bus, the
  scan service, the AI queue host, the clock, the secrets store. An operation that takes the
  guard, acts, publishes and then notifies the job queue can only be written where all four
  are in reach.
- `musaic-services` cannot host it: the AI queue host and `ai_jobs_changed()` sit above it,
  and services run under a guard somebody else took.
- The dependency direction already fits: `musaic-api` and `musaic-desktop` depend on
  `musaic-app`, nothing below does, and a later MCP crate will sit beside `musaic-api`.
  `musaic-app` already has no HTTP dependency (watch channels only, no runtime).
- Rejected: a new crate `musaic-ops` above `musaic-app` (it would need `App`'s internals
  public, which lets a face reach them too), `musaic-services` (above), and methods directly
  on `App` (about thirty public host methods already; a separate handle keeps "what a face
  may call" a reviewable list and gives the source checks one directory to scan).

Operations are synchronous. The HTTP face keeps calling them through `http::blocking`; the
face picks the thread. Source: `specs/001-operations-without-http/research.md` D1 and D2,
requirement FR-019.

## Open points settled

1. **The second SSE channel in `routes/fixes.rs` is the multi-disc conversion.** It is the
   `convert_multi_disc` handler (`POST .../multi-disc-layout/findings/{finding_id}/convert`),
   which answers JSON or, when the request says `Accept: text/event-stream`, a stream
   (`progress`, then one `result` or `refused`), under `WorkKind::Apply`. So it is a ninth
   streamed operation. It is one route with two answers, so the operation takes an optional
   sink and the HTTP face chooses by the `Accept` header. `quick-fix-all` in the same file is
   the other streamed fix (`start`, `progress`, `done`).
2. **`recording_canonical.rs` belongs to `checks`.** Its only caller is
   `GET /api/checks/duplicate-tracks/findings/{finding_id}/canonical` in `routes/checks.rs`:
   a read of a duplicate-tracks finding with no guard and no Missing refusal. It is
   `checks.duplicate_canonical`, converted in the checks slice, not with tags or fixes. The
   app-lifetime cache it needs (`st.0.recording_canonical`) moves with it into the `App`.

## The inventory

The table below has one row per row of `musaic_api::route_table()`, in registration order.
**98 rows**: the 71 routes of the Python app, the 25 `EXTRA_ROUTES`, and 2 routing aliases
(`DELETE /api/ai/jobs/stream`, `POST /api/ai/jobs/active/{album_id}`). **94 rows are
operation-bearing** (each names exactly one operation, which `operations_inventory.rs`
checks); 4 are HTTP face only (the two aliases and the two static-file routes).
**11 rows are streamed**: the scan and its follow-only reader, 5 runs (apply, cover upload, analyze, quick-fix-all and
the conversion, which streams only when asked) and 4 feeds (library events, fingerprint
pool, check runs, AI jobs). That is 11 streamed operations by name; `apply_changes` and
`upload_cover` are two entry points to one apply run (one event type), and `scan.follow`
reads the same scan stream as `scan.subscribe`, so there are nine distinct streams.

Columns: method, path, group, classification (thin adapter, or carries behaviour), what
moves into the operation (or stays HTTP, where said), operation, result type, `OpError`
variants it can return (statuses per `contracts/errors.md`), streamed (`no`, `run`, `scan`,
`feed`) and the order of preconditions where it differs from the default. The default is:
wire parse (422), meaning validation, guard (writes), library configured, target exists,
target not Missing. "body 422" means the structured 422 of `lax`, which stays in
`musaic-api`. Helpers that are not routes (`database.scratch`, `database.restore_allowed`)
are named in the row of the operation that uses them.

<!-- Table format, read by tests/operations_inventory.rs: one markdown table; each data row
starts with two cells, the HTTP method in upper case and the path, each in backticks, exactly
as `route_table()` prints them (`RouteRow { method, path }`: axum's spelling, so a catch-all
is `{*full_path}` and a path parameter keeps its name). Further cells are free text. -->

| Method | Path | Group | Classification | What moves | Operation | Result | Errors | Streamed | Precondition order |
|---|---|---|---|---|---|---|---|---|---|
| `GET` | `/api/health` | system | thin adapter | body construction | `system.health` | Health | none | no | default |
| `GET` | `/api/config` | library | carries behaviour | body construction (the folder check is in the operation) | `library.config` | LibraryConfig | none | no | default |
| `POST` | `/api/config` | library | carries behaviour | guard; folder validation (in the service); body construction (`kept_stored_spelling` only when true) | `library.set_library_dir` | LibraryChoice | Rejected, Busy, Internal | no | body 422, guard, folder checks (`Rejected`). Deviates from FR-008: a restore running answers 409 even for a bad folder, as the route always did (kept so the HTTP answer is unchanged) |
| `GET` | `/api/albums` | library | carries behaviour | guard only when `refresh=true`; library check; snapshot drop on refresh; body construction | `library.list_albums` | AlbumList of AlbumRecord | Busy (refresh only), NoLibrary, Internal | no | query 422, guard if refresh, library configured and present, list |
| `GET` | `/api/scan/status` | scan | thin adapter | body construction | `scan.status` | ScanState | none | no | default |
| `GET` | `/api/library-events/stream` | system | carries behaviour | stream assembly: relay connect before the response, `ready` frame first, ping | `system.library_events` | LibraryEvents (subscription yielding LibraryEvent: name and payload as the relay renders them) | none | feed | default |
| `GET` | `/api/fingerprint-pool/status` | system | thin adapter | body construction | `system.pool_status` | PoolState | none | no | default |
| `GET` | `/api/fingerprint-pool/stream` | system | carries behaviour | stream assembly: subscribe before the first read, coalesce, keepalive frame | `system.pool_feed` | Feed of PoolState | none | feed | default |
| `GET` | `/api/scan/stream` | scan | carries behaviour | guard; library check; start-or-join the scan; stream assembly | `scan.subscribe` | ScanSubscription | Busy, NoLibrary, Internal | scan | query 422, guard, library configured and present, subscribe (a guard not needed is released at once) |
| `POST` | `/api/scan/rescan` | scan | carries behaviour | guard; library check; snapshot drop; start scan; body construction | `scan.rescan` | RescanStarted (library folder and ScanState) | Busy, NoLibrary, Internal | no | guard, then library check (no body to validate) |
| `POST` | `/api/scan/cancel` | scan | thin adapter | body construction (`outcome`, `scan`); no guard, works during a restore | `scan.cancel` | CancelOutcome with ScanState | none | no | default |
| `GET` | `/api/scan/follow` | scan | thin adapter | join the running scan or replay the last one, never start one (`Idle` with no scan since startup, rendered as one `idle` event); no guard, no library check, works during a restore | `scan.follow` | ScanFollow (Following ScanSubscription, or Idle ScanState) | none | scan | default |
| `GET` | `/api/albums/{album_id}` | library | carries behaviour | library check in the handler; body construction | `library.album` | AlbumRecord | NoLibrary, NotFound, Internal | no | library configured, album |
| `DELETE` | `/api/albums/{album_id}` | library | carries behaviour | guard; publish (inside `library_ops`); body construction (`ok`, `deleted_album_id`) | `library.remove_album` | DeletedAlbum | Busy, NoLibrary, NotFound, Internal | no | guard, library, album |
| `GET` | `/api/albums/{album_id}/cover` | library | carries behaviour | album resolve; full-or-thumb choice; embedded-cover bytes. Stays HTTP: ETag, If-None-Match, 304, Cache-Control, file response | `library.cover` | CoverImage (path or bytes, mime) | NoLibrary, NotFound, Failed | no | album, then cover (404 when none) |
| `GET` | `/api/albums/{album_id}/tracks` | library | carries behaviour | album resolve; body construction (`album_id`, `tracks`) | `library.tracks` | TrackList of TrackRecord | NoLibrary, NotFound, Failed | no | default |
| `POST` | `/api/albums/{album_id}/mb-anchor/clear` | library | carries behaviour | guard; publish (inside `library_ops`); body construction (`blocked_mbid`) | `library.clear_mb_anchor` | ClearedAnchor | Busy, NoLibrary, NotFound, Failed, Internal | no | guard, library, album |
| `GET` | `/api/albums/{album_id}/resolution` | library | carries behaviour | guard although a read (it can store fingerprints and an anchor); album resolve; wording of the notes | `library.resolution` | Resolution | Busy, NoLibrary, NotFound, Failed, Internal | no | query 422, guard, album |
| `GET` | `/api/settings` | settings | carries behaviour | body construction (`keys_pending_removal` inserted after `keyring_available`, now in `SettingsRecord::new`) | `settings.get` | SettingsRecord | Internal | no | default |
| `PATCH` | `/api/settings` | settings | carries behaviour | guard; range / provider validation (in the service); `ai_jobs_changed()` notification; fingerprint pool kick when an AcoustID key was set; body construction | `settings.patch` | SettingsRecord | Rejected, Busy, Unavailable, Internal | no | body 422, guard, apply, notify jobs, kick pool, body. Values are checked after the guard (a restore running answers 409 even for a bad value, as the route always did) |
| `GET` | `/api/prompts/acoustid` | settings | thin adapter | body construction | `settings.acoustid_prompt` | AcoustidPrompt | Internal | no | default |
| `POST` | `/api/prompts/acoustid/dismiss` | settings | carries behaviour | guard; publish `SettingsChanged` in the handler; body construction | `settings.dismiss_acoustid_prompt` | AcoustidPrompt | Busy, Internal | no | guard, write, publish |
| `GET` | `/api/ui-prefs` | settings | thin adapter | body construction (range filtering of stored values) | `settings.ui_prefs` | UiPrefs | Internal | no | default |
| `PUT` | `/api/ui-prefs` | settings | carries behaviour | meaning validation (400s) before the guard; guard; writes; publish `SettingsChanged` only when something was written; body construction | `settings.set_ui_prefs` | UiPrefs | Rejected, Busy, Internal | no | body must be an object (400, stays in the handler), field meaning (400, in the operation, in body order), guard, write, publish |
| `GET` | `/api/db/info` | database | carries behaviour | takes the database gate for reading; body construction | `database.info` | DbInfo | Internal | no | default |
| `GET` | `/api/db/backup` | database | carries behaviour | backup copy and its filename (in `db_admin`), written into a scratch directory of its own, removed when the result drops. Stays HTTP: Content-Disposition, file response | `database.backup` | Backup (the BackupFile: path, suggested name) | Internal | no | default |
| `POST` | `/api/db/restore` | database | carries behaviour | restore guard; early busy check before the upload is read (`database.restore_allowed`, a helper the face calls before reading the upload; `database.scratch` holds the staging); temp staging of the upload; body construction. Stays HTTP: multipart reading and the upload cap (the cap becomes TooLarge when the operation takes a reader) | `database.restore_upload` | RestoreInfo | Rejected, Busy, Internal (TooLarge stays a 413 built in the handler, which reads the upload) | no | multipart 422, early busy check (`restore_allowed`), read upload with cap (413), restore guard, restore |
| `GET` | `/api/db/snapshots` | database | carries behaviour | body construction (`valid`, `counts`) | `database.snapshots` | list of SnapshotEntry | Internal | no | default |
| `POST` | `/api/db/snapshots` | database | carries behaviour | guard; body construction | `database.create_snapshot` | CreatedSnapshot | Busy, Conflict (snapshots disabled), Internal | no | guard, create |
| `DELETE` | `/api/db/snapshots/{name}` | database | carries behaviour | guard; name validation; body construction | `database.delete_snapshot` | SnapshotDeleted | Busy, Rejected (bad name), Internal | no | guard, then name check |
| `POST` | `/api/db/snapshots/{name}/restore` | database | carries behaviour | restore guard; body construction | `database.restore_snapshot` | RestoreInfo | Busy, Rejected (bad name or unknown snapshot: a 400 today, so not `NotFound`), Internal | no | restore guard first, then the name is looked at |
| `GET` | `/api/checks/stream` | checks | carries behaviour | stream assembly: the feed value read on a blocking thread, a `state` frame from the summaries (key order of the Python app), a `sweep` frame, ping | `checks.runs_feed` | Feed of CheckRunsState (the checks when they changed, the sweep when it did) | none | feed | default |
| `GET` | `/api/checks/sweep` | checks | thin adapter | body construction | `checks.sweep_status` | SweepState | none | no | default |
| `GET` | `/api/checks/findings` | checks | carries behaviour | filter assembly; body construction (finding wording, taken at the time of the read) | `checks.list_findings` | list of FindingRecord | Internal | no | query 422 |
| `GET` | `/api/checks` | checks | thin adapter | body construction (summaries) | `checks.list` | list of CheckSummaryRecord | Internal | no | default |
| `GET` | `/api/checks/{check_id}` | checks | carries behaviour | 404 for an unknown check decided in the operation; body construction | `checks.get` | CheckDetailRecord | NotFound, Internal | no | default |
| `POST` | `/api/checks/{check_id}/run` | checks | carries behaviour | guard (`CheckRun`); scope validation; library check; run and its log line; body construction | `checks.run` | CheckRun (the run row) | Rejected, Busy, NoLibrary, Internal (an unknown check is `Rejected`, a 400 today, not `NotFound`) | no | body 422, guard, scope (400), library (409), run (400 unknown check, albums scope without ids). The guard is before the meaning validation |
| `POST` | `/api/checks/{check_id}/findings/{finding_id}/ignore` | checks | carries behaviour | guard; the finding must belong to the check; body construction (finding or null) | `checks.ignore_finding` | Option of FindingRecord | Busy, NotFound, Internal | no | path int 422, body 422, guard, ignore |
| `POST` | `/api/checks/{check_id}/findings/{finding_id}/unignore` | checks | carries behaviour | guard; the finding must belong to the check; body construction (finding or null) | `checks.unignore_finding` | Option of FindingRecord | Busy, NotFound, Internal | no | path int 422, guard, unignore |
| `GET` | `/api/checks/filename-mismatch/findings/{finding_id}/plan-preview` | checks | thin adapter | the plan re-derived (`derive_plan`) and its album-gone fallback; body construction | `checks.filename_plan_preview` | PlanPreview (Derived, or AlbumGone) | NotFound, Rejected (finding without an album), Internal | no | path int 422 |
| `GET` | `/api/checks/duplicate-tracks/findings/{finding_id}/cluster-preview` | checks | carries behaviour | the finding read and the checks crate called in the operation (`cluster_preview`); its refusals mapped (404, 400, 500); body is the document the checks crate builds | `checks.cluster_preview` | ClusterPreview (record) | NotFound, Rejected (finding without an album), Internal | no | path int 422 |
| `GET` | `/api/checks/multi-disc-layout/findings/{finding_id}/conversion-preview` | checks | thin adapter | log directory taken from the config dir by the operation; the preview re-derived (`derive_conversion`); body construction; no guard (a read) | `checks.conversion_preview` | ConversionPreview (Derived, or NotInLibrary) | NotFound, Rejected (finding without an album), Internal | no | path int 422 |
| `GET` | `/api/checks/duplicate-tracks/findings/{finding_id}/canonical` | checks | carries behaviour | MusicBrainz client built from the app transport and the app-lifetime cache used by the operation; body construction (`reason_code` in the body); no guard, no Missing refusal | `checks.duplicate_canonical` | CanonicalRecording (title, artist, mbid, reason) | NotFound, Internal | no | path int 422, finding |
| `POST` | `/api/albums/{album_id}/apply/stream` | apply | carries behaviour | meaning validation of `clear` / `track_clear` (422); guard (`Apply`, also for a dry run); album resolve; request assembly; run on a blocking thread with the bus context; stream assembly | `apply.apply_changes` | ApplyOutcome (Finished, or Stopped); the events are ApplyEvent, `UpdatedAlbum` / `RemovedAlbum` / `Error` terminal (a dry run ends with its `Tags(Done)`) | InvalidInput, Busy, NoLibrary, NotFound, Conflict (Missing), Failed (album lookup), Internal | run | body 422, removals 422, guard, album (409 no library, 404, 409 Missing), run. A database failure after the 200 is an `error` item |
| `POST` | `/api/albums/{album_id}/analyze/stream` | analyze | carries behaviour | guard (`Analyze`); album resolve; request assembly; run with the bus context; stream assembly; abort reason logged | `analyze.run` | AnalyzeOutcome (Finished with the summary, or Aborted); the events are AnalyzeEvent | Busy, NoLibrary, NotFound, Conflict (Missing, or the folder cannot be listed), Internal | run | body 422 (`what`), guard, album, run. An aborted run ends the body with no final frame |
| `POST` | `/api/albums/{album_id}/cover/upload` | apply | carries behaviour | guard; image validation (400); album resolve; request assembly; the same apply run as above. Stays HTTP: multipart reading and the cap, both before the guard | `apply.upload_cover` | ApplyOutcome; the same ApplyEvent stream | Busy, Rejected, TooLarge, NoLibrary, NotFound, Conflict (Missing), Failed (album lookup), Internal | run | multipart 422, size cap (413, after the part's type check), guard, image type and content (400), album, run |
| `GET` | `/api/albums/{album_id}/tags` | tags | thin adapter | env assembly (`with_tag_env`) | `tags.album_tags` | AlbumTags (TagSet per track) | NoLibrary, NotFound, Internal | no | default |
| `PUT` | `/api/albums/{album_id}/tags` | tags | carries behaviour | shape validation of `track_tags` (422, body key order); guard; env assembly | `tags.replace_album_tags` | AlbumTags | Busy, NoLibrary, NotFound, Conflict (Missing), Rejected (a name that is not a FLAC of the album), Internal | no | body 422, guard, replace |
| `POST` | `/api/albums/{album_id}/tags` | tags | carries behaviour | guard; env assembly | `tags.add_album_tag` | AlbumTags | Busy, NoLibrary, NotFound, Conflict (Missing, or no tracks), Rejected, Internal | no | body 422, guard, add |
| `DELETE` | `/api/albums/{album_id}/tags/{tag}` | tags | carries behaviour | guard; env assembly | `tags.remove_album_tag` | AlbumTags | Busy, NoLibrary, NotFound, Conflict (Missing), Rejected (blank tag), Internal | no | guard, remove |
| `POST` | `/api/albums/{album_id}/tracks/{filename}/tags` | tags | carries behaviour | guard; env assembly | `tags.add_track_tag` | AlbumTags (the album's whole tag state, as the route answers) | Busy, NoLibrary, NotFound, Conflict (Missing), Rejected, Internal | no | body 422, guard, add |
| `DELETE` | `/api/albums/{album_id}/tracks/{filename}/tags/{tag}` | tags | carries behaviour | guard; env assembly | `tags.remove_track_tag` | AlbumTags | Busy, NoLibrary, NotFound, Conflict (Missing), Rejected, Internal | no | guard, remove |
| `GET` | `/api/tags` | tags | thin adapter | nothing beyond the call | `tags.list_tags` | TagCounts | Internal | no | default |
| `GET` | `/api/tags/{tag}/tracks` | tags | thin adapter | nothing beyond the call | `tags.tracks_for_tag` | TagTracks | Rejected (blank tag), Internal | no | default |
| `GET` | `/api/vocabulary` | tags | thin adapter | nothing beyond the call | `tags.list_vocabulary` | Vocabulary | Internal | no | default |
| `POST` | `/api/vocabulary` | tags | carries behaviour | guard; clock (`now_iso`) | `tags.add_vocabulary_entry` | Vocabulary | Busy, Rejected, Internal | no | body 422, guard, add |
| `POST` | `/api/vocabulary/orphans/{tag}/promote` | tags | carries behaviour | guard; clock | `tags.promote_orphan_tag` | Vocabulary | Busy, Rejected, Internal | no | guard, promote |
| `DELETE` | `/api/vocabulary/{tag}` | tags | carries behaviour | guard | `tags.delete_vocabulary_entry` | VocabularyDeleted | Busy, Rejected, Internal | no | query 422, guard, delete |
| `PATCH` | `/api/vocabulary/{tag}` | tags | carries behaviour | guard | `tags.patch_vocabulary_entry` | Vocabulary | Busy, Rejected, Conflict (several entries fold to the tag), Internal | no | body 422, guard, patch |
| `POST` | `/api/vocabulary/{tag}/merge` | tags | carries behaviour | guard | `tags.merge_vocabulary_entry` | VocabularyMerged | Busy, Rejected, Internal | no | body 422, guard, merge |
| `POST` | `/api/vocabulary/{tag}/rename` | tags | carries behaviour | guard | `tags.rename_vocabulary_entry` | VocabularyRenamed | Busy, Rejected, Conflict (the new name is taken), Internal | no | body 422, guard, rename |
| `POST` | `/api/checks/{check_id}/findings/{finding_id}/fix` | fixes | thin adapter | guard (`Other`) before the lookup; fix flow; job enqueue inside it; `ai_jobs_changed()` after a fix that enqueued, once the guard is released | `fixes.fix` | FixResult | Busy, NotFound, Rejected (check not registered, no fix path), Internal | no | path int 422, body 422, guard, lookup, fix, notify jobs only if a job was enqueued |
| `POST` | `/api/checks/{check_id}/quick-fix-all` | fixes | thin adapter | guard (`QuickFixAll`); unknown check; the loop under `in_bulk_run`; the pause hook (a test seam); one `BulkBatchCompleted` | `fixes.quick_fix_all` | QuickFixEnd (`Finished(QuickFixSummary)`, the `done` item, or `Stopped`) | Busy, NotFound, Internal | run | guard, unknown check (404), then the stream. A loop that fails is an `error` item; a database failure before the first item is an `error` item too |
| `POST` | `/api/checks/filename-mismatch/findings/{finding_id}/apply` | fixes | thin adapter | guard; env assembly; a refusal carries the rename code's `reason_code` and `user_message` as typed details | `fixes.apply_filename_rename` | RenameOutcome | Busy, NotFound, Rejected, Conflict (Missing album; a rename that did nothing, with details), Internal | no | path int 422, body 422, guard, rename |
| `POST` | `/api/checks/duplicate-tracks/findings/{finding_id}/delete-file` | fixes | thin adapter | guard; env assembly | `fixes.delete_duplicate_file` | DuplicateDeleted | Busy, NotFound, Rejected, Conflict (album folder unreachable), Internal | no | path int 422, body 422, guard, delete |
| `POST` | `/api/checks/inconsistent-artist-sort/findings/{finding_id}/apply-sort` | fixes | thin adapter | guard; env assembly | `fixes.apply_cluster_sort` | SortResult | Busy, NotFound, Rejected, Internal | no | path int 422, body 422, guard, apply |
| `POST` | `/api/checks/multi-disc-layout/findings/{finding_id}/convert` | fixes | thin adapter | guard (`Apply`); the `Accept: text/event-stream` choice between JSON and stream stays HTTP; convert env with a typed progress report (droppable); a refusal carries `reason_code`, `user_message`, `blockers` or the fresh `digest` as typed details | `fixes.convert_multi_disc` | ConversionRecord (the answer is the return value; the sink hears `Started`, then `Progress`) | Busy, Rejected, NotFound, Conflict (with details), Failed (with details), Internal | run | path int 422, body 422 (`digest`), guard, convert (re-derives the plan, refuses on a stale digest). With an `Accept` stream a refusal after the guard is a `refused` item, not a status |
| `POST` | `/api/checks/multi-disc-layout/findings/{finding_id}/abandon-conversion` | fixes | thin adapter | guard (`Apply`); same refusal details | `fixes.abandon_multi_disc` | AbandonRecord | Busy, Rejected, NotFound, Conflict (with details), Failed (with details), Internal | no | path int 422, guard, abandon |
| `GET` | `/api/ai/jobs` | jobs | thin adapter | status list splitting (values not trimmed; the HTTP face splits, the operation takes a slice); body construction | `jobs.list` | Vec of Job | Internal | no | default |
| `POST` | `/api/ai/jobs` | jobs | thin adapter | guard; job-type check before the database; enqueue; 409 body carries the existing job; notification (in the operation) | `jobs.enqueue` | Job | Rejected, Busy, NotFound, Conflict (with the existing job), Internal | no | body 422, guard, job type (400), enqueue (404, 409) |
| `DELETE` | `/api/ai/jobs` | jobs | thin adapter | guard; status-list meaning (400, 409 for `running`); notification | `jobs.delete_many` | DeletedJobs (count and statuses) | Rejected, Busy, Conflict, Internal | no | status missing 422, guard, status list (400), delete (409), notify jobs |
| `GET` | `/api/ai/jobs/stream` | jobs | thin adapter | stream assembly: subscribe before the first snapshot, `jobs` frame per change, ping | `jobs.feed` | Feed of Result of JobsList | none | feed | default |
| `DELETE` | `/api/ai/jobs/stream` | HTTP face only | carries behaviour | routing alias: Starlette sends it to `DELETE /api/ai/jobs/{job_id}` with id `stream`; no operation of its own | - | - | - | no | as DELETE /api/ai/jobs/{job_id} |
| `GET` | `/api/ai/jobs/active/{album_id}` | jobs | thin adapter | body construction (job or null) | `jobs.active_for_album` | Option of Job | Internal | no | default |
| `POST` | `/api/ai/jobs/active/{album_id}` | HTTP face only | carries behaviour | routing alias: `acted-on` and `retry` reach the job routes with id `active`; anything else is a 405; no operation of its own | - | - | - | no | as the aliased route |
| `GET` | `/api/ai/jobs/{job_id}` | jobs | thin adapter | body construction | `jobs.get` | Job | NotFound, Internal | no | default |
| `DELETE` | `/api/ai/jobs/{job_id}` | jobs | thin adapter | guard; notification | `jobs.delete` | DeletedJob | Busy, NotFound, Conflict (running), Internal | no | guard, delete (404, 409), notify jobs |
| `POST` | `/api/ai/jobs/{job_id}/acted-on` | jobs | thin adapter | guard; action meaning (400); notification | `jobs.mark_acted_on` | Job | Rejected, Busy, NotFound, Conflict, Internal | no | body 422, guard, action (400), mark (404, 409), notify jobs |
| `POST` | `/api/ai/jobs/{job_id}/retry` | jobs | thin adapter | guard; notification; 409 body carries the existing job | `jobs.retry` | Job | Busy, NotFound, Conflict (with the existing job), Internal | no | guard, retry (404, 409), notify jobs |
| `GET` | `/api/ai/jobs/{job_id}/cover-preview` | apply | carries behaviour | job read; cover URL taken from the stored result; remote fetch (`fetch_preview`). Stays HTTP: the response headers (no-store, nosniff, CSP) | `apply.job_cover_preview` | CoverPreview (bytes, mime) | NotFound, Upstream, Internal | no | job, stored URL (404), fetch (502) |
| `GET` | `/api/albums/{album_id}/cover-preview` | apply | carries behaviour | album existence and Missing refusal; the cover address derived from the anchor; the fetch; a 404 from the archive means no cover. Stays HTTP: the response headers | `apply.album_cover_preview` | CoverPreview (bytes, mime) | NotFound, Conflict (Missing), Upstream, Internal | no | album (404), Missing (409), anchor URL (404), fetch (502, or 404 when the archive has none) |
| `POST` | `/api/ai/queue/resume` | jobs | thin adapter | guard; notification (in the operation) | `jobs.resume_queue` | QueueState | Busy, Internal | no | guard, resume |
| `GET` | `/api/ai/providers` | jobs | thin adapter | body construction | `jobs.providers` | ProviderList | Internal | no | default |
| `GET` | `/api/agent/pairings` | agent | thin adapter | body construction (never a secret) | `agent.list_pairings` | list of PairingRecord (scopes and the effective decision per permission) | Internal | no | default |
| `POST` | `/api/agent/pairings` | agent | carries behaviour | name and scope validation; the 20-pairing cap; the secret goes to the OS keyring (service `musaic-agent`) and is never returned; publishes `AgentStateChanged` | `agent.create_pairing` | CreatedPairing (the record and how to connect) | InvalidInput, Conflict (20 active), Unavailable, Internal | no | body, cap, secret stored, row written, publish |
| `DELETE` | `/api/agent/pairings/{pairing_id}` | agent | carries behaviour | drops the pairing's tokens, deletes its keyring entry, denies the cards waiting on it; publishes `AgentStateChanged` | `agent.revoke_pairing` | PairingRecord | NotFound, Internal | no | pairing, revoke, tokens, secret, cards |
| `PUT` | `/api/agent/pairings/{pairing_id}/policy` | agent | thin adapter | decision validation (a read or a preview cannot be Ask; a permission outside the scope cannot be set); publishes `AgentStateChanged` | `agent.set_policy` | PairingRecord | InvalidInput, NotFound, Internal | no | pairing, validation, write |
| `GET` | `/api/agent/pairings/{pairing_id}/activity` | agent | thin adapter | body construction (tool, outcome, time; no argument values) | `agent.activity` | list of ActivityRecord (last 100) | NotFound, Internal | no | default |
| `GET` | `/api/agent/approvals` | agent | thin adapter | body construction; the diff is a label only | `agent.list_approvals` | list of ApprovalRecord | Internal | no | query `state` (pending or all) |
| `POST` | `/api/agent/approvals/{approval_id}/approve` | agent | carries behaviour | the decision happens once; an apply then runs under the work guard after revalidating everything (revision, files, expiry, pairing); a start spawns its worker; publishes `AgentStateChanged` | `agent.decide_approval` | ApprovalRecord | NotFound, Conflict (no longer pending), Internal | no | card, decision, revalidation, before-image, apply |
| `POST` | `/api/agent/approvals/{approval_id}/deny` | agent | thin adapter | the decision happens once; nothing is written; publishes `AgentStateChanged` | `agent.decide_approval` | ApprovalRecord | NotFound, Conflict (no longer pending), Internal | no | card, decision |
| `GET` | `/api/agent/changes` | agent | thin adapter | body construction; whether Undo would run now is derived from the files (state, album present, the files still hold what the change left); nothing is written | `agent.list_changes` | list of ChangeRecord (newest first, at most 20) | NotFound (unknown pairing), Internal | no | query `pairing` (optional) |
| `POST` | `/api/agent/changes/{change_id}/undo` | agent | carries behaviour | desktop only (no agent tool); takes the apply guard; refuses a change that is not applied or partial, already undone, an undo itself, on a Missing album, or whose files changed since (listing what); records its own before-image, renames back through the rename service, restores tags, pictures and cover files through the apply pipeline (`AlbumRenamed`, `AlbumTagsChanged`, `AlbumCoverChanged` as any rename and apply), marks the change `undone`, records an activity row, publishes `AgentStateChanged(changes)` | `agent.undo_change` | UndoOutcome (the change, the undo's record id, complete or what was not put back) | NotFound, Restoring, Busy, Conflict (state, Missing album), ChangedSince (409 with `changed`), Unavailable (nothing put back), Internal | no | guard, change, state, album, changed since, saved copies, own before-image, renames, apply, finish |
| `POST` | `/__musaic/agent/v1/hello` | agent | thin adapter | the agent route guard (loopback peer, exact Host, no Origin, JSON, 256 KiB) | `agent.hello` | HelloResponse (instance id, one-time nonce) | none | no | guard only |
| `POST` | `/__musaic/agent/v1/exchange` | agent | thin adapter | mutual HMAC proof of the pairing secret; every failure is one `NOT_PAIRED`; lockout after 5 failures in a minute | `agent.exchange` | ExchangeResponse (token, scopes, server proof) | NotPaired, LockedOut | no | guard, lockout, nonce, instance, pairing, secret, proof |
| `GET` | `/__musaic/agent/v1/tools` | agent | thin adapter | bearer token; the catalog is rendered by musaic-commands | `agent.list_tools` | RegistryInfo | NotPaired | no | guard, token |
| `POST` | `/__musaic/agent/v1/tools/{tool_id}` | agent | carries behaviour | bearer token; the body is parsed by `musaic_commands::parse_call` in the route; scope and policy decide before anything runs; reads take no work guard; writes take the apply guard after revalidation and a recorded before-image | `agent.call_tool` | ToolOutput (typed per tool) | the contract's tool errors, rendered as `{ok:false, error}` with status 200 | no | guard, token, parse, authorization, then the tool's own order |
| `GET` | `/` | HTTP face only | thin adapter | static file: `index.html` of the built UI; 404 without a UI dir | - | - | - | no | default |
| `GET` | `/{*full_path}` | HTTP face only | carries behaviour | static files and the SPA fallback; refuses `api/` paths, contains the path under the UI dir, `/assets/*` 404s when missing | - | - | - | no | default |

## Deviations and decisions

Where the code differs from the spec's ideal order (FR-008: invalid input, then guard, then
the rest) or from the first inventory, and why. **FR-008's order follows today's routes**:
the HTTP answer must not change, so each operation keeps the order its route had (several
take the guard before the meaning validation, listed per row and below).

- `POST /api/config` takes the guard before it checks the folder (400), so a restore running
  answers 409 even for a path that is not a directory. FR-008 and `contracts/errors.md` rule 4
  want the invalid input reported first; no test pins either order, and the HTTP answer must
  not change, so `library.set_library_dir` follows the route's order (guard, then folder).
  The other routes of the library and scan groups already matched their operations.
- The album lookup of the tracks, cover, anchor-clear and resolution routes answered a
  database failure as `500 {"detail": <cause>}`, not the plain-text 500 that `Internal` gives.
  `OpError::Failed` (status 500, cause kept in `detail`) carries exactly that case; nothing
  else raises it.
- `POST /api/db/restore` stages the upload in a `database.scratch` directory and hands it to
  `database.restore_upload`, which removes it right after the swap (as the route did), not
  after the scrub. `database.restore_from` restores from a file the caller keeps.
- The desktop shell's reveal-in-file-manager finds the album folder with
  `library.album_folder` (the same lookup as the tracks route), so the shell goes through
  `ops` like the HTTP face.
- `App::start_scan` and `App::scan_and_subscribe` are `pub(crate)`: only `ops/scan.rs` calls them.

- `POST /api/checks/{check_id}/run` takes the guard before it checks the scope (400) and the
  library (409), so a restore running answers 409 even for a bad scope. The lifecycle in
  `data-model.md` says validation first; this route is the exception it allows for.
- Several handlers answer an error with more than `detail`: the filename rename, the
  multi-disc convert and abandon (`reason_code`, `user_message`, `blockers` or a fresh
  `digest`, and their own status), and the job enqueue and retry conflicts (the existing
  job). `OpError::Conflict` needs a way to carry that, or those operations return a typed
  refusal of their own.
- Where today's meaning validation answers 400 (scope, UI preferences, job type, snapshot
  name, status list, rename and merge inputs), `contracts/errors.md` maps `InvalidInput` to
  422 and `Rejected` to 400. The Errors column therefore says `Rejected` for those and
  keeps `InvalidInput` for the apply removals check, which is a 422 today. T004 confirms.
- `GET .../resolution` takes the guard although it is a read (it can store fingerprints and
  an anchor), and `GET .../albums?refresh=true` takes it only then.
- `GET /api/ai/jobs/{job_id}/cover-preview` is placed in `apply` with the album cover
  preview (research D10), although its URL sits under the jobs.
- The two preview routes in `routes/preview.rs` and `cluster_preview` / `duplicate_canonical`
  in `routes/checks.rs` do their work in the handler (reading rows, deriving a URL, calling
  a client), not through a service. These move whole.
- Behaviour that stays in the HTTP face: multipart reading and the upload caps, ETag and 304
  for covers, response headers of the previews, `Content-Disposition` of the backup, SSE
  framing, pings, the `Accept` choice of the conversion, the Starlette routing aliases and
  the static files.

- The settings and database group (slice 3). `PATCH /api/settings` checks its values
  (400, 503) after the guard, so a restore running answers 409 even for a bad value;
  `FR-008` wants the invalid input first, and the HTTP answer must not change (as for
  `POST /api/config`). `PUT /api/ui-prefs` already validated first, and does in the operation.
  `T031` asked for `InvalidInput` for a bad patch value; today's answer is 400, so it is
  `Rejected` (as `contracts/errors.md` says).
- An unknown snapshot name on `POST /api/db/snapshots/{name}/restore` and every other
  `RestoreError` (including a database failure during the swap) answer 400 today, so
  they are all `Rejected`, not `NotFound` / `Internal`.
- A failed patch can have written the fields before the refused one (secrets included) and
  publishes `SettingsChanged` for them; `settings.patch` keeps that, and also wakes the job
  queue for them (and restarts the fingerprint pool when the AcoustID key was among them),
  which the route did not: an unpause saved before the refusal took effect only at the
  queue's next periodic wake (#312).
- The backup is written to a scratch directory the result owns (`database.backup`; `backup_to`
  writes into a directory the caller names) and no longer holds the database file lock while the HTTP face reads the copy into its response.
- `SettingsRecord` keeps `serde_json` (in `record.rs`, the allow-list) because the
  settings body's floats are rendered by `serde_json`, not `py_float_repr`; `musaic-app`
  therefore has `serde_json` as a normal dependency.

- The tags group (slice 4). Today's statuses govern, not the `NotFound` the inventory first had: no tag or
  vocabulary route answers 404 for a tag (a patch of an unknown tag changes nothing and
  answers 200), vocabulary `merge` and `delete` refusals are 400 (`Rejected`), `rename` and
  `patch` refusals are 409 (`Conflict`). A write to a Missing album is a 409 (`Conflict`,
  the apply text), after the album lookup and before the tag is checked; reads answer.
- The six album / track tag routes all answer the album's whole tag state, so the first-draft
  `TrackTags` result is `AlbumTags` for the track routes too. `TagSet` (in `record.rs`) is
  the tags of one track or the tags every track has.
- The album lookup answers a database failure with the plain-text 500 on the tag routes
  (`Internal`), where the library routes keep the cause in the body (`Failed`); the tags
  service maps `Failed` to `Internal`.
- A tag write publishes no event, as before: the recorded behaviour (the golden) has none and
  nothing subscribes to a tag. The task text expected `AlbumTagsChanged`; publishing it would
  change what the library-events stream carries, so it is left for a decision.
- `golden_tags_vocab.rs` moved from `musaic-services/tests` to `musaic-api/tests`
  (assertions unchanged): the body it compares is built by `render::tags` now. It still
  includes `musaic-services/tests/deltas/tags_vocab.rs`, which `tags_vocab_http.rs` includes
  by path, so that file stays where it is.

- The analyze group (slice 5), the first run with a sink:
  `analyze().run(album_id, what, sink: &mut dyn FnMut(AnalyzeEvent) -> Listening) ->
  Result<AnalyzeOutcome, OpError>`. Refusals (guard, library, album, Missing, unreachable
  folder) are returned before the sink is called; the order is the route's (guard, then the
  album lookup). The terminal event is `Done`; `Aborted` (the folder vanished after the
  checks) is `Ok(Aborted)` with no terminal event, as the body ended with no final frame
  before, and the operation logs it (target `musaic.api.analyze`, unchanged). A sink that
  panics counts as `Gone`.
  `analyze().run_cancellable(album_id, what, cancel, sink)` (the agent bridge's, #349) also
  takes a cancel flag, looked at only before each file: once set, no further file starts,
  no album gain is written, the album is rescanned and `AlbumTagsChanged` published when a
  file was written, `Done` carries the partial counts and the outcome is `Cancelled`. The
  route has no flag.
- Each of the three runs (`analyze.run`, `apply.apply_changes`, `apply.upload_cover`) is
  also offered in two steps: `prepare` / `prepare_changes` / `prepare_upload` take the
  guard and find the album, writing nothing, and return a run (`AnalyzeRun`, `ApplyRun`)
  whose `run(sink)` does the work. The HTTP face awaits the first step and only then
  starts the second, so a client that leaves during the album lookup starts no run, as
  before the operations existed (the handler was dropped while it awaited the lookup).
  The one-call form is the two steps back to back.
- The HTTP handler learns of a refusal through a one-shot: the operation runs on a blocking
  thread, the sink signals "first event out", a refusal signals the error, and the handler
  answers a status for the one and the stream for the other. SSE channel capacity 1 stays
  in the handler.
- `golden_analyze.rs` moved from `musaic-services/tests` to `musaic-api/tests`, and the
  analysis half of `rewrite_warnings.rs` to `analyze_rewrite_warnings.rs` (the apply half
  stays): both compare the rendered items. Assertions unchanged except that a refusal's
  status is read through `ApiError::from(OpError)` and the outcome carries `OpError`.
- `ApplyItem` and its `sse_frame` stay for the apply group; `analyze.rs` no longer uses
  them.

- The apply group (slice 6), the second run with a sink:
  `apply().apply_changes(album_id, &ApplyChanges, sink)` and
  `apply().upload_cover(album_id, content_type, bytes, sink)`, each returning
  `Result<ApplyOutcome, OpError>` (`Finished`, or `Stopped` when the sink went away
  and a dry run ended early). `ApplyEvent`: `CoverDownloading`, `CoverDownloaded`,
  `Tags(TagEvent)` (the tag writer's `Start`, `FileStarted`, `FileDone`,
  `FileRenamed`, `Done`), `CoverSaved`, `UpdatedAlbum(AlbumRecord)`, `RemovedAlbum`,
  `Error`. It is the services' `ApplyEvent<A>` with the album as an `AlbumRecord`
  (`ApplyEvent<AlbumRecord>`); the services use it with the album document, and
  `ops::apply::typed` converts (one list of steps, #314).
- The order is today's. An apply checks `clear` / `track_clear` first (422, as the
  handler did, rendered from the typed `RemovalProblem`s the operation also uses
  for its `InvalidInput`), then takes the guard (`Apply`, a dry run included), then
  the album (409 no library, 404, 409 Missing). An upload takes the guard unlabelled
  (`WorkKind::Other`, as the route did, not `Apply`), then checks the image type and
  content (400 `Rejected`, `Empty file` is 400 too), the size (413 `TooLarge`, the
  service's check; the handler's own 413 for the request body cap stays in the
  handler), then the album. `OpError::Failed` carries the album lookup's database
  failure, as for the library routes.
- A database failure after the 200 is still an `error` item: the operation opens a
  second connection for the run itself and answers `Error { "database unavailable" }`
  when that open fails, as the handler did.
- "Cover download" is the `cover_url` of an apply: the pipeline fetches it through
  `musaic-net`'s cover client (`check_public_url`: https, public hosts, every redirect
  checked) via `App::with_apply_env`. There is no separate download route.
- `apply_changes` has no separate "clear": the removals are part of its request.
- The preview operations keep the log line (`musaic.api.preview`) and their 502
  (`Upstream`). The `Cache-Control`, `nosniff` and CSP headers stay in the handler.
  `routes::preview` keeps its public message constants (re-exported from the
  operations) because `tests/cover_preview_http.rs` imports them.
- `refuse_missing`, `resolve_album` and `validate_cover_upload` in `musaic-services`
  return `OpError` now. `stored_cover_url` moved to `apply.rs` in
  services (the operations may not name `serde_json`).
- `golden_apply.rs`, `remove_tags.rs` and the apply half of `rewrite_warnings.rs`
  (now `apply_rewrite_warnings.rs`) moved from `musaic-services/tests` to
  `musaic-api/tests`, assertions unchanged except that an item is read through
  `render::apply` and a refusal's status through `ApiError::from(OpError)`;
  `tag_event_json` moved with them into `render::apply`. `renumber_pattern.rs` and
  `logging_seams.rs` stay and read the typed events.

- The checks group (slice 7). `checks.run` follows the route: the guard (`CheckRun`), then the
  scope (`Rejected`), then the library (`NoLibrary`, "No library configured"), then the run. An
  unknown check or an `albums` scope without ids is `Rejected` (a 400 today), not the `NotFound`
  the first inventory had; only `checks.get` and the finding lookups are `NotFound`.
  `checks.run_cancellable(…, cancel)` (the agent bridge's, #349) stops the check before its
  next album once the flag is set: the run ends `failed` ("check run cancelled") with the
  findings it wrote, resolves nothing, records no inputs and is still reported in
  `ChecksRerunFinished`. The route has no flag.
- The check summaries, a check's detail, a finding and the cluster preview are record newtypes
  (`CheckSummaryRecord`, `CheckDetailRecord`, `FindingRecord`, `ClusterPreview`, in `record.rs`):
  they are documents the checks crate builds from stored data (a finding's detail is stored text
  of any shape; the cluster's `recording_mbid` is copied as stored) in the key order its tests pin.
  The plan preview, the conversion preview and the canonical recording are typed (`PlanPreview`,
  `ConversionPreview`, `CanonicalRecording`); the JSON built from them moved to
  `render::checks`. The run is the stored row (`CheckRun` = `RunRow`).
- `checks.runs_feed()` is a `Feed<CheckRunsState>` over two counters (the checks tick and the
  sweep's change counter): a value says which of the two changed (`CheckList::Unchanged`, or
  `sweep: None`), so the stream sends a `state` frame only for a change of the checks and a
  `sweep` frame only for a change of the sweep, as before. A change of both between two reads is
  one value and the two frames in the old order. A value reads the database, so a face on an async
  thread waits with the new `Feed::wait` (resolves when a value is ready, reads nothing) and reads
  with `next(Duration::ZERO)` on a blocking thread, which is what the stream handler does.
  A list that cannot be read ends the stream, as before.
- `recording_canonical_golden.rs` moved from `musaic-services/tests` to `musaic-api/tests`,
  assertions unchanged except that the body is read through `render::checks::canonical` and an
  error's status through `ApiError::from(OpError)`. It skips without the private replay data, as
  before.
- `routes::checks::ordered` (the key-order helper of the stream's `state` frame) moved to
  `render::checks`. `check_ops::run_check_logged` takes a connection, not an opener, and no longer
  has an error type parameter.

- The fixes group (slice 8). `fixes().fix` takes the guard (`Other`) before the finding is
  looked up (a restore answers 409 even for an unknown finding), and calls
  `ai_jobs_changed()` itself once the guard is released, only when a job was enqueued. With no
  model backend a fix that would need one answers `ok: false` with the not-available message and
  enqueues nothing (as before).
- `quick_fix_all` is a run with a sink (`QuickFixEvent`: `Start`, `Progress`, `Done`, `Error`);
  `QuickFixItem` and its `sse_frame` are gone. A sink that answers `Gone` (or panics) stops the
  loop at the next finding, as a client that went away always did, and the one
  `BulkBatchCompleted` is still published; this is the route's behaviour kept, not the
  "finish the rest" of the other runs. The gate rule for `QuickFixAll` (FR-030) is the
  gate's only rule: a restore is refused while work runs and work while a restore runs, and nothing
  else, so a second quick-fix-all started while one runs is admitted and runs beside it (a
  second HTTP client gets the same stream). The stream's refusals (409, 404) are statuses
  before the first item; a database failure before the first item stays an `error` item.
- The conversion is the ninth streamed operation: `convert_multi_disc(finding_id, digest,
  Option<sink>)` returns its answer, and the sink hears `Started` (the guard is held; always
  first, so the HTTP face can tell a restore's 409 from a refusal that is an item), then
  `Progress { step, done, total }`. A refusal after `Started` is the return value (the stream
  renders it as a `refused` item). `convert` and `abandon` in `musaic-services` still return the
  JSON document because pre-existing `musaic-api` tests call them; progress is reported only
  typed, through `ConvertEnv::on_progress` (the JSON callback went in #315), and the operation's result is the record newtype `ConversionRecord` / `AbandonRecord` (in
  `record.rs`: the document keeps its sorted key order).
- A conversion refusal's `reason_code`, `user_message`, `blockers` and `digest` are
  `Details::Refusal` on `Rejected` / `Conflict`. A 404 refusal is `NotFound`
  (`Subject::Finding`, or `Subject::Album` for "album no longer in the library") and the renderer
  names its code from the subject; a 500 refusal (database error, record that could not be
  written) is `OpError::Failed`, which now carries `details` too, so its body keeps `detail`,
  `reason_code` and `user_message`. `OpError::Failed` is built with `OpError::failed(cause)`.
- `check_fixes::apply_filename_rename` no longer bridges `refuse_missing` to `HttpError`; a failed
  read of the album there is still the plain-text 500 (`Failed` becomes `Internal`, as the old
  bridge made it).
- `HttpError` no longer has a caller in the jobs group: `ai_jobs.rs` returns `OpError`, so
  `fix_flow::fix_finding` no longer maps a `JobError` itself (the type is gone).
- `golden_check_fixes.rs` moved from `musaic-services/tests` to `musaic-api/tests` (assertions
  unchanged except that a body or frame is read through `render::fixes` and a refusal's status
  through `ApiError::from(OpError)`). It still includes `musaic-services/tests/deltas/check_fixes.rs`,
  which `check_fixes_http.rs` includes by path. `fix_name_search_anchor.rs` and
  `recording_canonical_golden.rs` pin no moved body and stay (the latter moved in slice 7).
- `AppState::begin_work_as` is gone (nothing calls it).

- The jobs group (slice 9). `ai_jobs.rs` returns `OpError` (`JobRefusal` / `JobError` / `Conflict`
  are gone): 400 refusals are `Rejected`, 404 `NotFound` (`Subject::Job`, or `Subject::Album` for an
  album not in the cache), the 409 refusals `Conflict`. An album with an active job (enqueue, or a
  retry whose album has another) is `Conflict` with `Details::ExistingJob`, and
  `render::jobs::job_error` renders it as the `{"detail": {error, message, existing_job_id,
  existing_status}}` body. A job is the database row (`Job` = `musaic_db::ai_jobs::JobRow`), not a
  record newtype: it holds no document; `render::jobs` builds the Python app's key order
  (`job_to_py`, whose `result` is parsed there). The check fix's own enqueue (`enqueue_for_fix`)
  keeps its "an active job is an answer" shape and calls the database directly.
- Every writer takes the guard first (a restore running answers 409 even for a bad job type, status
  list or action, as the routes always did) and wakes the feeds itself: the services still take a
  `notify` callback (the acted-on test in `musaic-services` pins it), and the operation passes
  `ai_jobs_changed()`. A refusal, an enqueue that met an active job and a bulk delete that matched
  no row wake nobody.
- `jobs.feed()` is `Feed<Result<JobsList, OpError>>`, not `Feed<JobsList>`: a value reads the
  database, and today's stream answers a failure of the first read as an error response and skips a
  later one. The route reads the first value before the response begins and keeps the channel
  capacity (2) and the ping. The `jobs` frame is `Py::dumps` text, as before.
- `jobs.delete_many` takes a slice of statuses (the HTTP face splits the query on `,`); trimming and
  dropping blanks are the operation's. There is no cancel operation: no route cancels a job, the
  inventory's `acted-on` and `retry` are the ones that change a finished job.
- `musaic_app::jobs` keeps only `App::ai_provider_infos`, `ai_provider_ids` and `subscribe_jobs`
  (`ai_provider_infos` and `ai_provider_ids` are `pub(crate)` now; the settings and fixes tests call `subscribe_jobs`); `job_body`, `jobs_body`, `conflict_body`, `deleted_body`,
  `bulk_deleted_body`, `jobs_stream_payload`, `App::enqueue_job`, `enqueue_job_for_fix`,
  `resume_queue` and `ai_providers` are gone. `AppState::begin_work` and `conn` lost their last
  callers and are deleted with them.

## What stays public in `musaic-app`, and why

`AppState::begin_work` and `begin_work_as` are gone; the work gate is reached only inside an
operation (`self.app.gate().begin_work()`), and `thin_handlers.rs` bans `begin_work` in any
file under `routes/`. The guard-taking helpers are `pub(crate)`: `start_scan`,
`scan_and_subscribe`, `cancel_scan`, `cancel_scan_and_wait`, `with_apply_env`,
`with_analyze_env`, `with_tag_env`, `with_fix_env`, `ai_provider_infos`, `ai_provider_ids`.
These stay `pub`, because an existing test or another crate calls them:

- `App::with_convert_env`: called by `musaic-api/tests/multi_disc_convert_http.rs` and
  `multi_disc_gather_http.rs`, which must not be edited.
- `App::subscribe_jobs`: called by `musaic-app/tests/ops_fixes.rs` and `ops_settings.rs`.
- `App::gate()`, `begin_shutdown`, `shutdown_signal`, the builder and the other host methods
  the desktop shell and the tests use (not guard-taking helpers).

## Remaining deviations, in one list

- FR-008 order: see the rows' last column; `POST /api/config`, `PATCH /api/settings`,
  `POST /api/checks/{id}/run`, `fixes.fix`, the job writers and `GET .../resolution` take the
  guard before the meaning validation, as their routes always did.
- Tag writes publish no event, as today (no subscriber wants one).
- Meaning validation answers 400 (`Rejected`), not the 422 `InvalidInput` that
  `contracts/errors.md` first mapped; `InvalidInput` stays for the apply removals check.
- A quick-fix-all whose sink goes away stops the loop (the route's behaviour), unlike the
  other runs, which finish.
- `OpError::Failed` exists (a 500 with a cause in the body) and `OpError` can carry `Details`
  (refusal and existing-job bodies); neither was in the first contract.
- `thin_handlers.rs` scans every file under `routes/` but `mod.rs`; its allow-list of
  `musaic_services::` paths is empty and its allowed state methods are the HTTP-only settings
  (`frontend_dir`, `ping_interval`, `allowed_hosts`, `cover_body_limit`,
  `restore_upload_limit`) plus `ops` and `shutdown_signal`.
