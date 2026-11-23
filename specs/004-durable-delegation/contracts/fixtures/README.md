# Recorded stdio responses

These responses were captured on macOS on 2026-11-22 from the bundled Musaic CLI 0.2.0-rc.2, using rmcp 3.11.0 and the `musaic-test-host` 1 test client. The profiles were MCP 2026-07-28 and 2025-11-25. The client used a scratch loopback app, synthetic audio, memory-backed pairing secrets and offline network policy.

[stdio-responses.jsonl](stdio-responses.jsonl) includes reattached scan Tasks, the ordinary unnegotiated operation handle, a final scan result, the cancellation acknowledgment and terminal cancellation state, and acknowledgements for duplicate and unknown input keys. The denial example records a completed Task with an error tool result. Identifiers are replaced consistently within each conversation. Each conversation has separate JSON-RPC request IDs. The handshake was outside the recording.

The input replay scenario seeds an outstanding `roots/list` request in the scratch app database. The implemented scan, check and analysis start tools receive their command inputs from the original tool call. A pending desktop approval remains a working Task and cannot be decided through a host input.

To recapture the redacted responses from the Rust workspace, run:

```sh
MUSAIC_TASK_TRANSCRIPT=1 cargo test -p musaic-cli --test durable_tasks -- --nocapture
```
