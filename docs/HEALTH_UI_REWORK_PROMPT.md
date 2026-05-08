# Future-session prompt: rework the Health-jobs UI for the resolver-era model

Drop this into a fresh Claude Code session in this repo. Self-contained — no preamble needed.

---

## Context: what changed in the backend

This repo's metadata-correction model has shifted. Previously the user moved through three separate surfaces: **Health** (run checks → see findings), **AI Jobs** (queue / monitor Claude calls), and the per-album **Detail sheet** (review suggestions → apply). The mental load was real because each surface modeled its own concept of "work to do" — findings, jobs, and pending applies were three independent backlogs.

Recent backend work introduced a deterministic **metadata resolver** (`musaic/services/resolver.py`) that runs a tiered cascade per album: cache hit → MusicBrainz directory search → AcoustID fingerprint vote → Tier 1 alignment of FLAC tracks to canonical recordings. AI is now the last resort. Most "health" issues that used to require an AI job — `mb_canonical_mismatch`, `missing_mbid`, `missing_sort_tags` — now resolve deterministically via the resolver, with AI only invoked when the deterministic tiers fail.

Alongside that:
- **Eager fingerprinting** (`services/fingerprint_pool.py`) runs after every scan, so by the time the user opens the Library, most albums are anchored.
- **Album detail surfaces resolver state directly**: a `ResolutionBadge` shows anchor status; an `MbPreviewSection` displays free MB-derived diffs with an inline Apply button that bypasses the job queue entirely.
- The "Suggest fixes" button still routes through the job queue, but the worker now skips Claude when the resolver covers everything (synthesizer path) and enriches Claude's prompt with resolver state when it does need to call.

Result: the average album doesn't need an AI job at all. Only the truly hard cases (badly named folders + no fingerprint hits + ambiguous MB candidates) actually consume Claude quota.

## Why the existing Health UI now feels heavyweight

The existing `Health` page (`frontend/src/pages/Health.tsx`) is a grid of check tiles; clicking a tile opens `HealthDetail.tsx` with a per-finding list and per-finding Fix / Ignore buttons + an optional bulk action (e.g. "Identify on MusicBrainz"). That model assumes:

1. **Each check is its own concern.** It isn't anymore — `mb_canonical_mismatch`, `missing_mbid`, `missing_sort_tags` are all "the resolver has data your tags don't match." From the user's POV, the distinction matters less than the action ("apply MB corrections to these N albums").
2. **Findings are the unit of action.** They're not — most fixable findings can be batch-applied without per-finding review now that the deterministic synthesizer is reliable.
3. **AI escalation is a deliberate workflow.** It used to be — every fix click went through the AI queue. Now AI is a fallback that the user shouldn't have to think about until the resolver fails.
4. **The "AI Jobs" page is a primary surface.** It used to be where the user spent time waiting / reviewing / retrying. With most fixes now synchronous-deterministic, AI Jobs becomes a low-traffic page mostly relevant for retrying genuinely-failed identifications.

Plus: the resolver surfaces canonical data inside the detail sheet (via `MbPreviewSection`). The `mb_canonical_mismatch` check basically duplicates that signal at the library level. There's redundancy worth resolving.

## The task

**Rework the Health UI to reflect the simpler model.** Specifically:

- The user's primary mental model should shift from "checks → findings → fix" to "**actionable corrections → review/apply**." Findings are still the data backing — but the surface should organize around what the user can DO, not how the data was derived.
- AI should appear only when it's needed (genuine fallback for unresolvable albums + cover lookups for albums missing covers + the "AI metadata reviewed" stamp).
- The grid-of-checks layout is showing its age. A library-wide "what needs attention" view is probably more honest about what the user actually wants.

**You decide the right shape.** This isn't a "implement this design" prompt — it's a "design + implement" prompt. Read the existing code, talk to the user (via clarifying questions if needed), propose a shape, then build it.

## Constraints

- **Don't touch the backend data model.** `Check`, `Finding`, `CheckRun`, `AlbumResolution`, the SSE streams — all stable. The frontend's job is to organize them differently.
- **Don't break the Detail sheet.** `ResolutionBadge`, `MbPreviewSection`, `FingerprintPoolPill`, the active-job banner, `AISuggestions` — these all work and are the user's primary single-album surface. Reorganize the library-wide views around them, not against them.
- **Don't remove ignore / unignore.** Some findings are deliberate user choices ("yes I know this album has no cover, leave it alone"). The escape hatch needs to stay accessible.
- **Don't pretend AI is gone.** When the resolver fails, AI is still the right call. The UI should acknowledge AI exists; it just shouldn't lead with it.
- **Per-check views may still exist.** A user investigating "why does this album show up in `inconsistent-artist-sort`" deserves a focused list. But that view should be a deeper-dive, not the primary entry point.

## Where to start reading

