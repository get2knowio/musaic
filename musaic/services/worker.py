"""AI job worker: per-job dispatch, MBID persistence, and the
dispatcher loop. Driven by the `jobs_changed` event from
`services.sse`.

Failure policy: when a job fails, we set `ai_queue_paused=True` so the
queue stops dispatching new jobs. This is deliberate — most failures
we see are capacity / rate-limit related, and continuing to fire jobs
into a degraded state usually just produces more failures. The user
sees the pause banner on the Jobs page and can Resume after retrying
or waiting.
"""
from __future__ import annotations

import asyncio
import logging
from typing import Any

from .. import cache as cache_module
from .. import jobs as jobs_module
from .. import mb_lookup
from .albums import cached_album_dict, read_tracks
from .sse import jobs_changed, notify_jobs_changed

_log = logging.getLogger(__name__)

# Set by the worker so external consumers can know the worker is alive.
_worker_running = False

# AI-assigned MBID confidence score. We don't have an MB Lucene score for an
# AI-supplied MBID, but we DO know the AI claimed high confidence and we've
# verified the MBID exists in MB. Reference mb_lookup.HIGH_CONFIDENCE_SCORE
# directly so a future threshold bump in mb_lookup auto-updates this path.
_AI_MBID_CONFIDENCE = mb_lookup.HIGH_CONFIDENCE_SCORE

# Wall-clock cap for any single AI SDK call (suggest_metadata or
# suggest_cover). max_turns=20 inside the SDK isn't sufficient —
# observed in production: the Claude subprocess goes to 0% CPU
# blocked on a WebFetch/WebSearch tool call that never returns,
# job sits `running` indefinitely until the user manually kills
# the subprocess. 4 minutes is generous (healthy compilation
# identification with vision input runs ~1-2 min); this cap fires
# only on genuine wedges. On timeout we surface the same
# "subprocess exited" failure that other SDK errors produce, with
# the queue auto-pausing on the first failure (existing behavior).
_AI_CALL_WALL_CLOCK_TIMEOUT_S = 240.0


def queue_state_dict() -> dict[str, Any]:
    """Snapshot of worker / queue settings + counts. Bundled into the
    jobs SSE payload so a single subscriber (`useJobsStream`) gets
    everything in one cache update — no second connection for queue
    state."""
    from .. import settings as settings_module
    return {
        "paused": settings_module.ai_queue_paused(),
        "max_parallel": settings_module.ai_max_parallel(),
        "running": jobs_module.count_running(),
        "queued": jobs_module.count_queued(),
    }


def persist_ai_mbid(*, album_id: str, mbid: str) -> None:
    """Verify an AI-supplied MBID against MusicBrainz and persist the
    anchor on success. Synchronous (called via asyncio.to_thread).

    The verification step is critical: AI responses can
    contain hallucinated UUIDs, and a well-formed-but-fake one would
    poison the cache and cause every downstream MB call for this album
    to point at nonexistent data. We GET the release-group detail
    endpoint before writing — 200 means it's real, anything else means
    we drop the suggestion silently.

    Also refuses MBIDs the user has rejected via the "Wrong match"
    affordance — the AI doesn't know about per-album blocklists, and
    persisting one would silently re-trap the user in the same loop.
    """
    blocked = cache_module.get_blocked_mb_release_groups(album_id)
    if mbid in blocked:
        _log.info(
            "AI returned blocklisted MBID for album=%s mbid=%s; refusing to persist",
            album_id, mbid,
        )
        return
    import requests
    url = mb_lookup.MB_RELEASE_GROUP_DETAIL.format(mbid=mbid)
    try:
        resp = requests.get(
            url,
            params={"fmt": "json"},
            headers={"User-Agent": mb_lookup.USER_AGENT},
            timeout=10,
        )
    except requests.RequestException as e:
        _log.warning(
            "AI MBID verify failed for album=%s mbid=%s: %s",
            album_id, mbid, e,
        )
        # Don't record a failure here — this is a transient network
        # issue with our verification request, not "AI couldn't find
        # it." Leave the album in the not-yet-tried state so the next
        # check run can retry.
        return
    if resp.status_code != 200:
        _log.info(
            "AI returned non-existent MBID for album=%s mbid=%s "
            "(MB returned HTTP %s); ignoring",
            album_id, mbid, resp.status_code,
        )
        # AI gave us a fake/wrong UUID. Treat that as "AI tried,
        # nothing real found" so we don't ask it again next pass.
        try:
            cache_module.record_mb_lookup_failure(album_id)
        except Exception:
            _log.exception(
                "Couldn't record MB lookup failure for album=%s after "
                "MB rejected AI-supplied MBID",
                album_id,
            )
        return
    cache_module.set_album_mb_anchor(
        album_id,
        release_group_mbid=mbid,
        release_mbid=None,  # AI gave us release-group; release-id is unknown
        score=_AI_MBID_CONFIDENCE,
    )
    _log.info(
        "Persisted AI-identified MB anchor for album=%s mbid=%s",
        album_id, mbid,
    )

    # Publish AlbumMbAnchorSet — the _post_scan_rerun subscriber
    # picks up the scoped rerun so the unidentified-album finding
    # clears off-thread, and _resolver_invalidate drops the memo
    # so the next read sees canonical fields.
    try:
        from .. import library_events
        library_events.publish(
            library_events.AlbumMbAnchorSet(album_id=album_id, source="ai"),
        )
    except Exception:
        _log.exception(
            "library_events publish failed for AI anchor on %s",
            album_id,
        )


