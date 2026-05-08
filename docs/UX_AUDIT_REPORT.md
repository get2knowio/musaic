# Musaic — UX Audit

Audit conducted 2026-05-05 against the working tree at
`/Users/paul/Documents/Claude/Projects/Musaic`. This is a holistic
UX/UI audit, distinct from the code-correctness audit in
`AUDIT_REPORT.md` — it asks whether the actions are placed where users
would expect, whether patterns are consistent across surfaces, and where
the information architecture creates friction.

Severity legend (UX): 🔴 UX Bug · 🟡 Inconsistency · 🟢 Polish.

---

## Information architecture map

Top-level shell (`frontend/src/App.tsx:35-73`):

- **Titlebar** (full-width, 36px) — brand, hardcoded breadcrumb
  `~/Music/Library`, search input, view toggle (tiles / list), Rescan
  button, theme toggle, Settings gear icon.
- **Sidebar** (188px, left) — `Sidebar.tsx`. Three groups:
  - Workspace: Library, Playlists *(v1.1)*, Plex sync *(v1.1)*, Scan
    history *(v1.1)*, AI Jobs, Health.
  - Filters: All albums, Needs review, Missing art, Compilations.
  - Bottom-anchored: Settings (duplicated from titlebar).
- **Main canvas** — routed pages.

Routes (`App.tsx:42-55`):

- `/` → redirect to `/library`
- `/library` and `/library/:albumId` (nested route — DetailSheet renders
  in a Radix Dialog over the library when `:albumId` is present)
- `/jobs` — AI Jobs queue
- `/health` — check index (cards per check)
- `/health/:checkId` — drilldown for one check, with bulk dialog
- `/settings`

Modals / sheets / dialogs:

- **DetailSheet** (`DetailSheet/index.tsx`) — right-anchored Radix
  Dialog, drag-resizable. Footer holds "Find cover art", "Suggest fixes",
  and a `…` overflow menu (currently all disabled).
- **HintDialog** (`DetailSheet/HintDialog.tsx`) — gates every AI job;
  also opened by `FindingFixButton` for AI-job checks.
- **DuplicateResolveDialog** (`DuplicateResolveDialog.tsx`) — bespoke
  per-finding dialog with an inner confirmation dialog (nested).
- **BulkQuickFixDialog** (inline in `HealthDetail.tsx:144-322`) — modal
  bulk fix runner with confirm → running → done phases.
- **DeleteConfirmDialog** (inline in `Jobs.tsx:356-442`) — single + bulk
  delete confirm.

Action surfaces by user goal (cross-referenced):

| Goal | Surface(s) | File:line |
| --- | --- | --- |
| Configure library directory | **Nowhere reachable** | `Library.tsx:135-153` (dead-end), `api.ts:55-60` (endpoint exists) |
| Scan library | Auto on Library mount; titlebar **Rescan** button is a no-op | `Titlebar.tsx:100-103` |
| Cancel a running scan | Disabled placeholder | `ScanRibbon.tsx:57-59` |
| Run a single health check | Health card "Run now"; HealthDetail "Run now" | `Health.tsx:157-164`, `HealthDetail.tsx:103-113` |
| Bulk-fix a check | HealthDetail header, opens BulkQuickFixDialog | `HealthDetail.tsx:93-102` |
| Fix one finding | "Fix" button on finding row (Health detail OR DetailSheet Findings) | `FindingFixButton.tsx:236-261` |
| Resolve duplicate tracks | Custom "Resolve…" button → DuplicateResolveDialog | `FindingFixButton.tsx:126-144` |
| Identify album on MusicBrainz | (a) `missing-mbid` per-finding Fix, (b) "Identify on MusicBrainz" bulk, (c) implicit side-effect of Suggest fixes | `missing_mbid.py:62`, `DetailSheet/index.tsx:391-405` |
| Request AI metadata | DetailSheet footer "Suggest fixes" (always opens HintDialog) | `DetailSheet/index.tsx:391-405` |
| Request AI cover art | DetailSheet footer "Find cover art" | `DetailSheet/index.tsx:382-390` |
| Apply AI suggestions | "Apply N…" button in AISuggestions diff view | `AISuggestions.tsx:283-289` |
| Discard AI suggestions | "Discard" button next to Apply, OR "Dismiss" in empty-result state | `AISuggestions.tsx:133-138, 276-281` |
| Retry a failed AI job | Per-row retry icon in Jobs page | `Jobs.tsx:323-333` |
| Resume the auto-paused queue | Banner button on Jobs page; Settings button | `Jobs.tsx:208-219`, `Settings.tsx:104-110` |
| Delete a track file | Only via DuplicateResolveDialog | `DuplicateResolveDialog.tsx:307-316` |
| Rename an album folder | Disabled stub in MoreMenu | `MoreMenu.tsx:33` |
| Re-scan a single album | Disabled stub in MoreMenu | `MoreMenu.tsx:34` |
| Calculate BPM / ReplayGain | Disabled stub in AudioAnalysis | `AudioAnalysis.tsx:80-87` |
| Configure trash path | **Hardcoded** to `~/.musaic/trash` | `musaic/trash.py:65` |
| Configure AI parallelism, AcoustID key, FireCrawl key | Settings page | `Settings.tsx` |
| View prior cover-lookup attempts | Surfaces only inside HintDialog when about to retry | `HintDialog.tsx:174-202` |
| Restore an ignored finding | "Restore" button on each ignored row | `HealthDetail.tsx:619-625` |

