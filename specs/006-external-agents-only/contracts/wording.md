# Contract: wording

Applies to the desktop and `docs/agents.md`. The words "pairing" and "pair" stay in code, database and the tool contract.

## Settings → Agents (FR-030)

| Now | Becomes | Where |
| --- | --- | --- |
| Pair an assistant (form label) / Pair assistant (button) | Add an assistant / Add assistant | `frontend/src/pages/SettingsAgents.tsx:150,185` |
| Paired · 2 minutes ago | Added · 2 minutes ago | `frontend/src/lib/agentCopy.ts:119-124`, rendered `SettingsAgents.tsx:316-322` |
| Bridge reachable · not yet | Connected · not yet | same |
| Subscription active · no | Listening for work · no | same |
| Last work claimed | Last request picked up | same |
| Revoke | Revoke (unchanged) | |

The `GET /api/agent/diagnostics` row in `docs/operations.md` names the four facts with the new words.

## Activity, Review, album panel, finding buttons

| Now | Becomes |
| --- | --- |
| "AI jobs" card (`ActivityPanel.tsx:431`) | "Research requests" |
| "…bulk fixes and AI jobs." (`ActivityPanel.tsx:133`) | "…bulk fixes and research requests." |
| "Resume queue" (`ActivityPanel.tsx:441`, `Settings.tsx:655`), `PausedBanner` (`JobRow.tsx:29-62`) | removed |
| "Clear N applied or discarded" (`ActivityPanel.tsx:472`) | unchanged text; N is the jobs stream's `queue_state.clearable`, a count of requests (research D13) |
| "Jobs in progress and history" (Review rail) | "Research requests and history" |
| "Failed jobs" (Review rail) | "Failed research" |
| `JobStatusBadge` title "AI job"/"Job" (`:48`) | "Research request" |
| "An AI job is queued…"/"A job is queued…" (`FindingFixButton.tsx:256`) | "Research is pending for this album" |
| "Claude is working on this"/"A job is working on this" (`:268`) | "An assistant is working on this" |
| "An AI job already exists…", "Ask AI for corrections…" (`DetailSheet/index.tsx:710-713`) | "Research is already requested for this album", "Request research…" |
| "Ask AI to find a cover…" (`DetailSheet/AlbumHero.tsx:193`) | "Request cover research…" |
| "may still need AI help" (`DetailSheet/ResolutionBadge.tsx:92`), Claude variants (`:61-105`) | neutral text only |
| "AI reviewed" (`DetailSheet/PropTable.tsx:47`) | "Reviewed" |
| "Claude's suggestions" (`JobRow.tsx:228-268`) | "Suggestions" |
| "Failed (Claude error)" (`DetailSheet/HintDialog.tsx:324`); hint dialog Claude lines (`:71,75,199`) | "Failed"; "The resolver is tried first. Anything it can't settle goes to your assistant." |
| "AI jobs"/"Jobs" (`SettingsBackup.tsx:424,442`), "AI-driven MBID anchors" (`:158`) | "Research requests", "MusicBrainz anchors" |
| "never sent to Claude" (`Settings.tsx:833-834`) | "never sent anywhere but AcoustID" (or the existing neutral branch) |
| `useActivity.ts:79`, `activityModel.ts:264,282` "AI jobs" | "research requests" |

## Fix and Fix all (FR-007, FR-012, FR-018)

| Place | Text |
| --- | --- |
| Fix tooltip, the four research-capable checks (`lib/findingActions.ts:45-94`, one copy only) | "Tries MusicBrainz first. Anything it can't settle is sent to your assistant for research. Add a hint to help." (cover: "Tries MusicBrainz and the Cover Art Archive first…") |
| Fix all dialog choice | "Send what the resolver can't settle to my assistant" (checkbox; on when at least one assistant has been added) with "N findings in scope" |
| Fix all summary | "Settled A. Sent B for research (one batch). C already requested. Skipped D, failed E." |
| Batch entry (Review, Activity) | "B research requests from Fix all · <check name>" + counts by state; "from an unfinished run" when `unfinished` |
| No assistant added, request made | "Waiting for an assistant" (existing state label) |

## Removed text (SC-005)

"AI suggestions are not available in this build." (`aiGating.ts:50`), "AI backend" (`Settings.tsx:228,423,641`), "(this build has none)", "Provider set to …", "AI worker", "AI queue paused after a failure", "No AI quota will be burned.", "…can still be fixed individually with AI.", "Asking Claude…" (`check_fixes.rs:667`, `TAGS_AI`), "this build has no other source to try" (server message). A frontend test asserts that none of "AI job", "AI backend", "Resume queue", "not available in this build" appears in rendered text of the pages walked in User Story 3.
