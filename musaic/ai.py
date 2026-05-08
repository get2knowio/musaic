"""Claude-powered metadata + cover-art suggestions.

Mirrors flac-jacket's `claude_agent_sdk.query(...)` pattern. Authentication
follows the SDK's defaults (typically `ANTHROPIC_API_KEY` from env, or an
existing Claude Code session).

Two operations are exposed:

  - `suggest_metadata(album, tracks)` — Claude inspects the directory name,
    current Vorbis tags, and per-track titles, optionally using WebSearch +
    WebFetch (e.g. against MusicBrainz / Discogs / Wikipedia), and returns a
    structured JSON proposal of corrections.

  - `suggest_cover(album)` — Claude proposes a cover-art URL (typically from
    the Cover Art Archive) given the album identity, with rationale.

Each returns a dict matching the `MetadataSuggestion` / `CoverSuggestion`
shapes documented below.
"""
from __future__ import annotations

import asyncio
import json
import logging
import re
from dataclasses import dataclass, field
from typing import Any

log = logging.getLogger(__name__)

# User-Agent used when verifying AI-returned cover URLs (HEAD/GET probes).
# Matches the user-agent the actual download path sends so any host-side
# UA-based filtering treats verification and download identically.
_VERIFY_UA = "musaic/0.1"


# ---------- Result shapes ----------

@dataclass
class TrackSuggestion:
    """Per-file proposed tag changes. Only includes keys whose value should
    change — the apply path treats absent keys as "leave alone"."""
    filename: str
    suggestions: dict[str, str] = field(default_factory=dict)


@dataclass
class MetadataSuggestion:
    suggestions: dict[str, str] = field(default_factory=dict)   # album-level field -> proposed value
    tracks: list[TrackSuggestion] = field(default_factory=list)  # per-file changes
    notes: str = ""
    confidence: str = "unknown"                                  # low | medium | high
    raw: str = ""                                                # raw model output, for debugging
    # Optional MB release-group MBID, when AI confidently identified the
    # release. Validated to UUID format here; verified against MB before
    # the worker actually persists it. None when AI didn't include one
    # (or it failed validation).
    mb_release_group_mbid: str | None = None


@dataclass
class CoverSuggestion:
    cover_url: str | None = None
    source: str = ""        # e.g. "Cover Art Archive (release MBID xxx)"
    notes: str = ""
    confidence: str = "unknown"
    raw: str = ""


SUGGESTABLE_FIELDS = (
    "title", "album_artist", "artist", "year", "genre",
    # v8 — Vorbis sort variants. AI populates these when it can pull a
    # canonical sort form from MusicBrainz; the deterministic
    # missing-sort-tags check populates them via article rotation
    # without going through AI.
    "album_artist_sort", "artist_sort", "album_sort",
)
TRACK_SUGGESTABLE_FIELDS = ("title", "artist", "tracknumber")


# ---------- FireCrawl MCP integration ----------
#
# FireCrawl is OPT-IN. When the user has stored an API key under
# settings.firecrawl_api_key, we mount the FireCrawl MCP server into the
# claude-agent-sdk call and expose its tools to Claude. The system prompt
# tells Claude to treat FireCrawl as a last resort — try built-in
# WebSearch / WebFetch first, only reach for FireCrawl when those can't
# get past JS-rendered pages, paywalls, or anti-bot blocks. Each FireCrawl
# call burns paid quota.
#
# Subset of FireCrawl tools we expose. We don't enable `firecrawl_crawl`
# (multi-page recursive crawl, expensive) or `firecrawl_map` (sitemap
# discovery, rarely useful for a single album lookup) — `scrape`, `search`,
# and `extract` cover everything we want for "fetch one specific page" or
# "search for this album".
_FIRECRAWL_ALLOWED_TOOLS = (
    "mcp__firecrawl__firecrawl_search",
    "mcp__firecrawl__firecrawl_scrape",
    "mcp__firecrawl__firecrawl_extract",
)

# Hosted FireCrawl MCP. The API key is part of the URL path (FireCrawl's
# convention as of 2026 — no auth headers, no env vars). Using the hosted
# endpoint means we don't have to manage an `npx firecrawl-mcp` subprocess
# or its node_modules — claude-agent-sdk just opens an SSE connection.
_FIRECRAWL_MCP_URL_TMPL = "https://mcp.firecrawl.dev/{api_key}/v2/mcp"

# Appended to the system prompts when FireCrawl is enabled. Kept short so
# the cache hit on the static portion of the system prompt is preserved
# when the FireCrawl section is identical across calls (and the system
# prompt simply has one extra trailing paragraph).
_FIRECRAWL_PROMPT_HINT = """

WEB ACCESS LADDER
=================

Always try WebSearch and WebFetch FIRST. They're free and fast and handle
the vast majority of music-catalog queries. Only reach for FireCrawl
(`mcp__firecrawl__firecrawl_search`, `mcp__firecrawl__firecrawl_scrape`,
`mcp__firecrawl__firecrawl_extract`) when WebSearch / WebFetch can't get
through — JS-rendered pages, paywalls, anti-bot blocks, or sites that
return empty bodies to plain GETs. FireCrawl is paid quota; one or two
targeted calls is fine, more than that on a single album means you're
fishing and should stop and return your best-guess answer with low
confidence."""