---

## Strengths

1. **The "Needs review" affordance has a clear visual chain.** A
   completed AI job lights up `JobStatusBadge` on the album tile
   (`Tile.tsx:37-42`), the sidebar's AI Jobs item shows a count badge
   (`Sidebar.tsx:104-106`), the activity dot pulses while a job runs
   (`Sidebar.tsx:201-213`), and the sidebar **"Needs review" filter chip**
   plus a per-album mode in the DetailSheet all point at the same set.
   The user can drift between surfaces without losing the mental
   thread of "which albums are mid-flight."

2. **The HintDialog is well-considered as an interaction.** It pre-fills,
   auto-focuses, supports `⌘↵` to submit, surfaces character count, and
   — critically — shows **prior failed cover attempts** with timestamps
   and error excerpts (`HintDialog.tsx:174-202`) so the user sees at the
   moment of confirmation that they are about to retry a dead end. That's
   thoughtful.

3. **DuplicateResolveDialog earns its complexity.** It does the hard
   work the user can't do alone: lazy-fetches the MB canonical recording
   (`useDuplicateFindingCanonical`), tries to fuzzy-match each filename
   against the canonical title, and surfaces TWO separate "keep this"
   signals — `Matches MB` (tag agrees with MB) and `Keep this`
   (filesystem-portable filename). Both are tooltipped clearly
   (`DuplicateResolveDialog.tsx:282-299`). The nested confirm dialog
   before deletion is appropriate for an irreversible action, and the
   trash-path explanation is shown right there.

4. **Auto-pause + Resume banner** (`Jobs.tsx:192-221`) — when an AI job
   fails, the queue pauses and the user sees a single sticky banner with
   the count of queued-behind jobs and a one-click Resume. That is
   exactly the right surface; no toast-and-forget.

---

## Findings

### 🔴 UX Bug — confusing, broken, or hidden behavior that will trip users up

#### F1. The "Configure your library" empty state points users to a setting that doesn't exist

- **Where:** `frontend/src/pages/Library.tsx:135-153` (the
  `NoLibraryConfigured` placeholder); `frontend/src/pages/Settings.tsx`
  (no library-dir section anywhere).
- **What the user sees / experiences:** First run → Library page renders
  a centered empty state: *"Configure your library… Open Settings (gear
  icon, top right) to set the library directory."* They click the gear,
  land on Settings, scroll the entire page, and find sections for AI
  worker, Track fingerprinting, FireCrawl. **No library-directory
  field exists in the UI.** The only way to configure it is to edit
  `~/.musaic/config.json` by hand or POST to `/api/config`
  directly.
- **Why it's a problem:** This is a literal dead-end for first-run
  users. The instructional copy is wrong and the documented entry point
  is empty. The API exists (`api.ts:55-60`, `useSetLibraryDir` in
  `useLibraryConfig.ts:14-24`) — only the UI is missing.
- **Suggested fix:** Add a "Library" section at the top of
  `Settings.tsx` with a directory input (and a "Browse…" if a
  native-folder picker is workable, otherwise paste-only). Mirror the
  AcoustID write-once pattern: show "configured: /path" with a Change
  button, surface a validation message when the directory doesn't
  exist.

#### F2. Titlebar "Rescan" button does nothing

- **Where:** `frontend/src/components/Titlebar.tsx:99-103`.
- **What the user sees / experiences:** The button has the label
  **Rescan** with a refresh icon, sits prominently next to the search
  bar, and is fully styled (no opacity-60, no `disabled`, no tooltip
  saying "coming soon"). Clicking it does **nothing** — no API call,
  no toast, no visual change.
