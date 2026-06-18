"""AI-powered metadata + cover-art suggestions.

Runs through **airframe** (the vendor-neutral agent runtime) rather than
talking to any one vendor SDK directly, so the user can pick whichever agent
framework they have access to via `settings.ai_provider_id` / `ai_model_id`
(defaults: Claude Sonnet). Authentication follows the selected airframe
adapter's own chain (for Claude: `CLAUDE_CODE_OAUTH_TOKEN` / `ANTHROPIC_API_KEY`
/ the Claude Code credential file).

Two operations are exposed:

  - `suggest_metadata(album, tracks)` — the model inspects the directory name,
    current Vorbis tags, and per-track titles, optionally using the provider's
    hosted web tools (WebSearch + WebFetch on Claude, negotiated via airframe's
    `supported_native_tools`) against MusicBrainz / Discogs / Wikipedia, and
    returns a structured JSON proposal of corrections. Providers that don't
    serve hosted web tools degrade gracefully to the deterministic resolver
    payload already embedded in the prompt.

  - `suggest_cover(album)` — the model proposes a cover-art URL (typically from
    the Cover Art Archive) given the album identity, with rationale.

Each returns a dict matching the `MetadataSuggestion` / `CoverSuggestion`
shapes documented below.
"""
from __future__ import annotations

import asyncio
import base64
import contextvars
import inspect
import json
import logging
import os
import re
import shutil
from dataclasses import dataclass, field
from typing import Any

log = logging.getLogger(__name__)


# Per-call transcript buffer. The worker installs a fresh list before
# running an AI suggestion call; the agent runner appends every assistant
# text block + tool call/result to it as they stream. On failure the
# worker reads the buffer back so the user can see whatever Claude said
# before the call blew up — otherwise that context is lost to the void.
#
# Contextvar (not threadlocal) so the value follows asyncio.to_thread
# hops through the SDK runner correctly. Default `None` = "don't bother
# capturing"; a `[]` = "append to me." Capacity caps live at the
# read site (worker.run_one_job) — the buffer itself is unbounded for
# simplicity, since it's per-call and discarded between jobs.
_transcript_buffer: contextvars.ContextVar[list[str] | None] = contextvars.ContextVar(
    "_ai_transcript_buffer", default=None,
)


def install_transcript_buffer() -> list[str]:
    """Install a fresh transcript buffer for the current call. Returns
    the list — caller reads it back after the AI call completes (or
    fails). Idempotent within a single asyncio task."""
    buf: list[str] = []
    _transcript_buffer.set(buf)
    return buf


def _record_transcript(piece: str) -> None:
    """Append a streamed message to the active transcript buffer if any.
    No-op when no buffer is installed (e.g. tests, direct calls)."""
    buf = _transcript_buffer.get()
    if buf is not None and piece:
        buf.append(piece)


# How much of a single tool-call input or tool-result body to keep in
# the transcript. Tool results can be entire HTML pages (10s of KB);
# transcripts of failed jobs are surfaced in the Jobs UI, so we cap
# per-entry size so the buffer stays scannable. The whole buffer is
# also tail-capped by `services.worker._format_transcript`.
_TRANSCRIPT_BLOCK_MAX_CHARS = 600


def _summarize_tool_input(tool_input: Any) -> str:
    """One-line digest of a tool-use's input args. Examples:
        WebFetch  → 'url=https://musicbrainz.org/release-group/abc'
        WebSearch → 'query="101 Strings Country Greats"'
        mcp__firecrawl__firecrawl_search → 'query="..."'
    Falls back to a JSON dump (truncated) for unknown shapes."""
    if not isinstance(tool_input, dict):
        return _truncate(str(tool_input))
    # Most common shapes — surface the load-bearing arg first.
    for key in ("url", "query", "q", "command", "file_path", "path"):
        if key in tool_input:
            return f'{key}={_truncate(repr(tool_input[key]))}'
    # Generic fallback.
    try:
        return _truncate(json.dumps(tool_input, ensure_ascii=False))
    except Exception:
        return _truncate(str(tool_input))


def _summarize_tool_result(content: Any) -> str:
    """One-line digest of a tool-result's content. SDK content can be
    a string OR a list of content blocks (rich response shape). For
    rich shape we concatenate text-bearing entries."""
    if isinstance(content, str):
        return _truncate(content)
    if isinstance(content, list):
        parts: list[str] = []
        for entry in content:
            if isinstance(entry, dict):
                if entry.get("type") == "text":
                    parts.append(str(entry.get("text") or ""))
                else:
                    parts.append(f"<{entry.get('type', 'block')}>")
            else:
                parts.append(str(entry))
        return _truncate(" ".join(parts).strip()) or "(empty)"
    return _truncate(str(content))


