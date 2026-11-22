---
name: "speckit-next"
description: "End-to-end pipeline for one Musaic spec: take a GitHub issue, a Notion prompt (Musaic project, 'Prompt N · …' pages), a spec number or an unbuilt spec directory, confirm it with the user, then specify → clarify (auto-accept) → plan → tasks → analyze (auto-remediate) → implement (sonnet subagent) → code-review xhigh --fix (opus subagent) → commit, push, PR → opus subagents fix any issues filed during the run. Unattended after the single confirmation. Customized for get2knowio/musaic from the project-agnostic speckit-next in Notion (Home / Standards)."
argument-hint: "Optional: a GitHub issue (#N, N, or URL), a Notion prompt title or URL, or a spec number / specs/NNN-slug directory — instead of auto-detecting the next prompt"
compatibility: "get2knowio/musaic only: spec-kit (.specify/, speckit-* project skills), Rust workspace in rust/, React UI in frontend/"
metadata:
  author: "get2knowio"
  origin: "Copied from 'speckit-next (project-agnostic)' in Notion (Home / Standards) on 2026-11-20 and customized for Musaic: the project profile is fixed below instead of derived, the queue is the Musaic Notion project's prompt pages instead of a Spec Prompts database, and the issue sweep lands on the spec branch and never merges."
user-invocable: true
disable-model-invocation: false
---

# Speckit Next (Musaic): drive the next spec prompt end to end, unattended

Run the entire spec lifecycle for the next spec prompt with exactly ONE user
interaction: confirming the detected prompt (Step 0.5). After that
confirmation the user is not watching. Never stop to ask a question that a
documented recommendation or defensible default can answer. Stop only for
destructive actions, an architectural decision (Step 1.5), or a genuine
blocker, and say exactly what it is.

## Step 0.0 — The Musaic profile (fixed; re-check it against CLAUDE.md each run)

This copy is bound to this repository. State the profile in one short block at
the start of the run. If CLAUDE.md now says something different from a value
below, CLAUDE.md wins: use its value and say so in the final report.

