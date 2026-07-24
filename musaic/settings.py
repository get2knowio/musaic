"""Persistent application settings backed by the settings KV table.

Use this for user-configurable knobs that should survive restarts:

  - `ai_max_parallel`     int,  default 1   — concurrent AI jobs the worker
                                              will run at once.
  - `ai_queue_paused`     bool, default False — set true automatically when a
                                              job fails, blocking further
                                              dispatch until the user resumes
                                              the queue.

Lookups are O(1) (PK index), and the table is small enough that we don't
bother caching in process memory. The worker re-reads on every dispatch
iteration so changes take effect immediately without a restart.
"""
from __future__ import annotations

from typing import Any

from . import db as db_module

# ---------- Keys + defaults ----------

KEY_AI_MAX_PARALLEL = "ai_max_parallel"
KEY_AI_QUEUE_PAUSED = "ai_queue_paused"
# Agent backend selection. Musaic runs AI suggestions through airframe
# (the vendor-neutral agent runtime), so the user can pick whichever
# agent framework they have access to instead of being locked to Claude.
# `ai_provider_id` is an airframe provider id ("claude", "github-copilot",
# "bedrock", …); `ai_model_id` is the vendor model id for that provider.
# Defaults preserve the historical behaviour (Claude Sonnet) with zero
# config. Web research (album / cover lookup) uses the provider's hosted
# web tools when airframe reports it serves them (Claude does); providers
# without them degrade to the deterministic resolver + reasoning.
KEY_AI_PROVIDER_ID = "ai_provider_id"
KEY_AI_MODEL_ID = "ai_model_id"
DEFAULT_AI_PROVIDER_ID = "claude"
DEFAULT_AI_MODEL_ID = "claude-sonnet-4-6"
# Single API key for the selected provider. Treated generically: musaic
# injects it into the airframe adapter constructor when that adapter accepts
# an `api_key=` (Claude / Kimi / OpenAI-compatible gateways do). Adapters with
# their own auth chains (Copilot via gh, Bedrock via the AWS chain, the
# OpenCode server via loopback) ignore it and read their environment as usual.
# Write-only on the API surface, same as the FireCrawl / AcoustID keys.
KEY_AI_API_KEY = "ai_api_key"
# AcoustID API key for the per-track fingerprinting cascade (see
# fingerprint.py and the track-gaps fix path). Free key with registration
# at https://acoustid.org/new-application — we only ever read user-rated
# data so the public API tier is plenty.
KEY_ACOUSTID_API_KEY = "acoustid_api_key"
# FireCrawl API key. When set, ai.py wires the FireCrawl MCP server into
# claude-agent-sdk calls so Claude can fall back to FireCrawl for hard-to-
# find albums where its built-in WebSearch / WebFetch can't get past
# JS-rendered pages, paywalls, anti-bot blocks, etc. The system prompt
# tells Claude to treat FireCrawl as a last resort, since each call burns
# the user's paid FireCrawl quota.
KEY_FIRECRAWL_API_KEY = "firecrawl_api_key"
# Snapshot policy. The startup hook calls maybe_auto_snapshot() with
# these values; the user can adjust either in Settings. Setting
# retention to 0 disables automatic snapshotting entirely; setting
# the interval to 0 forces a snapshot on every app start.
KEY_SNAPSHOT_RETENTION = "snapshot_retention_count"
KEY_SNAPSHOT_INTERVAL_HOURS = "snapshot_min_interval_hours"
# Filename templates used by the filename-mismatch health check. Two
# templates because compilation tracks need the per-track artist baked
# into the filename (the album folder doesn't carry it), but
# single-artist tracks don't (the artist is already in the parent dir
# name and on every FLAC's albumartist tag — putting it in the filename
# too is just redundant). Picard, beets, and most other tag editors
# default to the same split for the same reason.
#
# Variables: {tracknumber} (with :02 zero-pad), {title}, {artist},
# {albumartist}, {album}, {year}, {disc}. The .flac extension is
# implicit; sanitization is always applied.
KEY_FILENAME_TEMPLATE_SINGLE = "filename_template_single_artist"
KEY_FILENAME_TEMPLATE_COMPILATION = "filename_template_compilation"
# Album-directory templates. Same single-vs-compilation split as the
# filename templates, for the same reason: compilation dirs use a
# fixed "Various Artists" credit rather than the (per-track varying)
# artist. Multi-disc handling is applied automatically — a "(disc N
# of M)" segment is appended when the album has disc_total > 1.
KEY_DIR_TEMPLATE_SINGLE = "dir_template_single_artist"
KEY_DIR_TEMPLATE_COMPILATION = "dir_template_compilation"