- **Why it's a problem:** This is the most visually-affordable button
  in the chrome. Users will click it expecting a scan, get no feedback,
  and either (a) click it twenty times wondering why their library
  didn't refresh or (b) lose trust in the UI. The endpoint exists
  (`/api/scan/rescan`, `api.ts:79-81`), it's just not wired up.
- **Suggested fix:** Wire it to `rescanLibrary()` from `api.ts`, then
  trigger a fresh `useScanStream` cycle. Disable + show a spinner while
  a scan is already in flight. If keeping it as a v1.1 placeholder, add
  `disabled` + `opacity-60` + `title="Coming in next milestone"` like
  every other unimplemented button in the app does.

#### F3. The DetailSheet footer's `…` overflow menu has only "Coming in next milestone" items

- **Where:** `frontend/src/components/DetailSheet/MoreMenu.tsx:33-39`.
- **What the user sees / experiences:** Every album sheet shows a `…`
  button in the footer next to "Suggest fixes". Clicking it reveals
  three disabled items (Rename folder, Re-scan this album, Force
  re-analyze) and a small italic *"Coming in next milestone"* note.
  No actions are reachable.
- **Why it's a problem:** The button is a content-empty affordance — the
  user pays attention cost (mouse hover, dropdown open, mental model
  shift) for no payoff every time they open an album. Worse, since the
  items have shortcut hints (⌘R, ⌘⇧R), users may try those keystrokes
  expecting them to work.
- **Suggested fix:** Either (a) hide the menu trigger entirely until at
  least one item is implemented, or (b) replace the disabled stubs with
  one real action — e.g. "Open in Finder" (trivial — `path` is already
  on the album) — so the menu has *something* useful in it today.

#### F4. "Find cover art" + "Suggest fixes" hide while an AI job is in flight, with no visible queue management for the user mid-flow

- **Where:** `frontend/src/components/DetailSheet/index.tsx:380-407`
  (`disabled={!!activeJob}` on both buttons).
- **What the user sees / experiences:** Open an album, click **Suggest
  fixes**, fill in the hint, submit. The DetailSheet shows a *Queued* or
  *Asking Claude…* banner (`index.tsx:282-294`). Both footer buttons
  become **disabled** with `opacity-60`. If the user wants to *cancel*
  the job they just queued (typed wrong hint, wrong album, changed
  mind), they have to: close the sheet → navigate to AI Jobs → find the
  row → click delete (and the delete is disabled while the job is
  *running*).
- **Why it's a problem:** The entire job lifecycle for "this" album is
  visible in this sheet via the banner, but the actions to manage the
  job (cancel-while-queued, retry-while-failed, jump-to-Jobs-page)
  are not. The user has only one verb here (wait), and the path to
  recourse is multiple navigations away.
- **Suggested fix:** When `isQueued && activeJob.status === "queued"`,
  show a small "Cancel queued job" link in the banner that calls
  `deleteJob(activeJob.id)`. When the job has failed, the banner should
  offer "Retry" + "View error" inline. The footer's disabled state
  still makes sense — one job per album — but the banner should grow
  to be the album-level mini-Jobs-page.

#### F5. The hint dialog appears inconsistently — sometimes before AI, sometimes after a click that runs MB silently

- **Where:** `FindingFixButton.tsx:218-234`, `missing_mbid.py:87-127`.
- **What the user sees / experiences:** Clicking "Fix" on a `missing-mbid`
  finding opens a HintDialog (because `HINT_MODE_FOR_CHECK` includes it).
  But the *bulk* "Identify on MusicBrainz" button in the Health detail
  header runs MB-direct against everything with no dialog. So the same
  goal — anchor an album to MB — has two paths with completely different
  UIs: one mandatory hint dialog, one batch-runner with progress. And
  per-finding **Fix with empty hint** silently tries MB first and only
  hits AI if MB fails — which the user can't see from the dialog. They
  click Fix, accept the dialog with no hint, and watch a "Queued" pill
  appear *only sometimes* (only when MB-direct missed).
- **Why it's a problem:** The user has no way to tell from the hint
  dialog what will actually happen. Empty-hint submit might be free (MB
  match) or might burn AI quota. Even worse, on `missing-mbid` empty-hint
  Fix that succeeds via MB doesn't go through the job queue — so the
  user gets *no feedback at all* in the UI beyond the row eventually
  disappearing on the next check refresh.
