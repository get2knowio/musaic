# Contract: Agent Tools (registry v1)

The source of truth is `rust/crates/musaic-commands`. The generated rendering lives in
`docs/agent-tools.json` and `docs/agent-tools.md`, and `musaic-cli catalog --check` fails
when either drifts from the code. Every input object has `additionalProperties: false`.
Every output is an object. The CLI name is the tool id without the `musaic_` prefix, with
`_` replaced by `-`.

Shared types:
- `AlbumId` is a string of 16 lowercase hex characters.
- `Cursor` is an opaque string, at most 512 characters.
- `IdempotencyKey` is a string of 8–128 characters from `[A-Za-z0-9._:-]`.
- `OperationHandle` has the shape `{operationId, state, approvalId?}`.

| Id | Input | Output | Kinds | Annotations (readOnly / destructive / idempotent / openWorld) |
|---|---|---|---|---|
| musaic_capabilities | `{}` | `{appVersion, contractVersion: "1", registryDigest, instanceId, scopes[], policy{kind→decision}, profiles[]}` | library.read | T / F / T / F |
| musaic_library_list | `{cursor?, limit? 1..100 = 50, query? ≤200}` | `{items: AlbumSummary[], nextCursor?, total}` | library.read | T / F / T / F |
| musaic_album_get | `{albumId}` | `{album: AlbumDetail, tracks: Track[], revision}` | library.read | T / F / T / F |
| musaic_resolution_get | `{albumId}` | `{status: "resolved"\|"not_resolved", anchor?, strategy?, fields{name→{value,source,confidence}}, coverCandidate: bool, tracks[{fileName, recordingMbid?}], notes[]}` | library.read | T / F / T / F |
| musaic_findings_list | `{albumId?, cursor?, limit? 1..100 = 50}` | `{items: Finding[], nextCursor?}` | library.read | T / F / T / F |
| musaic_fix_preview | `{albumId, baseRevision, proposedPatch}` | `{proposalId, digest, diff, effects[], kinds[], expiresAt}` | proposal.submit | F / F / F / F |
| musaic_fix_apply | `{proposalId, digest, idempotencyKey}` | `OperationHandle` plus `result?` once it is finished | the proposal's kinds | F / T / T / T (a URL cover is fetched) |
| musaic_scan_start | `{configuredRootId, idempotencyKey}` | `OperationHandle` | compute.scan | F / F / T / F |
| musaic_checks_start | `{albumIds? ≤500, checkIds? ≤20, idempotencyKey}` | `OperationHandle` | compute.scan | F / F / T / T (network checks) |
| musaic_analysis_start | `{albumIds 1..500, kinds ["bpm"\|"replaygain"] 1..2, idempotencyKey}` | `OperationHandle` | compute.analyze + tags.write | F / T / T / F |
| musaic_operation_get | `{operationId}` | `{operationId, tool, state, counts{done,total,failed}, approvalId?, result?, error?}` | library.read | T / F / T / F |
| musaic_operation_cancel | `{operationId}` | `{operationId, state, cancellation: "requested"\|"not_cancellable"\|"already_finished"}` | library.read | F / F / T / F |

`AlbumSummary` has the shape `{albumId, artist, title, year?, trackCount, missing,
hasCover}`. `artist` comes from `display_artist(…, true)`.

`AlbumDetail` has the fields of `AlbumSummary` plus `folderName, tags{FIELD→value},
discCount`. `Track` has the shape `{fileName, trackNumber?, discNumber?, title?, artist?,
durationSeconds?, tags{FIELD→value}}`. No output contains an absolute or relative
filesystem path other than a bare file or folder name.

`Finding` has the shape `{findingId, checkId, albumId?, severity, title, status, firstSeen,
lastSeen}`.

`proposedPatch` has this shape (it is validated in this order):
```
{ "tags"?:   { "album"?:  {FIELD: string|null},
               "tracks"?: {fileName: {FIELD: string|null}} },
  "cover"?:  {"source":"resolution"} | {"source":"url","url":"https://…"},
  "rename"?: true }
```
- The patch must not be empty.
- `FIELD` must be in the editable vocabulary, and a value is at most 1,000 characters.
- `fileName` must be a file `album_get` listed.
- A `url` must be `https` with a public host.

Patch parts map to kinds: `tags` → `tags.write`, `cover` → `covers.write`, and `rename` →
`files.rename`.

`effects[]` contains the values `writes_tags`, `writes_cover`, `renames_files`,
`renames_folder` and `fetches_network`.

## Tool error codes (`isError: true`, `structuredContent: {code, message, details?}`)

`INVALID_INPUT`, `NOT_FOUND`, `MISSING_ALBUM`, `STALE_REVISION`, `PROPOSAL_EXPIRED`,
`DIGEST_MISMATCH`, `FORBIDDEN` (the scope or a Refuse policy denies it), `DENIED` (the user
denied it), `APPROVAL_EXPIRED`, `IDEMPOTENCY_CONFLICT`, `CURSOR_STALE`, `BUSY`,
`RESTORING`, `NO_LIBRARY`, `UNAVAILABLE`, `INTERNAL`.

These are transport errors, reported by the CLI and not by a tool: `APP_NOT_RUNNING`,
`DESCRIPTOR_INVALID`, `NOT_PAIRED`, `PAIRING_REVOKED`, `INSTANCE_MISMATCH`, `LOCKED_OUT`,
`PROFILE_UNSUPPORTED`.

## Deliberately not offered (stated in the catalog)

The catalog does not offer: raw SQL, shell execution, arbitrary filesystem paths, key or
secret read and write, trash or delete, database restore or backup download, settings
changes, and setting or changing agent permissions and policies, including approving its
own requests.
