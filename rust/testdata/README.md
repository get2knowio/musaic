# Test data (public)

Everything here is self-authored and synthetic: inputs the tests build their
scenarios from and the results the Python app (the reference implementation)
produced for them. Tests find it through the dev-only crate `musaic-testdata`
(`musaic_testdata::public("golden/...")`).

| Path | What |
|---|---|
| `golden/` | Expected results recorded from the Python app, one JSON per area (`api/`, `checks/`, `events/`, `persistence/`, …). |
| `db/` | `v16-empty.db` (schema only) and `v16-synthetic-offline.db` (a database the Python app wrote for the synthetic library); tests copy them to a temp dir before opening. Absolute paths stored in them use the neutral root `/var/tmp/musaic-testdata/synthetic-roots`: data, not places to open. |
| `captures/` | The few files of recorded API / DSP runs over the synthetic library that tests replay. |
| `synthetic-library/` | The synthetic audio library: its spec (`albums.py`), its generator and the committed manifest with every file's SHA-256. The audio itself (`library/`) is generated, never committed. |

## Generating the synthetic library

From `rust/`, with Python 3.10+ and `numpy`, `soundfile` (libsndfile with
FLAC), `mutagen` and `Pillow`:

```bash
python3 testdata/synthetic-library/gen_fixture_library.py
```

It writes `testdata/synthetic-library/library/` byte for byte as listed in
`library.manifest.json` (same library versions). Tests that need the audio
fail with this command in the message when it is missing.

## Two tiers

- **Tier 1** reads only this directory (plus the generated library) and runs
  from any clone.
- **Tier 2** also needs private test data (recorded MusicBrainz / AcoustID /
  Cover Art Archive answers, data derived from a real music library). It is
  looked up in `$MUSAIC_PRIVATE_TESTDATA`, or, when that variable is unset, in
  `../private-testdata` next to this workspace. When it is not available, a
  Tier 2 test prints `skipped: private test data not available (<test>)` and
  passes; a test that has both public and private cases runs the public ones
  and prints `… (<test>: private cases only)`. Setting
  `MUSAIC_PRIVATE_TESTDATA` to an empty value (or to an empty directory)
  turns the fallback off.

Some files exist as two halves with the same relative path: the public half
here, the private half in the private data. The private half lists, under
`"_split"`, where its array elements belong; `musaic_testdata::json` merges
the halves when both are present.
