# Contributing

Thanks for helping. Musaic is a Rust workspace (`rust/`) with a React UI (`frontend/`),
packaged with Tauri 2. [CLAUDE.md](CLAUDE.md) describes the crates, the architectural
rules and where each lives; read it before a larger change.

## Setup

- Rust via [rustup](https://rustup.rs/). The toolchain is pinned in
  `rust/rust-toolchain.toml` and installed automatically the first time you run `cargo`
  in `rust/`.
- Node.js 22.12 or newer.
- Linux: the WebKitGTK development packages (see the README's "Build from source").
- Windows: the MSVC build tools and the WebView2 runtime.
- For tests that use audio: Python 3.10+ with `numpy`, `soundfile`, `mutagen` and
  `Pillow`, to generate the synthetic test library.

```bash
(cd frontend && npm ci && npm run build)    # the desktop crate's build needs frontend/dist

# the synthetic test library (audio is generated, never committed); a virtualenv keeps
# the pinned versions, which make the output byte-identical to the committed manifest
python3 -m venv .venv && . .venv/bin/activate
pip install numpy==2.5.4 soundfile==0.14.0 mutagen==1.48.1 Pillow==13.0.0
(cd rust && python3 testdata/synthetic-library/gen_fixture_library.py)
git checkout -- rust/testdata/synthetic-library/library.manifest.json   # the generator stamps a new timestamp
```

## Development

Run the desktop app on a throwaway config dir, with keys kept in memory:

```bash
cd rust
cargo run -p musaic-desktop -- --config-dir "$(mktemp -d)/config" --dev-memory-secrets --network offline
```

For UI work with hot reload, run the API server and Vite side by side:

```bash
# terminal 1 — API on 127.0.0.1:8765
cd rust && cargo run -p musaic-api -- --config-dir "$(mktemp -d)/config" --verification-memory-secrets --network offline

# terminal 2 — Vite on :5173, proxies /api to :8765
cd frontend && npm run dev
```

Open <http://localhost:5173> and choose a library folder in Settings. Use a copy of some
FLACs (or the generated synthetic library), never your real collection: Musaic writes
tags and renames files. Drop `--network offline` when you want live MusicBrainz, AcoustID
and Cover Art Archive lookups.

## Before you open a pull request

```bash
cd rust
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

cd ../frontend
npm run lint
npm run typecheck
npm run typecheck:test
npm test
npm run build
```

CI runs the same commands (`.github/workflows/ci.yml`, with the Rust steps in
`rust-tests.yml`) without the private test data. The Linux and macOS Rust jobs and the
frontend job are required; the `ci passed` job is green only when all of them are. The
Windows job runs but is not required yet, and no Windows installer is published until it
is (#178) — fixes that make it pass are welcome. A release runs the same Rust tests on
every platform it builds and builds nothing if any of them fails.

## Tests and test data

- Public test data is in `rust/testdata/`: synthetic inputs, databases and the expected
  results recorded for them. Add new test data there only if you wrote it yourself or it
  is synthetic.
- Some tests need private data (recorded third-party API answers, material derived from
  a real music library) that is not in this repository. They print
  `skipped: private test data not available (<test>)` and pass. Do not add such data
  here.
- Never commit audio or cover art. The synthetic library is generated; its manifest is
  committed.
- Tests copy fixture databases to a temp dir before opening them, and never touch
  `~/.musaic` or the OS keyring. Tests that do touch the real keyring are `#[ignore]`d.

## Changing behaviour

- Mutations publish an event on the bus; they do not call check reruns, the resolver
  cache or thumbnails directly (CLAUDE.md, rule 1).
- A Resolve dialog re-derives its plan from a preview endpoint (rule 2).
- Compare files with a same-file check, not by path string (rule 3).
- Use the shared album-field accessors (rule 4).
- Do not add `invalidateQueries` calls to UI mutation handlers; publish the event and let
  `useLibraryEventsStream` handle it (rule 5).
- Skip Missing albums in anything that reads or writes album files (rule 6).
- A deliberate change to recorded behaviour updates the golden file in the same change
  and says why in the commit message.

## Adding a health check

See "Adding a health check" in [CLAUDE.md](CLAUDE.md).

## Packaging

```bash
cargo install tauri-cli --version 2.12.4 --locked
cargo install cargo-about --version 0.9.3 --locked --features cli
scripts/package_macos.sh       # unsigned Musaic.app + .dmg under rust/target/release/bundle/
```

Linux bundles are built by `.github/workflows/release.yml`; its steps are the reference
for building them by hand (Windows too, though no Windows installer is published yet).

## Reporting bugs and security issues

Bugs: open an issue with your OS, the Musaic version (or commit) and the steps. For
security problems, see [SECURITY.md](SECURITY.md) — please do not open a public issue.