def _build_sdk_extras(system_prompt: str) -> tuple[str, list[str], dict[str, Any]]:
    """Return (system_prompt, allowed_tools, mcp_servers) for an SDK call.

    When FireCrawl is configured, this:
      - appends the "WEB ACCESS LADDER" guidance to the system prompt,
      - adds the FireCrawl MCP tools to allowed_tools,
      - returns a one-server mcp_servers dict pointing at the hosted SSE
        endpoint with the user's API key in the URL.

    When FireCrawl is NOT configured, returns the unmodified system prompt,
    just `["WebSearch", "WebFetch"]` for tools, and an empty mcp_servers
    dict — the SDK call is identical to the pre-FireCrawl behavior, so
    nothing changes for users who never set the key.

    The settings lookup happens once per call (it's a single SQLite SELECT)
    rather than at module import, so toggling the key in Settings takes
    effect on the next AI call without a server restart.
    """
    from . import settings as settings_module

    base_tools = ["WebSearch", "WebFetch"]
    fc_key = settings_module.firecrawl_api_key()
    if not fc_key:
        return system_prompt, base_tools, {}

    return (
        system_prompt + _FIRECRAWL_PROMPT_HINT,
        base_tools + list(_FIRECRAWL_ALLOWED_TOOLS),
        {
            "firecrawl": {
                "type": "sse",
                "url": _FIRECRAWL_MCP_URL_TMPL.format(api_key=fc_key),
            },
        },
    )


# ---------- Prompt construction ----------
#
# Caching strategy. claude-agent-sdk auto-caches the `system_prompt` + tool
# definitions across calls (5-minute ephemeral by default; 1-hour with
# ENABLE_PROMPT_CACHING_1H=true). User messages are NOT cached. So we put
# everything static — instructions, anchoring rules, schemas, formatting —
# in the system prompt, and keep the per-call user message minimal: just
# the dynamic album payload + optional hint, both wrapped in clear delimiters
# so the model knows where the static instructions end and the inputs begin.
#
# Concretely: every metadata call sends the same ~1.2k-token system prompt
# (cache hit after the first call), and a ~150-token user message. Before
# this restructure, every call was sending the full ~1.5k-token instruction
# block in the user message — no caching at all.