- **Suggested fix:** In the HintDialog, when the calling check has an
  MB-direct shortcut (i.e. `try_quick_fix` is non-null on the server),
  add inline copy: *"If you submit empty, we'll try MusicBrainz first
  for free; AI runs only if MB can't disambiguate."* Optionally, change
  the empty-hint behavior to surface a transient toast ("Tried
  MusicBrainz — no match, queuing AI") so the user always knows what
  fired.

#### F6. The Sidebar has THREE "v1.1" workspace items rendered at full visual weight

- **Where:** `frontend/src/components/Sidebar.tsx:22-26, 233-235`
  (Playlists, Plex sync, Scan history).
- **What the user sees / experiences:** Below the live "Library" link,
  three items with icons and labels and a small mono `v1.1` badge,
  rendered at `opacity-55` but still in the navigation flow.
- **Why it's a problem:** Each click on these items does nothing (the
  link is omitted because `item.later: true`). Mid-session the user
  forgets they're disabled and clicks them anyway. They take up vertical
  real estate in a primary nav, pushing AI Jobs and Health below the
  fold on shorter viewports.
- **Suggested fix:** Either move the "later" items to a single
  collapsible "Coming soon" footer below Settings, or remove them
  entirely until they ship — they're a roadmap document, not a nav.

### 🟡 Inconsistency — varying patterns, naming drift, or unpredictable behavior

#### F7. "Apply" vs "Save" vs "Accept" vs "Submit" — verb drift across modals

- **Where:**
  - AISuggestions: **Apply** (`AISuggestions.tsx:283-289`).
  - HintDialog: **Ask Claude** (`HintDialog.tsx:153-159`).
  - BulkQuickFixDialog: **Start** during confirm
    (`HealthDetail.tsx:292-298`).
  - DuplicateResolveDialog inner confirm: **Move to trash**
    (`DuplicateResolveDialog.tsx:233-243`).
  - DeleteConfirmDialog (Jobs): dynamic (`Delete`, `Delete N`)
    (`Jobs.tsx:402-405`).
  - Settings (AcoustID/FireCrawl): **Save** (`Settings.tsx:291-297`).
- **What the user sees / experiences:** Five different verbs for "commit
  this dialog's primary action," some referring to the action's effect
  and some to its mechanism. The pattern is inconsistent enough that
  users have to read each button rather than clicking the rightmost
  primary-colored one with confidence.
- **Why it's a problem:** Mostly minor — the *placement* (always
  bottom-right, accent-colored) is consistent. But the verbs swing
  between user-intent ("Apply") and provider-detail ("Ask Claude"),
  which mixes models.
- **Suggested fix:** Adopt one verb family for "commit": always
  describe the user-side effect, not the mechanism. "Ask Claude"
  → "Submit" or "Suggest fixes" (matching the launching button label).
  "Start" → "Run on N" (matching the launching button label). Keep
  destructive verbs concrete ("Delete N", "Move to trash"). Keep
  Save for in-place form mutations.

#### F8. "Discard" vs "Dismiss" vs "Cancel" — secondary verb drift

- **Where:**
  - AISuggestions changes-present panel: **Discard**
    (`AISuggestions.tsx:276-282`).
  - AISuggestions empty-result panel: **Dismiss**
    (`AISuggestions.tsx:133-138`).
  - All dialogs: **Cancel**.
  - Sheet header: ✕ icon.
- **What the user sees / experiences:** Two adjacent UI states show
  semantically identical buttons (mark the AI job acted-on with
  `action: "discarded"` per `index.tsx:248-252`) under different labels.
- **Why it's a problem:** Users may infer "Dismiss" is non-destructive
  (the suggestion stays around) and "Discard" is destructive (the
  suggestion is gone). They are the same call. Inconsistent terms
  invent distinctions where there are none.