def _truncate(s: str, *, limit: int = _TRANSCRIPT_BLOCK_MAX_CHARS) -> str:
    """Truncate to `limit` chars with a single-line marker; preserves
    leading and trailing context (the diagnostic value of a tool
    result usually lives in either the request shape OR the failure
    message at the end)."""
    s = (s or "").strip().replace("\n", " ")
    if len(s) <= limit:
        return s
    head = limit // 2 - 10
    tail = limit - head - 10
    return f"{s[:head]} … [{len(s) - limit} ch] … {s[-tail:]}"


def _summarize_tool_args(preview: Any) -> str:
    """Digest an airframe `ToolCallStart.arguments_preview` (a partial JSON
    string) the same way `_summarize_tool_input` digests a dict — surface the
    load-bearing arg (url=/query=…). Falls back to a truncated raw string when
    the preview isn't parseable (it can be incomplete mid-stream)."""
    if isinstance(preview, str):
        try:
            parsed = json.loads(preview)
        except Exception:
            return _truncate(preview)
        return _summarize_tool_input(parsed)
    return _summarize_tool_input(preview)


class _TranscriptRecorder:
    """Accumulate airframe stream events into transcript lines.

    Same diagnostic surface the old per-SDK-message recorder produced, adapted
    to airframe's `RuntimeEvent` stream. Distinguishes:
      - text:        assistant prose (streamed as `TextDelta`s, flushed as one
                     line per contiguous run)
      - reasoning:   the model's extended-thinking trace (`ReasoningDelta`)
      - tool call:   `→ {name} {args}` — `ToolCallStart`
      - tool result: `← {summary}` / `← ERROR …` — `ToolCallResult`
      - result:      `[final] …` — the trailing `TurnComplete`

    This is the load-bearing diagnostic for failed / timed-out jobs: when a job
    hangs on a stuck WebFetch, the tool-call line recorded the instant the call
    started makes it obvious WHICH URL the model was last waiting on — even
    though no `TurnComplete` ever arrives. Text / reasoning deltas are buffered
    and flushed at each tool-call boundary (and at the end) so the transcript
    reads like a chrono log instead of token confetti.
    """

    def __init__(self) -> None:
        self._text: list[str] = []
        self._reasoning: list[str] = []

    def feed(self, event: Any) -> None:
        from airframe import (
            ReasoningDelta,
            TextDelta,
            ToolCallResult,
            ToolCallStart,
            TurnComplete,
        )

        if isinstance(event, TextDelta):
            self._text.append(event.text)
        elif isinstance(event, ReasoningDelta):
            self._reasoning.append(event.text)
        elif isinstance(event, ToolCallStart):
            self._flush()
            _record_transcript(f"→ {event.tool_name}  {_summarize_tool_args(event.arguments_preview)}")
        elif isinstance(event, ToolCallResult):
            self._flush()
            arrow = "← ERROR" if event.is_error else "←"
            _record_transcript(f"{arrow}  {_summarize_tool_result(event.output)}")
        elif isinstance(event, TurnComplete):
            self._flush()
            if event.result.text:
                _record_transcript(f"[final] {_truncate(event.result.text)}")

    def _flush(self) -> None:
        if self._reasoning:
            txt = "".join(self._reasoning).strip()
            if txt:
                _record_transcript(f"💭 {_truncate(txt)}")
            self._reasoning.clear()
        if self._text:
            txt = "".join(self._text).strip()
            if txt:
                _record_transcript(_truncate(txt))
            self._text.clear()

    def finish(self) -> None:
        """Flush any buffered text/reasoning that didn't precede a tool call
        or turn-complete (e.g. a stream cut short by cancellation)."""
        self._flush()

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
# agent call and expose its tools to the model. The system prompt tells the
# model to treat FireCrawl as a last resort — try the provider's hosted
# WebSearch / WebFetch first, only reach for FireCrawl when those can't get
# past JS-rendered pages, paywalls, or anti-bot blocks. Each FireCrawl call
# burns paid quota.
#
# airframe auto-allows every tool an attached MCP server exposes
# (`mcp__firecrawl__*`), so we no longer enumerate the FireCrawl tool names
# for an allowlist — we just attach the server. We still don't want the model
# leaning on FireCrawl's expensive multi-page crawl, so the prompt hint names
# the `search` / `scrape` / `extract` tools as the intended ones.

