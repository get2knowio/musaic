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

`v20-jobs.db`, `v22-jobs.db` and `v23-jobs.db` are synthetic databases as the
builds that wrote schemas v20, v22 and v23 left them: nine albums under a
made-up library path and ten old research jobs in every status (queued,
running, completed, failed, applied, discarded, one unsupported status) and
every type (`metadata`, `cover`, `metadata+cover`, `lyrics`), one whose album
row is gone and one whose stored result is not JSON, plus three retired
provider settings rows. They were made from `v16-empty.db` by the ignored
generator `crates/musaic-db/tests/make_job_fixtures.rs`
(`cargo test -p musaic-db --test make_job_fixtures -- --ignored`); for v22
and v23 the jobs went in before the v21 -> v22 step, so that step linked
them. They are frozen on purpose: tests copy them to a temp dir before
opening them.