- **Suggested fix:** Use **Discard** in both places (the action *is*
  discarding the AI job's output). Reserve **Cancel** for "abandon
  this dialog without committing."

#### F9. Job status labels disagree across surfaces

- **Where:**
  - Jobs page status pills: `Queued`, `Running`, **Ready**, `Failed`,
    `Applied/Discarded` (`Jobs.tsx:444-479`).
  - JobStatusBadge on tile/row: `Queued`, `Running`, **Ready**
    (`JobStatusBadge.tsx:23-35`).
  - FindingFixButton states: `Queued`, `Running…`, **Review**
    (`FindingFixButton.tsx:148-191`).
  - DetailSheet banner: "Queued" or "Asking Claude…"
    (`DetailSheet/index.tsx:286-292`).
- **What the user sees / experiences:** A completed AI job is "Ready"
  on the tile, "Ready" on the Jobs page, but **"Review"** on the
  finding's Fix button. Same status, three labels. The DetailSheet
  banner's running state is "Asking Claude…" while the Jobs page
  calls the same state "Running".
- **Why it's a problem:** The user has to mentally translate between
  surfaces to know they're looking at the same thing. "Ready" and
  "Review" are doing the same affordance (both clickable, both
  resolve to the AI suggestion).
- **Suggested fix:** Pick one verb pair per state. Suggested:
  `Queued · Running · Ready · Failed`. Drop "Review" on the FindingFix
  button — call it "Ready" with the same icon and let the click target
  open the album sheet (already does).

#### F10. "Suggest fixes" vs "Find cover art" vs "Fix" vs "Resolve" — naming the same family of actions

- **Where:**
  - DetailSheet footer (`DetailSheet/index.tsx:382-405`): **Find cover
    art** + **Suggest fixes** (with bonus suffix " (+ cover)" when no
    cover present).
  - FindingFixButton (`FindingFixButton.tsx:236-246`): **Fix**
    (everywhere, except duplicate-tracks which is **Resolve…**).
  - HintDialog title (`HintDialog.tsx:32-35`): **Suggest metadata
    fixes** / **Find cover art**.
  - Health detail bulk button: dynamic ("Try MusicBrainz on all",
    "Identify on MusicBrainz", "Fingerprint all")
    (`HealthDetail.tsx:93-102`).
- **What the user sees / experiences:** Six distinct verbs to express
  variations of "ask the system to fix something." When the user opens
  an album with a `no-cover-art` finding and clicks the Findings-section
  Fix button, they see a HintDialog titled "Find cover art." If they
  close the sheet and click the footer's "Find cover art," they get the
  *same* HintDialog with the same title. But the launcher labels
  disagree.
- **Why it's a problem:** This is mostly tolerable — different verbs at
  different specificity levels. But the naming pattern means users
  can't predict what dialog they'll see. The bulk action verbs
  ("Try MusicBrainz on all") expose implementation, mixing with
  outcome-oriented per-finding verbs ("Fix").
- **Suggested fix:** Standardize the per-album launchers to mirror the
  HintDialog titles ("Suggest fixes" → "Suggest metadata fixes"). For
  the bulk button, switch from MB-mechanism phrasing to outcome
  phrasing: *"Try free fixes on all (N)"*, with an info icon
  explaining MB is the source. Keep "Identify on MusicBrainz" for
  `missing-mbid` since that's a literally accurate, user-facing
  description.

#### F11. The sidebar Library link's filter-chip behavior is subtle and lossy

- **Where:** `frontend/src/components/Sidebar.tsx:51-70` and
  `:127-133`.
- **What the user sees / experiences:** The user is on `/library` with
  `?filter=needs-review&q=jazz&albumId=abc`. They click the Library
  workspace item in the sidebar. The link target is the
  `rememberedLibraryUrl` (the last `/library` URL), so they bounce back
  to the *same* state. But if they click a filter chip while on
  `/jobs`, that builds a new `/library?filter=...` and **drops the
  `:albumId` segment** intentionally (`Sidebar.tsx:60`). And clicking
  the same filter chip toggles it off (`Sidebar.tsx:63-69`).
- **Why it's a problem:** Three subtle behaviors crammed into one nav
  pattern. Toggling-off-by-clicking-active is a power-user feature with
  no affordance; new users won't know they can do it. The "drop album
  segment when navigating from /jobs" rule is right but invisible.
- **Suggested fix:** Add a small × on the active filter chip explicitly
  (the FilterBar already shows a `filter: needs-review` pill in
  `FilterBar.tsx:25-32` — give that the × instead of relying on the
  sidebar toggle). Keep the navigation logic as-is.

#### F12. Settings has no grouping for "external service keys" — they're parallel sections

- **Where:** `frontend/src/pages/Settings.tsx:13-26`.
- **What the user sees / experiences:** Three sections stacked: AI
  worker, Track fingerprinting, FireCrawl fallback. Two of them
  (Fingerprinting + FireCrawl) are essentially "third-party API keys
  with the same write-once UX." They have similar headers (`Sparkles`,
  `Fingerprint`, `Globe`), but no grouping or hierarchy. There is no
  Library section (see F1).
- **Why it's a problem:** The page reads as a flat list of unrelated
  config blobs. As you add more (Plex creds, Spotify, etc. per the
  v1.1 sidebar items), this becomes a swamp.
- **Suggested fix:** Group as: **Library** (path), **AI** (worker
  parallelism, queue state), **External services** (AcoustID,
  FireCrawl), **About** (version / version-strings, log location).

