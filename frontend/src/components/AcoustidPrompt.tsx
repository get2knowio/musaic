import { Fingerprint, X } from "lucide-react";
import { useNavigate } from "react-router-dom";

import { useAcoustidPrompt, useDismissAcoustidPrompt } from "@/hooks/useAcoustidPrompt";
import { useSettings } from "@/hooks/useJobs";
import { shouldShowAcoustidPrompt } from "@/lib/fingerprintStatus";

/** Where the Settings page finds the AcoustID key field (see FingerprintSection). */
export const ACOUSTID_KEY_HASH = "#acoustid-key";

/**
 * One dismissible suggestion, shown on the Library page after a scan has
 * completed while no AcoustID key is set. It stays away once dismissed
 * (remembered by the server) or once a key is set.
 */
export default function AcoustidPrompt({ scannedAt }: { scannedAt: string | null }) {
  const navigate = useNavigate();
  const { data: settings } = useSettings();
  const { data: prompt } = useAcoustidPrompt();
  const dismiss = useDismissAcoustidPrompt();

  const show = shouldShowAcoustidPrompt({
    scannedAt,
    keySet: settings ? !!settings.acoustid_api_key_set : undefined,
    dismissed: prompt?.dismissed,
  });
  if (!show) return null;

  return (
    <div
      role="region"
      aria-label="Improve matching"
      className="flex items-start gap-3 px-4 py-2.5 border-b border-line bg-panel-2"
    >
      <Fingerprint className="w-4 h-4 text-fg-3 shrink-0 mt-0.5" />
      <p className="flex-1 text-sm text-fg-2 leading-relaxed">
        <span className="font-medium text-fg">Improve matching: add an AcoustID key.</span>{" "}
        With a key, Musaic can match albums by their audio when names and tags are not enough.
        It is free for non-commercial use.
      </p>
      <button
        type="button"
        onClick={() => navigate(`/settings${ACOUSTID_KEY_HASH}`)}
        // Optional: a secondary action, so the scan's own next step (Review
        // findings) stays the one primary button on the page.
        className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition whitespace-nowrap"
      >
        Add a key in Settings
      </button>
      <button
        type="button"
        aria-label="Dismiss"
        title="Dismiss"
        disabled={dismiss.isPending}
        onClick={() => dismiss.mutate()}
        className="h-8 w-8 grid place-items-center rounded text-fg-3 hover:bg-bg-3 transition disabled:opacity-60"
      >
        <X className="w-4 h-4" />
      </button>
    </div>
  );
}