# Hosted FireCrawl MCP. The API key is part of the URL path (FireCrawl's
# convention as of 2026 — no auth headers, no env vars). Using the hosted
# endpoint means we don't have to manage an `npx firecrawl-mcp` subprocess
# or its node_modules — airframe just opens an SSE MCP connection.
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


def _build_agent_extras(system_prompt: str) -> tuple[str, list[Any]]:
    """Return (system_prompt, mcp_servers) for an airframe agent call.

    When FireCrawl is configured, this:
      - appends the "WEB ACCESS LADDER" guidance to the system prompt,
      - returns a one-element list holding an airframe `McpServerRef` pointing
        at the hosted SSE endpoint with the user's API key in the URL.

    When FireCrawl is NOT configured, returns the unmodified system prompt and
    an empty list — nothing changes for users who never set the key.

    The provider's own hosted web tools (Claude's WebSearch / WebFetch) aren't
    decided here: the runner negotiates them against
    `runtime.supported_native_tools()` so a non-Claude provider that doesn't
    serve them degrades cleanly instead of erroring.

    The settings lookup happens once per call (a single SQLite SELECT) so
    toggling the key in Settings takes effect on the next AI call without a
    server restart.
    """
    from airframe import McpServerRef

    from . import settings as settings_module

    fc_key = settings_module.firecrawl_api_key()
    if not fc_key:
        return system_prompt, []

    return (
        system_prompt + _FIRECRAWL_PROMPT_HINT,
        [
            McpServerRef(
                name="firecrawl",
                transport="sse",
                url=_FIRECRAWL_MCP_URL_TMPL.format(api_key=fc_key),
            ),
        ],
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
  - tracks: [{ filename, tracknumber, title, artist, duration,
              fingerprint?: { recording_mbid, score, mb_canonical_title?,
                              mb_canonical_artist? } }, …]
  - anchor (optional): { release_group_mbid, strategy, confidence, notes }
                       — present when Musaic already identified the release
                       via deterministic means (cache, MB search, or
                       AcoustID fingerprint vote). When non-null, DO NOT
                       redo identification — trust this MBID and focus
                       your effort on the per-track gaps.
  - resolver_canonical (optional): { title, album_artist, year,
                                     album_artist_sort } — what MB says
                                     about the anchored release. Use these
                                     verbatim for the matching album-level
                                     suggestions when current_tags differ.
  - unresolved_filenames (optional): [filename, …] — tracks the
                                     deterministic resolver couldn't pin to
                                     a canonical recording (no fingerprint
                                     match or alignment ambiguity). These
                                     are exactly the per-track entries
                                     where your effort is most valuable.

The user may also include a <hint> block. Treat its content as high-confidence
ground truth supplied by the user. If you see strong evidence the hint is wrong
(e.g. the tracklist clearly identifies a different album), call that out in
`notes` rather than silently overriding it.

COVER ART (when present)
========================

The user message may include the album's cover art as an attached image.
When present, treat it as a HIGH-CONFIDENCE signal for identification —
album/artist text rendered on a cover is almost always the canonical
form, which beats drifted Vorbis tags and cryptic directory names. Use
it especially for:

  - Compilations / soundtracks where text metadata is sparse but the
    cover shows the series/film/era clearly.
  - Box sets and re-issues where multiple releases share text metadata
    but differ visually (edition, year, label).
  - Foreign-language releases where text transliteration breaks matching
    but the cover often shows a Latin-alphabet form.
  - Confirming an existing anchor: if the anchor's release-group looks
    visually consistent with the attached cover, you can return high
    confidence with no album-level changes.

When the cover and the tags disagree, the cover is usually right. When
the cover and an explicit user `<hint>` disagree, the hint wins (the
user may have moved files into a placeholder cover and is asking you
to retag).

PRIOR DETERMINISTIC RESOLUTION
==============================

When `anchor` is present, the deterministic tier already did the
identification work. Your job changes:

  - DO NOT search MB for the album — the release-group is already known.
    Echo `anchor.release_group_mbid` back as `mb_release_group_mbid`.
  - For album-level fields, propose corrections using `resolver_canonical`
    when current_tags disagree. No web lookups needed for those.
  - For per-track fields, use each track's `fingerprint.mb_canonical_*`
    when present (those are AcoustID-resolved and authoritative for that
    file). Focus your MB / web-lookup effort on `unresolved_filenames` —
    those are tracks the deterministic pass couldn't match (covers,
    bonus tracks, mistitled rips, etc.).