#### F13. "Apply" button label in AISuggestions is dynamic and very long

- **Where:** `AISuggestions.tsx:295-305` (`applyButtonLabel`).
- **What the user sees / experiences:** Depending on what's being
  applied, the button reads e.g. *"Apply 3 album fields + 8 tracks, 4
  renames + cover"*. On a narrow sheet (the user has dragged it down
  to 400px) this almost certainly wraps or gets truncated by the
  flex container (`AISuggestions.tsx:283-289` has no `truncate`).
- **Why it's a problem:** Information-rich is good, but at narrow sheet
  widths the diff button becomes unreadable while the diff content
  above is what tells the user what's changing anyway.
- **Suggested fix:** Truncate to a summary form: *"Apply (3 album, 8
  tracks, 4 renames, cover)"* using shorter labels and counts only;
  put the long form in `title` for the tooltip.

#### F14. Album-level findings (`finding.album_id == null`) render as `Library-level` italic with no link or action

- **Where:** `HealthDetail.tsx:597-598`,
  `FindingFixButton.tsx:118-122` returns `null` for null `album_id`.
- **What the user sees / experiences:** A row that says *Library-level*,
  with the finding summary, no Fix button, only "Ignore". The user has
  no way to act on it.
- **Why it's a problem:** Today no check actually emits library-level
  findings (verified via grep — every `Finding(...)` in
  `musaic/checks/*` carries an `album_id`). The dead branch
  exists in the UI but isn't reachable. If a future check yields one,
  the user will see a row they can do nothing with except ignore.
- **Suggested fix:** Until a check actually needs it, drop the
  `Library-level` rendering — or at minimum show a hint like *"This
  applies to your whole library; no per-album action available."*

#### F15. The two filter views ("Needs review" sidebar chip, Jobs page filter pills) overlap with subtly different membership

- **Where:**
  - Sidebar's "Needs review" chip filters albums where any job is
    `completed` or `failed` (`filters.ts:53-61`).
  - Jobs page "Needs review" filter (`Jobs.tsx:25`) shows jobs in
    statuses `["completed", "failed"]`.
- **What the user sees / experiences:** Same name, two different
  granularities (albums vs jobs). If an album has a completed AI job,
  it shows in both. But the album version coalesces multiple jobs for
  the same album to one "needs review" badge.
- **Why it's a problem:** Mostly OK — the granularity matches each
  page's domain. But hovering both filters in quick succession and
  seeing different counts (e.g. 3 in sidebar, 5 in Jobs) is confusing
  and there's no inline copy explaining why.
- **Suggested fix:** No big change needed; consider tooltipping the
  sidebar count: *"3 albums · 5 jobs awaiting review"* so the user
  sees both perspectives at once.

#### F16. Mid-apply, the user can navigate away — no warning, no recovery

- **Where:** `DetailSheet/index.tsx:376-377` (the apply footer message
  is "Writing — keep this panel open").
- **What the user sees / experiences:** The user clicks Apply. The
  footer shows *"Writing — keep this panel open"* in muted text. The
  sheet's close button and overlay-click are *not* disabled, so the
  user can hit Escape, click outside, or click the X — and they will,
  not knowing the apply is mid-write. The apply continues server-side
  but the client loses the SSE stream and the album won't update its
  cache until the next scan.
- **Why it's a problem:** The copy says "keep this panel open" — but
  nothing prevents the user from violating that contract. There's no
  "are you sure?" gate.
- **Suggested fix:** While `apply.state.status` is `"writing"`, set
  `onEscapeKeyDown={(e) => e.preventDefault()}` and pass `modal` /
  intercept `onOpenChange` to prevent close. Keep the X button visible
  but make it open a small confirm. Or: keep the close affordances
  alive but re-attach to the apply stream on reopen using job-id /
  album-id. (Simpler: prevent close.)

#### F17. The Findings section in DetailSheet doesn't tell the user where the canonical fix surface is

- **Where:** `frontend/src/components/DetailSheet/FindingsSection.tsx`.
- **What the user sees / experiences:** Per-album findings show with
  Fix and Ignore buttons. There's a tiny `<Link to="/health/{checkId}">`
  on the check label *(`FindingsSection.tsx:131-137`)*, but it's mono
  and small. The user doesn't know that the Health page exists as a
  cross-album view of the same thing.
- **Why it's a problem:** Users who land on the album page and fix
  findings one-by-one never discover the bulk affordances on
  `/health/:checkId`. That's where the time-saver lives.