| Role | Value |
| --- | --- |
| **repo** | `get2knowio/musaic` (GitHub visibility is private, but write everything as if public), default branch `main` |
| **prerequisites** | Build the UI once before any whole-workspace cargo command: `(cd frontend && npm ci && npm run build)`. Tests that need audio require the synthetic library: from `rust/`, in a venv under the scratchpad, `pip install numpy==2.5.4 soundfile==0.14.0 mutagen==1.48.1 Pillow==13.0.0` and then `python3 testdata/synthetic-library/gen_fixture_library.py`. Afterwards restore the manifest with `git checkout rust/testdata/synthetic-library/library.manifest.json` and never commit it. |
| **testAll** | From `rust/`, run `cargo test --workspace`. From `frontend/`, run `npm test`. |
| **testParallel** | Same as testAll. Both runners are parallel already, so add no flag. |
| **testTargeted** | Rust: `cargo test -p <crate> [--test <file>] [<name filter>]` from `rust/`. UI: `npx vitest run <files>` from `frontend/`. |
| **ciLocal** | The chain CI runs, as `ci passed (linux, macos, frontend)`. From `rust/`: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. From `frontend/`: `npm run lint`, `npm run typecheck`, `npm run typecheck:test`, `npm test`, `npm run build`. When the change touches `musaic-desktop`, also run `cargo test -p musaic-desktop --test boot -- --ignored --nocapture` after building the UI. |
| **specsDir** | `specs/`, sequential `NNN-slug` (`.specify/init-options.json`: `feature_numbering: sequential`) |
| **branchFormat** | Specs use `NNN-slug`, the same as the spec directory (precedent: `001-operations-without-http`, PR #316). The issue sweep uses `fix/issue-sweep-NNN`. Other work uses typed prefixes (`fix/`, `ux/`, `refactor/`, `test/`). |
| **commitFormat** | A sentence-case imperative summary with no type prefix, with any issue numbers in parentheses, e.g. `Native menus and one command registry (#333, #334)`. A squash merge appends `(#PR)`. |
| **commitTrailer** | The `Co-Authored-By` line from the session's current attribution reminder (e.g. `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`). PR bodies end with that reminder's PR line. |
| **merge** | Squash. The user merges, or says "merge once CI passes" for that PR. This pipeline never merges on its own. The required check is `ci passed (linux, macos, frontend)`. |
| **queue** | Notion, Get2Know → Home → Projects → **Musaic** (page `413acfdc-54e8-81e7-b21d-f4d8d48b622e`). Its numbered child pages (e.g. "7 · MCP implementation specs …") hold child pages titled `Prompt N · <title>` plus a "Delivery order" section. There is no Spec Prompts database for Musaic; the EarlyBird one (`collection://5aef1e4e-…`) has no musaic Repo option, so never use it here. |

**Invariants.** Pass these verbatim to every subagent you spawn:

- Read `CLAUDE.md` first. Its architectural rules 1–7 are binding:
  1. publish events, and never invoke the fan-out;
  2. findings store references, and dialogs re-derive;
  3. path identity by same-file, never by string;
  4. one accessor per album-field cascade;
  5. SSE-driven cache invalidation: the UI never calls `invalidateQueries` from a mutation handler, and uses `staleTime: Infinity` unless a comment says why;
  6. presence is data: scans observe, users decide, and anything that touches album files skips Missing albums;
  7. every face goes through `musaic_app::ops`: routes only parse, call one operation and render, and every new route gets a row in `docs/operations.md`.

  The security posture holds too: loopback only, Host/Origin check, launch cookie, CSP, no webview IPC, navigation lock, and keys only in the OS keyring.
- `unsafe` is forbidden. Clippy `-D warnings` and `cargo fmt` must be clean.
- Never point a development run at `~/.musaic` or at a real music library. Use `--config-dir "$(mktemp -d)/config"` with `--dev-memory-secrets` (desktop) or `--verification-memory-secrets` (server), and `--network offline`. Live MusicBrainz, AcoustID or Cover Art Archive requests need the user's go-ahead each time.
- Goldens under `rust/testdata/golden/` change only as a deliberate behaviour change that says why, in the same change. Fixture databases are copied to a temp dir before opening.
- Public-text rule. Nothing about paid AI, pricing or a hosted relay goes in code, docs, commits, issues or PR bodies. The same applies to internal ids (`UNK-nnn`, obligation ids, private issue numbers), the word "oracle", Notion URLs, and private test data (recorded API answers, anything derived from a real library). A Notion prompt may be named by title in `spec.md`'s Input line; link it nowhere.
- Deferred work becomes a GitHub issue in `get2knowio/musaic`, never an inline TODO.
- Keep workspaces clean, because disk is tight. Remove worktrees and their branches as soon as their work is integrated (`git worktree remove -f -f`, never `git fetch --prune`). Delete temp library copies, venvs and screenshots. Stop any servers you started. Never leave Paul's own untracked files or pre-existing branches altered.

## Step 0 — Pick the spec prompt

If `$ARGUMENTS` names something, use it. This takes precedence over
auto-detection.

- **A GitHub issue** (`#N`, `N` with no `specs/N-*` directory, or a URL):
  `gh issue view N --comments` is the prompt.
- **A Notion prompt** (a title like "Prompt 4" or "player", or a URL): search
  the Musaic project (`notion-search` with `page_url` set to the Musaic page)
  and fetch the page body in full.
- **A number**: check `specs/NNN-*/` FIRST. If it exists, it means that spec
  directory. Otherwise it means a GitHub issue. If both readings are live,
  confirm at Step 0.5 rather than guessing.

Otherwise auto-detect, in this order:

1. **Unbuilt spec directories first.** A directory under `specs/` with
   `spec.md` but no `plan.md` is unbuilt work: resume it and skip Step 1.
   **"Built" means `plan.md` exists, NOT that the directory exists.** If more
   than one exists, take the lowest number.
2. **The Notion queue.** Fetch the Musaic project page, then each numbered
   planning page with `Prompt N · …` children. Read the planning page's
   delivery order and each prompt's stated dependencies. A prompt is
   **submitted** when some `specs/*/spec.md` names its title on the
   `**Input**` line (`grep -l "Prompt N ·" specs/*/spec.md`), and **built**
   when that directory also has `plan.md`. The next prompt is the first one
   in delivery order that is not submitted and whose dependencies are built.
   A prompt may say that part of it ships independently of a dependency (as
   Prompt 4's desktop player does of Prompt 1). Such a prompt is eligible,
   but flag the unmet dependency at Step 0.5.
3. **Repo fallback.** If the Notion MCP is unavailable or the queue is empty,
   report that and offer, at Step 0.5, the oldest open GitHub issue labelled
   `enhancement` that has no linked PR. Musaic keeps no prompt files on disk.
4. Read the chosen prompt fully, plus every GitHub issue it references
   (`gh issue view N --comments`). Notion pages can be edited after
   submission; the Notion body is authoritative until the spec exists, and
   the spec after that.

## Step 0.5 — Confirm the pick with the user (the ONLY routine interactive gate)

Use AskUserQuestion. Give the prompt's source (Notion page title, issue number,
or spec directory), its title, the target spec directory (`NNN-slug`, next free
number), a one-sentence summary, and any unmet dependency. Offer:

1. **Proceed with <prompt> (Recommended)**: continue unattended.
2. **Pick a different prompt**: the user names another one through "Other".
   Re-run Step 0 with it and confirm once more.

In a truly headless run, proceed and flag the unconfirmed pick prominently in
the report and the PR body.

## Step 1 — Specify

- Make sure the working tree is clean and on an up-to-date `main`
  (`git switch main && git pull --ff-only`), then create branch `NNN-slug`.
- Invoke `speckit-specify` with the prompt body (and the issue texts it
  references). The spec's `**Input**` line names the source by title only (a
  Notion prompt's title, or `#N`), never a Notion URL. Step 0's
  queue detection depends on that title.
- Make sure `.specify/feature.json` points at `specs/NNN-slug`.
- **Mark the queue (bookkeeping, never a gate).** For a Notion prompt, insert
  one line at the very top of its page: `Submitted 2026-MM-DD as specs/NNN-slug
  (branch NNN-slug).` Later, in Step 8, add the PR number to that line. A
  failed Notion write is noted in the report and never blocks the run.

## Step 1.5 — Reconcile a stale draft (only when the spec already existed)

This applies when Step 0 resolved to an existing `spec.md` with no `plan.md`.
A spec written in this same run skips it.

### Detect

Indented, not fenced, so this file can be republished inside a fenced block:

    DIR=specs/NNN-slug
    drafted=$(git log --diff-filter=A --format=%ad --date=short -1 -- "$DIR/spec.md")
    # an uncommitted draft has no date: treat it as drafted today
    [ -z "$drafted" ] && drafted=$(date +%F)
    for d in specs/[0-9]*/; do
      [ -f "$d/plan.md" ] || continue
      p=$(git log --diff-filter=A --format=%ad --date=short -1 -- "$d/plan.md")
      [ "$p" \> "$drafted" ] && basename "$d"
    done
    git log --since="$drafted" --oneline main | wc -l   # Musaic ships much outside specs

Musaic delivers most work as issue PRs, not specs. So also reconcile when
`main` has moved materially since the draft date: new routes or operations in
`docs/operations.md`, new events in `musaic-events/src/event.rs`, a change to
the command registry or menus, or new rules in CLAUDE.md. Do so unless the
spec already has a `### Session` block dated at or after those changes.

### Do

Verify every claim in the spec's Dependencies and Assumptions against the code
(delegate a read-only Explore subagent if it is large). In particular check:
every "does not exist yet" or "until X ships" sentence; every named seam
(`musaic_app::ops` groups, `docs/operations.md` rows, events, the command
registry in `frontend/src/lib/commands.ts` and `musaic-desktop/src/menu.rs`,
the CLAUDE.md rules); and whether part of it is already built. Record a dated
`### Session` reconciliation under `## Clarifications`, placed before
`## User Scenarios`. It holds three things: a "what shipped underneath" table
(change, file:line, effect); a ruling per drift, amending superseded clauses in
place and marking them, deleting nothing; and a "still verified good" list
with the date and the `main` SHA.

### Stop for the user

Editorial rulings are allowed unattended. **Architecture is not.** A drift
that raises a "how does this coexist with rule N / with the ops layer / with
the security posture" question gets marked **OPEN**, with its options and
their blast radii. Then stop and ask, even in an unattended run. In a headless
run, stop.

## Step 2 — Clarify (auto-accept)

Invoke `speckit-clarify` with: *"The user is not available interactively: for
every clarification question, choose and accept the recommended or
most-defensible option yourself and encode it into the spec."* Record each Q→A
in the spec's `## Clarifications` session block.

## Step 3 — Plan

Invoke `speckit-plan`. Before writing it, delegate a read-only Explore subagent
to verify every file and line anchor the prompt cites, and to map the real
seams. Notion prompts cite pinned commit SHAs that `main` has since moved past.
Produce `plan.md`, `research.md`, `data-model.md`, `contracts/` and
`quickstart.md`. The Constitution Check gates on
`.specify/memory/constitution.md` (principles I–VI), and CLAUDE.md's rules 1–7
are the detailed form of those principles. There are no
`SPECKIT START/END` markers in CLAUDE.md, so there is no plan pointer to
update. Plans name the slices that each leave the workspace green; Musaic
reviews in slices.

## Step 4 — Tasks

Invoke `speckit-tasks`. All checkboxes start `[ ]`. Include test tasks for
every FR that needs test-backed verification. Musaic's precedent is that every
new operation gets an ops test, every new route gets an inventory row, and
boundary tests (`ops_boundary`, `thin_handlers`, `operations_inventory`,
`ops_all_groups`) keep passing. Include doc tasks for the CLAUDE.md crate map
and rules, `docs/operations.md`, and `docs/desktop.md` when they change.

## Step 5 — Analyze (auto-remediate)

Invoke `speckit-analyze` with: *"The user is not available: after reporting
findings, apply every suggested remediation yourself."* If it runs as a
background fork, wait for its completion notification.

## Step 6 — Implement (sonnet subagent)

Spawn a **general-purpose subagent with `model: sonnet`** that invokes
`speckit-implement` and executes ALL tasks in `tasks.md`. Its prompt includes:

- the spec directory, the branch, and pointers to all design artifacts;
- the Step 0.0 profile, including the prerequisites, the test commands, and
  the invariants verbatim, plus the feature's own invariants from
  `plan.md`/`spec.md`;
- use parallel subagents for `[P]` tasks. Any that build Rust in a worktree
  set `CARGO_TARGET_DIR=/Users/paul/GitHub/musaic/rust/target` (one shared
  target; disk is tight) and remove their worktrees when done. Keep
  sequential dependencies and mark each checkbox `[X]` as it completes;
- actually file every `gh issue create` task (public-text rule applies) and
  record the numbers in `tasks.md`;
- run each checkpoint's targeted tests, then testAll, and make it green.
  Record pre-existing baseline failures rather than chasing them;
- run tests in the **foreground**. A backgrounded suite stalls the subagent;
- verify UI work in the real app when the spec asks for it (the `run` skill,
  scratch config dir, offline). Delete screenshots and temp dirs afterwards;
- do NOT commit or push; the parent session owns git;
- report per-task status, test counts, deviations, issues filed, and concerns.

Wait for its completion notification. If it pauses with work still running,
wait for the follow-up rather than duplicating its work.

## Step 7 — Review & fix (opus subagent)

Spawn a **general-purpose subagent with `model: opus`** to run
`/code-review xhigh --fix` on the working tree. If that skill is unavailable,
it does an equivalent adversarial review instead: real defects in the diff
against the spec and the invariants (rules 1–7 and the security posture
especially), each verified against the code, confirmed ones fixed, and the
affected tests re-run. It must not commit or push. Wait for it, and re-run
testAll if it changed anything.

## Step 8 — Commit, push, PR

1. `git status` and `git diff --stat`: only intended changes. Make sure no
   synthetic-library manifest, `rust/target` output, temp library or private
   data is staged.
2. Run **ciLocal** as pre-flight.
3. Commit on `NNN-slug` using commitFormat, with `Closes #N` for every issue
   the prompt says to close, and the trailer.
4. `git push -u origin NNN-slug`, then `gh pr create`. The body covers what
   and why, the design summary, test evidence (counts), follow-up issues
   filed, and what was not verified: live-desktop checks, platforms, audible
   output, anything needing hardware or live network. Apply the public-text
   rule, with no Notion links. End with the PR attribution line.
5. Watch remote CI in the background (`gh pr checks <PR> --watch` or a
   Monitor). **Do not merge.** Report the PR URL and continue to Step 9.

## Step 9 — Address issues filed during the run (opus subagents)

1. Collect the issues filed in Steps 6–7 (from `tasks.md` and the reports).
2. Triage each one. It is **addressable now** if it is pure code or test work
   in this repo. It is **blocked** if it needs a product decision, live
   network or credentials, hardware, or another prompt's groundwork (e.g.
   Prompt 1's MCP bridge). Leave blocked ones open and name the blocker.
3. **Where the fixes land.** While the spec PR is unmerged (the normal case),
   the fixes depend on its code, so they go onto the spec branch `NNN-slug`
   as extra commits in the same PR. The squash merge folds them in. Do not
   stack a PR on the spec branch: deleting a base branch closes its stacked
   PRs. If the spec PR is already merged, use `fix/issue-sweep-NNN` off the
   latest `main` and open one PR.
4. Spawn one **general-purpose subagent with `model: opus` and
   `isolation: worktree`** per addressable issue, in parallel, at most 3
   building Rust at once. Each gets `CARGO_TARGET_DIR` set to the shared
   target. Tell agents that share a seam about each other up front. Each
   agent reads the issue including its comments, implements with tests, and
   validates with **testTargeted only**. It commits clean commits carrying
   `Closes #N` and the trailer, then reports. It never pushes to the spec
   branch, never merges, never opens a PR, and never files issues; anything
   out of scope goes in its report.
5. **Integrate.** Cherry-pick each agent's commits onto the target branch in
   dependency order and resolve overlaps once. Remove the worktrees and their
   branches.
6. **Pay the test tax once.** Run ciLocal on the integrated branch, and push
   so remote CI runs at the same time. Add a "Follow-up fixes" section to the
   PR body with one `Closes #N` line and a short summary per issue. If one
   fix turns out bad, drop its commits; its issue stays open with the reason.
7. Report the run done only when all of these hold:
   - the PR is green (local ciLocal and remote CI);
   - the issues it closes are listed;
   - anything left open is listed with its blocker;
   - the Notion prompt line is updated with the PR number;
   - workspace cleanup is done: worktrees, branches, temp dirs, venv, and the
     synthetic library if this run generated it. Ask before clearing
     `rust/target` if a build is still useful.

   Merging waits for the user's word.

## Ground rules

- **Test economy.** Iterate with testTargeted. Run testAll only at the gates:
  end of implement, after a review that changed production code, and once
  per integration. Start long local runs and remote CI at the same time.
- **CLAUDE.md wins** over this file wherever they disagree; say so in the
  report.
- **Invariants are absolute** and go to every subagent.
- **Notion is bookkeeping, never a gate.** If a Notion read fails, use the
  fallback. If a write fails, report it and continue.
- Never mark a task done that wasn't done. Report failures verbatim.
- If a stage is genuinely blocked, stop and name the blocker.

## Known gotchas (Musaic)

- **`cargo test --workspace` fails without `frontend/dist`.** The desktop
  build script copies it, so build the UI first, including in each worktree.
- **The synthetic library is generated, never committed.** Regenerating it
  rewrites `library.manifest.json`'s timestamp; restore it with
  `git checkout`. Using other tool versions changes the digests, and that
  manifest must not be committed.
- **Deleting a merged base branch closes its stacked PRs.** If that happens,
  push the base ref back, reopen the PRs, run `gh pr edit --base main`, and
  only then delete the branch.
- **Subscribing to `/api/scan/stream` starts a scan.** Watching a scan without
  starting one uses `/api/scan/follow` (`Scan::follow`).
- **A hidden window runs no `requestAnimationFrame`.** The desktop boot test
  runs hidden; code it exercises must not wait on rAF.
- **Notion prompts cite pinned SHAs** (e.g. `c8e9097e`). Rebase every anchor
  onto current `main` in Step 3.
- **A number is ambiguous.** `specs/NNN-*` is checked first; otherwise it is a
  GitHub issue.
- **Subagents run tests in the foreground.**
