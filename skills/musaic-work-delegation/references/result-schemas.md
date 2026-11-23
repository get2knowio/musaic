# Result schemas

`musaic_work_complete` takes the work id, your claim generation and lease token, an idempotency key, the input digest from `musaic_work_get`, and a `result`. The exact JSON schema is in the tool reference (`references/tools.md`); this page explains the shapes.

## Common fields of `result`

| Field | Rule |
| --- | --- |
| `schemaVersion` | `1` |
| `workId`, `kind`, `inputDigest` | Copy them from the work item |
| `outcome` | `proposal`, `no_match` or `needs_input` |
| `evidence` | Up to 50 records: `source`, `retrievedAt` (RFC 3339), `claim`, `limitations` |
| `confidence` | A number from 0 to 1 |
| `uncertainties` | Up to 20 short sentences |
| `recommendation` | One of the three shapes below |

A `proposal` needs at least one evidence record.

## Recommendation shapes

**Metadata** (for `metadata.identify` and `metadata.correct`, outcome `proposal`): `type: "metadata"`, `identifiers` (type and UUID, up to 100), `albumFields` (a map of tag name to value), and `tracks` (a list of `trackId` and a map of fields). Use the track ids from the work context. Provide at least one album field or one track change.

**Cover** (for `cover.research`, outcome `proposal`): `type: "cover"` with `url`, `sourcePage`, `width`, `height`, `rationale`, `rightsUncertainty`.

**Unresolved** (outcome `no_match` or `needs_input`): `type: "unresolved"` with `explanation` and `question`. Say what you did and what the person must decide.

## Bounds

The whole result may not exceed 256 KiB. Values are plain text, bounded, with no control characters. Do not put file paths or secrets anywhere in it.

## Example (synthetic)

A `no_match` result for the album "Nobody - Unreleased Demo":

```json
{
  "schemaVersion": 1,
  "workId": "00000000-0000-4000-8000-000000000001",
  "kind": "metadata.identify",
  "inputDigest": "0000000000000000000000000000000000000000000000000000000000000000",
  "outcome": "no_match",
  "evidence": [],
  "confidence": 0,
  "uncertainties": [],
  "recommendation": {
    "type": "unresolved",
    "explanation": "Searched MusicBrainz by artist and title and by track count; no release matched.",
    "question": "Is there a catalogue number or another spelling of the artist?"
  }
}
```