_SYSTEM_PROMPT_METADATA = """You are a music catalog expert. Your job is to identify a music album from
its directory name, embedded tags, and per-track titles, then propose
corrections to album-level AND per-track metadata where you have HIGH
confidence the current value is wrong. Use authoritative sources
(MusicBrainz, Discogs, label catalogs, Wikipedia) via WebSearch and WebFetch
to verify. Always respond as STRICT JSON in the exact schema below — no
prose, no markdown fences.

INPUT FORMAT
============

The user message contains ONE album wrapped in <album> tags. Inside is a JSON
object with these keys:

  - directory:    { dir_name, matched, dir_artist, dir_album, is_compilation,
                    disc_number, disc_total }
  - current_tags: { title, album_artist, artist, year, genre }
  - track_count:  integer
  - total_seconds: number
  - tracks: [{ filename, tracknumber, title, artist, duration }, …]

The user may also include a <hint> block. Treat its content as high-confidence
ground truth supplied by the user. If you see strong evidence the hint is wrong
(e.g. the tracklist clearly identifies a different album), call that out in
`notes` rather than silently overriding it.

ANCHORING LADDER (use in this order)
====================================

  1. `directory.dir_artist` and `directory.dir_album` — extracted from the
     folder name using a strict convention enforced by the user's ripping
     pipeline. The most trustworthy signal of who the album is by and what
     it's called. Use them to seed your MusicBrainz / Discogs / Wikipedia
     search.

  2. `tracks` (filenames + titles + count) — confirm the specific release /
     edition (original vs deluxe vs remaster vs compilation vs boxset). If the
     track count is short for the canonical release, call it out in `notes`
     rather than silently picking a different release.

  3. `current_tags` — the values currently embedded in the FLAC files. These
     may be stale or wrong (the entire reason this request exists). Treat them
     as "what software last touched these files thought," not as ground truth.
     Do not refuse to suggest a correction just because a current_tags value
     matches one of your candidates.

  4. If `directory.matched` is false (folder name didn't fit the convention),
     fall back to the raw `directory.dir_name` plus the track listing. Be
     more conservative — note your reduced confidence in `notes`.

COMPILATION HANDLING
====================

  - If `directory.is_compilation` is true, the album is a multi-artist
    compilation. Do NOT propose a single primary artist for the album as a
    whole. Set `album_artist` to "Various Artists" (the MusicBrainz canonical
    value) if it isn't already.
  - The per-track `artist` values matter most for compilations — fill in the
    actual track artist for each FLAC.

PER-TRACK SUGGESTIONS
=====================

  - For each FLAC, you may propose corrections to title, artist, and/or
    tracknumber. Match each suggestion to a FLAC by its `filename`.
  - Only include a key whose value should change. Tracks with no changes
    should be omitted from the `tracks` array entirely.
  - **Track number normalization:** `tracknumber` MUST be a plain linear
    integer string ("1", "2", "12"). The user's library stores multi-disc
    sets as a single linear sequence (disc 1 tracks first, then disc 2
    starting at the next number). If you see disc-prefixed numbers like
    "1-05", "1.05", or "01.05" in `current_tags`, normalize them — this
    itself is a high-confidence correction worth proposing.
  - Keep titles, artists, and tracknumbers internally consistent: if you're
    reordering tracks, update tracknumber for each affected track. The
    apply pipeline will rename the underlying files to match.

SORT TAGS (Vorbis *SORT fields)
===============================

Music players that respect Vorbis conventions (Plex, Roon, Symfonium,
foobar2000, etc.) use the sort variants for sidebar / library
alphabetization. The user's library benefits from having these set
when the primary tag would otherwise sort under the wrong letter.

You may propose `album_artist_sort`, `artist_sort`, and/or `album_sort`
when you have HIGH confidence in the canonical sort form. Three cases:

  1. **Articles ("The", "A", "An")**: rotate the article to the end —
     "The Beatles" → "Beatles, The". This is mechanical; only worth
     proposing if the matching sort field is currently missing.

  2. **Personal names**: prefer "Last, First" form — "Bob Dylan" →
     "Dylan, Bob". ONLY do this when MusicBrainz's canonical
     `sort-name` field for that artist agrees. Don't guess at family
     name vs. first name when the artist is a band, a stage name, or
     ambiguous. When uncertain, omit the field.

     Examples of what to do and NOT do:
       - "Bob Dylan" → "Dylan, Bob"          ✓ (personal name; MB confirms)
       - "Henry Mancini" → "Mancini, Henry"  ✓ (personal name; MB confirms)
       - "Mannheim Steamroller" → unchanged  ✗ — band name, do NOT rotate
       - "Steely Dan" → unchanged            ✗ — band name, do NOT rotate
       - "Pink Floyd" → unchanged            ✗ — band name, do NOT rotate
       - "Various Artists" → unchanged       ✗ — canonical compilation
                                                   marker, do NOT rotate
       - "Soundtrack" → unchanged            ✗ — generic credit, leave alone
       - "AC/DC" → unchanged                 ✗ — band name, do NOT rotate

     The rule of thumb: if the second token is a common surname (like
     "Dylan" or "Mancini") AND MB has the artist filed as a person
     with a "Last, First" sort-name, rotate. If the second token is a
     non-personal noun ("Steamroller", "Floyd", "Dan", "Orchestra",
     "Brothers", "Sisters", "Quartet", "Trio", "Singers"), leave it
     alone — it's a band name regardless of how Western it sounds.

  3. **Credit permutations of the same act**: the user uses sort tags
     as a normalization key. When album_artist is a credit variant
     like "Ray Conniff & His Orchestra", "Ray Conniff, his Orchestra
     and Chorus", or "Ray Conniff Featuring [...]", the sort form
     should reduce to the PRIMARY artist's canonical sort-name —
     "Conniff, Ray" in all those cases. The user's library has many
     such cases (jazz / classical / easy-listening era); a Ray
     Conniff solo album, a "Ray Conniff & His Orchestra" album, and
     a "Ray Conniff and the Singers" album should ALL get
     artist_sort="Conniff, Ray" so they group together in Plex /
     Roon sidebars.

     Strip these credit suffixes when deriving the sort:
       " & His/Her [Orchestra/Choir/Singers/Big Band/...]"
       " and His/Her [...]"
       ", his/her [Orchestra/Singers/...] and [...]"
       " Featuring [...]"  /  " feat. [...]"  /  " with [...]"
       " and [Other Artist]"  (when MB shows them as separate credits
                                of the same primary artist)

     Then look up the primary artist on MusicBrainz and use its
     sort-name. Fall back to omitting the field rather than guessing
     when MB doesn't have the act.

If MusicBrainz doesn't have the album OR doesn't expose a useful
sort-name, omit the sort field. The user has a separate deterministic
check (missing-sort-tags) that handles the article case without AI;
your job is the cases that check can't reach — and the credit-
permutation normalization is the most valuable of those.

MUSICBRAINZ IDENTIFICATION
==========================

If — and only if — you confidently identified the release on MusicBrainz
(via WebSearch/WebFetch on https://musicbrainz.org), include the
release-group MBID in the response. This is the canonical anchor for every
downstream lookup; once we have it, future cover/year/tracklist queries
short-circuit. NEVER fabricate or guess an MBID — UUID format is easy to
mimic and a fake one poisons the cache. Omit the field rather than guess.

Format: 8-4-4-4-12 lowercase hex (e.g. "f8b29c2c-9f5d-4c4d-89cb-7c2e5fbf6f0a").
Use the release-group ID from `https://musicbrainz.org/release-group/{id}`,
NOT the release ID (releases are pressings; release-groups are the album
identity that survives across editions).

OUTPUT SCHEMA — STRICT JSON, NO PROSE, NO MARKDOWN
==================================================

{
  "suggestions": {
    "title"?: string,              // album title — the album as a single work
    "album_artist"?: string,       // primary album artist, or "Various Artists" for compilations
    "artist"?: string,             // album-level artist tag (often same as album_artist)
    "year"?: string,               // 4-digit original release year
    "genre"?: string,
    "album_artist_sort"?: string,  // canonical sort form of album_artist (omit if uncertain)
    "artist_sort"?: string,        // canonical sort form of artist (omit if uncertain)
    "album_sort"?: string          // canonical sort form of title (omit if uncertain)
  },
  "tracks": [
    {
      "filename": string,     // exact filename from input — used to match to the FLAC
      "suggestions": {
        "title"?: string,
        "artist"?: string,
        "tracknumber"?: string   // linear integer, no disc prefix
      }
    }
  ],
  "mb_release_group_mbid"?: string,  // canonical MB release-group UUID; omit if uncertain
  "notes": string,            // brief — flag any concerns (track count, ambiguous edition, etc.)
  "confidence": "low" | "medium" | "high"
}

Only include keys whose value should change. Omit `tracks` entries with no
changes. If you have no high-confidence corrections at all, return empty
`suggestions` and empty `tracks` — but still include `mb_release_group_mbid`
if you confidently identified the release."""


