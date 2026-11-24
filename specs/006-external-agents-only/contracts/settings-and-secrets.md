# Contract: settings and secrets removed

## Settings

| Name | Where today | After |
| --- | --- | --- |
| `ai_provider_id` | `musaic-secrets/src/view.rs:19,154-169`; `settings_patch.rs:101,187-202`; `routes/settings.rs:82-88` | gone from defaults, view, patch, response; v24 deletes the row |
| `ai_model_id` | same | same |
| `ai_max_parallel` | `musaic-db/src/settings.rs:15`; `settings_patch.rs:174` | same |
| `ai_queue_paused` | `musaic-db/src/settings.rs:16`; `settings_patch.rs:182` | same |
| `ai_api_key` (`ai_api_key_set`) | `SECRET_KEYS`; `settings_patch.rs:209` | no read, write or `_set` field; row scrubbed; keyring entry deleted once |
| `firecrawl_api_key` (`firecrawl_api_key_set`) | `musaic-db/src/settings.rs:33`; `SECRET_KEYS`; `settings_patch.rs:211` | same |

A `PATCH /api/settings` naming any of them is treated as an unknown key (ignored, as today for unknown names). No `SettingsChanged` is published for them.

## Secrets

- `musaic_secrets::SECRET_KEYS` stays `["ai_api_key", "acoustid_api_key", "firecrawl_api_key"]`; it is now the union of `MIGRATED_SECRET_KEYS = ["acoustid_api_key"]` (moved to the keyring, as today) and `RETIRED_SECRET_KEYS = ["ai_api_key", "firecrawl_api_key"]` (scrubbed, never moved).
- Every place that scrubs by `SECRET_KEYS` keeps doing so: the live database and its copies (`musaic-secrets/src/migrate.rs`), the pre-upgrade snapshot (`musaic-services/src/startup.rs:398-409`), restore (`ops/database.rs:59-60` → `App::scrub_keys`). The log redaction list keeps both names (FR-027).
- Startup: `musaic_services::keyscrub::remove_retired_keys`, called by `musaic-app/src/app.rs` right after the startup key scrub, deletes keyring entries `ai_api_key` and `firecrawl_api_key` under service `musaic` through the existing `SecretStore::delete` (a missing entry is already `Ok`). When both succeed, write settings row `retired_keys_removed = 1`; while it is present the step is skipped. A failure is logged by name only and retried next start. The in-memory test store (`--dev-memory-secrets`, `--verification-memory-secrets`) already implements the same delete and records each call, which the startup test reads.
- `musaic-desktop/src/nav.rs`: the FireCrawl sign-up link is removed from the external-link allowlist (`:12`, `:121`).

## Tests

`musaic-secrets/tests/{migration,old_schema_backup,settings_golden}.rs` (retired rows scrubbed, not moved), a new startup test `musaic-services/tests/retired_keys.rs` (entries present → deleted once; second start makes no keyring call; failure retried), `musaic-api/tests/log_secrets.rs` (both names still redacted, no provider builder), `musaic-api/tests/schema_upgrade_copy.rs` (snapshot has neither row).