When `anchor` is absent, fall back to the full identification cascade
below.

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


def _album_cover_for_prompt(album_dict: dict[str, Any]) -> tuple[str, str] | None:
    """Return `(base64_data, media_type)` for the album's cover, or None
    when no cover is available.

    Strategy (cheapest first):
      1. Pre-extracted 384px thumb at `~/.musaic/thumbs/{id}.jpg` —
         already JPEG, already sized for low token cost. Free.
      2. Source image (on-disk cover_file OR embedded FLAC picture)
         resized to 384px via the existing `make_thumbnail` helper.

    The 384px size is intentional — Claude's vision API tokenizes by
    image area, and 384×384 ≈ 850 input tokens is a reasonable tax
    for the identification lift. Album covers are predominantly text +
    a recognizable image; resolution beyond 384 doesn't materially
    help identification while doubling/tripling token cost.

    Returns None silently when no cover exists; callers should fall
    back to the text-only prompt shape.
    """
    from . import thumbs as thumbs_module

    album_id = album_dict.get("id")
    if not album_id:
        return None

    # Path 1: pre-extracted thumb (most common — the scan service
    # populates these for every album with cover art).
    thumb_p = thumbs_module.thumb_path(album_id)
    if thumb_p.is_file():
        try:
            data = thumb_p.read_bytes()
            return base64.standard_b64encode(data).decode("ascii"), "image/jpeg"
        except OSError as e:
            log.warning("ai cover-attach: thumb read failed for %s: %s", album_id, e)

    # Path 2: source image (on-disk cover_file OR embedded picture)
    # resized down. Doing this on demand only when the thumb is missing —
    # which is rare in practice (the scan service warms thumbs for every
    # album with cover art).
    source = thumbs_module.get_source_image(album_dict)
    if source is None:
        return None
    raw_bytes, _src_mime = source
    resized = thumbs_module.make_thumbnail(raw_bytes)
    if resized is None:
        return None
    return base64.standard_b64encode(resized).decode("ascii"), "image/jpeg"


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

