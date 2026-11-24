# Quickstart: checking the Claude Desktop connection

Never run against `~/.musaic`, a real music library or a real Claude configuration. Automated tests inject a scratch home and a scratch temporary root.

## Automated

```bash
(cd frontend && npm ci && npm run build)
cd rust
cargo test -p musaic-commands claude_desktop
cargo test -p musaic-cli --test setup --test no_db_owner
cargo test -p musaic-app --test ops_agent_claude --test ops_boundary --test ops_all_groups
cargo test -p musaic-api --test claude_desktop_http --test thin_handlers --test operations_inventory --test inventory
cargo test -p musaic-desktop
cargo run -p musaic-cli -- catalog --check
(cd ../frontend && npx vitest run src/components/ConnectClaudeDesktop.test.tsx src/pages/settingsAgents.test.tsx)
```

## By hand, without Claude Desktop

```bash
cd rust
cargo run -p musaic-api -- --config-dir "$(mktemp -d)/config" --verification-memory-secrets --network offline
curl -s http://127.0.0.1:8765/api/agent/claude-desktop
out="$(mktemp -d)/skill.zip"; curl -s -o "$out" http://127.0.0.1:8765/api/agent/skill-archive && unzip -l "$out"
```

The status reflects the machine it runs on; it only reads.

## With a real Claude Desktop (the owner, on their own machine)

1. Open the packaged app, Settings, Assistants, Connect Claude Desktop. Check the command shown.
2. Confirm. Record what Claude Desktop shows (including any publisher wording) and click Install.
3. Add an assistant in Musaic. In Claude, ask for Musaic's tools and a library summary.
4. Record each row of `evidence/compatibility.md` with the Claude Desktop version and the operating system.
