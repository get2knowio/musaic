# Tool reference

Generated from the tool registry; do not edit by hand. Your pairing may allow fewer tools than are listed: call `musaic_capabilities` for what you may do. A tool result never contains a filesystem path.

## `musaic_capabilities`

What this pairing may do: its scopes and policy, the contract version, the registry digest and the configured library roots. Start here.

Needs: `library.read`.

Input:

- (no fields)

## `musaic_library_list`

Page through the albums of the library, optionally filtered by a text query on the artist or title. Returns ids and display fields, never paths.

Needs: `library.read`.

Input:

- `cursor`: string (minLength 1) (maxLength 512), optional
- `limit`: integer (min 1) (max 100), optional
- `query`: string (maxLength 200), optional

## `musaic_album_get`

One album: its tags, its tracks (file names and tags) and its revision, which a fix proposal is bound to.

Needs: `library.read`.

Input:

- `albumId`: string, required

## `musaic_resolution_get`

The metadata resolution already stored for an album. It never starts a lookup; with nothing stored it answers not_resolved.

Needs: `library.read`.

Input:

- `albumId`: string, required

## `musaic_findings_list`

Open health-check findings, by reference (check, album, severity, title), optionally for one album.

Needs: `library.read`.

Input:

- `albumId`: string, optional
- `cursor`: string (minLength 1) (maxLength 512), optional
- `limit`: integer (min 1) (max 100), optional

## `musaic_fix_preview`

Validate a proposed change to one album's tags, cover or file names against its current revision and store it as a proposal. Writes nothing to the collection.

Needs: `proposal.submit`.

Input:

- `albumId`: string, required
- `baseRevision`: string, required
- `proposedPatch`: object, required
  - `cover`: object, optional
  - `rename`: boolean, optional
  - `tags`: object, optional

## `musaic_fix_apply`

Apply a previewed proposal. Depending on the pairing's policy the change runs now or waits for the user to approve it in Musaic; poll musaic_operation_get.

Needs: `tags.write`, `covers.write`, `files.rename`.

Input:

- `digest`: string, required
- `idempotencyKey`: string, required
- `proposalId`: string, required

## `musaic_scan_start`

Scan the configured library folder. Returns a handle; if a scan is already running the handle follows it.

Needs: `compute.scan`.

Input:

- `configuredRootId`: string, required
- `idempotencyKey`: string, required

## `musaic_checks_start`

Run health checks over some or all albums. Findings are written; no file is touched. Returns a handle.

Needs: `compute.scan`.

Input:

- `albumIds`: array of string (maxItems 500), optional
- `checkIds`: array of string (maxItems 20), optional
- `idempotencyKey`: string, required

## `musaic_analysis_start`

Measure BPM or ReplayGain for some albums and write the result to their tags. Returns a handle.

Needs: `compute.analyze`, `tags.write`.

Input:

- `albumIds`: array of string (minItems 1) (maxItems 500), required
- `idempotencyKey`: string, required
- `kinds`: array of bpm | replaygain (minItems 1) (maxItems 2), required

## `musaic_operation_get`

The state, counts and result of an operation this pairing started.

Needs: `library.read`.

Input:

- `operationId`: string, required

## `musaic_operation_cancel`

Ask an operation this pairing started to stop. Honoured between items, and for an apply only while it waits for approval.

Needs: `library.read`.

Input:

- `operationId`: string, required

## `musaic_work_list`

List available research requests and your claimed work.

Needs: `library.read`.

Input:

- `cursor`: string, optional
- `kind`: object, optional
- `limit`: integer (min 1) (max 100), optional
- `state`: object, optional

## `musaic_work_get`

Read the immutable album context and input digest for a research request.

Needs: `library.read`.

Input:

- `workId`: string, required

## `musaic_work_claim`

Reserve this request for five minutes using an idempotency key.

Needs: `library.read`, `proposal.submit`.

Input:

- `idempotencyKey`: string, required
- `workId`: string, required

## `musaic_work_renew`

Extend the expiry of your current claim. Renew once a minute while you work.

Needs: `library.read`, `proposal.submit`.

Input:

- `claimGeneration`: integer (min 1), required
- `leaseToken`: string, required
- `workId`: string, required

## `musaic_work_progress`

Update the current attempt using a monotonically increasing sequence.

Needs: `library.read`, `proposal.submit`.

Input:

- `claimGeneration`: integer (min 1), required
- `completed`: integer (min 0), optional
- `leaseToken`: string, required
- `message`: string (maxLength 500), optional
- `sequence`: integer (min 1), required
- `stage`: string (minLength 1) (maxLength 80), required
- `total`: integer (min 0), optional
- `workId`: string, required

## `musaic_work_complete`

Submit bounded evidence and a recommendation for desktop review.

Needs: `library.read`, `proposal.submit`.

Input:

- `claimGeneration`: integer (min 1), required
- `idempotencyKey`: string, required
- `inputDigest`: string, required
- `leaseToken`: string, required
- `result`: object, required
  - `confidence`: number (min 0) (max 1), required
  - `evidence`: array of object (maxItems 50), required
    - `claim`: string, required
    - `limitations`: array of string, required
    - `retrievedAt`: string, required
    - `source`: string, required
  - `inputDigest`: string, required
  - `kind`: metadata.identify | metadata.correct | cover.research, required
  - `outcome`: proposal | no_match | needs_input, required
  - `recommendation`: object, required
  - `schemaVersion`: integer (min 1) (max 1), required
  - `uncertainties`: array of string (maxItems 20), required
  - `workId`: string, required
- `workId`: string, required

## `musaic_work_fail`

End this attempt and allow Musaic to decide whether to retry.

Needs: `library.read`, `proposal.submit`.

Input:

- `claimGeneration`: integer (min 1), required
- `code`: string, required
- `leaseToken`: string, required
- `message`: string, required
- `retryable`: boolean, required
- `workId`: string, required

## `musaic_work_release`

Give your current claim back without submitting a result.

Needs: `library.read`, `proposal.submit`.

Input:

- `claimGeneration`: integer (min 1), required
- `leaseToken`: string, required
- `reason`: string, required
- `workId`: string, required

## `musaic_work_cancel`

Cancel work by revision when explicitly allowed by the pairing policy.

Needs: `library.read`, `work.cancel`.

Input:

- `expectedRevision`: integer (min 1), required
- `workId`: string, required