_SYSTEM_PROMPT_COVER = """You are a music catalog expert. Your job is to find canonical front cover
art for a given album, preferring the Cover Art Archive (linked via
MusicBrainz). Use WebSearch and WebFetch as needed. Always respond as STRICT
JSON in the exact schema below — no prose, no markdown fences.

INPUT FORMAT
============

The user message contains ONE album wrapped in <album> tags, with the same
shape used by the metadata identification flow:

  - directory:    { dir_name, matched, dir_artist, dir_album, is_compilation,
                    disc_number, disc_total }
  - current_tags: { title, album_artist, artist, year, genre }
  - tracks: [{ filename, tracknumber, title, artist, duration }, …]

The user may also include a <hint> block. Treat its content as high-confidence
ground truth supplied by the user.

LOOKUP STRATEGY
===============

  1. If the album has a known MusicBrainz release ID, use the front-cover URL
     for that release on coverartarchive.org.
  2. Otherwise, search MusicBrainz for the release using `directory.dir_artist`
     and `directory.dir_album` as your primary search terms — these were
     parsed from the folder name and are the most authoritative anchor.
  3. Use the track listing to confirm which specific release/edition this is.
  4. If `directory.is_compilation` is true, search for a compilation /
     various-artists release rather than a single-artist album.

The `cover_url` must be a direct URL to an image (jpg/png). It is OK and
expected to set it to `null` if you can't identify a cover with reasonable
confidence — say so in `notes`.

OUTPUT SCHEMA — STRICT JSON, NO PROSE, NO MARKDOWN
==================================================

{
  "cover_url": string | null,
  "source": string,
  "notes": string,
  "confidence": "low" | "medium" | "high"
}"""


def _format_user_message(album_payload: dict[str, Any], user_hint: str = "") -> str:
    """Build the per-call (uncached) user message: just the dynamic inputs.

    Wrapped in <album>/<hint> tags so the model unambiguously knows where
    the cached system instructions end and the per-call inputs begin. We
    keep this lean — every byte here is uncached and re-billed per call."""
    hint = (user_hint or "").strip()
    # Truncate egregiously long hints — anything over ~2k chars is more
    # likely a paste mistake than useful context.
    if len(hint) > 2000:
        hint = hint[:2000] + "\n[…hint truncated to 2000 characters…]"

    parts: list[str] = []
    if hint:
        parts.append(f"<hint>\n{hint}\n</hint>")
    parts.append(f"<album>\n{json.dumps(album_payload, indent=2)}\n</album>")
    return "\n\n".join(parts)


# ---------- Album payload builder ----------

def _album_payload(album_dict: dict[str, Any], tracks: list[dict[str, Any]]) -> dict[str, Any]:
    # `directory.matched=False` if the dir name didn't fit the strict
    # `Artist - Album [FLAC]` convention; the prompt tells Claude to fall
    # back to the raw name + tracks in that case.
    return {
        "directory": {
            "dir_name": album_dict.get("dir_name"),
            "matched": album_dict.get("dir_artist") is not None or album_dict.get("dir_album") is not None or bool(album_dict.get("is_compilation")),
            "dir_artist": album_dict.get("dir_artist"),
            "dir_album": album_dict.get("dir_album"),
            "is_compilation": bool(album_dict.get("is_compilation")),
            "disc_number": album_dict.get("disc_number"),
            "disc_total": album_dict.get("disc_total"),
        },
        "current_tags": {
            "title": album_dict.get("title"),
            "album_artist": album_dict.get("album_artist"),
            "artist": album_dict.get("artist"),
            "year": album_dict.get("year"),
            "genre": album_dict.get("genre"),
        },
        "track_count": album_dict.get("track_count"),
        "total_seconds": album_dict.get("total_seconds"),
        "tracks": [
            {
                "filename": t.get("filename"),
                "tracknumber": t.get("tracknumber"),
                "title": t.get("title"),
                "artist": t.get("artist"),
                "duration": t.get("duration"),
            }
            for t in tracks
        ],
    }


