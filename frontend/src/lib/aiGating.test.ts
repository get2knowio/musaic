import { describe, it, expect } from "vitest";
import {
  aiStatus,
  canQueueJob,
  fixIsMusicBrainzOnly,
  jobNeedsModel,
  metadataJobType,
  modelFreeCoverage,
  offersHintEntry,
  type AiStatus,
} from "@/lib/aiGating";
import type { AiProvidersResponse } from "@/lib/api";
import type { AlbumResolution } from "@/lib/types";

// The body the no-model build answers for GET /api/ai/providers
// (rust/crates/musaic-api/src/routes/jobs.rs ai_providers + the
// Unconfigured provider in musaic-ai/src/provider.rs): the stored
// selection still says "claude", the one listed provider is not ready.
const UNCONFIGURED: AiProvidersResponse = {
  selected: { provider_id: "claude", model_id: "claude-sonnet-4-6", api_key_set: false },
  providers: [
    {
      id: "unconfigured",
      accepts_api_key: false,
      auth_hint: "This build has no AI backend configured.",
      ready: false,
      requirement: "an AI backend",
      remedy: "This build has no AI backend configured.",
    },
  ],
  uninstalled: [],
};

const provider = (id: string, ready: boolean) => ({
  id,
  accepts_api_key: true,
  auth_hint: null,
  ready,
  requirement: null,
  remedy: null,
});

const READY: AiStatus = { known: true, ready: true, anyReady: true };
const NOT_READY: AiStatus = { known: true, ready: false, anyReady: false };

function resolution(over: {
  anchor?: number | null;
  cover?: number | null;
  unresolved?: number;
}): AlbumResolution {
  const fact = (c: number) => ({ value: "x", source: "musicbrainz" as const, confidence: c });
  return {
    anchor: over.anchor == null ? null : fact(over.anchor),
    cover_url: over.cover == null ? null : fact(over.cover),
    unresolved_track_count: over.unresolved ?? 0,
  } as unknown as AlbumResolution;
}

describe("aiStatus", () => {
  it("is not ready for the no-model build's providers answer", () => {
    expect(aiStatus(UNCONFIGURED)).toEqual({ known: true, ready: false, anyReady: false });
  });

  it("is unknown (and not ready) before the answer arrives", () => {
    expect(aiStatus(undefined)).toEqual({ known: false, ready: false, anyReady: false });
  });

  it("follows the selected provider when it is listed", () => {
    const data: AiProvidersResponse = {
      selected: { provider_id: "copilot", model_id: "m", api_key_set: false },
      providers: [provider("claude", true), provider("copilot", false)],
      uninstalled: [],
    };
    // The worker would use copilot, which can't run: not ready, but the
    // settings keep the provider picker (another provider is ready).
    expect(aiStatus(data)).toEqual({ known: true, ready: false, anyReady: true });
  });

  it("is ready when the selected provider is ready", () => {
    const data: AiProvidersResponse = {
      selected: { provider_id: "claude", model_id: "m", api_key_set: true },
      providers: [provider("claude", true)],
      uninstalled: [],
    };
    expect(aiStatus(data).ready).toBe(true);
  });

  it("uses the only provider when the selection names none of them", () => {
    const data: AiProvidersResponse = {
      selected: { provider_id: "claude", model_id: "m", api_key_set: false },
      providers: [provider("only", true)],
      uninstalled: [],
    };
    expect(aiStatus(data).ready).toBe(true);
  });

  it("is not ready when the selection names none of several providers", () => {
    const data: AiProvidersResponse = {
      selected: { provider_id: "gone", model_id: "m", api_key_set: false },
      providers: [provider("a", true), provider("b", true)],
      uninstalled: [],
    };
    expect(aiStatus(data)).toEqual({ known: true, ready: false, anyReady: true });
  });
});

describe("modelFreeCoverage (the worker's thresholds)", () => {
  it("covers metadata at anchor confidence 0.85 with every track resolved", () => {
    expect(modelFreeCoverage(resolution({ anchor: 0.85 })).metadata).toBe(true);
    expect(modelFreeCoverage(resolution({ anchor: 0.84 })).metadata).toBe(false);
    expect(modelFreeCoverage(resolution({ anchor: 0.99, unresolved: 1 })).metadata).toBe(false);
    expect(modelFreeCoverage(resolution({ anchor: null })).metadata).toBe(false);
  });

  it("covers the cover at cover confidence 0.85", () => {
    expect(modelFreeCoverage(resolution({ cover: 0.85 })).cover).toBe(true);
    expect(modelFreeCoverage(resolution({ cover: 0.5 })).cover).toBe(false);
    expect(modelFreeCoverage(resolution({ cover: null })).cover).toBe(false);
  });

  it("covers nothing while the resolution is loading", () => {
    expect(modelFreeCoverage(undefined)).toEqual({ metadata: false, cover: false });
  });
});

describe("jobNeedsModel / canQueueJob", () => {
  const both = { metadata: true, cover: true };
  const metaOnly = { metadata: true, cover: false };
  const none = { metadata: false, cover: false };

  it("needs a model unless the resolver covers every part of the job", () => {
    expect(jobNeedsModel("metadata", metaOnly)).toBe(false);
    expect(jobNeedsModel("cover", metaOnly)).toBe(true);
    expect(jobNeedsModel("metadata+cover", metaOnly)).toBe(true);
    expect(jobNeedsModel("metadata+cover", both)).toBe(false);
  });

  it("without AI, queues only covered jobs", () => {
    expect(canQueueJob("metadata", metaOnly, NOT_READY)).toBe(true);
    expect(canQueueJob("cover", metaOnly, NOT_READY)).toBe(false);
    expect(canQueueJob("metadata", none, NOT_READY)).toBe(false);
  });

  it("with AI, queues anything (the Python version's behaviour)", () => {
    expect(canQueueJob("metadata", none, READY)).toBe(true);
    expect(canQueueJob("metadata+cover", none, READY)).toBe(true);
  });
});

describe("metadataJobType", () => {
  it("bundles a cover lookup for a coverless album when AI is ready", () => {
    expect(metadataJobType(false, { metadata: false, cover: false }, READY)).toBe("metadata+cover");
    expect(metadataJobType(true, { metadata: true, cover: true }, READY)).toBe("metadata");
  });

  it("without AI, bundles the cover only when the resolver covers it", () => {
    expect(metadataJobType(false, { metadata: true, cover: false }, NOT_READY)).toBe("metadata");
    expect(metadataJobType(false, { metadata: true, cover: true }, NOT_READY)).toBe("metadata+cover");
  });
});

describe("hint entry and ai-job fixes", () => {
  it("offers hint entry only when AI is ready", () => {
    expect(offersHintEntry(READY)).toBe(true);
    expect(offersHintEntry(NOT_READY)).toBe(false);
  });

  // ai-job fixes are always offered; without a ready provider
  // they run MusicBrainz only.
  it("marks ai-job fixes MusicBrainz-only exactly when AI is not ready", () => {
    expect(fixIsMusicBrainzOnly("ai-job", NOT_READY)).toBe(true);
    expect(fixIsMusicBrainzOnly("ai-job", READY)).toBe(false);
    expect(fixIsMusicBrainzOnly("deterministic", NOT_READY)).toBe(false);
    expect(fixIsMusicBrainzOnly("none", NOT_READY)).toBe(false);
  });
});
