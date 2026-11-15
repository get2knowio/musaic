/** The album panel's narrowest docked width. */
export const PANEL_MIN_WIDTH = 360;

/**
 * True when a row `available` px wide cannot hold both the list's minimum
 * and the panel's: the panel then takes the whole row and the list waits,
 * hidden but mounted, behind it. Unknown width (not measured yet) docks.
 */
export function panelFillsRow(available: number | null | undefined, listMinWidth: number): boolean {
  return available != null && available > 0 && available < listMinWidth + PANEL_MIN_WIDTH;
}