# ---------- JSON extraction ----------

_FENCE_RE = re.compile(r"^```(?:json)?\s*|\s*```\s*$", re.IGNORECASE | re.MULTILINE)


def _extract_json(text: str) -> dict[str, Any] | None:
    if not text:
        return None
    cleaned = _FENCE_RE.sub("", text.strip()).strip()
    first = cleaned.find("{")
    last = cleaned.rfind("}")
    if first == -1 or last == -1:
        return None
    try:
        return json.loads(cleaned[first : last + 1])
    except Exception:
        return None


# ---------- SDK runner ----------

# If the SDK's exception message matches this pattern, we treat it as a
# transient failure (almost always a Claude Code subscription rate-limit
# hit) and retry once after a backoff. Any other failure is surfaced
# immediately.
def _looks_like_rate_limit(message: str) -> bool:
    return ("exit code 1" in message) and ("Check stderr output" in message)


async def _run_sdk(
    prompt: str,
    *,
    allowed_tools: list[str],
    model: str,
    max_turns: int,
    system_prompt: str,
    mcp_servers: dict[str, Any] | None = None,
) -> str:
    """Run a single claude-agent-sdk query and return the assistant's final text.

    Runs the actual SDK call in a separate thread with a fresh asyncio event
    loop. This isolates the SDK's subprocess management from uvicorn's
    long-running event loop — a direct `await query(...)` inside the FastAPI
    handler causes the `claude` CLI subprocess to exit 1 partway through long
    multi-turn calls (signal/pipe-handling interaction). The same call path
    from a fresh `asyncio.run(...)` works fine, hence the isolation.

    Failure policy: any SDK exception is surfaced immediately to the caller.
    There used to be an in-band 30s auto-retry on the opaque "exit code 1"
    error, but it created a long stretch of frozen "in progress" UI state
    with no feedback, AND now the job-queue layer auto-pauses on failure
    + offers a manual Retry button, so retrying is fully under user control.
    Letting the SDK error bubble up immediately gets the user back to a
    state they can act on within seconds, not a minute later.

    The cost is one OS thread per AI call. For the per-album rate of these
    requests, that's a non-issue.
    """
    import asyncio

    def _run_in_thread() -> str:
        return asyncio.run(_query_async(
            prompt=prompt,
            allowed_tools=allowed_tools,
            model=model,
            max_turns=max_turns,
            system_prompt=system_prompt,
            mcp_servers=mcp_servers or {},
        ))

    try:
        return await asyncio.to_thread(_run_in_thread)
    except Exception as e:
        last_error = e
    message = str(last_error)
    # Quieter logging for the known opaque "exit code 1" pattern — the
    # message itself is informative and the full traceback adds no signal.
    # Truly unexpected failures still get the full exception trace at ERROR.
    if _looks_like_rate_limit(message):
        log.warning(
            "claude-agent-sdk call failed (model=%s, tools=%s, max_turns=%s) "
            "— marking job failed; user can retry from the AI Jobs page",
            model, allowed_tools, max_turns,
        )
        log.debug("full traceback for SDK failure", exc_info=last_error)
        friendly = (
            "Claude Code subprocess exited with no detail. Most often this is "
            "a rate-limit hit (subscriptions have shared 5-hour and weekly "
            "windows); it can also be a transient CLI/network issue or an "
            "incompatibility between the chosen model and the tools requested. "
            "Hit Retry on the AI Jobs page after a moment, or run "
            "`.venv/bin/python diagnose_claude.py <album_id>` to triage — if "
            "test 5 (cover) also fails, the bug is model/tool-side, not quota."
        )
        raise type(last_error)(friendly) from last_error
    log.exception(
        "claude-agent-sdk call failed (model=%s, tools=%s, max_turns=%s)",
        model, allowed_tools, max_turns,
    )
    cause = getattr(last_error, "__cause__", None)
    if cause:
        raise type(last_error)(f"{last_error} (caused by {type(cause).__name__}: {cause})") from cause
    raise last_error