def _is_no_op_confirmation(result: dict[str, Any]) -> bool:
    """True when an AI job result represents a verified no-op the user
    doesn't need to review: the album was identified on MusicBrainz
    AND there are no album/track/cover changes to apply.

    Used to auto-acknowledge such jobs server-side so they never enter
    the frontend's "needs review" queue. The user just sees the toast
    + the IDENTIFIED badge on the album.

    Gating on `mb_release_group_mbid` is intentional: a no-op result
    WITHOUT identification ("Claude couldn't find this on MB either")
    still warrants user attention — they may want to retry with a hint
    or fix the directory name. Only the IDENTIFIED + nothing-to-apply
    intersection is the no-decision case.

    Tied to the synthesizer's emit shape (only emits suggestions that
    actually differ from current values): for AI-generated results
    that may include same-as-current values, this check will conserva-
    tively NOT auto-ack (suggestions dict is non-empty even when the
    diff would render as "no change"), so the user still gets the
    review UI. That's the safer failure mode.
    """
    meta = result.get("metadata") or {}
    cover = result.get("cover") or {}

    mbid = (meta.get("mb_release_group_mbid") or "").strip()
    if not mbid:
        return False  # Not identified — user still needs to decide.

    album_suggestions = meta.get("suggestions") or {}
    if album_suggestions:
        return False  # Has album-level fields to apply.

    for track in meta.get("tracks") or []:
        if track.get("suggestions"):
            return False  # Has at least one track-level change.

    # Cover-included jobs: a proposed cover URL = something to apply.
    # Metadata-only jobs: cover dict is absent, so this check is a
    # no-op for them.
    if cover.get("cover_url"):
        return False

    return True


def _format_transcript(buffer: list[str], *, max_bytes: int) -> str | None:
    """Join a transcript buffer into a single string, keeping the
    tail when the joined size exceeds `max_bytes`. The tail is more
    informative for debugging since the failure happened at the end.

    Returns None for an empty buffer so `mark_failed` skips writing
    `result_json` entirely.

    Format: one entry per line. The structured `_TranscriptRecorder`
    capture in ai.py already truncates per-block content, so entries
    are short enough that a plain newline is the right separator —
    the transcript reads like a chronological log of text / tool
    calls / tool results, e.g.:

        Searching MusicBrainz for "101 Strings Country Greats"
        → WebSearch  query="'101 Strings Country Greats'"
        ← (4 results: musicbrainz.org/release-group/...)
        → WebFetch  url='https://musicbrainz.org/release-group/...'
        ← ERROR  HTTP 503 Service Unavailable

    This is the load-bearing diagnostic for the Jobs page's failure
    expand — it should make WHICH tool call / URL was last in flight
    immediately obvious.
    """
    if not buffer:
        return None
    joined = "\n".join(piece.strip() for piece in buffer if piece.strip())
    if not joined:
        return None
    encoded = joined.encode("utf-8")
    if len(encoded) <= max_bytes:
        return joined
    truncated = encoded[-max_bytes:].decode("utf-8", errors="replace")
    omitted = len(encoded) - max_bytes
    return f"… [{omitted} bytes truncated] …\n\n{truncated}"


