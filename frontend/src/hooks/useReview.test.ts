import { describe, expect, it } from "vitest";

import { buildReviewItems, filterReviewItems, reviewBadge, type ReviewInbox, type SourceStatus } from "@/hooks/useReview";
import type { AgentApproval } from "@/lib/api";

const approval = (id: string, over: Partial<AgentApproval> = {}): AgentApproval => ({
  id,
  pairingId: "p1",
  pairingName: "Helper",
  action: "scan_start",
  albumId: null,
  albumLabel: null,
  kinds: ["compute.scan"],
  state: "pending",
  reason: null,
  createdAt: `2026-01-01T00:00:0${id}Z`,
  expiresAt: "2999-01-01T00:00:00Z",
  decidedAt: null,
  diff: null,
  ...over,
});

const source = (label: string, state: SourceStatus["state"]): SourceStatus => ({
  label,
  state,
  retry: () => {},
  retrying: false,
});

function inbox(approvals: AgentApproval[], approvalsState: SourceStatus["state"]): ReviewInbox {
  const items = buildReviewItems([], [], [], approvals);
  return {
    items,
    count: items.length,
    batched: [],
    countIsLowerBound: approvalsState === "unavailable",
    inProgress: [],
    finishedJobs: [],
    sources: {
      findings: source("findings", "ok"),
      jobs: source("jobs", "ok"),
      approvals: source("assistant requests", approvalsState),
    },
    complete: approvalsState === "ok",
    degraded: approvalsState === "unavailable",
    allClear: approvalsState === "ok" && items.length === 0,
    isLoading: false,
  };
}

describe("approval items", () => {
  it("only pending approvals are items, oldest first, and they carry their album", () => {
    const items = buildReviewItems(
      [],
      [],
      [],
      [approval("2", { albumId: "a1" }), approval("1"), approval("3", { state: "approved" }), approval("4", { state: "expired" })],
    );
    expect(items.map((i) => i.key)).toEqual(["approval:1", "approval:2"]);
    expect(items[1]!.albumId).toBe("a1");
    expect(items.every((i) => i.kind === "approval" && i.urgency === "approval")).toBe(true);
  });

  it("the approvals filter keeps only them", () => {
    const items = buildReviewItems([], [], [], [approval("1")]);
    expect(filterReviewItems(items, { kind: "approvals" })).toHaveLength(1);
    expect(filterReviewItems(items, { kind: "failed" })).toHaveLength(0);
  });
});

describe("badge", () => {
  it("counts approvals toward the single badge", () => {
    expect(reviewBadge(inbox([approval("1"), approval("2")], "ok"))).toMatchObject({ text: "2", uncertain: false });
  });

  it("empty and answered is no badge; unavailable is a question mark, not an empty inbox", () => {
    expect(reviewBadge(inbox([], "ok"))).toBeNull();
    expect(reviewBadge(inbox([], "unavailable"))).toMatchObject({ text: "?", uncertain: true });
  });
});
