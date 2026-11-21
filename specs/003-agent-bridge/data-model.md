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
| state | TEXT | `recorded`, then `applied`, `partial` or `failed` once apply finishes |
| before | TEXT | JSON: `{files: {fileName: {"album.FIELD" \| "track.FIELD": value \| null \| [values]}}, renames: {files: [{from, to}], folder?: {from, to}}, cover?: {savedAs: "agent-changes/<id>/cover.<ext>", source: "embedded" \| "sidecar" \| "none"}}`. Names are bare names, not paths |
| outcome | TEXT NULL | per-file outcomes, as reported to the agent |

Rows are kept for 90 days, then pruned at startup together with their
`<config dir>/agent-changes/<id>/` directory. The prior cover is copied byte for byte and
is not re-encoded. A failure to write the row or the cover copy refuses the apply before
any write.

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
