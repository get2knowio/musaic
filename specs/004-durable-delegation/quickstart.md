# Verify the feature
Build the frontend with `npm ci && npm run build` from `frontend/`, and provide the generated synthetic audio fixture described in CLAUDE.md. Run all commands against a scratch config and fixture copy in offline mode with memory secrets.
1. Exercise the database migration with examples of every legacy state, run it twice and compare work, mappings and review groups.
2. Enqueue ambiguous metadata via the desktop. Pair two test agents, race their claims, advance the test clock, and retry a stale completion. Use a fresh bridge to replay the winning completion and verify a single proposal.
3. Inspect the current evidence and preview, approve an edit in the desktop, check the backup and file changes, and test discard independently. Repeat with a stale revision and offline verification.
4. Start a scan with a negotiated modern client and a legacy client. Reconnect, poll, submit a repeated input response and request cancellation. Confirm the final state and any partial result. Restart, restore and switch libraries in separate scenarios.
5. Run repository checks and retain redacted conformance evidence using public fixtures only.