- **Suggested fix:** Add a one-line note above the findings list:
  *"This album has open findings — you can also bulk-fix similar
  issues across the library from [Health → Missing core tags](…)."*

#### F18. "Ignore" has no reason field on the Health detail page

- **Where:** `HealthDetail.tsx:557-562`,
  `FindingsSection.tsx:110-114`,
  backend `api.ts:220-225` accepts `reason`.
- **What the user sees / experiences:** Click Ignore → the row dims
  with `opacity-55`. No reason captured. The dimmed row shows
  "ignored: …" only if `ignored_reason` was set, but the UI never
  asks for one.
- **Why it's a problem:** Three months later the user looks at the
  same finding and can't remember why they ignored it. The data
  surface is there (the API takes a reason, the row renders it) — only
  the input UX is missing.
- **Suggested fix:** Replace bare `Ignore` button with a small dropdown:
  Ignore (no reason) / Ignore with reason… (opens a tiny popover with
  a textarea). Or always open a popover; default to empty.

### 🟢 Polish — small wins, copy improvements, affordance adds

#### F19. Hardcoded breadcrumb in titlebar

- **Where:** `Titlebar.tsx:58-63` shows `~/Music/Library` regardless of
  the actual configured library_dir or current route.
- **Why it's a problem:** Misleading on Library *(if their library is
  `/Volumes/Audio/FLAC`, this lies)* and on every other route *(it's
  not the user's location)*.
- **Suggested fix:** Either render the abbreviated current route
  (`Library`, `AI Jobs`, etc.) or the abbreviated `library_dir` from
  `useLibraryConfig()`. The FilterBar already does the latter
  (`FilterBar.tsx:39-46`) — pull that helper up.

#### F20. The traffic-light dots in the titlebar are decorative-only and macOS-styled on every platform

- **Where:** `Titlebar.tsx:45-49`.
- **Why it's a problem:** Cosmetic, but on Linux/Windows builds (or in
  a browser tab outside an Electron-style shell) they communicate "this
  is a window, click these" — and they don't do anything.
- **Suggested fix:** Drop them when not running inside a frameless
  window shell, or replace with the BrandMark scaled up so the brand
  is the leftmost element.

#### F21. The "no cover art" badge on tiles is two different things in two views

- **Where:**
  - Tile (`Tile.tsx:43-47`): a small red ❓ at top-left.
  - List row (`AlbumList.tsx:88-92`): a yellow uppercase `no art`
    pill.
- **Why it's a problem:** Same warning, two visual encodings.
- **Suggested fix:** Pick one — probably the pill, since "?" reads as
  a question rather than a warning.

#### F22. The Settings page tab title doesn't match the route name

- **Where:** Sidebar lists "Settings"; titlebar gear icon's tooltip
  is "Settings". The page itself shows "Settings" as `<h1>`. 
  But the document `<title>` isn't set anywhere.
- **Why it's a problem:** Browser tabs all show the same title (likely
  the `index.html` default). Users with multiple windows open can't
  tell which is which.
- **Suggested fix:** Add `useEffect(() => { document.title = "Settings · Musaic" }, [])`
  per page, or pull a small `usePageTitle` hook.

#### F23. "Last run … " vs "scanned … " formats

- **Where:** Both `Health.tsx:152-156` and `FilterBar.tsx:47-54` show
  relative time, but Health says "Last run 5m ago" and FilterBar says
  "scanned 5m ago". Same time, different phrasing.
- **Suggested fix:** Lower-priority consistency; both are clear
  in-place. Worth flagging if you ever standardize a time-ago
  component.

#### F24. The duplicate-resolve dialog uses `<Music />` icon for the dialog header (`Copy3Icon`)

- **Where:** `DuplicateResolveDialog.tsx:121-124, 320-322`.
- **Why it's a problem:** The icon is named `Copy3Icon` but renders a
  `Music` icon. Lucide has `Copy` / `CopyMinus` / `Files` icons that
  would communicate "duplicates" more clearly. Most other places use
  `Files` for duplicates (see `Health.tsx:186`).
- **Suggested fix:** `import { Files } from "lucide-react"` and replace
  the `Music` icon in the dialog header.

#### F25. The HintDialog placeholder examples mention scenarios that don't apply to all checks

- **Where:** `HintDialog.tsx:25-30`.
- **What the user sees / experiences:** The metadata-mode placeholder
  is *"e.g. This is a 2012 reissue, original was 1979. Track 7 is
  mislabeled — it's really 'Comfortably Numb'…"*. When the dialog
  opens for a `missing-mbid` Fix on an album with no track-level
  problem, the "track 7" example is misleading.
