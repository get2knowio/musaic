/**
 * When an AI-backed control may be offered.
 *
 * A build can ship with no usable model backend: `GET /api/ai/providers`
 * then lists providers none of which is `ready`. A job that needs a model
 * is certain to fail there (with the not-available message, which does not
 * pause the queue), so the UI does not offer to queue one. Jobs the
 * deterministic resolver covers still go through the queue without calling
 * a model (the worker skips the model when the resolver covers the job), so
 * those stay available.
 *
 * The rule, in one place:
 *
 *  - AI is ready when the provider the worker uses is `ready`: the
 *    provider whose id is `selected.provider_id`, or, when no provider has
 *    that id and exactly one is listed, that one. Unknown (still loading,
 *    or the request failed) counts as not ready.
 *  - A job needs a model unless the resolver covers it, by the worker's own
 *    thresholds: `metadata` is covered when the anchor's confidence is
 *    >= 0.85 and no track is unresolved; `cover` when a Cover Art Archive
 *    cover is verified with confidence >= 0.85; `metadata+cover` when both.
 *  - A job may be queued when AI is ready or the job needs no model.
 *  - Hint entry (the HintDialog), and every album-panel control that only
 *    exists to queue an AI job, is offered only when AI is ready; without a
 *    model the hint has nothing to steer. The album panel's MusicBrainz
 *    corrections need no job at all.
 *  - A health-check fix of kind `ai-job` is always offered. It
 *    tries MusicBrainz first; where it would escalate to an AI job and no
 *    backend exists, the server answers `ok: false` with the not-available
 *    message instead of queueing a job, and the button shows that message.
 *    The hint dialog in front of it is offered only when AI is ready.
 *  - The AI backend settings are replaced by a notice only when no listed
 *    provider is ready at all (a selected-but-not-ready provider keeps the
 *    AI backend settings, so the user can switch to a ready one); the AI
 *    worker card is hidden then too.
 *  - Copy: when AI is ready the original wording stands; when
 *    it is not, no label, tooltip or message names AI or Claude ("AI Jobs"
 *    becomes "Jobs", "no AI quota" and "AI reviewed" go, hints that need a
 *    model are not suggested). The Settings notice above is the one place
 *    that says AI is unavailable.
 */

import type { AiProvidersResponse } from "./api";
import type { AlbumResolution, CheckFixKind, JobType } from "./types";

/** The worker's confidence threshold for skipping the model. */
export const MODEL_FREE_CONFIDENCE = 0.85;

/**
 * The Jobs page's name. Without a ready provider nothing in the UI names AI;
 * the queue still runs the jobs MusicBrainz covers.
 */
export function jobsPageLabel(ai: AiStatus): string {
  return ai.ready ? "AI Jobs" : "Jobs";
}

/** Neutral text shown where AI-only controls would be. */
export const AI_UNAVAILABLE = "AI suggestions are not available in this build.";

export interface AiStatus {
  /** The providers response has arrived. */
  known: boolean;
  /** The provider the worker uses can run a model. */
  ready: boolean;
  /** At least one listed provider can run a model. */
  anyReady: boolean;
}

export function aiStatus(data: AiProvidersResponse | undefined | null): AiStatus {
  if (!data) return { known: false, ready: false, anyReady: false };
  const providers = Array.isArray(data.providers) ? data.providers : [];
  const selectedId = data.selected?.provider_id;
  const used =
    providers.find((p) => p.id === selectedId) ??
    (providers.length === 1 ? providers[0] : undefined);
  return {
    known: true,
    ready: used?.ready === true,
    anyReady: providers.some((p) => p.ready === true),
  };
}

export interface ModelFreeCoverage {
  /** A metadata job would be synthesized from the resolver. */
  metadata: boolean;
  /** A cover job would use the resolver's verified cover. */
  cover: boolean;
}

export function modelFreeCoverage(
  resolution: AlbumResolution | undefined | null,
): ModelFreeCoverage {
  return {
    metadata:
      resolution?.anchor != null &&
      resolution.anchor.confidence >= MODEL_FREE_CONFIDENCE &&
      resolution.unresolved_track_count === 0,
    cover:
      resolution?.cover_url != null &&
      resolution.cover_url.confidence >= MODEL_FREE_CONFIDENCE,
  };
}

export function jobNeedsModel(jobType: JobType, coverage: ModelFreeCoverage): boolean {
  switch (jobType) {
    case "metadata":
      return !coverage.metadata;
    case "cover":
      return !coverage.cover;
    case "metadata+cover":
      return !(coverage.metadata && coverage.cover);
  }
}

export function canQueueJob(
  jobType: JobType,
  coverage: ModelFreeCoverage,
  ai: AiStatus,
): boolean {
  return ai.ready || !jobNeedsModel(jobType, coverage);
}

/**
 * Whether AI-only entry points are offered: the hint dialog, and the album
 * panel's "Ask AI for corrections…" and "Ask AI to find a cover…" that open
 * it. Without a ready backend none is shown; the panel's corrections come
 * from the resolver alone.
 */
export function offersHintEntry(ai: AiStatus): boolean {
  return ai.ready;
}

/**
 * Whether an `ai-job` fix runs MusicBrainz only (no model can run): the
 * button then says so, and a refusal from the server is the not-available
 * message.
 */
export function fixIsMusicBrainzOnly(fixKind: CheckFixKind, ai: AiStatus): boolean {
  return fixKind === "ai-job" && !ai.ready;
}