async def _query_async(*, prompt: str, allowed_tools: list[str], model: str,
                        max_turns: int, system_prompt: str,
                        mcp_servers: dict[str, Any]) -> str:
    """The actual SDK iteration. Lives in its own coroutine so we can pass
    it to `asyncio.run()` cleanly inside the worker thread."""
    from claude_agent_sdk import (
        AssistantMessage,
        ClaudeAgentOptions,
        ResultMessage,
        query,
    )

    # Only pass mcp_servers when there's at least one server configured —
    # passing an empty dict would still flip the SDK into "MCP mode" and
    # could affect cache keys / tool advertisements. Keep the call shape
    # identical to pre-FireCrawl when no MCP is wired up.
    options_kwargs: dict[str, Any] = dict(
        allowed_tools=allowed_tools,
        model=model,
        max_turns=max_turns,
        system_prompt=system_prompt,
    )
    if mcp_servers:
        options_kwargs["mcp_servers"] = mcp_servers

    last_text = ""
    final_text = ""
    async for message in query(
        prompt=prompt,
        options=ClaudeAgentOptions(**options_kwargs),
    ):
        if isinstance(message, AssistantMessage):
            for block in message.content:
                if hasattr(block, "text") and block.text:
                    last_text = block.text
        if isinstance(message, ResultMessage):
            if message.result:
                final_text = message.result
    return final_text or last_text


# ---------- Public API ----------

# Override hook for testing. If set to a callable taking (kind, prompt, payload),
# it's used instead of calling the SDK. Returns the raw model text.
_sdk_override = None  # type: ignore


def set_sdk_override(fn) -> None:
    """Install a fake SDK runner. Used by tests."""
    global _sdk_override
    _sdk_override = fn


async def suggest_metadata(album_dict: dict[str, Any], tracks: list[dict[str, Any]], user_hint: str = "") -> MetadataSuggestion:
    """Ask Claude for high-confidence metadata corrections.

    `user_hint` is optional free-form context the user provided in the UI
    (e.g. "this is a 2012 reissue, the original is 1979"). It's wrapped in
    a <hint> block in the user message, where the system prompt instructs
    the model to treat it as ground truth.
    """
    payload = _album_payload(album_dict, tracks)
    user_message = _format_user_message(payload, user_hint=user_hint)

    if _sdk_override:
        raw = await _sdk_override("metadata", user_message, payload)
    else:
        # max_turns=20: with the async job queue the user isn't waiting on
        # us, so erring on the side of more headroom is free for hard-to-
        # identify albums. Sonnet terminates as soon as it has an answer;
        # the cap is a safety net for pathological cases, not a budget.
        #
        # The bulky instruction template lives in _SYSTEM_PROMPT_METADATA,
        # which the SDK auto-caches. The per-call user message is just the
        # tagged album payload (+ optional hint), so we pay full input rate
        # only on the dynamic ~150 tokens, not the ~1.2k-token template.
        # _build_sdk_extras layers FireCrawl tools + the "last resort"
        # prompt hint on top when the user has configured a FireCrawl key.
        sys_prompt, allowed_tools, mcp_servers = _build_sdk_extras(_SYSTEM_PROMPT_METADATA)
        raw = await _run_sdk(
            prompt=user_message,
            allowed_tools=allowed_tools,
            model="claude-sonnet-4-6",
            max_turns=20,
            system_prompt=sys_prompt,
            mcp_servers=mcp_servers,
        )

    parsed = _extract_json(raw) or {}
    sug = parsed.get("suggestions") or {}
    # Whitelist suggestion keys to the fields we know how to apply.
    sug = {k: str(v) for k, v in sug.items() if k in SUGGESTABLE_FIELDS and v not in (None, "")}

    # Per-track suggestions. Each entry must have a filename and at least one
    # whitelisted field with a non-empty value.
    tracks: list[TrackSuggestion] = []
    for raw_track in parsed.get("tracks") or []:
        if not isinstance(raw_track, dict):
            continue
        filename = raw_track.get("filename")
        track_sug = raw_track.get("suggestions") or {}
        if not filename or not isinstance(track_sug, dict):
            continue
        cleaned = {
            k: str(v).strip()
            for k, v in track_sug.items()
            if k in TRACK_SUGGESTABLE_FIELDS and v not in (None, "")
        }
        if cleaned:
            tracks.append(TrackSuggestion(filename=str(filename), suggestions=cleaned))

    # Extract MB release-group MBID if AI included one. Strict UUID-format
    # validation here catches obvious hallucinations ("idk-just-a-string")
    # before they ever leave the parser. We re-verify the MBID against
    # MusicBrainz before persisting (in the worker post-job hook), so even
    # a well-formatted-but-fake UUID still gets rejected — but format
    # filtering keeps the live MB call from running on garbage.
    mbid_raw = parsed.get("mb_release_group_mbid")
    mb_mbid = _validate_mbid(mbid_raw) if mbid_raw else None
    if mbid_raw and not mb_mbid:
        log.info("AI returned malformed mb_release_group_mbid %r; ignoring", mbid_raw)

    return MetadataSuggestion(
        suggestions=sug,
        tracks=tracks,
        notes=str(parsed.get("notes") or ""),
        confidence=str(parsed.get("confidence") or "unknown"),
        raw=raw,
        mb_release_group_mbid=mb_mbid,
    )


