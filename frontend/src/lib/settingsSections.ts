/**
 * The Settings page is indexed by task. Each section has a stable id that is
 * also its URL hash (`/settings#tags`), so a link from elsewhere in the app
 * lands on it. `#acoustid-key` (components/AcoustidPrompt.tsx) is older and
 * stays: it points into "Matching & integrations" and the fingerprint
 * section handles it itself.
 */
export const SETTINGS_SECTIONS = [
  { id: "library-folder", label: "Library folder" },
  { id: "tags", label: "Tags" },
  { id: "file-naming", label: "File naming" },
  { id: "matching-integrations", label: "Matching & integrations" },
  { id: "backup-restore", label: "Backup & restore" },
] as const;

export type SettingsSectionId = (typeof SETTINGS_SECTIONS)[number]["id"];

/** Marks the control inside a section that a link to the section focuses. */
export const FOCUS_TARGET_ATTR = "data-settings-focus";

/** The section a URL hash names, or null (`#acoustid-key` is not one). */
export function sectionForHash(hash: string): SettingsSectionId | null {
  const id = hash.replace(/^#/, "");
  return SETTINGS_SECTIONS.find((s) => s.id === id)?.id ?? null;
}

/**
 * Scroll a section into view and put focus on its marked control, or on the
 * section itself (it has tabindex -1) when it marks none.
 */
export function focusSection(id: SettingsSectionId): boolean {
  const section = document.getElementById(id);
  if (!section) return false;
  const target = section.querySelector<HTMLElement>(`[${FOCUS_TARGET_ATTR}]`) ?? section;
  target.scrollIntoView?.({ block: "start" });
  target.focus({ preventScroll: true });
  return true;
}

/** sessionStorage flag: the user came back from Settings to an album and
 *  its My Tags section should take focus (see Settings "Back to album"). */
export const RETURN_FOCUS_KEY = "mm.settings.returnFocus";

/** Router state a link to a Settings section may carry: the page to return to. */
export interface SettingsLinkState {
  returnTo?: string;
}