- **Suggested fix:** Make the placeholder dependent on the calling
  check id. Reach a fourth `HINT_PLACEHOLDER_FOR_CHECK` map:
  `missing-mbid` → "Year, regional pressing, alternate name…",
  `missing-core-tags` → existing copy, etc.

---

## Cross-cutting themes

**Theme 1 — Phantom affordances.** Three of the most visually-prominent
buttons in the chrome do nothing right now: the **Rescan** button in
the Titlebar (F2), the `…` MoreMenu in every album sheet (F3), and
both **Calculate** buttons in the AudioAnalysis section
(`AudioAnalysis.tsx:80-87`). Three sidebar items are also dead
(F6). The codebase has a clear "coming in next milestone" idiom —
disabled + opacity-60 + tooltip — but it's applied inconsistently.
The Rescan button breaks the pattern entirely (no disabled, no
tooltip, full styling), making it the single worst offender. The
fix is uniform: either ship the action or hide it. Half-shipped UI
trains users to mistrust button labels.

**Theme 2 — The "fix" verb is wildly overloaded.** Across the codebase
"fix," "resolve," "apply," "ask Claude," "suggest," "find," "identify,"
"try MusicBrainz," and "quick-fix" all describe overlapping behaviors:
sometimes free MB lookup, sometimes paid AI, sometimes a synchronous
file mutation, sometimes enqueueing async work. The HintDialog title
("Suggest metadata fixes") doesn't agree with the launcher button
("Fix"), and the result section renders something else again
("Apply N + cover"). Inside one user session a single verb refers to
3–4 different mechanisms. The user can't predict cost
(free vs AI-quota), can't predict latency (sync vs queued), and can't
predict where the result lands (in-place vs Jobs page vs suggestion
sheet). The deepest UX win available here is **a status-line
description** under each launcher button: *"Free MusicBrainz lookup,
~1 second"* vs *"AI lookup, queued — typically 30s, burns ~1 Claude
call"*.

**Theme 3 — Settings is incomplete in a load-bearing way.** The first-run
user flow is broken (F1) because Settings has no library-directory
field, even though the empty state in `Library.tsx` directs them
there. Combined with the Rescan button doing nothing and the dead
Library `setLibraryDir` API exposed in `api.ts`, a new user genuinely
has no in-app path to bootstrap. Beyond first-run, the trash directory
is hardcoded (`trash.py:65`), there's no way to clear it from the UI,
no way to see how many trashed files exist, and no "Open trash folder"
shortcut — for an app whose primary irreversible action is
"move to trash." Settings should grow a Library section, a Trash
management section ("12 files in trash · Restore all · Empty"), and
ideally surface the AI Jobs daily/monthly quota / cost so the user
sees the resource implications of their actions.

---

## Top recommendations

Ranked by friction reduction for a typical session:

1. **Add a Library section to Settings + wire the Rescan button**
   (F1 + F2). These two together fix first-run and the most
   visible-but-broken control in the chrome. Low implementation cost,
   highest gain in trust.

2. **Standardize job-state vocabulary across surfaces** (F9, partial
   F10). One pass through `JobStatusBadge`, `FindingFixButton`,
   `Jobs.tsx`, and `DetailSheet/index.tsx` to use
   `Queued · Running · Ready · Failed` everywhere. The user navigates
   between Library tiles, AI Jobs page, Health detail rows, and the
   DetailSheet during a normal session — vocabulary parity reduces
   cognitive load on every transition.

3. **Make the AI cost/path explicit in the launcher and HintDialog**
   (F5, F10, F25). Add a one-line "what this will do" under each
   launcher button: *"Free MusicBrainz lookup"* / *"AI lookup,
   queued"* / *"Tries MB first, falls back to AI"*. Add per-check
   placeholder copy in the HintDialog. Surface a transient toast when
   empty-hint Fix succeeds via MB-direct ("Tried MusicBrainz — match
   applied"). This kills the entire class of "did anything happen?"
   confusion.

4. **Lock the DetailSheet during apply** (F16). One-line fix
   (`onEscapeKeyDown={(e) => e.preventDefault()}` + intercept
   `onOpenChange` while `apply.state.status === "writing"`). Prevents
   silent loss of apply progress when users instinctively click away.

5. **Hide or replace stub UI** (F3, F6, F20). Remove the MoreMenu
   trigger when every item is disabled. Move "v1.1" sidebar items to
   a small "Coming soon" footer. Drop the macOS traffic-light dots when
   running outside a frameless shell. Reduces the per-session load of
   "is this clickable?" decisions.