# UUID regex matching MusicBrainz canonical form: lowercase 8-4-4-4-12.
# We're strict on case so we don't accidentally normalize a wrong-cased
# value into the cache (canonical MBIDs are always lowercase). MB-format
# UUIDs are always v4 but we don't enforce the version byte — version 4
# requires bit 65–72 to be 0100b, and a malicious actor could fake it
# anyway. The MB live-verify step in the worker catches fakes regardless.
_MBID_RE = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")


def _validate_mbid(value: Any) -> str | None:
    """Return the canonical MBID string if `value` looks like one, else None."""
    if not isinstance(value, str):
        return None
    s = value.strip().lower()
    return s if _MBID_RE.match(s) else None


async def suggest_cover(album_dict: dict[str, Any], tracks: list[dict[str, Any]], user_hint: str = "") -> CoverSuggestion:
    """Ask Claude for a canonical cover-art URL, with a deterministic
    MusicBrainz fallback when the AI subprocess errors opaquely.

    `user_hint` is optional free-form context (see `suggest_metadata`).

    Failure-mode policy: the Claude Code subprocess sometimes exits 1 with
    no useful detail — most often on hard-to-identify albums. When that
    happens we fall through to `mb_lookup.find_cover` (direct MusicBrainz
    search + Cover Art Archive HEAD), which gives us a deterministic answer:
    either a real cover URL, or a clear "no MB match" / "MB has it but no
    CAA cover" note. The caller (background worker) can then mark the job
    completed with that result instead of bubbling up a generic Claude error
    that the user can't act on. We only re-raise if the MB fallback ALSO
    fails to produce any answer (rare — usually means MB is down).
    """
    payload = _album_payload(album_dict, tracks)
    user_message = _format_user_message(payload, user_hint=user_hint)

    try:
        if _sdk_override:
            raw = await _sdk_override("cover", user_message, payload)
        else:
            # Use Sonnet (not Haiku) for tool-using calls. We discovered via
            # diagnose_claude.py test 5 that Haiku + WebSearch/WebFetch fails
            # consistently with "Command failed with exit code 1" inside the
            # claude-agent-sdk agent loop — appears to be a Claude Code-side
            # incompatibility between Haiku and the tool-calling protocol in
            # multi-turn calls. Sonnet is proven to work in the same combo.
            #
            # max_turns=20 (matched to suggest_metadata): with the async job
            # queue we're not blocking the user, and Sonnet terminates when it
            # has an answer. The cap is a safety net for pathological cases,
            # not a budget. Earlier 6-turn cap was choking on hard-to-find
            # albums that needed extra MB query refinement.
            #
            # Caching note (matches suggest_metadata): the bulky lookup-strategy
            # template lives in _SYSTEM_PROMPT_COVER (auto-cached by SDK); the
            # per-call user message is just the tagged album payload + optional
            # hint, so the dynamic portion stays minimal.
            # _build_sdk_extras layers FireCrawl tools + the "last resort"
            # prompt hint on top when the user has configured a FireCrawl key.
            sys_prompt, allowed_tools, mcp_servers = _build_sdk_extras(_SYSTEM_PROMPT_COVER)
            raw = await _run_sdk(
                prompt=user_message,
                allowed_tools=allowed_tools,
                model="claude-sonnet-4-6",
                max_turns=20,
                system_prompt=sys_prompt,
                mcp_servers=mcp_servers,
            )
    except Exception as e:
        # AI subprocess died (usually the opaque exit-1). Try the
        # deterministic MB / CAA path before giving up.
        log.info(
            "AI cover lookup raised %s; falling back to direct MusicBrainz",
            type(e).__name__,
        )
        return await _fallback_cover_lookup(album_dict, user_hint, ai_error=e)

    parsed = _extract_json(raw) or {}
    cover_url = parsed.get("cover_url")
    if cover_url is not None:
        cover_url = str(cover_url) or None
    notes = str(parsed.get("notes") or "")
    source = str(parsed.get("source") or "")
    confidence = str(parsed.get("confidence") or "unknown")

    # Verify the URL actually resolves to an image before handing it to the
    # user — Claude has hallucinated 404 URLs more than once. We don't want
    # the user to see a "Found cover!" result, click Apply, and then get a
    # download error. Better to surface the failure now and let the user
    # supply a hint or try a different lookup.
    if cover_url:
        ok, reason = await asyncio.to_thread(_verify_cover_url, cover_url)
        if not ok:
            log.info("AI cover URL failed verification (%s): %s", reason, cover_url)
            verify_note = f"Claude's suggested URL didn't resolve to an image ({reason})."
            notes = f"{notes}\n\n{verify_note}".strip() if notes else verify_note
            cover_url = None
            # AI proposed a bogus URL — try MB directly as a second chance.
            mb = await _fallback_cover_lookup(album_dict, user_hint, ai_error=None)
            if mb.cover_url:
                # MB found one. Return that; preserve the AI's reasoning in
                # notes so the user can see why we ended up using MB.
                return CoverSuggestion(
                    cover_url=mb.cover_url,
                    source=mb.source,
                    notes=f"{notes}\n\nFell back to MusicBrainz: {mb.notes}".strip(),
                    confidence=mb.confidence,
                    raw=raw,
                )
            # MB also empty — combine notes so the user sees the full story.
            notes = f"{notes}\n\n{mb.notes}".strip() if mb.notes else notes

    return CoverSuggestion(
        cover_url=cover_url,
        source=source,
        notes=notes,
        confidence=confidence,
        raw=raw,
    )


