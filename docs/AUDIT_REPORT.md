# Media Manager — Independent Audit Report

Audit conducted 2026-05-05 against the working tree at
`/Users/paul/Documents/Claude/Projects/Media Manager`. No git history was
available in the workspace mount; findings are derived from a direct read of
the source.

Scope:
- `media_manager/checks/*` (six checks + runner + `_apply.py`)
- `media_manager/{cache,fingerprint,mb_lookup,ai,trash}.py`
- Worker side-effects in `media_manager/main.py`
- Frontend `frontend/src/lib/types.ts`,
  `frontend/src/components/FindingFixButton.tsx`,
  `frontend/src/components/DetailSheet/{index,AISuggestions}.tsx`

Severity legend: 🔴 Bug · 🟡 Inconsistency · 🟢 Hygiene.

---

## 🔴 Bugs

### 1. `_persist_ai_mbid` URL is constructed from the search endpoint, not the detail endpoint

- **File:** `media_manager/main.py:1286-1306`
- **What:** The verification call is built with
  `mb_lookup.MB_RELEASE_GROUP_SEARCH.rstrip("/") + f"/{mbid}"`. The constant
  `MB_RELEASE_GROUP_SEARCH = "https://musicbrainz.org/ws/2/release-group/"`
  happens to be the same prefix as the MB release-group detail endpoint, so
  the URL works *by coincidence*. The intent is to hit
  `MB_RELEASE_GROUP_DETAIL` (which doesn't exist in `mb_lookup`).  The
  docstring also says “We HEAD the release-group endpoint” but the call uses
  `requests.get(…)`. Renaming or repointing `MB_RELEASE_GROUP_SEARCH` in the
  future will silently break AI MBID verification.
- **Fix:** Add an explicit
  `MB_RELEASE_GROUP_DETAIL = "https://musicbrainz.org/ws/2/release-group/{mbid}"`
  constant in `mb_lookup.py` (same shape as `MB_RELEASE_DETAIL`) and call
  `requests.get(MB_RELEASE_GROUP_DETAIL.format(mbid=mbid), …)`.  Update the
  docstring to match (it says HEAD; the code uses GET — the GET is fine, the
  comment is wrong).

### 2. `/api/ai/suggest-metadata/{album_id}` response drops `mb_release_group_mbid`

- **File:** `media_manager/main.py:489-515` (the response dict)
- **What:** `MetadataSuggestion` carries `mb_release_group_mbid`, the worker
  job result includes it (`main.py:1377`), the frontend
  `MetadataSuggestionResponse` type declares it
  (`frontend/src/lib/types.ts:97`), and `AISuggestions.tsx` reads it
  (`metadata?.mb_release_group_mbid`, line 89). But the *synchronous*
  `/api/ai/suggest-metadata/{album_id}` endpoint returns a hand-built dict
  that omits the field entirely. Any caller of the sync endpoint will see
  `mbid` as undefined and the `MbidStatusLine` will render "n/a" even when
  AI confidently identified the album.  The job-queue path is the primary
  surface in current UI, so most users won't hit this — but the type
  contract is broken.
  - As a related issue, the sync endpoint *also* never triggers
    `_persist_ai_mbid` (that's wired only in `_run_one_job`), so the AI
    anchor is silently never written when this endpoint is the entry point.
- **Fix:** Either (a) deprecate / remove the sync endpoint if the queue path
  has fully replaced it, or (b) add `"mb_release_group_mbid":
  result.mb_release_group_mbid` to the response dict and call
  `_persist_ai_mbid` (in a thread) before returning.

### 3. `delete_duplicate_file` reads `library_path` from a dict that never contains it

- **File:** `media_manager/main.py:1114` and
  `media_manager/cache.py:_row_to_album` (~`cache.py:56-94`)
- **What:** `library_dir = album.get("library_path") or
  str(album_path.parent.resolve())` — but `_row_to_album` doesn't include
  `library_path` in its returned dict (verified by inspection: there is no
  `"library_path": row["library_path"]` line). So the `.get()` always
  returns `None` and the code always falls through to
  `album_path.parent.resolve()`.  That works *only* when the album sits
  exactly one level under the library root. For nested layouts (multi-disc
  rips, year-based subfolders, etc.) the upserted row will land under the
  wrong `library_path`, which then ghost-orphans the album from
  `cache.list_albums(library_path)` lookups.
- **Fix:** Either add `"library_path": row["library_path"]` to
  `_row_to_album` so the existing `or` logic actually has something to
  read, or — simpler — replicate what `_apply.py` does and pull
  `cfg.library_dir` directly via `config_module.load_config()`.

### 4. `MissingMbid.try_quick_fix` returns `FixResult` instead of `None` for unknown-album case

- **File:** `media_manager/checks/missing_mbid.py:138-141`
- **What:** When `finding.album_id is None`, `try_quick_fix` returns `None`
  (correct — runner treats as not-applicable). But when the album lookup
  fails, it returns `FixResult(ok=False, …)`. The bulk dispatcher in
  `main.py:1148-…` (`quick_fix_all`) maps `FixResult(ok=False)` to
  `outcome="skipped"` rather than `"not-applicable"`. That's harmless except
  the count then increments the "skipped" bucket for what is really a cache
  miss (an environmental issue, not "MB couldn't find it"). Compare against
  the parallel methods in `missing_core_tags.py:140-143` and
  `no_cover_art.py:154-156` — they use the same pattern, so this is
  consistent across the family. Not a bug per se, but the bucket label is
  misleading.
- **Fix:** Either change the convention to return `None` for unknown-album
  (it's not a fixable finding), or add a third outcome label
  (`"album-missing"`) so the UI can distinguish "MB had no answer" from
  "the cache forgot the album existed." Low priority.

---

## 🟡 Inconsistencies

### 5. AI MBID confidence sentinel and MB Lucene score share a constant value but not a meaning

- **Files:** `media_manager/main.py:1271` (`_AI_MBID_CONFIDENCE = 88`) vs
  `media_manager/mb_lookup.py:66` (`HIGH_CONFIDENCE_SCORE = 88`)
- **What:** Both paths write `score=88` to `mb_match_score` on a confident
  anchor. The deterministic path writes whatever MB returned (90, 100,
  etc.); the AI path *always* writes exactly 88 as a sentinel. Downstream
  reads (`find_release_cached`,  `_is_stale`) treat them identically, so
  there is no live bug, but observability is muddied: if you see a row
  with score=88 in the DB you can't tell whether MB scored it
  exactly-on-the-threshold or whether it was AI. The two paths also live in
  different modules — the comment near `_AI_MBID_CONFIDENCE` documents the
  parallel but the symbol isn't imported from `mb_lookup`, so a future
  threshold bump in `mb_lookup` won't auto-update the AI path.
- **Fix:** Reference `mb_lookup.HIGH_CONFIDENCE_SCORE` directly instead of
  redefining a copy. Optionally distinguish the AI path with a different
  score (e.g. `HIGH_CONFIDENCE_SCORE - 1` so it's still confident but
  visually distinct in row dumps).

### 6. Comment in `FindingFixButton.tsx` is stale w.r.t. `missing-mbid`

- **File:** `frontend/src/components/FindingFixButton.tsx:23-25, 223-225`
- **What:** Two block comments still describe `missing-mbid` as a
  deterministic check that fires straight through with no dialog:
  - L23–25: "Deterministic checks (missing-mbid, missing-fingerprints) don't
    enqueue AI jobs at all — their fix path is synchronous."
  - L223–225: "Deterministic check (currently missing-mbid /
    missing-fingerprints): fire straight through, no dialog."
  Both are now wrong: `missing-mbid` is `fix_kind="ai-job"`, has entries in
  both `JOB_TYPES_FOR_CHECK` and `HINT_MODE_FOR_CHECK`, and DOES open the
  hint dialog and enqueue a metadata job when MB-direct fails. The actual
  code paths handle `missing-mbid` correctly; only the comments lie.
- **Fix:** Update both comments to say "deterministic checks
  (missing-fingerprints, duplicate-tracks) — `missing-mbid` is a hybrid that
  tries MB-direct and falls through to AI."

### 7. `duplicate_tracks.py` module docstring still describes the old MBID-grouping design

- **File:** `media_manager/checks/duplicate_tracks.py:1-28`
- **What:** The opening docstring (lines 5–7) reads: "Groups
  `track_fingerprints` rows on `(album_id, acoustid_recording_mbid)` having
  count > 1 — i.e. multiple files in the same album that AcoustID
  identified as the same canonical recording." That's the *previous*
  design. The current implementation pulls every row and clusters by
  pairwise Chromaprint similarity (lines 100-115 explicitly reverse this
  design and explain why). The module docstring contradicts the class
  docstring three pages later. A reader skimming the file header will get
  the wrong mental model.
- **Fix:** Rewrite the docstring's first paragraph to match the
  Chromaprint-clustering implementation. The class-level docstring
  (`run`, lines 97-115) is already accurate — paste a condensed version
  of that into the module header.

### 8. `acoustid_score` selected from DB but never used in duplicate-tracks

- **File:** `media_manager/checks/duplicate_tracks.py:128-149`
- **What:** The SELECT pulls `acoustid_score`, but the dict comprehension
  at line 144-149 only stores `filename / duration_s / fingerprint / mbid`.
  The score is dropped on the floor. Comments at L41-48 and L120-126
  carefully explain *why* the score filter was removed, but the column is
  still in the SELECT — wasted I/O and reader confusion ("oh, is the score
  used elsewhere? *grep* … no, it's just dead").
- **Fix:** Drop `acoustid_score` from the SELECT.

### 9. `find_release_cached` short-circuits but misses one cache write

- **File:** `media_manager/mb_lookup.py:271-341`
- **What:** When the cache hit path calls `_fetch_tracklist`, it can update
  `match.release_mbid` (line 331). But that fresher `release_mbid` is *not*
  written back to the anchor cache — only the original `find_release()`
  path persists via `cache.set_album_mb_anchor`. So the next
  `find_release_cached` call still has the stale (or null)
  `release_mbid`. AI-anchored albums (`release_mbid=None` after the
  worker's `_persist_ai_mbid` call at `main.py:1310`) will keep re-fetching
  the tracklist forever rather than caching the discovered release. Not
  catastrophic — `_fetch_tracklist` is one MB call, not a search — but it's
  also free to fix.
- **Fix:** After the `_fetch_tracklist` block in the cache-hit branch,
  call `cache.set_album_mb_anchor` again (same score, same group MBID,
  newly-discovered `release_mbid`) when `release_mbid` is non-empty and
  changed.

### 10. `MissingCoreTags.try_quick_fix` returns `ok=False` after a partial apply

- **File:** `media_manager/checks/missing_core_tags.py:135-155`
- **What:** When MB partially fills (e.g. title+year but not artist),
  `_try_mb` returns `_MBOutcome(fully_resolved=False, applied=True, …)`.
  `try_quick_fix` sees `not fully_resolved` and returns `FixResult(ok=False,
  message=mb_outcome.message …)`. From the caller's perspective in
  `main.py:1180` (`quick_fix_all`), `ok=False` increments the "skipped"
  bucket — even though a write actually happened. The user sees "12 skipped"
  in the bulk-run summary while N albums were partially fixed. The detail
  comment at lines 149-151 acknowledges the issue ("A partial apply may
  have happened — that's still progress").
- **Fix:** Either return `FixResult(ok=True, message=…)` for partial
  applies (treating "we made progress" as success) — and accept that the
  finding row will reopen on the next scan with a smaller `missing_fields`
  detail — or extend the bulk-run protocol with a `"partial"` outcome that
  the SSE stream can count separately.

---

## 🟢 Hygiene

### 11. `cover_lookup.find_cover` references in comments — module no longer exists

- **Files:** `media_manager/main.py:1342`,
  `media_manager/mb_lookup.py:3, 432, 436`,
  `media_manager/ai.py:663, 762`
- **What:** Multiple comments say "fall through to `cover_lookup.find_cover`"
  or "the older `cover_lookup.find_cover()`". The module was renamed to
  `mb_lookup` (and `find_cover` is now the back-compat shim at
  `mb_lookup.py:435`). Stale `__pycache__/cover_lookup.cpython-{310,312}.pyc`
  files are also still on disk under
  `media_manager/__pycache__/`. No bug, but `grep` now produces noise.
- **Fix:** Update comments to say `mb_lookup.find_cover`. Optional: remove
  the orphan `.pyc` files.

### 12. `_persist_ai_mbid` docstring says "HEAD" but code uses GET

- **File:** `media_manager/main.py:1281-1283`
- **What:** "We HEAD the release-group endpoint before writing — 200 means
  it's real". The implementation at line 1288 is `requests.get(…)`. (Either
  works; GET is fine since MB returns small JSON; but the doc lies.)
- **Fix:** Change "HEAD" to "GET" in the docstring, or change the call to
  `requests.head(url, allow_redirects=True, …)` to match the doc.

### 13. `DuplicateTracks.fix()` is a stub but its docstring contradicts itself

- **File:** `media_manager/checks/duplicate_tracks.py:157-180`
- **What:** Doc says it's "a stub" but also that it "should treat
  duplicate-tracks as non-actionable in bulk mode." In reality the bulk
  dispatcher uses `try_quick_fix` (which is *not* overridden — default
  returns None → "not-applicable"), so `fix()` is reached only via the
  per-finding `fix_finding(finding_id)` runner path. The frontend handles
  `duplicate-tracks` specially in `FindingFixButton.tsx:121-139` and never
  calls the standard fix endpoint. So `DuplicateTracks.fix()` is effectively
  dead code: it can only be reached if a future caller forgets the special
  case.
- **Fix:** Either delete `fix()` and let the base-class `NotImplementedError`
  trigger if anyone reaches it (defense in depth), or simplify the
  docstring to "Defensive fallback. The frontend has a custom dialog; this
  method should never be called in normal flow."

### 14. `NoCoverArt.run` doesn't filter on the `metadata` job type

- **File:** `media_manager/checks/no_cover_art.py:84-96`
- **What:** The SQL says
  `j.job_type IN ('cover', 'metadata+cover')` for "recent failed lookups."
  But pure `metadata` jobs can ALSO produce a cover side-effect via the
  worker writing the cover URL to the album row (no — wait, only `cover`
  and `metadata+cover` produce cover URLs; metadata-only does not). So
  this is correct. No issue. (Initially flagged this as a possible bug but
  on re-read it's right; leaving the note as a hygiene-level explanation
  for the next reader who might wonder.)
- **Fix:** Add a one-liner comment in the SQL block explaining why
  `metadata` is excluded ("metadata-only jobs never produce cover_url").

### 15. `_run_post_scan_checks` runs `asyncio.run(...)` per check

- **File:** `media_manager/main.py:191-203`
- **What:** Five `runs_during_scan=True` checks → five separate
  `asyncio.run` calls per scan. Each spins up its own loop, runs sequentially,
  no parallelism. Not a correctness issue but the `_run_post_scan_checks_async`
  variant (lines 206-228) does the same thing serially with `await`. Both
  paths suggest later parallelism but neither tries it.
- **Fix:** No change required; future optimization would be to gather
  the coroutines.

### 16. Stale comment about `track-gaps` in `runner.py:_resolve_unseen`

- **File:** `media_manager/checks/runner.py:270-281`
- **What:** Docstring says "We don't run two concurrent runs of the same
  check, so any row not freshly upserted here is by definition stale." This
  is correct *given* serial dispatch (see #15) but the comment doesn't
  cite where that serialization is enforced — it's only because
  `_run_post_scan_checks` calls `asyncio.run` sequentially. If we ever
  parallelize the runner, this assumption breaks silently.
- **Fix:** Either add a module-level lock or leave a TODO referencing
  this constraint.

### 17. Empty `__pycache__` entries / orphan compiled modules

- **Files:** `media_manager/__pycache__/cover_lookup.cpython-{310,312}.pyc`
- **What:** As mentioned in #11; module was deleted, `.pyc` artifacts
  remain. Won't be re-imported (no source file) but it's confusing.
- **Fix:** `find . -name "*.pyc" | xargs rm` or check in a `.gitignore`
  rule for `__pycache__/`.

### 18. No `tests/` directory

- **What:** Confirmed via glob — no `tests/` dir, no `test_*.py` in repo
  root. Diagnostic scripts (`diagnose_album.py`,
  `diagnose_chromaprint.py`, `diagnose_claude.py`,
  `diagnose_fingerprints.py`) exist but are one-off scripts that exercise
  paths against live data, not assertions. So no out-of-date tests to
  flag — but also no automated coverage of the audit's high-risk areas
  (cache preservation, MB anchor write, AI MBID validation,
  similarity-based clustering).
- **Fix:** Beyond audit scope; mentioned for completeness.

---

## Cross-cutting summary

**Health-check meta consistency** is broadly correct:
- All six `Check` subclasses implement `fix(self, finding, *, user_hint="")`
  with matching signatures. Deterministic ones (`missing-fingerprints`,
  `duplicate-tracks`) accept and ignore `user_hint`; AI-job ones plumb it
  through.
- `fix_kind` values match the implementation: `ai-job` for the four checks
  that enqueue jobs (`missing-mbid`, `missing-core-tags`, `track-gaps`,
  `no-cover-art`), `deterministic` for `missing-fingerprints` and
  `duplicate-tracks`. (`duplicate-tracks` is borderline — its `fix()` is a
  no-op stub and the real action goes through a custom endpoint, but the
  `deterministic` label is defensible.)
- All four AI-job checks honor the `if not user_hint: try_mb_first(); else
  go_straight_to_AI` cascade (no_cover_art:124, missing_core_tags:106,
  track_gaps:120, missing_mbid:105). Pattern is consistent.

**MusicBrainz identification — three paths cohere:**
- `find_release` writes the anchor on confident match
  (`mb_lookup.py:253-264`).
- `find_release_cached` reads it back and skips the search.
- `_persist_ai_mbid` writes via `cache.set_album_mb_anchor` after live
  verification.
All three end up in the same column (`mb_release_group_mbid`); see #5
for the score-comparability nit.

**`save_snapshot` rewrite is consistent:**
- `cache.save_snapshot` (lines 238-296) now uses the same
  `INSERT … ON CONFLICT(id) DO UPDATE SET <_SCANNER_OWNED_COLUMNS>`
  pattern as `cache.upsert_album` (lines 323-355). Post-apply paths in
  `_apply.py` (line 137), `apply-stream` (`main.py:1661`), and
  `delete_duplicate_file` (`main.py:1115`) all use `upsert_album`, so
  fingerprints and MB anchors are preserved across re-scans on every
  path. No DELETE-then-INSERT survivors found.

**Trash / soft-delete:**
- Idempotent path is wired in `main.py:1069-1082` (`file_already_gone`
  flag).
- Fingerprint cleanup happens regardless via
  `cache.delete_track_fingerprint(album_id, payload.filename)` at
  `main.py:1105`.
- File-reappears-later is implicitly handled — re-trashing collides on
  filename, gets ISO-suffixed (`trash.py:101-106`).

**Frontend ↔ backend contracts** are consistent for the metadata MBID
flow except #2 (sync endpoint) — every other consumer reads the field
correctly.

---

## Top recommendations

Ranked by user-visible impact:

1. **Fix #2 — synchronous `/api/ai/suggest-metadata` endpoint silently drops
   `mb_release_group_mbid`** AND skips `_persist_ai_mbid`. If this endpoint
   has any callers (frontend or external), they will see "AI couldn't
   identify" for albums where AI in fact did identify — and the cache will
   never receive the anchor. Highest user-visible impact.

2. **Fix #3 — `delete_duplicate_file` reads a non-existent `library_path`
   key** and silently falls back to `parent.resolve()`. Users with nested
   album dirs will see albums orphan from cache lookups after deduping,
   manifesting as "the album disappeared from the library page."

3. **Fix #1 — `_persist_ai_mbid` reuses `MB_RELEASE_GROUP_SEARCH` for
   detail lookups**. Works today, breaks the moment someone changes the
   constant or moves it. One-line constant addition prevents a future
   silent-failure regression on the AI MBID path.

4. **Fix #7 + #8 — duplicate-tracks docstring + dead column** mislead the
   next reader (and probably a future you) about how the check actually
   works. Both are pure comment/SQL hygiene with no behavior risk; do them
   together since they're the same file.

5. **Fix #6 — `FindingFixButton.tsx` stale comments about `missing-mbid`**.
   Code paths are correct; the comments will absolutely cause confusion the
   next time this file is touched. Two ~one-line edits.

Items #4, #9, #10 are real but lower-impact. The remaining 🟢 items are
optional cleanup.