DEFAULTS: dict[str, str] = {
    KEY_AI_MAX_PARALLEL: "1",
    KEY_AI_QUEUE_PAUSED: "0",
    KEY_ACOUSTID_API_KEY: "",
    KEY_FIRECRAWL_API_KEY: "",
    KEY_SNAPSHOT_RETENTION: "10",
    KEY_SNAPSHOT_INTERVAL_HOURS: "24",
    KEY_FILENAME_TEMPLATE_SINGLE: "{tracknumber:02} - {title}",
    KEY_FILENAME_TEMPLATE_COMPILATION: "{tracknumber:02} - {artist} - {title}",
    KEY_DIR_TEMPLATE_SINGLE: "{albumartist} - {album} [FLAC]",
    KEY_DIR_TEMPLATE_COMPILATION: "Various Artists - {album} [FLAC]",
}


# ---------- Generic accessors ----------

def get_raw(key: str) -> str | None:
    """Return the raw stored string, or None if not set."""
    with db_module.connect() as conn:
        row = conn.execute("SELECT value FROM settings WHERE key = ?", (key,)).fetchone()
        return row["value"] if row else None


def set_raw(key: str, value: str) -> None:
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                "INSERT INTO settings(key, value) VALUES (?, ?) "
                "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                (key, value),
            )


def get_int(key: str, default: int) -> int:
    raw = get_raw(key)
    if raw is None:
        return default
    try:
        return int(raw)
    except (TypeError, ValueError):
        return default


def set_int(key: str, value: int) -> None:
    set_raw(key, str(int(value)))


def get_bool(key: str, default: bool = False) -> bool:
    raw = get_raw(key)
    if raw is None:
        return default
    return raw.strip().lower() in ("1", "true", "yes", "on")


def set_bool(key: str, value: bool) -> None:
    set_raw(key, "1" if value else "0")


# ---------- Typed accessors for the AI worker settings ----------

def ai_max_parallel() -> int:
    """Concurrent AI jobs the worker may run. Clamped to [1, 8] — Anthropic
    rate limits and local CPU make values higher than 4 unlikely to help."""
    n = get_int(KEY_AI_MAX_PARALLEL, 1)
    if n < 1:
        return 1
    if n > 8:
        return 8
    return n


def set_ai_max_parallel(value: int) -> int:
    if value < 1:
        value = 1
    if value > 8:
        value = 8
    set_int(KEY_AI_MAX_PARALLEL, value)
    return value


def ai_queue_paused() -> bool:
    return get_bool(KEY_AI_QUEUE_PAUSED, default=False)


def set_ai_queue_paused(paused: bool) -> None:
    set_bool(KEY_AI_QUEUE_PAUSED, paused)


def ai_provider_id() -> str:
    """The airframe provider id the AI worker dispatches through. Defaults
    to "claude" so existing installs behave exactly as before with no
    config. Empty/unset falls back to the default rather than erroring."""
    return (get_raw(KEY_AI_PROVIDER_ID) or "").strip() or DEFAULT_AI_PROVIDER_ID


def set_ai_provider_id(value: str) -> None:
    set_raw(KEY_AI_PROVIDER_ID, (value or "").strip())


def ai_model_id() -> str:
    """The vendor model id passed to the selected provider. Defaults to
    Claude Sonnet (the model the suggestion prompts were tuned against)."""
    return (get_raw(KEY_AI_MODEL_ID) or "").strip() or DEFAULT_AI_MODEL_ID


def set_ai_model_id(value: str) -> None:
    set_raw(KEY_AI_MODEL_ID, (value or "").strip())


def ai_api_key() -> str:
    """The API key for the selected provider, or '' if not configured.
    Injected into the airframe adapter constructor when it accepts `api_key=`;
    empty means "rely on the adapter's own env-based auth chain"."""
    return (get_raw(KEY_AI_API_KEY) or "").strip()


def set_ai_api_key(value: str) -> None:
    set_raw(KEY_AI_API_KEY, (value or "").strip())


def acoustid_api_key() -> str:
    """The user's AcoustID API key, or '' if not configured. The
    fingerprint module treats empty as "feature disabled" rather than
    erroring — fingerprint-based fixes simply skip and the cascade falls
    through to AI."""
    return (get_raw(KEY_ACOUSTID_API_KEY) or "").strip()


def set_acoustid_api_key(key: str) -> None:
    set_raw(KEY_ACOUSTID_API_KEY, (key or "").strip())


def firecrawl_api_key() -> str:
    """The user's FireCrawl API key, or '' if not configured. ai.py treats
    empty as "feature disabled" — the FireCrawl MCP isn't attached to the
    SDK call, FireCrawl tools aren't exposed to Claude, and the system
    prompt's FireCrawl-mention is omitted. Setting a key turns it on
    without a server restart (next AI call picks it up; existing prompt
    cache invalidates because tool definitions change)."""
    return (get_raw(KEY_FIRECRAWL_API_KEY) or "").strip()


