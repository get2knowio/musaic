/**
 * Sheet footer overflow menu. Currently no items are wired up — the trigger
 * was hidden during the UX audit pass because every item was a placeholder
 * for a v1.1 endpoint (Rename folder, Re-scan this album, Force re-analyze)
 * and the empty menu cost attention on every album open for no payoff.
 *
 * Re-enable this component (uncomment the trigger and add real items) once
 * at least one of the underlying endpoints ships.
 */
export default function MoreMenu(_props: { disabled?: boolean }) {
  return null;
}
