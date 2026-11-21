# Data Model: Agent Bridge

## Persisted (SQLite schema v21)

The migration is `v20_to_v21` in `rust/crates/musaic-db/src/migrate.rs`. The existing
startup snapshot covers it. All timestamps are ISO-8601 UTC text, matching the other
tables.

### agent_pairings
| Column | Type | Rules |
|---|---|---|
| id | TEXT PK | `p_` + 16 hex random |
| name | TEXT NOT NULL | 1–64 chars, trimmed |
| scopes | TEXT NOT NULL | JSON array of permission kinds, non-empty, subset of the 7 |
| secret_fingerprint | TEXT NOT NULL | SHA-256 hex of the secret (not a credential) |
| created_at | TEXT NOT NULL | |
| last_used_at | TEXT | set on successful exchange |
| revoked_at | TEXT | set once; a revoked row is never reactivated |

Invariant: at most 20 rows with `revoked_at IS NULL`.

### agent_policies
| Column | Type | Rules |
|---|---|---|
| pairing_id | TEXT FK → agent_pairings.id | |
| kind | TEXT | one of the 7 kinds |
| decision | TEXT | `allow` \| `ask` \| `refuse` |
PK (pairing_id, kind). Missing row = the default (FR-032).

### agent_proposals
| Column | Type | Rules |
|---|---|---|
| id | TEXT PK | `pr_` + 16 hex |
| pairing_id | TEXT FK | |
| album_id | TEXT | |
| base_revision | TEXT | |
| patch | TEXT | canonical JSON |
| digest | TEXT | SHA-256 hex |
| kinds | TEXT | JSON array derived from patch parts |
| diff | TEXT | JSON, used for labels only |
| created_at, expires_at | TEXT | expiry = created + 30 min |

Immutable after insert. Rows older than 7 days are pruned at startup.

### agent_approvals
| Column | Type | Rules |
|---|---|---|
| id | TEXT PK | `ap_` + 16 hex |
| pairing_id | TEXT FK | |
| action | TEXT | `fix_apply` \| `scan_start` \| `checks_start` \| `analysis_start` |
| proposal_id | TEXT NULL | for `fix_apply` |
| request | TEXT | canonical JSON of the start request (non-apply) |
| request_digest | TEXT | proposal digest or SHA-256 of request |
| album_revision | TEXT NULL | |
| kinds | TEXT | JSON array |
| state | TEXT | `pending` → `approved` \| `denied` \| `expired` |
| reason | TEXT NULL | e.g. "app restarted", "timed out" |
| created_at, expires_at, decided_at | TEXT | expiry = created + 30 min |
| operation_id | TEXT NULL | run-scoped handle that waits on it |

Every transition goes from `pending` to one terminal state, exactly once (`UPDATE … WHERE
state='pending'`). At startup every `pending` row becomes `expired` with reason "app
restarted".

### agent_changes (before-image, FR-054)
| Column | Type | Rules |
|---|---|---|
| id | TEXT PK | `ch_` + 16 hex |
| pairing_id | TEXT | |
| proposal_id | TEXT | |
| approval_id | TEXT NULL | NULL when the policy was Allow |
| album_id | TEXT | album id before any rename |
| created_at | TEXT | written before the first file changes |
| state | TEXT | `recorded`, then `applied`, `partial` or `failed` once apply finishes; `undone` once the desktop user undid it (FR-055) |
| before | TEXT | JSON: `{files: {fileName: {"album.FIELD" \| "track.FIELD": value \| null \| [values]}}, aliases?: {fileName: {VORBISKEY: value \| null \| [values]}}, renames: {files: [{from, to}], folder?: {from, to}}, cover?: {sidecars: [{name, savedAs: "agent-changes/<id>/sidecars/<name>"}], embedded: {fileName: [{savedAs: "agent-changes/<id>/pictures/<hash>.<ext>", type, mime, description, width, height, depth, colors}]}}}`. Names are bare names, not paths. `aliases` holds the other spellings a removal deletes with its field (`album_artist`, `year`), only for fields the change removes |
| outcome | TEXT NULL | per-file outcomes, as reported to the agent (for an undo: `{albumId, error}`) |
| after | TEXT NULL | written once an `applied` or `partial` change finished (also `after.json` beside `before.json`): `{albumId, folder, files: {nameBefore: {name, fields?: {key: value}, aliases?: {key: value}, pictures?: [digest], unread?: true}}, sidecars?: {name: digest}}`: the album's id and folder name now, and for every file the record names, the name it has now, the same keys' values, and digests of its PICTURE blocks and of the cover files when the change set a cover. NULL: cannot be undone |
| undoes | TEXT NULL | on an undo's own record: the change it undid |