def set_firecrawl_api_key(key: str) -> None:
    set_raw(KEY_FIRECRAWL_API_KEY, (key or "").strip())


def snapshot_retention() -> int:
    """How many automatic snapshots to keep. 0 disables snapshotting."""
    n = get_int(KEY_SNAPSHOT_RETENTION, 10)
    if n < 0:
        return 0
    if n > 100:
        return 100
    return n


def set_snapshot_retention(value: int) -> int:
    if value < 0:
        value = 0
    if value > 100:
        value = 100
    set_int(KEY_SNAPSHOT_RETENTION, value)
    return value


def snapshot_min_interval_hours() -> float:
    """Minimum hours between automatic snapshots. 0 = always snapshot
    on app start; negative is treated as 0."""
    raw = get_raw(KEY_SNAPSHOT_INTERVAL_HOURS)
    if raw is None:
        return 24.0
    try:
        v = float(raw)
    except (TypeError, ValueError):
        return 24.0
    return max(0.0, v)


def set_snapshot_min_interval_hours(value: float) -> float:
    v = max(0.0, float(value))
    set_raw(KEY_SNAPSHOT_INTERVAL_HOURS, str(v))
    return v


def filename_template_single_artist() -> str:
    """Template for tracks on single-artist albums. Empty / missing →
    default. The check / dialog only validate the template at apply
    time so the user can be experimenting in Settings without
    constantly tripping errors."""
    raw = (get_raw(KEY_FILENAME_TEMPLATE_SINGLE) or "").strip()
    return raw or DEFAULTS[KEY_FILENAME_TEMPLATE_SINGLE]


def set_filename_template_single_artist(value: str) -> str:
    set_raw(KEY_FILENAME_TEMPLATE_SINGLE, (value or "").strip())
    return filename_template_single_artist()


def filename_template_compilation() -> str:
    raw = (get_raw(KEY_FILENAME_TEMPLATE_COMPILATION) or "").strip()
    return raw or DEFAULTS[KEY_FILENAME_TEMPLATE_COMPILATION]


def set_filename_template_compilation(value: str) -> str:
    set_raw(KEY_FILENAME_TEMPLATE_COMPILATION, (value or "").strip())
    return filename_template_compilation()


def dir_template_single_artist() -> str:
    raw = (get_raw(KEY_DIR_TEMPLATE_SINGLE) or "").strip()
    return raw or DEFAULTS[KEY_DIR_TEMPLATE_SINGLE]


def set_dir_template_single_artist(value: str) -> str:
    set_raw(KEY_DIR_TEMPLATE_SINGLE, (value or "").strip())
    return dir_template_single_artist()


def dir_template_compilation() -> str:
    raw = (get_raw(KEY_DIR_TEMPLATE_COMPILATION) or "").strip()
    return raw or DEFAULTS[KEY_DIR_TEMPLATE_COMPILATION]


def set_dir_template_compilation(value: str) -> str:
    set_raw(KEY_DIR_TEMPLATE_COMPILATION, (value or "").strip())
    return dir_template_compilation()


def all_settings() -> dict[str, Any]:
    """Snapshot of all settings, with defaults filled in. API response shape."""
    return {
        KEY_AI_MAX_PARALLEL: ai_max_parallel(),
        KEY_AI_QUEUE_PAUSED: ai_queue_paused(),
        KEY_AI_PROVIDER_ID: ai_provider_id(),
        KEY_AI_MODEL_ID: ai_model_id(),
        # Don't return the actual key value to the client — it's a low-stakes
        # secret, but the API surface still treats it write-only. Frontend
        # only needs to know whether one is set.
        KEY_AI_API_KEY + "_set": bool(ai_api_key()),
        KEY_ACOUSTID_API_KEY + "_set": bool(acoustid_api_key()),
        KEY_FIRECRAWL_API_KEY + "_set": bool(firecrawl_api_key()),
        KEY_SNAPSHOT_RETENTION: snapshot_retention(),
        KEY_SNAPSHOT_INTERVAL_HOURS: snapshot_min_interval_hours(),
        KEY_FILENAME_TEMPLATE_SINGLE: filename_template_single_artist(),
        KEY_FILENAME_TEMPLATE_COMPILATION: filename_template_compilation(),
        KEY_DIR_TEMPLATE_SINGLE: dir_template_single_artist(),
        KEY_DIR_TEMPLATE_COMPILATION: dir_template_compilation(),
    }
