# Library maintenance scripts — candidate list

A working catalog of periodic / on-demand checks Media Manager could run against the FLAC library. Compiled from beets/Picard practice, FLAC community wisdom, and the existing tools in `flac-jacket/tools`.

Each entry calls out:

- **What:** the actual check
- **How:** data we already have vs. what we'd need to add
- **Action:** report-only, suggest-fix, or auto-fix
- **Existing:** anything in Media Manager or flac-jacket that already does part of the job
- **AI?** whether the check benefits from Claude vs being deterministic

The first column tags each check by category for sorting/filtering later.

---

## Metadata completeness

### M1. Missing core album-level tags
- **What:** any album where `title`, `album_artist` (or `artist`), `year`, or `genre` is empty.
- **How:** straight SQL over the `albums` cache.
- **Action:** report; deep-link to detail sheet so user can hit "Suggest fixes".
- **Existing:** the cache already has all these columns; no new I/O needed.
- **AI?** No to detect; AI to fill in.

### M2. Missing per-track tags
- **What:** tracks where `title`, `tracknumber`, or `artist` (on compilations) is empty.
- **How:** scan walks each FLAC; we'd need to surface this from `_read_tracks` or persist it.
- **Action:** report; group by album.
- **Existing:** `tags.py:_read_tracks` already reads these; just need to aggregate.
- **AI?** Optional. AI metadata suggestion already covers per-track corrections.

### M3. Track-number gaps and duplicates
- **What:** album has tracks numbered 1,2,4,5 (missing 3) or 1,2,2,3 (duplicate 2).
- **How:** sort track numbers per album, look for non-contiguous or repeated values.
- **Action:** report; usually means an incomplete rip or a manual mis-edit.
- **Existing:** none yet — purely deterministic, easy to add.
- **AI?** No.

### M4. Track count mismatch with canonical release
- **What:** local album has N tracks but MusicBrainz says canonical release has M.
- **How:** MB release lookup using `dir_artist`/`dir_album` (already done by `cover_lookup`).
- **Action:** report with the diff. Often means bonus tracks dropped or extra material from a deluxe edition.
- **Existing:** `flac-jacket/tools/mb_tracklist.py` does exactly this lookup.
- **AI?** No — MB is authoritative.

### M5. Inconsistent `album_artist` across tracks
- **What:** two FLACs in the same album folder have different `album_artist` values.
- **How:** group by directory, count distinct values per album.
- **Action:** report; surface the divergent values so user can normalize.
- **Existing:** none — easy add.
- **AI?** No.

### M6. Year format / plausibility
- **What:** `year` is not a 4-digit string; or is in the future; or is before, say, 1900.
- **How:** simple regex + range check.
- **Action:** report. (Could auto-strip "2003-05-12" → "2003", but that loses info — only do that on user opt-in.)
- **Existing:** none.
- **AI?** No.

### M7. Genre normalization drift
- **What:** "Alternative Rock" vs "Alt-rock" vs "alternative rock" sprinkled across the library.
- **How:** count distinct genre values, flag near-duplicates (case-insensitive, fuzzy match).
- **Action:** suggest a canonical form per cluster; user picks. Optional auto-remap.
- **Existing:** none.
- **AI?** Helpful for fuzzy clustering; deterministic fallback on string distance works too.

### M8. Sort tags missing
- **What:** Picard-style `ARTISTSORT`, `ALBUMARTISTSORT` not set, hurting "The X" → "X, The" sort behavior in Plex.
- **How:** read those Vorbis comments during scan.
- **Action:** suggest sort form (drop leading articles); auto-fix if user opts in.
- **Existing:** not currently read into the cache.
- **AI?** No — deterministic article-stripping is enough.

---

## MusicBrainz / external ID anchoring