Backend (skim — the resolver model is the key thing to internalize):
- `CLAUDE.md` — overview, especially the **Metadata Resolver** and **Health Check Framework** sections
- `musaic/services/resolver.py` — the cascade, the `AlbumResolution` shape, the `to_dict()` payload
- `musaic/checks/__init__.py` — list of registered checks; categories (`metadata`, `external`, `audio`, `filesystem`)
- `musaic/checks/base.py` — `CheckMeta` (esp. `fix_kind`, `bulk_action_label`), `Finding`, `FixResult`
- `musaic/checks/runner.py` — how findings are produced + auto-resolved; `try_quick_fix_finding` for bulk
- `musaic/api/health.py` — the health REST + SSE surface (what the frontend already has access to)

Frontend (read closely — this is what you're rewriting):
- `frontend/src/pages/Health.tsx` — current grid layout
- `frontend/src/pages/HealthDetail.tsx` — current per-check detail page
- `frontend/src/pages/Jobs.tsx` — current AI Jobs page; figure out which parts still matter
- `frontend/src/components/DetailSheet/MbPreviewSection.tsx` — the inline-preview pattern that proves "deterministic apply with no job" works at the per-album level. Consider whether the library-wide view should be a list of these.
- `frontend/src/hooks/useChecks.ts`, `useChecksStream.ts` — the data layer; available to whatever you build
- `frontend/src/hooks/useAlbumResolution.ts` — per-album resolver fetch; if you need bulk resolution data, you'll likely add a counterpart or scale this out

History worth a glance — `git log --oneline --since="last week"` will show the resolver work commits with detailed messages explaining the conceptual shift.

## Shape questions worth thinking through (not prescriptive)

Some directions to consider — pick what feels right after you read the code. Don't feel obligated to do all of these.

1. **Is the unit of presentation a check, a finding, or an album?** Today it's a check. The resolver makes "album" a more coherent unit because one album can have multiple correlated findings (missing MBID + canonical mismatch + missing sort) that all clear at once when you anchor it.
2. **Do you want a single "Apply all MusicBrainz corrections" sweep?** It would be the natural extension of `MbPreviewSection` to library-wide. Cost: walks every anchored album, runs the synthesizer, applies. Could be SSE-streamed like the existing scan / quick-fix flows.
3. **What happens to the AI Jobs page?** Options: collapse into Health as a "needs review" tab; keep as a queue monitor; keep but de-emphasize. The page still has real value when AI runs are in flight or have failed, but it's no longer the primary "where corrections happen" surface.
4. **What about the cover-art fix path?** It's still genuinely AI-flavored when MB has no CAA cover. Where does that live in the new model?
5. **Bulk vs per-finding.** Today the bulk action is per-check ("Identify on MusicBrainz" runs `try_quick_fix` across every open finding for that one check). With the resolver the per-album batching might be more useful — "all the deterministic corrections this resolver-anchored album needs, in one Apply."
6. **Do "checks" remain visible at all?** They have to remain runnable (some are still genuinely independent — `track_gaps`, `duplicate_tracks`, `missing_fingerprints`). But the navigation to them might move from primary to secondary.
7. **What's the right empty state?** "Library is healthy" is more meaningful in the new model — the deterministic checks running clean is a stronger signal than it used to be.

## Suggested working pattern

1. **Read first, design second, code third.** Don't start editing files until you've got the resolver model in your head and a clear opinion on the new shape.
2. **Propose the design to the user before coding.** Sketch it in 3–5 sentences plus a short ASCII layout. Don't enter plan mode formally — just share the proposal in chat and let the user redirect if needed.
3. **Land it incrementally.** A first commit might add the new top-level view alongside the existing Health page (under a different route); a second swaps the default; a third removes the old code. Each commit should leave the app in a usable state.
4. **Keep the test suite green.** All 48 backend tests should still pass. If you find yourself wanting to delete a frontend hook, double-check no other page consumes it.
5. **Update CLAUDE.md** at the end with the new pages / components.

## Out of scope for this session

- New backend checks
- Resolver-internal changes (Tier math, alignment rules — unit-tested in `test_resolver.py`, leave alone)
- Any changes to the apply pipeline (`services/apply.py`)
- `frontend/src/components/DetailSheet/*` — these are fine. Read them for the patterns, don't rewrite them.

## What "done" looks like

- A library-wide view that lists actionable albums (or correction-batches, or whatever shape you land on) ordered by what the user is most likely to want to act on.
- AI is visibly de-emphasized but still available where it's truly needed.
- The user can still drill into a specific check when they want to (the existing per-check view either becomes secondary or gets repurposed).
- `frontend/src/pages/Jobs.tsx` either gets a clear new role or moves into a corner of the Health surface.
- `npm run typecheck` clean, `uv run pytest` clean, `npm run build` produces a working bundle.
- A short user-facing note in the chat explaining what changed and how the new flow works.
