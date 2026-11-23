# Checks

## Baseline (untouched branch, main at d8f6019e)

`cargo test -p musaic-commands -p musaic-mcp`: musaic-commands 24 unit + 7 (descriptor) + 10 (schemas) passed; musaic-mcp 1 unit + 7 (profiles) passed. 0 failures.

## Final chain (T050)

- `cargo fmt --all --check`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace --no-fail-fast`: 1860 passed, 0 failed, 10 ignored (the ignored ones are pre-existing, e.g. the desktop boot test).
- `cargo run -p musaic-cli -- catalog --check --dir ../docs`: clean (tool catalog, event catalog, skill references, skill lock).
- Frontend: `npm run lint` 0 errors (40 warnings, all in files this work did not add), `typecheck` and `typecheck:test` clean, `npm test` 511 passed in 54 files, `npm run build` ok.
- `cargo test -p musaic-desktop --test boot -- --ignored --nocapture`: 2 passed.

## Quickstart walk (T051), offline, scratch config dir, `musaic-api` dev server + `musaic-cli`

Steps 1 to 5 and 7 were walked without the desktop window (the window cannot be driven here): pairing shows only "paired" in `/api/agent/diagnostics`; `doctor` reports the three offered extensions, the skill status and the (empty) host report; `setup --skill --dry-run` against a temporary home prints the plan and "Installed. This does not show that the host has loaded the skill." and writes nothing; a modern `musaic-cli mcp` session with `skills/list` and `events/poll` makes the pairing show Bridge reachable and the host as reported; a research request appears at `/api/research/work` as queued with `waitingForAgent: true`; revoking the pairing makes the next CLI call fail. Step 6 (a real agent working the item) and the Settings and Review screens in the desktop window were not walked. Temporary directories were removed.