def _album_payload(
    album_dict: dict[str, Any],
    tracks: list[dict[str, Any]],
    resolution: Any = None,
) -> dict[str, Any]:
    # `directory.matched=False` if the dir name didn't fit the strict
    # `Artist - Album [FLAC]` convention; the prompt tells Claude to fall
    # back to the raw name + tracks in that case.
    payload = {
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

    # Resolver enrichment — only included when the worker ran the
    # deterministic cascade and is asking AI for help with the
    # leftovers. When present, the system prompt tells Claude to trust
    # the anchor (no re-identification work) and focus on the
    # `unresolved_filenames`.
    if resolution is None:
        return payload

    fp_by_filename: dict[str, dict[str, Any]] = {}
    for tr in resolution.tracks:
        if tr.recording_mbid:
            entry: dict[str, Any] = {
                "recording_mbid": tr.recording_mbid,
                "score": tr.fingerprint_score,
            }
            if tr.canonical_title:
                entry["mb_canonical_title"] = tr.canonical_title.value
            if tr.canonical_artist:
                entry["mb_canonical_artist"] = tr.canonical_artist.value
            fp_by_filename[tr.filename] = entry

    unresolved: list[str] = []
    for tr in resolution.tracks:
        if tr.canonical_title is None:
            unresolved.append(tr.filename)

    payload["anchor"] = (
        {
            "release_group_mbid": resolution.anchor.value,
            "strategy": resolution.anchor_strategy,
            "confidence": resolution.anchor.confidence,
            "notes": resolution.notes,
        }
        if resolution.anchor is not None
        else None
    )
    payload["resolver_canonical"] = {
        "title": resolution.canonical_title.value if resolution.canonical_title else None,
        "album_artist": resolution.canonical_album_artist.value if resolution.canonical_album_artist else None,
        "year": resolution.canonical_year.value if resolution.canonical_year else None,
        "album_artist_sort": resolution.canonical_album_artist_sort.value if resolution.canonical_album_artist_sort else None,
    }
    if fp_by_filename:
        for entry in payload["tracks"]:
            fp = fp_by_filename.get(entry["filename"])
            if fp is not None:
                entry["fingerprint"] = fp
    if unresolved:
        payload["unresolved_filenames"] = unresolved

    return payload


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


# ---------- Agent runner (airframe) ----------

def runtime_accepts_api_key(provider_id: str) -> bool:
    """Whether the airframe adapter for `provider_id` takes an `api_key=`
    constructor arg. True for key-based providers (Claude / Kimi / the
    OpenAI-compatible gateways); False for ones that authenticate via their
    own environment / CLI chain (Copilot via `gh`, Bedrock via the AWS chain,
    the OpenCode server via loopback). Drives whether the settings UI offers a
    key field or an auth hint. Returns False if the provider id is unknown."""
    from airframe import runtime_for

    try:
        cls = runtime_for(provider_id)
    except Exception:
        return False
    try:
        return "api_key" in inspect.signature(cls).parameters
    except (TypeError, ValueError):  # pragma: no cover — builtin/uninspectable
        return False


# ---------- Provider runtime readiness (issue #8, Phase 5) ----------
#
# Some airframe adapters shell out to an external CLI: the Claude adapter
# (claude-agent-sdk) spawns the `claude` Node CLI; github-copilot's uses `gh`.
# In a packaged desktop app that CLI may simply not be installed — and a
# GUI-launched macOS app doesn't inherit the shell PATH, so even an installed
# one can be invisible (the Electron shell augments PATH to mitigate that).
# Rather than let the worker crash opaquely, we detect the prerequisite up
# front and surface an actionable message; the deterministic MusicBrainz
# resolver keeps working regardless, so AI being unavailable degrades the app
# instead of breaking it.

_PROVIDER_CLI: dict[str, str] = {
    "claude": "claude",
    "github-copilot": "gh",
}

_CLI_REMEDY: dict[str, str] = {
    "claude": (
        "Install the Claude CLI (`npm i -g @anthropic-ai/claude-code`) and make "
        "sure it's on PATH, or switch to an API-key provider in Settings. "
        "Deterministic MusicBrainz matching keeps working without it."
    ),
    "gh": (
        "Install the GitHub CLI and run `gh auth login`, or switch providers. "
        "Deterministic MusicBrainz matching keeps working without it."
    ),
}


def _resolve_cli(cmd: str) -> str | None:
    """Find an external CLI the way the desktop bundle will: an explicit
    ``MUSAIC_<CMD>_CLI`` override first (so a future bundled CLI can be pinned),
    then ``PATH``."""
    override = os.environ.get(f"MUSAIC_{cmd.upper()}_CLI")
    if override and os.path.isfile(override) and os.access(override, os.X_OK):
        return override
    return shutil.which(cmd)


def provider_runtime_status(provider_id: str) -> dict[str, Any]:
    """Whether the selected provider's external prerequisites are satisfiable.

    Returns ``{ready, requirement, remedy}``. ``requirement``/``remedy`` are
    ``None`` when ready. Only the CLI-backed providers are probed; key- and
    SDK-chain providers report ``ready=True`` because their actual auth is
    verified at call time (a missing key surfaces as a ``RuntimeAuthError`` with
    its own guidance)."""
    cli = _PROVIDER_CLI.get(provider_id)
    if cli is None:
        return {"ready": True, "requirement": None, "remedy": None}
    if _resolve_cli(cli) is not None:
        return {"ready": True, "requirement": f"the `{cli}` CLI", "remedy": None}
    return {
        "ready": False,
        "requirement": f"the `{cli}` CLI",
        "remedy": _CLI_REMEDY.get(cli, f"Install the `{cli}` CLI or switch providers."),
    }


def _build_runtime(provider_id: str, api_key: str) -> Any:
    """Construct the airframe runtime for `provider_id`, injecting the user's
    `api_key` only when the adapter accepts one. Adapters with env/CLI auth
    chains are constructed bare and resolve their own credentials — so a
    stored key never gets force-fed to an adapter that wouldn't understand it.
    """
    from airframe import runtime_for

    cls = runtime_for(provider_id)
    if api_key and runtime_accepts_api_key(provider_id):
        return cls(api_key=api_key)
    return cls()


# Default max model sub-turns per call. With the async job queue the user
# isn't blocked on us, so we give hard-to-identify albums generous headroom;
# the model terminates as soon as it has an answer, so this is a safety net
# for pathological cases, not a budget.
_AGENT_MAX_TURNS = 20


async def _run_agent(
    prompt: Any,
    *,
    system_prompt: str,
    mcp_servers: list[Any],
) -> str:
    """Run one airframe agent turn and return the assistant's final text.

    Runs the actual call in a separate thread with a fresh asyncio event loop.
    This isolates the agent's subprocess management (the Claude adapter spawns
    the `claude` CLI) from uvicorn's long-running event loop — a direct
    `await` inside the FastAPI handler can make the CLI subprocess exit 1
    partway through long multi-turn calls (signal/pipe-handling interaction).
    A fresh `asyncio.run(...)` works fine, hence the isolation. The cost is one
    OS thread per AI call — a non-issue at the per-album request rate.

    Failure policy: any agent exception is surfaced to the caller. airframe
    classifies failures into typed errors (`RuntimeTransientError`,
    `RuntimeAuthError`, …) so we can give an actionable message; the job-queue
    layer auto-pauses on failure and offers a manual Retry.
    """

    def _run_in_thread() -> str:
        return asyncio.run(
            _agent_async(prompt, system_prompt=system_prompt, mcp_servers=mcp_servers)
        )

    try:
        return await asyncio.to_thread(_run_in_thread)
    except Exception as exc:  # noqa: BLE001 — re-raised below with context
        raise _friendly_agent_error(exc) from exc


def _friendly_agent_error(exc: Exception) -> Exception:
    """Map an airframe error to an actionable exception for the Jobs UI.

    Returns the exception to raise (caller does `raise … from exc`). Transient
    failures (rate-limit / brief outage) and auth failures get specific
    guidance; everything else is surfaced verbatim with its cause appended.
    """
    from airframe import RuntimeAuthError, RuntimeTransientError

    from . import settings as settings_module

    provider = settings_module.ai_provider_id()

    if isinstance(exc, RuntimeTransientError):
        log.warning("agent call transient failure (provider=%s): %s", provider, exc)
        log.debug("full traceback for transient agent failure", exc_info=exc)
        return RuntimeTransientError(
            "The agent backend reported a transient failure — most often a "
            "rate-limit hit (subscriptions have shared rolling windows), a "
            "brief upstream outage, or a model/tool incompatibility. Hit Retry "
            "on the AI Jobs page after a moment. To triage, run "
            "`.venv/bin/python diagnose_claude.py <album_id>`."
        )
    if isinstance(exc, RuntimeAuthError):
        log.warning("agent call auth failure (provider=%s): %s", provider, exc)
        return RuntimeAuthError(
            f"The agent backend ({provider!r}) rejected the credentials. Check "
            f"the auth for your selected provider (for Claude: "
            f"CLAUDE_CODE_OAUTH_TOKEN / ANTHROPIC_API_KEY, or the Claude Code "
            f"credential file)."
        )

    log.exception("agent call failed (provider=%s)", provider)
    cause = getattr(exc, "__cause__", None)
    if cause:
        return type(exc)(f"{exc} (caused by {type(cause).__name__}: {cause})")
    return exc


async def _agent_async(
    prompt: Any,
    *,
    system_prompt: str,
    mcp_servers: list[Any],
) -> str:
    """The actual airframe iteration. Lives in its own coroutine so we can pass
    it to `asyncio.run()` cleanly inside the worker thread.

    `prompt` accepts either a string (text-only) or an airframe `Prompt` list
    (interleaved text + `ImageInput`), used by `suggest_metadata` to attach the
    album's cover art when available — see `_album_cover_for_prompt`.

    The provider/model come from settings; hosted web tools are negotiated
    against the runtime (`supported_native_tools`) so a provider without them
    degrades to the resolver payload already embedded in the prompt rather than
    erroring. Streamed events feed `_TranscriptRecorder` for the failure-
    excerpt breadcrumbs, and the trailing `TurnComplete` carries the final
    text.
    """
    from airframe import (
        NativeCapability,
        NativeTool,
        ProviderModel,
        TextDelta,
        TurnComplete,
    )

    from . import settings as settings_module

    provider_id = settings_module.ai_provider_id()

    # Preflight: if the provider needs an external CLI that isn't present, fail
    # with the remedy instead of letting the adapter crash opaquely partway
    # through. The resolver-first worker only reaches here when AI is actually
    # needed, so this never fires for albums the deterministic tier covered.
    status = provider_runtime_status(provider_id)
    if not status["ready"]:
        raise RuntimeError(
            f"AI provider {provider_id!r} isn't ready — it needs {status['requirement']}. "
            f"{status['remedy']}"
        )

    model_id = settings_module.ai_model_id()
    runtime = _build_runtime(provider_id, settings_module.ai_api_key())
    model = ProviderModel(provider_id, model_id)

    # Negotiate the provider's hosted web tools. Claude serves WEB_SEARCH +
    # WEB_FETCH; providers that don't get an empty list (resolver-only).
    served = runtime.supported_native_tools(model)
    native_tools = [
        tool
        for tool, cap in (
            (NativeTool.web_search(), NativeCapability.WEB_SEARCH),
            (NativeTool.web_fetch(), NativeCapability.WEB_FETCH),
        )
        if cap in served
    ]

    recorder = _TranscriptRecorder()
    text_acc: list[str] = []
    final_text = ""

    session = runtime.session(
        system=system_prompt,
        model=model,
        native_tools=native_tools or None,
        mcp_servers=mcp_servers or None,
    )
    try:
        async for event in session.stream(prompt, max_turns=_AGENT_MAX_TURNS):
            recorder.feed(event)
            if isinstance(event, TextDelta):
                text_acc.append(event.text)
            elif isinstance(event, TurnComplete):
                final_text = event.result.text or final_text
    finally:
        recorder.finish()
        await session.close()
        await runtime.close()

    # Prefer the canonical final text; fall back to accumulated deltas when no
    # TurnComplete arrived (rare — early subprocess exit / cancellation).
    return final_text or "".join(text_acc)


# ---------- Public API ----------

# Override hook for testing. If set to a callable taking (kind, prompt, payload),
# it's used instead of calling the SDK. Returns the raw model text.
_sdk_override = None  # type: ignore


def set_sdk_override(fn) -> None:
    """Install a fake SDK runner. Used by tests."""
    global _sdk_override
    _sdk_override = fn


async def suggest_metadata(
    album_dict: dict[str, Any],
    tracks: list[dict[str, Any]],
    user_hint: str = "",
    resolution: Any = None,
    attach_cover: bool = True,
) -> MetadataSuggestion:
    """Ask Claude for high-confidence metadata corrections.

    `user_hint` is optional free-form context the user provided in the UI
    (e.g. "this is a 2012 reissue, the original is 1979"). It's wrapped in
    a <hint> block in the user message, where the system prompt instructs
    the model to treat it as ground truth.

    `resolution` (optional `services.resolver.AlbumResolution`) is the
    output of the deterministic tiered resolver. When supplied, the
    payload is enriched with the anchor + per-track fingerprint matches +
    list of unresolved filenames; the system prompt knows how to use
    them. The worker should run the resolver first and only call this
    when AI is actually needed (anchor missing or per-track gaps remain).

    `attach_cover` (default True) controls whether the album's cover
    art is sent as an image content block with the prompt. The
    cover-attach pattern usually boosts identification quality for
    compilations / soundtracks / foreign-language releases, but
    multimodal calls have higher per-turn latency. Setting False
    forces the text-only path; useful for A/B testing or when
    debugging job-reliability issues.
    """
    payload = _album_payload(album_dict, tracks, resolution=resolution)
    user_message = _format_user_message(payload, user_hint=user_hint)

    if _sdk_override:
        raw = await _sdk_override("metadata", user_message, payload)
    else:
        # The bulky instruction template lives in _SYSTEM_PROMPT_METADATA;
        # airframe's adapter auto-caches the system prompt + tool definitions,
        # so we pay full input rate only on the dynamic per-call user message
        # (~150 tokens), not the ~1.2k-token template. _build_agent_extras
        # layers the FireCrawl MCP server + the "last resort" prompt hint on
        # top when the user has configured a FireCrawl key.
        sys_prompt, mcp_servers = _build_agent_extras(_SYSTEM_PROMPT_METADATA)

        # Attach the album's cover art when we have one AND the
        # caller didn't opt out. Cover often carries the canonical
        # identity (artist + title rendered in human-readable form)
        # when tags have drifted or the dir name is unhelpful —
        # particularly valuable for compilations, soundtracks,
        # foreign-language releases, and box sets.
        # `_album_cover_for_prompt` uses the pre-extracted 384px thumb
        # when available (no extra IO), falls back to resizing the
        # source image. Returns None for coverless albums; in that
        # case (or when attach_cover=False) we pass the bare text
        # prompt — no behavior change vs the pre-cover-attach path.
        cover = _album_cover_for_prompt(album_dict) if attach_cover else None
        if cover is not None:
            from airframe import ImageInput

            b64, mime = cover
            agent_prompt: Any = [
                ImageInput(bytes_=base64.b64decode(b64), media_type=mime),
                user_message,
            ]
        else:
            agent_prompt = user_message

        raw = await _run_agent(
            agent_prompt,
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


# ---------- Resolver → suggestion synthesizers ----------
#
# Used by the worker to skip the AI call entirely when the deterministic
# resolver gave us everything: high-confidence anchor + 0 unresolved
# tracks. Output shape matches the AI response so downstream code (the
# apply pipeline, the UI's suggestion display) is identical regardless
# of whether the suggestions came from Claude or from MusicBrainz.


def synthesize_metadata_from_resolution(
    album_dict: dict[str, Any],
    tracks: list[dict[str, Any]],
    resolution: Any,
) -> MetadataSuggestion:
    """Build a MetadataSuggestion from an AlbumResolution by diffing
    canonical fields against current_tags. Includes only fields that
    would actually change."""
    suggestions: dict[str, str] = {}

    def diff(field: str, current_key: str, fact: Any) -> None:
        if fact is None:
            return
        proposed = str(fact.value).strip()
        current = str(album_dict.get(current_key) or "").strip()
        if proposed and proposed != current:
            suggestions[field] = proposed

    diff("title", "title", resolution.canonical_title)
    diff("album_artist", "album_artist", resolution.canonical_album_artist)
    # `artist` mirrors album_artist when MB only gave us the album-level credit.
    diff("artist", "artist", resolution.canonical_album_artist)
    diff("year", "year", resolution.canonical_year)
    diff("genre", "genre", resolution.canonical_genre)
    diff("album_artist_sort", "album_artist_sort", resolution.canonical_album_artist_sort)

    track_suggestions: list[TrackSuggestion] = []
    by_filename = {t.get("filename"): t for t in tracks}
    for tr in resolution.tracks:
        ft = by_filename.get(tr.filename) or {}
        cleaned: dict[str, str] = {}

        def t_diff(field: str, current_key: str, fact: Any) -> None:
            if fact is None:
                return
            proposed = str(fact.value).strip()
            current = str(ft.get(current_key) or "").strip()
            if proposed and proposed != current:
                cleaned[field] = proposed

        t_diff("title", "title", tr.canonical_title)
        t_diff("artist", "artist", tr.canonical_artist)
        t_diff("tracknumber", "tracknumber", tr.canonical_tracknumber)
        if cleaned:
            track_suggestions.append(TrackSuggestion(filename=tr.filename, suggestions=cleaned))

    return MetadataSuggestion(
        suggestions=suggestions,
        tracks=track_suggestions,
        notes="Derived from MusicBrainz; no AI call needed.",
        confidence="high",
        raw="",
        mb_release_group_mbid=resolution.anchor.value if resolution.anchor else None,
    )


def synthesize_cover_from_resolution(resolution: Any) -> CoverSuggestion:
    """Build a CoverSuggestion from an AlbumResolution. Returns
    cover_url=None when the resolver didn't find a CAA URL — caller
    should escalate to AI in that case rather than ship a blank
    CoverSuggestion."""
    if resolution.cover_url is None:
        return CoverSuggestion(
            cover_url=None,
            source="MusicBrainz / Cover Art Archive",
            notes="No cover in CAA for the anchored release-group.",
            confidence="low",
        )
    return CoverSuggestion(
        cover_url=str(resolution.cover_url.value),
        source="Cover Art Archive (anchored release-group)",
        notes="Derived from MusicBrainz; no AI call needed.",
        confidence="high",
    )


async def suggest_cover(
    album_dict: dict[str, Any],
    tracks: list[dict[str, Any]],
    user_hint: str = "",
    resolution: Any = None,
) -> CoverSuggestion:
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
    payload = _album_payload(album_dict, tracks, resolution=resolution)
    user_message = _format_user_message(payload, user_hint=user_hint)

    try:
        if _sdk_override:
            raw = await _sdk_override("cover", user_message, payload)
        else:
            # Model choice matters for tool-using calls: diagnose_claude.py
            # test 5 showed Haiku + WebSearch/WebFetch fails consistently with
            # "exit code 1" inside the Claude agent loop (a Claude Code-side
            # Haiku/tool-calling incompatibility in multi-turn calls); the
            # default model id is Sonnet, which is proven in the same combo.
            #
            # Caching note (matches suggest_metadata): the bulky lookup-strategy
            # template lives in _SYSTEM_PROMPT_COVER (auto-cached by the
            # adapter); the per-call user message is just the tagged album
            # payload + optional hint, so the dynamic portion stays minimal.
            # _build_agent_extras layers the FireCrawl MCP + "last resort"
            # prompt hint on top when the user has configured a FireCrawl key.
            sys_prompt, mcp_servers = _build_agent_extras(_SYSTEM_PROMPT_COVER)
            raw = await _run_agent(
                user_message,
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