An undo (FR-055) compares the files with `after` (file by file, field by field; files by
same-file identity) and refuses, writing nothing, when anything differs. It then records
its own before-image as a new row (same pairing and proposal, `undoes` set; never
undoable itself), renames back, restores tags, pictures and cover files, finishes its row
(`applied`, `partial` or `failed`) and marks the original `undone` unless it wrote
nothing. The `after` and `undoes` columns and the `undone` state were added to the v21
definition in place: v21 had not been released.

Rows are kept for 90 days, then pruned at startup together with their
`<config dir>/agent-changes/<id>/` directory. A cover apply replaces both kinds of cover
art: it drops every PICTURE block of every FLAC file and embeds one, writes `cover.jpg`
and deletes `cover.jpeg` / `cover.png` (in any case). So the before-image of a change
that sets a cover keeps all of it, byte for byte and not re-encoded:

- `sidecars`: every `cover.jpg`, `cover.jpeg` or `cover.png` the folder lists (names
  matched without regard to case, kept as listed), each copied under `sidecars/`. An
  empty list means the folder had none, so a restore removes the `cover.jpg` the apply
  wrote.
- `embedded`: every FLAC file by bare name, with its PICTURE blocks in file order (an
  empty list is a file that had none). Each entry keeps the block's fields and points at
  its image data under `pictures/`, named by the first 16 hex characters of the data's
  SHA-256. Files whose pictures are identical (the usual album, one front cover in every
  file) share one copy; files that differ each keep their own. Saving per file rather
  than one album picture is what lets a restore rebuild every block exactly, including a
  back cover or a picture only some files carry.

A failure to write the row or any of these copies refuses the apply before any write.

### agent_activity
| Column | Type | Rules |
|---|---|---|
| id | INTEGER PK | |
| pairing_id | TEXT | |
| at | TEXT | |
| tool | TEXT | tool id or `exchange` |
| outcome | TEXT | `ok` \| `refused:<code>` \| `failed:<code>` \| `awaiting_approval` |
Holds no argument values. Each pairing keeps its last 100 rows, older ones pruned on insert.

## Keyring (not in the DB)
Service `musaic-agent`, account `<configDirId>:<pairingId>`, value: hex of the 32-byte
secret. Written at pairing creation and deleted at revoke.

## In memory (AgentHub, one app run)
- **Instance**: `instanceId` (`i_` + 16 hex), created at server start.
- **Nonce**: `serverNonce → expiresAt` (60 s, single use).
- **Token**: `token → {pairingId, instanceId, scopes, expiresAt}` (15 min).
- **Lockout**: `{failures: VecDeque<Instant>, lockedUntil, lockoutCount}`.
- **Operation**: `{id "op_…", pairingId, tool, state, counts {done, total, failed}, result |
  error, approvalId?, cancel: AtomicBool, createdAt}`. States: `awaiting_approval →
  running → succeeded | failed | cancelled`; `awaiting_approval → denied | expired |
  cancelled`.
- **Idempotency**: `(pairingId, key) → {inputDigest, operationId}`.

## Principal
`{pairingId, pairingName, scopes, instanceId}`. Resolved from a token by
`ops.agent().authenticate(token)` and passed into every tool operation. A decision is
computed as `decide(principal, kinds) = strictest over kinds of (if kind ∉ scopes then
Refuse else policy(kind))`.

## Descriptor (file, not DB)
`<config dir>/agent-instance.json`; see [contracts/descriptor.md](contracts/descriptor.md).

## Permission kinds → tools
| Tool | Kinds |
|---|---|
| capabilities, library_list, album_get, resolution_get, findings_list, operation_get | library.read |
| operation_cancel | library.read (own operations only) |
| fix_preview | proposal.submit |
| fix_apply | union of the proposal's kinds: tags.write, covers.write, files.rename |
| scan_start | compute.scan |
| checks_start | compute.scan |
| analysis_start | compute.analyze + tags.write |

`checks_start` uses `compute.scan`: it is computation that writes only findings. There
is no separate kind for it, because the prompt lists seven kinds.