async def _fallback_cover_lookup(
    album_dict: dict[str, Any], user_hint: str, *, ai_error: Exception | None,
) -> CoverSuggestion:
    """Try `mb_lookup.find_cover` and shape the result as a CoverSuggestion.

    `ai_error` is the exception that triggered the fallback (None if we're
    falling back because the AI returned a bogus URL rather than crashing).
    Either way we surface the original cause in the notes so the user knows
    what happened — not just "no cover found" out of nowhere.
    """
    from . import mb_lookup
    try:
        result = await asyncio.to_thread(
            mb_lookup.find_cover, album_dict, hint=user_hint or "",
        )
    except Exception as fallback_err:
        # MB itself blew up — re-raise the ORIGINAL AI error so the worker
        # marks the job failed with the most-informative message. (If there
        # was no original AI error, raise the fallback one.)
        log.warning("MB fallback also failed: %s", fallback_err)
        if ai_error is not None:
            raise ai_error
        raise

    cover_url = result.get("cover_url")
    notes = str(result.get("notes") or "")
    source = str(result.get("source") or "")
    confidence = str(result.get("confidence") or "low")

    if ai_error is not None:
        prefix = (
            "Claude's cover-art search exited with an error before returning a "
            "result; tried MusicBrainz directly as a fallback."
        )
        notes = f"{prefix}\n\n{notes}" if notes else prefix

    return CoverSuggestion(
        cover_url=str(cover_url) if cover_url else None,
        source=source,
        notes=notes,
        confidence=confidence,
        raw="",  # No AI raw output to preserve when we took the fallback path.
    )


def _verify_cover_url(url: str, *, timeout: float = 12.0) -> tuple[bool, str]:
    """Probe a candidate cover URL to confirm it serves an image, AND cache
    the bytes so the apply path doesn't have to download them again.

    Returns (ok, reason). On failure, `reason` is a short human-readable
    explanation (HTTP status, content-type mismatch, network error) used to
    surface context to the user via the suggestion's `notes` field.

    Strategy:
      1. Full GET with redirects allowed. We're going to need the bytes
         anyway when the user clicks Apply, and grabbing them now via the
         shared cover_cache means the apply path can skip the second
         download entirely. (The previous HEAD-then-maybe-GET dance saved
         a tiny amount of bandwidth at the cost of double-downloading on
         every successful apply.)
      2. Accept any 2xx with Content-Type starting "image/" — or missing
         Content-Type (some CDNs are lazy; we match what the apply-path
         download accepts).
      3. On image-content responses, populate cover_cache so the apply
         path's call to tags.download_cover hits a warm cache.
    """
    try:
        import requests
    except ImportError:
        # If requests isn't available we can't verify — be permissive rather
        # than blocking the suggestion entirely.
        return True, "requests-not-available"

    from . import cover_cache

    headers = {"User-Agent": _VERIFY_UA, "Accept": "image/*,*/*;q=0.5"}

    try:
        with requests.get(url, timeout=timeout, allow_redirects=True, headers=headers, stream=True) as resp:
            content_type = resp.headers.get("content-type")
            ok, reason = _evaluate_response(resp.status_code, content_type)
            if not ok:
                # Drain a tiny bit so the connection can be reused / closed
                # cleanly, but don't pull the full body for a rejected URL.
                return False, reason
            # Cap how much we'll buffer — Cover Art Archive's "front" can
            # weigh in at ~2-3 MB; an 8 MB ceiling matches cover_cache.
            data = resp.content
            if len(data) > cover_cache.MAX_BYTES_PER_ENTRY:
                return False, f"image too large ({len(data)} bytes)"
            mime = (content_type or "image/jpeg").split(";", 1)[0].strip().lower() or "image/jpeg"
            cover_cache.put(url, data, mime)
            return True, "ok"
    except requests.RequestException as e:
        return False, _short_exception(e)


def _evaluate_response(status: int, content_type: str | None) -> tuple[bool, str]:
    if not (200 <= status < 300):
        return False, f"HTTP {status}"
    if content_type:
        ct = content_type.split(";", 1)[0].strip().lower()
        if ct and not ct.startswith("image/"):
            # text/html on a 200 usually means the URL points to a landing
            # page rather than the raw image — common when AI grabs a
            # Discogs/Wikipedia HTML URL by mistake.
            return False, f"content-type {ct}"
    return True, "ok"


def _short_exception(e: Exception) -> str:
    """Compress a verbose requests exception into something note-friendly."""
    name = type(e).__name__
    msg = str(e).splitlines()[0] if str(e) else ""
    if len(msg) > 80:
        msg = msg[:77] + "…"
    return f"{name}: {msg}" if msg else name
