import { useState } from "react";
import { Link } from "react-router-dom";
import { AlertCircle, Info, XCircle } from "lucide-react";
import { toast } from "sonner";

import {
  useChecks,
  useFindingsForAlbum,
  useUnignoreFinding,
} from "@/hooks/useChecks";
import type { CheckFixKind, Finding } from "@/lib/types";

import FindingFixButton from "../FindingFixButton";
import IgnoreFindingControl from "../IgnoreFindingControl";
import Section from "./Section";

interface Props {
  albumId: string;
}

/**
 * Album-centric view of open findings — the per-album mirror of the
 * Health page's per-check list. Renders nothing when the album has no
 * open findings, so healthy albums don't carry a stub section.
 *
 * Per the Phase-1 spec: stacks between the "Album info" PropTable and the
 * "Audio analysis" section. Same Fix / Ignore actions as the Health page,
 * funneling through the same hooks (so a fix from one surface propagates
 * to the other via the shared findings cache).
 */
export default function FindingsSection({ albumId }: Props) {
  // Includes ignored — we render them in a collapsed sub-bucket inside the
  // section so the user can see them but they don't dominate the count.
  const all = useFindingsForAlbum(albumId);
  const open = all.filter((f) => !f.ignored_at);
  const ignored = all.filter((f) => f.ignored_at);

  // Hide the section entirely when there's nothing to show. This is the
  // "collapses for healthy albums" behavior from the spec.
  if (open.length === 0 && ignored.length === 0) return null;

  return (
    <Section
      id={`findings-${albumId}`}
      title="Findings"
      defaultOpen={open.length > 0}
      meta={
        <span>
          {open.length > 0 ? (
            <span className="text-warn font-semibold">{open.length} open</span>
          ) : (
            <span className="text-fg-4">{ignored.length} ignored</span>
          )}
          {open.length > 0 && ignored.length > 0 && (
            <span className="text-fg-4"> · {ignored.length} ignored</span>
          )}
        </span>
      }
    >
      {open.length > 0 && <BulkHealthHint findings={open} />}
      <FindingsList findings={open} />
      {ignored.length > 0 && <IgnoredCollapsed findings={ignored} />}
    </Section>
  );
}

/**
 * Note above the per-album findings list pointing the user at the Health
 * detail page for the same check. That's where the bulk affordances live —
 * users who only ever fix findings one-by-one from the album sheet never
 * discover the time-saver. We use the most-common open check as the deep
 * link target so the click lands on the bulk view they'd actually want.
 */
function BulkHealthHint({ findings }: { findings: Finding[] }) {
  const { data: checks = [] } = useChecks();

  // Pick the single open check most represented across this album's findings;
  // tie-broken by check label order to keep the link target deterministic.
  const checkCounts = new Map<string, number>();
  for (const f of findings) {
    checkCounts.set(f.check_id, (checkCounts.get(f.check_id) ?? 0) + 1);
  }
  let topCheckId: string | null = null;
  let topCount = 0;
  for (const [cid, count] of checkCounts) {
    if (count > topCount) {
      topCheckId = cid;
      topCount = count;
    }
  }
  if (!topCheckId) return null;
  const checkLabel =
    checks.find((c) => c.meta.id === topCheckId)?.meta.label ?? topCheckId;

  return (
    <p className="px-4 py-2 text-xs text-fg-3 leading-relaxed border-b border-line bg-bg-2">
      You can also bulk-fix similar findings across the library from{" "}
      <Link
        to={`/health/${encodeURIComponent(topCheckId)}`}
        className="text-accent hover:underline"
      >
        Health → {checkLabel}
      </Link>
      .
    </p>
  );
}

function FindingsList({ findings }: { findings: Finding[] }) {
  if (findings.length === 0) return null;
  return (
    <div className="divide-y divide-line">
      {findings.map((f) => <FindingRow key={f.id} finding={f} />)}
    </div>
  );
}

/**
 * Collapsed disclosure for ignored findings — out of the way by default,
 * one-click expand for the user who's auditing what they've dismissed.
 */
function IgnoredCollapsed({ findings }: { findings: Finding[] }) {
  const [expanded, setExpanded] = useState(false);
  return (
    <div className="border-t border-line">
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        className="w-full px-4 py-1.5 text-left mono text-[11px] text-fg-4 hover:bg-bg-2 transition"
      >
        {expanded ? "▾" : "▸"} Ignored ({findings.length})
      </button>
      {expanded && (
        <div className="divide-y divide-line opacity-60">
          {findings.map((f) => <FindingRow key={f.id} finding={f} />)}
        </div>
      )}
    </div>
  );
}

function FindingRow({ finding }: { finding: Finding }) {
  // Look up fix_kind from the registered check so we know whether to render
  // the Fix button. Cheap — checks list is small and cached.
  const { data: checks = [] } = useChecks();
  const checkMeta = checks.find((c) => c.meta.id === finding.check_id)?.meta;
  const fixKind: CheckFixKind = checkMeta?.fix_kind ?? "none";
  const checkLabel = checkMeta?.label ?? finding.check_id;

  const unignoreMutation = useUnignoreFinding();

  const onUnignore = () => {
    unignoreMutation.mutate(
      { checkId: finding.check_id, findingId: finding.id },
      { onError: (e) => toast.error((e as Error).message || "Couldn't restore") },
    );
  };

  const SevIcon = finding.severity === "error" ? XCircle : finding.severity === "info" ? Info : AlertCircle;
  const sevClass = finding.severity === "error" ? "text-err" : finding.severity === "info" ? "text-fg-3" : "text-warn";

  return (
    <div className="px-4 py-2 flex items-start gap-3 text-sm">
      <SevIcon className={`w-4 h-4 shrink-0 mt-0.5 ${sevClass}`} />
      <div className="flex-1 min-w-0">
        <div>{finding.summary}</div>
        <div className="mono text-[11px] text-fg-4 mt-0.5 truncate">
          <Link
            to={`/health/${encodeURIComponent(finding.check_id)}`}
            className="hover:underline text-fg-3"
          >
            {checkLabel}
          </Link>
          <span> · </span>
          <span>seen {fmtRelative(finding.last_seen_at)}</span>
          {finding.ignored_reason && (
            <>
              <span> · </span>
              <span className="italic">ignored: {finding.ignored_reason}</span>
            </>
          )}
        </div>
      </div>
      <div className="flex items-center gap-1 shrink-0">
        <FindingFixButton finding={finding} fixKind={fixKind} />
        {finding.ignored_at ? (
          <button
            onClick={onUnignore}
            disabled={unignoreMutation.isPending}
            className="h-7 px-2 rounded border border-line text-xs text-fg-2 hover:bg-bg-3 hover:text-fg transition disabled:opacity-60"
          >
            Restore
          </button>
        ) : (
          <IgnoreFindingControl checkId={finding.check_id} findingId={finding.id} />
        )}
      </div>
    </div>
  );
}

function fmtRelative(iso: string | null): string {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const diff = (Date.now() - t) / 1000;
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return `${Math.floor(diff / 86400)}d ago`;
}
