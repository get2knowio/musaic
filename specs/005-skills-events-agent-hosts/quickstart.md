# Quickstart: verifying spec 005

Prerequisites: `(cd frontend && npm ci && npm run build)`; the synthetic library for audio tests (see CLAUDE.md). Never use `~/.musaic` or a real library.

## Checks

```bash
cd rust
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p musaic-cli -- catalog --check      # tools, events and skill assets
cd ../frontend && npm run lint && npm run typecheck && npm run typecheck:test && npm test && npm run build
```

Targeted:

```bash
cargo test -p musaic-commands skill
cargo test -p musaic-mcp --test conformance
cargo test -p musaic-app --test ops_agent_events
cargo test -p musaic-launch
```

## By hand, on a scratch config

```bash
cd rust
CFG="$(mktemp -d)/config"
cargo run -p musaic-desktop -- --config-dir "$CFG" --dev-memory-secrets --network offline
```

1. Settings → Agents: create a pairing. It shows Paired and nothing else.
2. `cargo run -p musaic-cli -- --config-dir "$CFG" doctor` reports the pairing, the offered extensions and the skill status.
3. `cargo run -p musaic-cli -- --config-dir "$CFG" setup --skill --host claude-code --dry-run` prints the install plan and the "does not show the host loaded it" line.
4. Connect an MCP host through `musaic-cli mcp`; the pairing shows Bridge reachable, then the host name and profile.
5. Request research on a synthetic album; Review → Delegated work shows it queued and "Waiting for an agent".
6. In the host, "Process pending Musaic work": the row moves to claimed, investigating, then awaiting review; approve or discard in the desktop.
7. Revoke the pairing; the host's next call is refused.

## Evidence to record

`specs/005-skills-events-agent-hosts/evidence/compatibility.md`: one row per host/version/OS with SDK version, profile, skill activation, Tasks polling/input/cancel, Events mode and the observed unattended wake result; harness results in a separate table. Every cell is a result, "unsupported" or "untested".