async def run_one_job(job: jobs_module.Job) -> None:
    """Execute a single job and persist its result/error.

    Tier-aware: runs the deterministic resolver first
    (`services.resolver.resolve_album`), then either synthesizes the
    suggestion from MB data (no AI quota burned) or escalates to Claude
    with the resolver output as additional context.
    """
    from .. import ai
    from .. import settings as settings_module
    from . import resolver as resolver_service

    jobs_module.mark_running(job.id)
    notify_jobs_changed()

    # Install a fresh transcript buffer for this job. Anything Claude
    # streams during the call ends up here (assistant text + result
    # message). We pull it back out in the except clause so the user
    # can see why a failed job failed — currently we discard everything
    # but the exception text, which is often just "exit code 1."
    transcript_buffer = ai.install_transcript_buffer()

    try:
        # Re-resolve album from cache (it may have been mutated since enqueue).
        album = cached_album_dict(job.album_id)
        if album is None:
            raise RuntimeError(f"Album {job.album_id!r} no longer in cache")
        tracks = read_tracks(album["path"])

        # Run the deterministic cascade first. Results are memoized;
        # subsequent calls (e.g. a checks rerun for the same album) will
        # hit the in-process cache. Off the loop because mb_lookup +
        # fpcalc are both sync.
        resolution = await asyncio.to_thread(
            resolver_service.resolve_album,
            job.album_id,
            user_hint=job.hint,
        )

        result: dict[str, Any] = {}

        if job.job_type in ("metadata", "metadata+cover"):
            # Skip AI entirely when the resolver gave us a confident
            # anchor with every track aligned. The synthesizer turns the
            # resolution into the same MetadataSuggestion shape AI
            # returns, so downstream code is identical.
            can_skip_ai = (
                resolution.anchor is not None
                and resolution.anchor.confidence >= 0.85
                and resolution.unresolved_track_count == 0
            )
            if can_skip_ai:
                meta = ai.synthesize_metadata_from_resolution(album, tracks, resolution)
                _log.info(
                    "job %s (%s, album=%s): deterministic resolver covered everything; AI skipped",
                    job.id, job.job_type, job.album_id,
                )
            else:
                # Wall-clock cap on the SDK call. `max_turns=20` alone
                # isn't enough: the SDK can sit blocked on a single
                # WebFetch / WebSearch tool call that never returns
                # (seen with MB outages — the Claude subprocess goes
                # to 0% CPU waiting on a network read that's already
                # hung). Without a wall-clock timer, the job stays
                # `running` indefinitely until the user manually
                # kills the subprocess.
                #
                # 4 minutes is generous: healthy compilation
                # identification takes ~1-2 min with cover attached;
                # this cap fires only on genuine wedges. Hitting the
                # cap surfaces the same "subprocess exited" failure
                # message the user already sees on other SDK errors;
                # they can Retry from the AI Jobs page when conditions
                # improve.
                meta = await asyncio.wait_for(
                    ai.suggest_metadata(
                        album, tracks, user_hint=job.hint,
                        resolution=resolution,
                        # v16: per-job opt-out from the cover-attach
                        # path. User-controlled via the hint-dialog
                        # checkbox; defaults True (current behavior).
                        attach_cover=job.attach_cover,
                    ),
                    timeout=_AI_CALL_WALL_CLOCK_TIMEOUT_S,
                )
            result["metadata"] = {
                "suggestions": meta.suggestions,
                "tracks": [
                    {"filename": t.filename, "suggestions": t.suggestions}
                    for t in meta.tracks
                ],
                "notes": meta.notes,
                "confidence": meta.confidence,
                "mb_release_group_mbid": meta.mb_release_group_mbid,
            }
            # Side-effect: if a real MBID came out (AI or synthesizer),
            # persist the anchor. The synthesizer's MBID came from the
            # resolver which already verified it; for AI's, the
            # persist_ai_mbid helper does its own MB GET to reject
            # hallucinated UUIDs.
            if meta.mb_release_group_mbid and not can_skip_ai:
                try:
                    await asyncio.to_thread(
                        persist_ai_mbid,
                        album_id=job.album_id,
                        mbid=meta.mb_release_group_mbid,
                    )
                except Exception:
                    _log.exception(
                        "AI MBID persist failed for job %s (album=%s, mbid=%s)",
                        job.id, job.album_id, meta.mb_release_group_mbid,
                    )
            elif not meta.mb_release_group_mbid:
                # Neither path produced an MBID — record the attempt so
                # missing-mbid health checks back off for the staleness
                # window.
                try:
                    await asyncio.to_thread(
                        cache_module.record_mb_lookup_failure,
                        job.album_id,
                    )
                    _log.info(
                        "No MB anchor found for album=%s (resolver+AI); "
                        "recorded lookup attempt.",
                        job.album_id,
                    )
                except Exception:
                    _log.exception(
                        "Couldn't record MB lookup failure for album=%s",
                        job.album_id,
                    )

        if job.job_type in ("cover", "metadata+cover"):
            # If the resolver already verified a CAA cover, use it
            # directly. Falls back to AI when no CAA cover exists for
            # the anchored release-group, or no anchor is known yet.
            if resolution.cover_url is not None and resolution.cover_url.confidence >= 0.85:
                cover = ai.synthesize_cover_from_resolution(resolution)
                _log.info(
                    "job %s (%s, album=%s): cover from CAA via resolver; AI skipped",
                    job.id, job.job_type, job.album_id,
                )
            else:
                cover = await asyncio.wait_for(
                    ai.suggest_cover(
                        album, tracks, user_hint=job.hint, resolution=resolution,
                    ),
                    timeout=_AI_CALL_WALL_CLOCK_TIMEOUT_S,
                )
            result["cover"] = {
                "cover_url": cover.cover_url,
                "source": cover.source,
                "notes": cover.notes,
                "confidence": cover.confidence,
            }

        jobs_module.mark_completed(job.id, result)
        _log.info("job %s (%s, album=%s) completed", job.id, job.job_type, job.album_id)

        # Auto-acknowledge no-op results so they never enter the
        # "needs review" queue. A job whose result is genuinely a
        # confirmation ("MB identified the album; no tag changes
        # needed; no cover change") gives the user nothing to decide
        # — surfacing it in the review pile just adds noise the user
        # has to dismiss manually. The frontend now sees a job that
        # went straight from `running` → `acted_on(applied)`, and
        # the toast wording can reflect "verified" rather than
        # "ready to review."
        #
        # Narrow gate: ONLY auto-ack when (a) the album was
        # identified AND (b) there's nothing in the result that
        # would require user action. The non-identified no-op case
        # ("no high-confidence corrections AND no MB match either")
        # still needs the user — they might want to try a hint or
        # fix the dir name.
        if _is_no_op_confirmation(result):
            jobs_module.mark_acted_on(job.id, "applied")
            _log.info(
                "job %s auto-acknowledged: no-op confirmation (album=%s)",
                job.id, job.album_id,
            )
    except asyncio.TimeoutError:
        # Wall-clock cap tripped — the SDK call ran longer than
        # _AI_CALL_WALL_CLOCK_TIMEOUT_S. Almost always indicates the
        # Claude subprocess is wedged on a tool call (WebFetch /
        # WebSearch / FireCrawl) that's not coming back. Surface a
        # specific message so the user can distinguish this from a
        # rate-limit failure (different recovery: rate-limits self-
        # resolve after the window; timeouts mean retry once the
        # upstream tool is responsive again).
        mins = int(_AI_CALL_WALL_CLOCK_TIMEOUT_S // 60)
        msg = (
            f"AI call exceeded the {mins}-minute wall-clock cap and was "
            f"cancelled. Almost always means an upstream tool (MusicBrainz "
            f"/ Cover Art Archive / FireCrawl) is slow or down — the Claude "
            f"subprocess sits idle waiting for a tool response that never "
            f"returns. Hit Retry once the affected service is responsive, "
            f"or skip this album and come back to it later."
        )
        _log.warning(
            "job %s (%s, album=%s) wall-clock timeout (%ds); marking failed",
            job.id, job.job_type, job.album_id, int(_AI_CALL_WALL_CLOCK_TIMEOUT_S),
        )
        excerpt = _format_transcript(transcript_buffer, max_bytes=16_000)
        if not excerpt:
            # Buffer is empty → the SDK never streamed a single
            # message in the full window. That's diagnostic in
            # itself: subprocess wedged BEFORE producing any output
            # (initial handshake, Anthropic API not responding, MCP
            # server connect hang, etc.). Surface this explicitly
            # so the chevron in the Jobs UI always appears on
            # timeouts and the user knows "no breadcrumbs" IS the
            # answer — rather than thinking the UI is broken.
            excerpt = (
                f"(no SDK messages received in {mins} minutes — the Claude "
                f"subprocess was completely silent until the wall-clock cap "
                f"fired. Usually this means the subprocess was wedged before "
                f"the first model response: Anthropic API not reachable, "
                f"FireCrawl MCP server failing to connect, or the subprocess "
                f"itself failed to initialize. Check ~/.musaic/server.log "
                f"for any related warnings around the job's start time.)"
            )
        jobs_module.mark_failed(job.id, msg, failure_excerpt=excerpt)
        try:
            settings_module.set_ai_queue_paused(True)
            _log.warning("AI queue paused due to job timeout; user must resume manually")
        except Exception:
            _log.exception("Failed to set ai_queue_paused after job timeout")
        notify_jobs_changed()
        return
    except Exception as e:
        # Most failures are the known opaque CLI exit-1 pattern — the
        # downstream message is already informative; full traceback adds
        # noise. The user sees the failure in the AI Jobs dashboard.
        msg = str(e)
        if "exit code 1" in msg and "Check stderr output" in msg:
            _log.warning("job %s (%s, album=%s) failed — moving on",
                         job.id, job.job_type, job.album_id)
        else:
            _log.exception("job %s (%s, album=%s) failed",
                           job.id, job.job_type, job.album_id)
        # Persist whatever Claude streamed before the failure so the
        # user can see context in the Jobs UI. Cap the size: long agent
        # transcripts can be hundreds of KB and we don't want them in
        # every job-list payload. ~16 KB keeps the last few assistant
        # turns in full while bounding the result_json blob.
        excerpt = _format_transcript(transcript_buffer, max_bytes=16_000)
        jobs_module.mark_failed(job.id, msg, failure_excerpt=excerpt)
        # Auto-pause the queue on first error.
        try:
            settings_module.set_ai_queue_paused(True)
            _log.warning("AI queue paused due to job failure; user must resume manually")
        except Exception:
            _log.exception("Failed to set ai_queue_paused after job failure")
    finally:
        notify_jobs_changed()


async def worker_loop() -> None:
    """Dispatcher loop. Runs up to `ai_max_parallel` jobs concurrently and
    pauses dispatch when `ai_queue_paused=True` (set automatically on any
    job failure; cleared by the user via Resume).
    """
    from .. import settings as settings_module

    global _worker_running
    _worker_running = True

    # Recover any jobs left in 'running' state from a previous unclean
    # shutdown — without this, they'd be invisible (worker only picks up
    # 'queued' rows) and the queue would be effectively stalled.
    try:
        reset = jobs_module.reset_stuck_running()
        if reset > 0:
            _log.info("Reset %d stuck running job(s) to queued on worker startup", reset)
            notify_jobs_changed()
    except Exception:
        _log.exception("Worker startup recovery failed (continuing)")

    in_flight: set[str] = set()

    async def _run_and_track(job: jobs_module.Job) -> None:
        try:
            await run_one_job(job)
        finally:
            in_flight.discard(job.id)
            # Wake the dispatcher — a slot just freed up.
            jobs_changed.set()

    _log.info("AI job worker started")
    try:
        while True:
            try:
                paused = settings_module.ai_queue_paused()
                max_parallel = settings_module.ai_max_parallel()

                if paused:
                    try:
                        await asyncio.wait_for(jobs_changed.wait(), timeout=30.0)
                    except asyncio.TimeoutError:
                        pass
                    jobs_changed.clear()
                    continue

                running = max(jobs_module.count_running(), len(in_flight))
                if running >= max_parallel:
                    try:
                        await asyncio.wait_for(jobs_changed.wait(), timeout=30.0)
                    except asyncio.TimeoutError:
                        pass
                    jobs_changed.clear()
                    continue

                job = jobs_module.get_next_queued_excluding(in_flight)
                if job is None:
                    try:
                        await asyncio.wait_for(jobs_changed.wait(), timeout=30.0)
                    except asyncio.TimeoutError:
                        pass
                    jobs_changed.clear()
                    continue

                in_flight.add(job.id)
                # Fire-and-forget: don't await, so we can immediately re-loop
                # and consider dispatching another concurrent job.
                asyncio.create_task(_run_and_track(job), name=f"ai-job-{job.id}")
                # Yield control briefly so mark_running has a chance to land
                # before our next get_next_queued_excluding read.
                await asyncio.sleep(0)
            except asyncio.CancelledError:
                raise
            except Exception:
                _log.exception("Worker dispatcher iteration failed; sleeping 5s and continuing")
                await asyncio.sleep(5.0)
    except asyncio.CancelledError:
        _log.info("AI job worker stopped")
        raise
    finally:
        _worker_running = False
