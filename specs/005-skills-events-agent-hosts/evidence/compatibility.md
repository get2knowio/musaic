# Compatibility report

Targets: MCP `2026-07-28` (Skills and Events extensions, Beta) and `2025-11-25`; rmcp `=3.11.0`.
Harness results are not results from a real host and are never listed in the second table.

## Harness (in-memory pipes, fake backend; `musaic-mcp/tests/conformance.rs`, `musaic-api/tests/agent_events_bridge.rs`)

| Check | Modern client (2026-07-28) | Legacy client (2025-11-25) |
| --- | --- | --- |
| `skills/list` returns one skill with digest and size for every file | pass (every entry read and verified) | `-32601` (as designed) |
| `skills/get` by `SKILL.md` URI, with and without a prior list | pass | `-32601` |
| Frontmatter in the listing equals the frontmatter of the bytes read | pass | not applicable |
| `resources/list`, `resources/read` serve the same bytes | pass | pass, same bytes |
| Prompt `musaic_research_album` and `album_id` validation | pass | pass |
| Refused URIs (traversal, encoding, backslash, query, fragment, trailing slash, other schemes, unlisted, lock file) | pass, one message, never echoed | not applicable |
| `resources/directory/read` | `-32601` (not declared) | `-32601` |
| `events/list` equals the checked-in catalog | pass | `-32601` |
| `events/poll`: bootstrap cursor, new work, unusable cursor is `truncated`, bad parameters | pass | `-32601` |
| `events/poll` without declaring the Events extension | `-32601` | not applicable |
| `events/subscribe`, `events/unsubscribe` | `-32026` | `-32601` |
| `events/stream`: `active` first, events, subscription id in `_meta`, re-`active` on truncation | pass | `-32601` |
| `events/stream`: heartbeat at the granted interval (5 s), not faster | pass | `-32601` |
| `events/stream`: cancel stops delivery and leaves the outbox untouched | pass | `-32601` |
| `events/stream`: revoked pairing sends `terminated`, then the final result | pass | `-32601` |
| Nine concurrent streams: the ninth is `-32025`, a freed slot is reusable | pass | not applicable |
| Two agents hear the same event and race a claim: one wins | pass | not applicable |
| Real bridge: pair, poll, create work, restart the server on the same database, same event ids | pass | not applicable |

## Real hosts

| Host | Version | OS | SDK version | Profile | Skill activation | Tasks poll / input / cancel | Events mode | Unattended wake |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Claude Code | untested | untested | untested | untested | untested | untested | untested | untested |
| Claude Desktop | untested | untested | untested | untested | untested | untested | untested | untested |
| Any other host | untested | untested | untested | untested | untested | untested | untested | untested |

Nothing was run against a real agent host for this release. Spec 003 recorded Claude Code
2.1.330 on macOS connecting over both profiles for tools (`specs/003-agent-bridge/evidence/host-connection.md`);
that run did not exercise Skills or Events. Whether a host loads the skill, declares the Events
extension, or wakes by itself is therefore unobserved.

## Known gaps

- The `ext-events` and `ext-skills` repositories were not read; shapes follow the published
  overview pages. The Events error codes are provisional upstream.
- The CLI's `events/stream` abandons a parked poll on cancel rather than aborting it; the poll
  ends on its own wait (at most 25 s). The CLI shuts its runtime down with a one-second timeout.
