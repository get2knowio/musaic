# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project aims to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!--
Add entries under Unreleased as you go. On release, rename the section to the version +
date and start a fresh Unreleased. Categories: Added, Changed, Deprecated, Removed, Fixed,
Security.
-->

## [Unreleased]

### Added

- The library-events stream now reports when a batch of health-check runs has finished
  (`ChecksRerunFinished`: the checks that ran, the ones that failed, the albums it was
  limited to), published once the findings are written: after a post-scan or rename rerun,
  after a background sweep pass, and after a check run you start. The UI refreshes findings
  and checks from it as well as from the checks stream. Part of [#204](https://github.com/get2knowio/musaic/issues/204).

- A "Tags don't match canonical" finding whose genre is blank now says when MusicBrainz
  suggests one: "MusicBrainz suggests "folk" as the genre. Review it in the album's
  corrections." On Needs attention the line links to the album's panel; in the panel it
  shows while the corrections list has the genre row. The genre is never written by the fix
  or by the bulk action: the suggestion is MusicBrainz's top-voted tag, which is not always a
  genre, so it stays a row in the album's corrections that starts unticked, for you to review.
  The severity is unchanged (a blank genre alone is still informational). Existing findings
  gain the line when the check reruns once in the background, which needs the app to be
  online. Part of [#259](https://github.com/get2knowio/musaic/issues/259).

- The database is upgraded to schema v20 on first start: each album row records the disc
  number of every file (from its DISCNUMBER tag, or from a disc-prefixed track number such
  as `2-05`) and the highest disc total its tags name, so a later check can find multi-disc
  sets without reading every file. Existing rows start empty; the first scan after the
  upgrade reads again only the albums whose track numbers repeat or whose folder name
  names a disc, and every other album is filled when it is next read. Nothing is shown in
  the app yet. A copy of the v19 database is saved as a snapshot first; the previous
  release cannot open a v20 database, so going back means restoring that snapshot. Part
  of [#296](https://github.com/get2knowio/musaic/issues/296).

### Changed

- A "Tags don't match canonical" finding whose only blank core tag is the genre is now
  informational. A blank title, album artist or year is still a warning. This build has no
  way to fill in a missing genre, so the warning had no fix; the finding still says the
  genre is missing ([#255](https://github.com/get2knowio/musaic/issues/255)). Existing
  findings take the new severity when the check reruns once in the background, which needs
  the app to be online (the check asks MusicBrainz); ignored ones stay ignored.
- "Inconsistent artist sort" findings where the same artist carries two different sort
  values are now warnings, so they count in the Needs attention badge
  ([#255](https://github.com/get2knowio/musaic/issues/255)). A missing sort value that can
  simply be copied, and a sort value missing everywhere, stay informational suggestions.
  Existing findings take the new severity on the next launch; ignored ones stay ignored.
- On the Needs attention page the "Other" view is now "Suggestions", and the tab shows how
  many suggestions there are. The main view says "N suggestions" (it switches to them),
  including when nothing needs a decision, so a low badge no longer hides fixable items.
  Suggestions are the informational findings; finished jobs sit in their own "Finished
  jobs" section below them and are not in the tab's count. The badge counts the same as
  before, and `?view=other` still opens the Suggestions view.
- The Jobs and Health pages are one "Needs attention" page at `/attention`
  ([#188](https://github.com/get2knowio/musaic/issues/188)), with one sidebar entry and
  one badge. It lists what needs a decision: open findings of severity warn or error,
  grouped by check, and jobs that are proposed (awaiting review) or failed; the badge
  counts exactly those. Informational findings and finished jobs are on its "Other" view
  (renamed "Suggestions" below) and are not counted; queued or running jobs show as a progress strip. Opening an album
  shows its panel, and Back returns to the same view and scroll position. There is no
  "Check now": checks run on their own. The old `/health`, `/health/<check>` and `/jobs`
  links redirect (a check lands filtered to that check; `/jobs/<id>` and `/jobs?job=<id>`
  show and highlight that job). The per-finding Fix, Ignore and Resolve dialogs, the
  per-check bulk actions and the job Retry and Delete actions are unchanged. A proposed
  job's status reads "To review".

- The apply progress view lists each file that kept its old name after a track-number
  rename failed, with the reason ("3 files kept their old names"). The tags were saved;
  previously only the error count showed
  ([#217](https://github.com/get2knowio/musaic/issues/217)).
- Windows is back ([#178](https://github.com/get2knowio/musaic/issues/178)). The Windows
  Rust tests are now a required part of the `ci passed` check, and the release workflow
  builds an unsigned Windows installer (NSIS, `-setup.exe`) after the Windows tests pass,
  from the next release on. v0.2.0-rc.1 has no Windows installer. The check keeps its
  name, `ci passed (linux, macos, frontend)`, so the repository ruleset keeps matching.
- Case-only corrections work end to end. In the album panel's corrections list a value
  that differs only in capitalisation ("the beatles" → "The Beatles") is now a change:
  it is listed, applied and reported; differences only in leading, trailing or repeated
  spaces are still ignored, and an empty proposal is still never one. The album title is
  compared with the files' ALBUM tags (the track listing now includes each file's ALBUM
  tag), not with the title taken from the folder name, so a title correction disappears
  once applied. Renaming files and folders to match the templates carries out case-only
  renames on every volume: a folder whose new name differs only in case is moved through
  a temporary name in the same parent (it gets the new album id, as any folder rename),
  a different file or folder with the other spelling is never replaced, and after each
  rename the folder is read back: on a volume that does not keep the new capitalisation
  the file or folder is put back under its old name and reported as not renamed. Two
  tracks whose template names differ only in case are shown as a collision in the
  rename dialog. A rename whose folder cannot be read back is kept and reported as not
  verified. Renames of files and folders whose names are close to the 255-byte limit no
  longer fail for want of room for the temporary name.
- Wording: the transient AI-job error no longer points at a script that does not exist, and
  the Settings text about FireCrawl and the Backup page no longer say "paid".
- The album panel proposes MusicBrainz corrections in one place and applies them one way
  ([#184](https://github.com/get2knowio/musaic/issues/184),
  [#183](https://github.com/get2knowio/musaic/issues/183),
  [#189](https://github.com/get2knowio/musaic/issues/189)). A single "MusicBrainz
  corrections" list near the top shows each change as current → proposed with its own
  checkbox, now including genre (unticked by default) and a cover row with a preview of
  the proposed cover. A finished job's result appears in the same list with the same
  checkboxes, instead of replacing the album info, findings and tracks. Both sources use
  the "Tags don't match canonical" check's rule for what counts as a change (case and
  extra spaces are ignored), and an empty proposal is never one. Apply and Discard sit
  in the panel's footer. The footer's "Find MusicBrainz corrections" / "Suggest fixes"
  and "Get cover from MusicBrainz" / "Find cover art" buttons, the "(+ cover)" bundling
  and the "identified as …" status line are gone. Covers have one control under the
  album art, "Add cover" or "Change cover", with "Use MusicBrainz cover" (previewed in the
  list), "Upload, paste or URL…" and, only with an AI backend, "Ask AI to find a cover…";
  it is absent for a Missing album. The upload dialog is titled "Add cover" or "Replace
  cover", and its "Replace" link reads "Choose another image". "Ask AI for corrections…"
  and the hint dialog appear only when an AI backend is ready.
- Health no longer asks you to run checks: the per-check "Run now" buttons, the
  "Last run" / "Never run" column, the "Recent runs" list, the category column and the
  check count are gone, and so is the temporary "Check now" button, since every check
  now runs on its own in the background (see Added)
  ([#187](https://github.com/get2knowio/musaic/issues/187)).
- The panel of a Missing album says one thing and offers one action
  ([#195](https://github.com/get2knowio/musaic/issues/195)). A single notice says the
  folder was not found, where, and by which scan, and no longer promises that a moved
  album comes back on its own: a folder back at the same path returns with its
  MusicBrainz match, fingerprints and My Tags, while one moved or renamed inside the
  library is added as a separate album. "Remove from library…" opens a dialog that lists
  what is deleted from Musaic's database and says your music files are not touched,
  instead of a browser prompt. Sections that need the files are hidden: the empty track
  list (the header keeps the stored track count), job suggestions and retries that could
  not be applied, the resolution chip, and the cover upload (the cached thumbnail still
  shows).
- Fix is disabled, with the reason on hover, for a finding whose album is Missing
  ([#186](https://github.com/get2knowio/musaic/issues/186)).
- The filename and duplicate-track Resolve dialogs say what will happen to your files
  ("Rename the album folder and 12 files in it to match your templates") instead of how
  it is done. The duplicate dialog's row button reads "Move to trash", matching what it
  does, and the trash is named as Musaic's trash folder rather than a fixed path (it
  follows the config folder). The Resolve buttons on findings get icons that fit each
  check instead of a copy icon. The requests the dialogs send are unchanged
  ([#196](https://github.com/get2knowio/musaic/issues/196)).
- **The album sheet is now a side panel beside the library grid**
  ([#190](https://github.com/get2knowio/musaic/issues/190)). The grid is no longer
  dimmed or blocked: it narrows to make room (keeping at least 320 px), and clicking
  another album shows that album in the panel. Browsing albums this way replaces the
  history entry instead of adding one per album. Closing an album opened from another
  page (Health, Jobs) goes back to that page; otherwise it returns to the library with
  the same filters. Esc closes the panel when focus is in it, F6 moves focus between the
  grid and the panel, Enter on a tile opens the album with focus in the panel, and
  closing puts focus back on the album's tile. The open album's tile is highlighted and
  scrolled into view. The resize handle also works from the keyboard (arrow keys,
  Home / End) and a double-click restores the default width. Dialogs opened from the
  panel are ordinary dialogs over the whole window. When the window is too narrow for
  both the grid (320 px) and the panel (360 px), the panel takes the whole width; the
  grid waits underneath with its scroll position and comes back on close.
- Applying changes no longer holds the album open: you can switch albums or close the
  panel while tags are written (the app finishes the write either way), and the
  progress, or the result, is shown again when you reopen that album.
### Added

- Every health check runs on its own
  ([#187](https://github.com/get2knowio/musaic/issues/187)). After startup, a scan or any
  change to the library, a background sweep reruns each check where what it reads has
  changed since it last ran, so the tag and filename comparisons no longer wait for a
  button. It waits while a scan, a restore or other work runs; its MusicBrainz requests
  only use the gaps between your own, so they never slow down what you are waiting for;
  it stops at once when you quit, without asking. A pill in the title bar shows
  "Checking… n of m" while it works. Changing a filename template reruns the filename
  check. The sweep only records findings: it never changes your files or tags. It does
  nothing while the library folder is missing (an unmounted drive), and offline it skips
  the MusicBrainz comparison.
- The database is upgraded to schema v19 on first start: a new `check_inputs` table
  records what each check last ran against, so the sweep resumes where it stopped and
  skips what has not changed. Before this or any later upgrade of a database this app
  wrote, a copy is saved as a snapshot (Settings → Backup), without any API key in it,
  and pruned with the other snapshots; if the copy cannot be written the database is
  left as it is and the app says why.
- `GET /api/albums/{id}/cover-preview`: a preview of the Cover Art Archive cover the
  resolver proposes for an album anchored to MusicBrainz, without queueing a job
  (groundwork for #183 and #184). The request names only the album; the server derives
  the address from the stored anchor and fetches it under the cover-download rules
  (HTTPS, public hosts, every redirect checked, size cap, image types only). An album
  with no anchor, or none the archive has a front cover for, answers 404; a Missing
  album 409.
- **HTTPS certificates are checked by your operating system.** MusicBrainz, AcoustID and
  Cover Art Archive connections are verified against the system's trust store (Keychain
  on macOS, the Windows certificate store, the system CA bundle on Linux) instead of a
  list of root certificates built into the app, so a root certificate you or your
  organisation installed (for example for a proxy that inspects HTTPS) is trusted, and
  one the system distrusts is not. The built-in list is used only on a system that has
  no trust store at all. When a certificate is refused, the log and the error shown say
  so and name the server ("refused the connection to musicbrainz.org: the server's
  certificate is not trusted (…)") instead of reading like a network outage
  ([#214](https://github.com/get2knowio/musaic/issues/214)).

### Fixed

- A findings or jobs request that failed no longer reads as "Nothing needs attention"
  ([#323](https://github.com/get2knowio/musaic/issues/323)). The all-clear shows only when
  both have answered; when one fails, what did load stays listed, a notice names what is
  missing (or out of date, when a refresh failed and the last answer is kept) with a Retry
  for it, and the sidebar badge becomes a lower bound ("2+", or "?" when nothing is known).
  The Library page likewise tells a settings request that failed (Retry) from a library with
  no folder set (setup) and a folder that is not there, such as a disconnected drive
  (recovery: try again, or choose another folder).
- The Rename files dialog no longer closes on a rename that was refused, only partly done,
  or that skipped files it could not find. It stays open and says what happened: which files
  were left alone, whether the album folder was renamed, and whether any file changed.
- The track-number check no longer reports false "Duplicate track numbers" (an error, so it
  counted in the Needs attention badge) on an album kept in one folder as several discs
  that are each numbered from 1
  ([#256](https://github.com/get2knowio/musaic/issues/256)). Gaps and repeats are now
  judged per disc, using the `DISCNUMBER` tag or a disc-prefixed track number such as
  `2-05`; findings on such an album name the disc. Albums with no disc information are
  judged as before. Existing false findings close on the next launch. The automatic
  renumbering fix no longer runs on an album that spans several discs, because the
  MusicBrainz tracklist it uses is numbered straight through all discs.
- The automatic check sweep and the copy taken before a schema upgrade are stricter about
  what counts as an answer ([#251](https://github.com/get2knowio/musaic/issues/251)). A
  resolver error leaves an album "could not evaluate" (its findings stay and it is checked
  again) instead of closing the MusicBrainz-derived ones. Only a 404 from MusicBrainz is an
  answer; any other status (a proxy's 403 or 407, a 400, a rate limit, a server error) and,
  for the Cover Art Archive, anything but 200 or 404, leaves the album unevaluated. If an
  existing database cannot be read before an upgrade, startup stops with a message naming
  the file instead of upgrading without a copy; the read waits out a short lock. A second
  start after one that did not finish upgrading reuses the copy if the database has not
  changed since, instead of taking another full copy. The newest pre-upgrade copy is exempt
  from snapshot pruning until a later upgrade supersedes it. An AI job that names a release
  group no longer overwrites an anchor the user changed while the job ran.

- A check can no longer close findings for albums it could not look at: an album whose
  folder cannot be read, or whose MusicBrainz lookup failed (offline, a server error),
  keeps its findings and is checked again later, whether the check was started by you,
  by a scan or in the background.
- A background MusicBrainz lookup can no longer put back a match you just rejected with
  "Wrong match" or cleared: it only writes if the album's match is still the one it
  started from and the release group is not blocked.
- Refreshing a stale MusicBrainz match that stays the same no longer makes the album
  list reload.
- The titlebar no longer makes the window content wider than the window, which cut off
  the right-hand controls at the default 1280 px width and anything narrower
  ([#201](https://github.com/get2knowio/musaic/issues/201), first item). The location
  and search box shrink; below 1024 px the tile-size slider is hidden and Rescan shows
  only its icon.
- A library scan can be cancelled: the scan ribbon's Cancel button (and quitting the
  app) stops it after the album being read, and nothing it read is saved. A scan that
  ends without saving (cancelled, aborted because too many albums seemed to vanish, or
  failed) no longer announces completion: no post-scan check reruns, no fingerprinting
  pass, no "up to date" message; the scan reports `cancelled` / `aborted` instead, and
  the bus publishes `LibraryScanStopped`. New route `POST /api/scan/cancel` (#193).
- A tag save whose new tags no longer fit (the file is rewritten) now keeps the file's
  extended attributes (Finder tags and comments, quarantine, `user.*`, POSIX ACLs),
  permission bits, group (and owner where allowed) and, on macOS and Windows, its
  creation date; on Windows its alternate data streams. A symlinked file is rewritten
  through the link. Anything that could not be kept, including a hard link that the
  rewrite breaks, is listed as a warning next to the file in the apply and audio
  analysis progress ([#211](https://github.com/get2knowio/musaic/issues/211)).
- Snapshots: only files named the way Musaic names its snapshots
  (`library-<date>-<time>Z-v<schema>.db`, with `.1`, `.2`, … after a same-second clash)
  are listed, counted for retention, pruned, restored from or deleted. Other files in
  `backups/` are left alone. (#213)
- Snapshots taken in the same second are now ordered by their clash number, so retention
  removes the oldest of them first (it removed `.1` before the un-numbered file, and
  `.10` before `.9`). (#213)
- `snapshot_min_interval_hours` refuses infinity and NaN; a stored non-finite value reads
  as the 24-hour default instead of `null` (infinity) or 0 (NaN), and the startup
  snapshot uses the same reading as the Settings page. (#213)
- Settings accepts every AI provider that `GET /api/ai/providers` lists, and a refused
  provider's message names those providers. (#199)
- **Quick-fix-all no longer reports findings as resolved while they stay open**
  (#216). After the run the check is rerun over the albums it fixed; the summary counts
  only the findings that are gone as fixed and shows the rest as "still open" (for
  example a fix that could fill only some of the missing fields). The `done` event
  gains `still_open`.
- **Checks run for a few albums now look only at those albums.** "Tags don't match
  canonical" read and looked up every album in the library (MusicBrainz requests for
  each identified album) even when run for one album, and the duplicate-tracks check
  loaded every album's fingerprints. Both now read only the albums asked for; a
  library-wide run is unchanged.
- **Fixing one album of an inconsistent-artist-sort cluster now clears the finding**
  even when the finding is listed on another album of the cluster (#206). Before, it
  stayed open until the next library-wide check run.
- **Albums tagged with ARTIST but no ALBUMARTIST now take part in
  inconsistent-artist-sort at album level** (#207), credited with their ARTIST, so a
  conflicting album artist sort on them is reported. Multi-artist albums and
  compilations without ALBUMARTIST are left out. Expect some new or reworded findings
  on existing libraries.
- **Check runner** (#205): a check run that fails now reports the findings it recorded
  before failing instead of 0, and no-cover-art's 30-day "recent failed lookup" window
  is exact (it could count a lookup up to a day older as recent).
- **A tag can now be removed through apply** (#210). The apply request takes `clear`
  (album-level fields) and `track_clear` (per-file fields); every value and spelling of
  the tag is removed, the dry-run diff shows the removal as `new: null`, and the album's
  database row follows. An empty or whitespace-only suggestion still changes nothing, so
  MusicBrainz, check fixes and suggestions never clear a tag because their data is
  missing; a whitespace-only value is no longer written as an empty tag. No screen asks
  for a removal yet; it is available in the API for inline tag editing.
- Windows: API keys were left in the pre-migration database backup
  (`library.v16.before-rust.db`), because the scrubbed copy could not be flushed to
  disk there ("Access is denied"); it is now scrubbed as on macOS and Linux.
- Windows: an album's id no longer depends on how the library folder's path was spelled
  (short `8.3` names, letter case, a `\\?\` prefix); paths resolve to the folder's real
  name, as the earlier Python app did on Windows.
- Windows: `~\Music` is expanded to the home folder like `~/Music`.
- Windows: file errors during BPM / ReplayGain analysis read "Permission denied" / "No
  such file or directory", as on other platforms.
- The filename Resolve no longer renames a Missing album: it is refused (409, "Album is
  missing from disk …") before any file is touched
  ([#212](https://github.com/get2knowio/musaic/issues/212)).
- After a filename rename the album row stays in its own library instead of moving to
  the library currently configured (#212).
- A cover file is recorded with the name it has on disk (`Cover.JPG`, not `cover.jpg`) on
  case-insensitive volumes too (#212).
- Moving a file to Musaic's trash on another volume keeps its modification time, and a
  copy whose original cannot then be removed is deleted again, so a failed move never
  leaves the file twice (#212).
- A filename rename says what it left alone and what failed: the message names files
  that were not found ("1 not found, skipped: '3 c.flac'"), counts ones already renamed,
  names files that kept their old name after an error, and says when a file could not
  be moved back from its temporary name; the response carries the skipped entries as
  `skipped`. A staging failure names the file and says that nothing was renamed
  ([#217](https://github.com/get2knowio/musaic/issues/217)).
- An apply that writes new track numbers but cannot rename a file names it: the stream's
  `done` event carries `rename_failures` (file, intended name, reason), including files
  rolled back after another file failed (#217).
- A MusicBrainz name search answered with an HTTP error (such as a 503 when rate
  limited) or with a body that is not JSON is logged in `server.log`, not only in the
  resolution trace (#217).

## [0.2.0-rc.1] - 2026-11-14

A rewrite. The Python/FastAPI backend and the Electron shell are replaced by a Rust
application packaged with Tauri 2. The React interface and the SQLite database format are
kept; an existing database is migrated in place. The Python implementation is preserved
at the tag `python-final`.

**About these builds.** Installers are published for macOS (Apple Silicon and Intel)
and Linux (x64, AppImage and deb), with their SHA-256 sums in `SHA256SUMS.txt`. They are
**not code-signed or notarised**.

**No Windows build.** The automated tests do not yet pass on Windows, so no Windows
installer is published until they do. A Windows installer was attached to this release
at first and has been withdrawn for that reason. Progress is tracked in
[#178](https://github.com/get2knowio/musaic/issues/178).

- **Run by a person so far: only the macOS Apple Silicon build.** The macOS Intel and
  Linux builds are produced by CI from the same source and have not been tested by hand
  yet. Reports of what works and what does not are very welcome. The automated tests
  pass on macOS and Linux.
- **Opening the unsigned app on macOS:** install with the one-line `install.sh` command
  from the README (a `curl` download should not be quarantined; this is not yet confirmed on a second machine), or, for a `.dmg` downloaded in a
  browser, use **System Settings → Privacy & Security → Open Anyway** after the first
  refusal, or run `xattr -dr com.apple.quarantine /Applications/Musaic.app`. The
  browser-download route has not been tested.
- **Cover downloads now refuse plain `http` URLs and local-network addresses** (for
  example a NAS, `localhost` or a `192.168.x.x` address); only `https` URLs on public hosts
  are fetched. Use **Upload** on the album's cover for an image on your own machine or
  network.

### Added

- Native desktop app: the API server runs inside the app on a random loopback port; one
  window, single instance per config folder, remembered window size and position.
- Native folder picker for the library folder; "Show in Finder / Explorer / file
  manager" for an album folder and for the app's trash.
- Fingerprinting (Chromaprint), ReplayGain and BPM computed in-process; `fpcalc` is no
  longer needed.
- Asking before quitting while work (a scan, an apply, a check run…) is in progress.
- Third-party notices shipped with the app (`NOTICES.txt`).

### Changed

- The database is upgraded to schema v18 on first start, after a backup copy is written
  to `~/.musaic/library.v16.before-rust.db`.
- API keys are stored in the OS keyring (macOS Keychain, Windows Credential Manager,
  Secret Service on Linux) instead of the database; existing keys are moved there on
  first start and removed from the database, its backup and older snapshots.
- External links open in the system browser; only public `https` links are opened.
- The IBM Plex fonts are bundled instead of loaded from Google Fonts.

### Removed

- The built-in AI backend. Controls that need a language model are hidden; MusicBrainz,
  AcoustID and Cover Art Archive lookups and the fixes that use them work as before. An
  MCP server, so that an external agent can drive Musaic, is the next planned piece.
- The Electron shell, the bundled Python runtime and the Python package.

### Security

- The local API checks `Host` and `Origin` on every request, and the desktop app requires
  a per-launch cookie and sends a strict Content-Security-Policy.
- Cover downloads accept only `https` URLs on public hosts, re-checked on every redirect
  and at connection time. Plain-`http` and local-network cover URLs are now refused; use
  cover upload for those images.

[Unreleased]: https://github.com/get2knowio/musaic/compare/v0.2.0-rc.1...HEAD
[0.2.0-rc.1]: https://github.com/get2knowio/musaic/compare/python-final...v0.2.0-rc.1