### X1. Missing MusicBrainz Album ID
- **What:** files have no `MUSICBRAINZ_ALBUMID` / `MUSICBRAINZ_RELEASEGROUPID` Vorbis tag.
- **How:** read tag during scan; flag absent ones.
- **Action:** look up MB, write the ID. Once anchored, future tag corrections become a deterministic `mbsync`-style refresh instead of guessing.
- **Existing:** not currently captured. `cover_lookup.py` already searches MB and pulls release-group MBIDs — same code path can stamp the ID.
- **AI?** No, deterministic.

### X2. Stale MB tags vs upstream
- **What:** MB upstream has updated artist credits / track titles since we last imported.
- **How:** for albums with an MBID, periodically refetch and diff.
- **Action:** surface as suggested updates (don't auto-overwrite — user might have intentional overrides).
- **Existing:** none. This is essentially beets' `mbsync`.
- **AI?** No.

### X3. AcoustID fingerprint coverage
- **What:** files without an AcoustID fingerprint stored.
- **How:** Chromaprint via `pyacoustid` or the `fpcalc` binary.
- **Action:** compute and store; doubles as a duplicate-detection key (see D2).
- **Existing:** `flac-jacket/tools/fingerprint.py`.
- **AI?** No.

---

## Cover art

### C1. No cover art at all
- **What:** album has neither embedded picture block nor a sidecar `cover.*` / `folder.*` / `front.*`.
- **How:** scan already records `has_embedded_cover` and `cover_file`.
- **Action:** queue an AI cover lookup (or MB fallback). Already a filter chip in the sidebar today.
- **Existing:** ✅ implemented — "missing-art" filter, AI cover suggestion, MB fallback.
- **AI?** Yes (with MB fallback).

### C2. Cover present but only embedded (no sidecar)
- **What:** Plex / Roon / mpd prefer a sidecar `cover.jpg`. If only embedded, some players miss it.
- **How:** `has_embedded_cover && cover_file is null`.
- **Action:** extract embedded picture, write to album directory as `cover.jpg`.
- **Existing:** the inverse already works during apply (we write both); this would handle albums imported before that landed.
- **AI?** No.

### C3. Cover present but only sidecar (no embedded)
- **What:** flip side of C2 — sidecar exists but the FLAC files don't carry the picture block. Mobile sync, AirPlay, and Plex Direct Play all prefer embedded.
- **How:** `cover_file is not null && !has_embedded_cover`.
- **Action:** embed the sidecar into all FLACs. Use existing `tags.embed_cover_in_flac`.
- **Existing:** machinery exists; just needs an audit + batch-apply mode.
- **AI?** No.

### C4. Cover resolution below threshold
- **What:** embedded or sidecar art smaller than e.g. 600×600.
- **How:** Pillow on the bytes; record width/height during scan.
- **Action:** report; user can re-fetch via AI / MB.
- **Existing:** `thumbs.py` already opens the image — easy to capture dimensions.
- **AI?** No to detect; AI/MB to replace.

### C5. Multiple inconsistent cover files
- **What:** album dir has `cover.jpg`, `folder.jpg`, AND `front.png` — players pick different ones.
- **How:** scan records first found; we'd need to enumerate all candidates.
- **Action:** pick one canonical (`cover.jpg`), delete the rest after confirmation.
- **Existing:** `tags.save_cover_to_album_dir` already removes stale `cover.{jpeg,png}` siblings on apply; this would be a one-shot sweeper.
- **AI?** No.

### C6. Cover differs between embedded and sidecar
- **What:** the picture block in the FLAC and the file on disk are different images (rare but happens after manual edits).
- **How:** compare bytes hash.
- **Action:** flag for human review; we don't know which is canonical.
- **Existing:** none.
- **AI?** No.

### C7. Cover bytes are corrupt / not a valid image
- **What:** Pillow refuses to open the embedded picture.
- **How:** try-decode during scan.
- **Action:** report. Often cause of black-square Plex thumbnails.
- **Existing:** `thumbs.py` already catches Pillow errors silently — surface them.
- **AI?** No.

---

## Audio integrity

### A1. FLAC stream integrity (`flac -t`)
- **What:** decode the file, verify against the STREAMINFO MD5.
- **How:** shell out to `flac -t <file>`, capture exit code.
- **Action:** report failures. **Highest signal of bitrot in the whole list.** Run on a slow rotating schedule (e.g. one album a day) since it's I/O-heavy.
- **Existing:** none in Media Manager.
- **AI?** No.

### A2. Missing STREAMINFO MD5
- **What:** older encoders produced files with the audio-data MD5 zeroed out, which makes A1 a no-op.
- **How:** `metaflac --show-md5sum`; flag `00000000…`.
- **Action:** re-encode at the same compression level so a real MD5 gets written. (Lossless re-encode, no quality loss.)
- **Existing:** none.
- **AI?** No.

### A3. Sample-rate / bit-depth outliers
- **What:** a 22 kHz file in an otherwise 44.1/16 library, or unflagged 24-bit hi-res alongside CD rips.
- **How:** scan already captures `sample_rate`, `bits_per_sample`, `channels`.
- **Action:** report distribution; user decides if outliers are intentional.
- **Existing:** ✅ already in the cache — just needs a UI surface.
- **AI?** No.

### A4. Suspiciously short / long files
- **What:** track durations under ~30 s (truncated rip?) or over ~30 min (probably a single side of vinyl as one file — fine, but worth flagging).
- **How:** straight from cache.
- **Action:** report.
- **Existing:** duration is in the cache.
- **AI?** No.

### A5. ReplayGain coverage
- **What:** files missing `REPLAYGAIN_TRACK_GAIN` / `REPLAYGAIN_ALBUM_GAIN`.
- **How:** read tag during scan.
- **Action:** compute via `loudgain` / `bs1770gain` per album.
- **Existing:** `flac-jacket/tools/replaygain.py` and `bulk_replaygain.py`.
- **AI?** No.

### A6. ReplayGain plausibility
- **What:** values that suggest a stale calc (e.g. peak > 1.0, gain wildly different from album peers).
- **How:** simple range checks.
- **Action:** flag for recompute.
- **Existing:** none.
- **AI?** No.

### A7. BPM coverage (if you want it)
- **What:** files missing a BPM tag.
- **How:** librosa / `aubio`.
- **Action:** compute and write.
- **Existing:** `flac-jacket/tools/bpm.py` and `bulk_bpm.py`.
- **AI?** No.

---

## File / directory conventions

### F1. Directory name doesn't match strict convention
- **What:** the flac-jacket convention `Artist - Album [FLAC]`. Already parsed; `dir_artist`/`dir_album` go null when it doesn't match.
- **How:** filter cache for `dir_artist IS NULL AND is_compilation = 0`.
- **Action:** report; suggest a normalized name based on tags.
- **Existing:** ✅ data already in cache; UI could add a "non-conforming directories" filter.
- **AI?** No.

### F2. Track filename doesn't match `NN. Title.flac`
- **What:** files like `track 5.flac`, `05_title.flac`, leftover `_remastered.flac`.
- **How:** regex per file during scan.
- **Action:** propose rename derived from current tags; require user confirm.
- **Existing:** apply path already renames on tracknumber change.
- **AI?** No.

### F3. Cross-platform-unsafe characters in paths
- **What:** colons, question marks, trailing dots — break SMB/exFAT and some media-server URL routers.
- **How:** regex over paths.
- **Action:** propose a safe rename.
- **Existing:** none.
- **AI?** No.

### F4. Junk / non-music files in album folders
- **What:** `.DS_Store`, `Thumbs.db`, leftover `.cue`/`.log` files, `*.m3u`, multiple `*.jpg` (related to C5).
- **How:** directory listing per album.
- **Action:** report (don't auto-delete). Especially `.cue`/`.log` — those are often kept on purpose.
- **Existing:** none.
- **AI?** No.

### F5. Orphaned cache rows / orphaned files
- **What:** cache row pointing to a path that no longer exists; OR a FLAC on disk that isn't in the cache (scan anomaly).
- **How:** cross-check at scan time.
- **Action:** delete missing-from-disk rows; flag missing-from-DB files for inclusion next scan.
- **Existing:** scan already removes stale rows.
- **AI?** No.

---

## Duplicates

### D1. Tag-based near-duplicates
- **What:** two albums with the same `(album_artist, title)` tuple, possibly at different paths.
- **How:** SQL group-by.
- **Action:** report; user decides which to keep (often a re-rip or a deluxe vs original).
- **Existing:** none.
- **AI?** No.

### D2. AcoustID fingerprint duplicates
- **What:** same recording with different tags (e.g. live vs studio version; same track on two compilations).
- **How:** Chromaprint hash from X3.
- **Action:** report. Don't auto-merge — these are often legitimately distinct.
- **Existing:** depends on X3.
- **AI?** No.

### D3. Same MBID applied to multiple albums
- **What:** two cache rows share a `MUSICBRAINZ_ALBUMID` — implies a botched import (e.g. one folder per disc both anchored to the multi-disc release).
- **How:** SQL group-by once X1 lands.
- **Action:** report.
- **Existing:** depends on X1.
- **AI?** No.

---

## External sync

### E1. Plex ↔ library drift
- **What:** album in our cache that Plex doesn't have, or vice versa.
- **How:** Plex API list vs cache.
- **Action:** trigger Plex rescan / surface for investigation.
- **Existing:** `flac-jacket/tools/plex_dance.py` does the rescan trick.
- **AI?** No.

### E2. Last.fm scrobble import
- **What:** pull listening history into a local play-count column. Drives playlist criteria ("hidden gems I haven't played in a year").
- **How:** Last.fm API.
- **Action:** background sync, expose in UI.
- **Existing:** none in this app; this would be new.
- **AI?** No.

---

## Suggested first build

If we're picking the first batch to ship, I'd weight the report effort against the user-visible payoff. Top picks:

1. **C1, C2, C3** (cover art coverage — embedded, sidecar, neither). Already most of the data; biggest visible win in Plex/mobile.
2. **M1, M5, M6** (missing core tags + album_artist drift + year sanity). Very cheap, high-signal.
3. **M3** (track-number gaps). One SQL query.
4. **F1, F5** (non-conforming dirs + orphaned rows). Already free off scan data.
5. **A1** (FLAC integrity). Highest data-protection value, but I/O-heavy — design as a slow-rotating background pass.
6. **X1** (MB ID anchoring). Unlocks a lot of downstream determinism (X2, M4, D3).

Things I'd defer:
- ReplayGain / BPM coverage already exist in `flac-jacket`; lift them as periodic refreshers when we wire in the playlist criteria feature.
- AcoustID fingerprinting (X3, D2) is meaningful but uses a third-party API and a binary tool — extra setup, not core to the launch.
- Last.fm sync (E2) is a separate feature, not maintenance.

## How a "Maintenance" page might look

A new sidebar entry between "AI Jobs" and "Settings": **Health checks**. Each check above is a row in a table:

- Name, last-run time, count of issues found, "Run now" button.
- Click into a check → drilldown list of affected albums (links to detail sheet).
- Some checks ("Missing core tags", "Track gaps") run as part of every scan and just show their cached results; heavier ones ("FLAC integrity", "MB stale tags") run on-demand or scheduled.

This dovetails with the existing AI job queue — a check that wants to *fix* something can enqueue per-album AI jobs through the same machinery the user already knows.

---

## Sources

- beets plugin reference (beets.readthedocs.io/en/stable/plugins/) — `missing`, `duplicates`, `fetchart`, `embedart`, `info`, `check`, `mbsync`, `replaygain`, `lastimport`, `chroma`, `lastgenre`, `bareasc`.
- MusicBrainz Picard documentation — Vorbis Comment / FLAC tag mapping; `originaldate` vs `date`.
- xiph.org FLAC format spec — STREAMINFO MD5, `METADATA_BLOCK_PICTURE`.
- /r/DataHoarder and /r/audiophile community threads on FLAC tagging hygiene.
- EBU R128 / ReplayGain 2.0 specs.
- `flac-jacket/tools/` (existing personal scripts).
