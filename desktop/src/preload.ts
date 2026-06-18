/**
 * Preload script — runs in an isolated context bridging renderer ↔ main.
 *
 * Deliberately minimal: the renderer is the existing web SPA loaded over
 * HTTP from the sidecar, and it talks to the backend with plain relative
 * `fetch` / `EventSource`. It needs nothing from Electron today, so we
 * expose no IPC surface (smallest possible attack surface with
 * contextIsolation on). Add narrowly-scoped bridges here only if a future
 * desktop-only feature (native file dialogs, menu wiring) needs one.
 */
export {};
