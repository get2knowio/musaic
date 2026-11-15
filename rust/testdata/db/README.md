# Database fixtures

`v16-synthetic-offline.db` is the SQLite database the Python app wrote during
the offline API capture of the synthetic 13-album library (the run whose
files are in `captures/api-offline/`),
copied with SQLite's backup API into a single file. Schema v16. It holds 13
albums, 42 fingerprints, the findings from that run and one settings row (the
dummy AcoustID key the offline capture uses). Album paths are absolute paths
on the machine that recorded it, with that machine's root rewritten to the
neutral `/var/tmp/musaic-testdata/synthetic-roots` (same length, so the file
is otherwise byte-identical); they are data, not locations to open. Album ids
still hash the recorded paths, so a test that checks path-to-id pairs needs
the unmodified file (private test data).

Rust tests copy it to a temp dir before opening it. Never open it read-write
in place.
