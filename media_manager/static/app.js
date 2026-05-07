// Media Manager — frontend
"use strict";

const VIEW_KEY = "mm_view_mode";

const els = {
  search: document.getElementById("search"),
  viewTiles: document.getElementById("view-tiles"),
  viewList: document.getElementById("view-list"),
  settingsBtn: document.getElementById("settings-btn"),
  status: document.getElementById("status-bar"),
  albums: document.getElementById("albums"),
  empty: document.getElementById("empty-state"),
  modal: document.getElementById("settings-modal"),
  libraryDirInput: document.getElementById("library-dir-input"),
  saveSettings: document.getElementById("save-settings"),
  settingsError: document.getElementById("settings-error"),
  detail: document.getElementById("detail"),
  detailOverlay: document.getElementById("detail-overlay"),
  detailHeading: document.getElementById("detail-heading"),
  detailBody: document.getElementById("detail-body"),
  suggestMetadataBtn: document.getElementById("suggest-metadata-btn"),
  suggestCoverBtn: document.getElementById("suggest-cover-btn"),
  applyBtn: document.getElementById("apply-btn"),
  discardBtn: document.getElementById("discard-btn"),
  rescanBtn: document.getElementById("rescan-btn"),
  scanProgress: document.getElementById("scan-progress"),
  progressText: document.getElementById("progress-text"),
  progressCounter: document.getElementById("progress-counter"),
  progressFill: document.getElementById("progress-fill"),
};

const state = {
  albums: [],
  albumsById: new Map(),   // id → album, for fast updates during streaming
  filtered: [],
  view: localStorage.getItem(VIEW_KEY) || "tiles",
  filter: "",
  libraryDir: null,
  scannedAt: null,
  selectedId: null,
  pendingMetadata: null,   // { suggestions, notes, confidence }
  pendingCover: null,      // { cover_url, source, notes, confidence }
  scan: null,              // { es, total, scanned, freshCount, cachedCount, removedCount, renderQueued }
};

// ---------- API helpers ----------

async function api(path, opts = {}) {
  const res = await fetch(path, {
    headers: { "Content-Type": "application/json", ...(opts.headers || {}) },
    ...opts,
  });
  let body = null;
  try { body = await res.json(); } catch (_) { /* empty body */ }
  if (!res.ok) {
    const msg = body && body.detail ? body.detail : `${res.status} ${res.statusText}`;
    const err = new Error(msg);
    err.status = res.status;
    throw err;
  }
  return body;
}

// ---------- Rendering ----------

function fmtDuration(seconds) {
  if (!seconds && seconds !== 0) return "";
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

function placeholderArt() {
  return `<span aria-hidden="true">♪</span>`;
}

// Display rule: compilations always render as "Various Artists" regardless of
// what's in the album_artist tag. Per-track artists are still shown in the
// detail panel's track listing.
function displayArtist(album) {
  if (album && album.is_compilation) return "Various Artists";
  return (album && (album.album_artist || album.artist)) || "Unknown artist";
}

function coverImg(album, sizeAttr) {
  if (!album.has_embedded_cover && !album.cover_file) {
    return placeholderArt();
  }
  return `<img src="/api/albums/${album.id}/cover" alt="" loading="lazy" ${sizeAttr || ""} onerror="this.replaceWith(Object.assign(document.createElement('span'),{textContent:'♪'}));" />`;
}

function escapeHtml(s) {
  if (s === null || s === undefined) return "";
  return String(s)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

function renderTiles(albums) {
  els.albums.className = "albums tiles";
  if (albums.length === 0) {
    els.albums.innerHTML = `<div class="empty-state"><p>No albums match your filter.</p></div>`;
    return;
  }
  els.albums.innerHTML = albums.map((album) => {
    const artist = displayArtist(album);
    const sub = [album.year, album.track_count ? `${album.track_count} tracks` : ""]
      .filter(Boolean).join(" · ");
    return `
      <article class="tile${album.is_compilation ? ' tile-compilation' : ''}" data-id="${album.id}" title="${escapeHtml(album.path)}">
        <div class="tile-art">${coverImg(album)}</div>
        <div class="tile-meta">
          <div class="tile-title">${escapeHtml(album.title || album.dir_name)}</div>
          <div class="tile-artist">${escapeHtml(artist)}</div>
          ${sub ? `<div class="tile-sub">${escapeHtml(sub)}</div>` : ""}
        </div>
      </article>
    `;
  }).join("");
}

function renderList(albums) {
  els.albums.className = "albums list";
  if (albums.length === 0) {
    els.albums.innerHTML = `<div class="empty-state"><p>No albums match your filter.</p></div>`;
    return;
  }
  const header = `
    <div class="row header">
      <div></div>
      <div>Album</div>
      <div>Artist</div>
      <div>Year</div>
      <div>Tracks</div>
      <div>Length</div>
    </div>
  `;
  const rows = albums.map((album) => {
    const artist = displayArtist(album);
    return `
      <div class="row" data-id="${album.id}" title="${escapeHtml(album.path)}">
        <div class="row-art">${coverImg(album)}</div>
        <div class="cell-strong cell-truncate">${escapeHtml(album.title || album.dir_name)}</div>
        <div class="cell-truncate">${escapeHtml(artist)}</div>
        <div class="cell-muted">${escapeHtml(album.year || "")}</div>
        <div class="cell-muted">${album.track_count || ""}</div>
        <div class="cell-muted">${escapeHtml(fmtDuration(album.total_seconds))}</div>
      </div>
    `;
  }).join("");
  els.albums.innerHTML = header + rows;
}

function render() {
  if (state.view === "list") {
    renderList(state.filtered);
  } else {
    renderTiles(state.filtered);
  }
}

function applyFilter() {
  const q = state.filter.trim().toLowerCase();
  if (!q) {
    state.filtered = state.albums;
  } else {
    state.filtered = state.albums.filter((a) => {
      const haystack = [
        a.title, a.artist, a.album_artist, a.year, a.genre, a.dir_name,
        a.dir_artist, a.dir_album,
        a.is_compilation ? "various artists compilation" : "",
      ].filter(Boolean).join(" ").toLowerCase();
      return haystack.includes(q);
    });
  }
  render();
  updateStatus();
}

function updateStatus() {
  if (!state.libraryDir) {
    els.status.textContent = "";
    return;
  }
  const total = state.albums.length;
  const shown = state.filtered.length;
  const counter = state.filter
    ? `${shown} of ${total} albums`
    : `${total} ${total === 1 ? "album" : "albums"}`;
  els.status.textContent = `${counter} · ${state.libraryDir}`;
}

// ---------- View toggle ----------

function setView(view) {
  state.view = view;
  localStorage.setItem(VIEW_KEY, view);
  els.viewTiles.setAttribute("aria-selected", view === "tiles" ? "true" : "false");
  els.viewList.setAttribute("aria-selected", view === "list" ? "true" : "false");
  render();
}

// ---------- Settings modal ----------

function openSettings() {
  els.libraryDirInput.value = state.libraryDir || "";
  els.settingsError.classList.add("hidden");
  els.settingsError.textContent = "";
  els.modal.classList.remove("hidden");
  els.modal.setAttribute("aria-hidden", "false");
  setTimeout(() => els.libraryDirInput.focus(), 0);
}

function closeSettings() {
  els.modal.classList.add("hidden");
  els.modal.setAttribute("aria-hidden", "true");
}

async function saveSettings() {
  const value = els.libraryDirInput.value.trim();
  if (!value) {
    els.settingsError.textContent = "Please enter a directory path.";
    els.settingsError.classList.remove("hidden");
    return;
  }
  els.saveSettings.disabled = true;
  els.settingsError.classList.add("hidden");
  try {
    const cfg = await api("/api/config", {
      method: "POST",
      body: JSON.stringify({ library_dir: value }),
    });
    state.libraryDir = cfg.library_dir;
    closeSettings();
    await loadAlbums();
  } catch (err) {
    els.settingsError.textContent = err.message;
    els.settingsError.classList.remove("hidden");
  } finally {
    els.saveSettings.disabled = false;
  }
}

// ---------- Data loading ----------

// ---------- Album state helpers ----------

function setAlbums(albumList) {
  state.albums = albumList;
  state.albumsById = new Map(albumList.map((a) => [a.id, a]));
  resortAlbums();
}

function upsertAlbum(album) {
  const existing = state.albumsById.get(album.id);
  if (existing) {
    Object.assign(existing, album);
  } else {
    state.albums.push(album);
    state.albumsById.set(album.id, album);
  }
}

function removeAlbum(id) {
  if (!state.albumsById.has(id)) return;
  state.albumsById.delete(id);
  state.albums = state.albums.filter((a) => a.id !== id);
}

function resortAlbums() {
  state.albums.sort((a, b) => {
    const aa = (a.album_artist || a.artist || "").toLowerCase();
    const ba = (b.album_artist || b.artist || "").toLowerCase();
    if (aa !== ba) return aa < ba ? -1 : 1;
    const ay = a.year || "";
    const by_ = b.year || "";
    if (ay !== by_) return ay < by_ ? -1 : 1;
    const at = (a.title || "").toLowerCase();
    const bt = (b.title || "").toLowerCase();
    return at < bt ? -1 : at > bt ? 1 : 0;
  });
}

// rAF-throttled re-render so streaming bursts don't pin the main thread.
function scheduleRender() {
  if (state.scan && state.scan.renderQueued) return;
  if (state.scan) state.scan.renderQueued = true;
  requestAnimationFrame(() => {
    if (state.scan) state.scan.renderQueued = false;
    resortAlbums();
    applyFilter();
  });
}

// ---------- Loading + streaming ----------

async function loadAlbums() {
  els.empty.classList.add("hidden");
  // 1) Pull whatever's in the persistent cache instantly so the UI populates
  //    without waiting for the disk walk.
  try {
    const cached = await api("/api/albums");
    state.libraryDir = cached.library_dir;
    state.scannedAt = cached.scanned_at || null;
    setAlbums(cached.albums || []);
    applyFilter();
  } catch (err) {
    if (err.status === 409) {
      state.albums = [];
      state.albumsById = new Map();
      state.filtered = [];
      els.empty.classList.remove("hidden");
      els.status.textContent = "";
      return;
    }
    els.status.textContent = `Error: ${err.message}`;
    return;
  }

  // 2) Then open the SSE stream to refresh + extend the cache.
  startScanStream({ force: false });
}

function startScanStream({ force }) {
  if (state.scan && state.scan.es) {
    state.scan.es.close();
  }

  const url = `/api/scan/stream${force ? "?force=true" : ""}`;
  const es = new EventSource(url);
  state.scan = {
    es,
    total: 0,
    scanned: 0,
    freshCount: 0,
    cachedCount: 0,
    removedCount: 0,
    renderQueued: false,
  };

  showProgress(true, force ? "Rescanning…" : "Scanning library…");
  if (els.rescanBtn) {
    els.rescanBtn.disabled = true;
    els.rescanBtn.classList.add("scanning");
  }

  es.addEventListener("start", (ev) => {
    const data = JSON.parse(ev.data);
    state.scan.total = data.total;
    state.scan.cachedCount = data.cached_count;
    setProgressBar(0, data.total);
    setProgressLabel(force
      ? `Rescanning (cache cleared)…`
      : `Scanning library… ${data.cached_count > 0 ? `(${data.cached_count} cached)` : ""}`);
  });

  es.addEventListener("album", (ev) => {
    const album = JSON.parse(ev.data);
    if (album._status === "fresh") state.scan.freshCount++;
    upsertAlbum(album);
    state.scan.scanned++;
    setProgressBar(state.scan.scanned, state.scan.total);
    scheduleRender();
  });

  es.addEventListener("removed", (ev) => {
    const data = JSON.parse(ev.data);
    if (data.id) removeAlbum(data.id);
    state.scan.removedCount++;
    scheduleRender();
  });

  es.addEventListener("progress", (ev) => {
    const data = JSON.parse(ev.data);
    setProgressBar(data.scanned, data.total);
  });

  es.addEventListener("done", (ev) => {
    const data = JSON.parse(ev.data);
    state.scannedAt = data.scanned_at;
    es.close();
    state.scan.es = null;
    showProgress(false);
    if (els.rescanBtn) {
      els.rescanBtn.disabled = false;
      els.rescanBtn.classList.remove("scanning");
    }
    resortAlbums();
    applyFilter();
    const summary = state.scan.freshCount === 0 && state.scan.removedCount === 0
      ? "library up to date"
      : `${state.scan.freshCount} new/updated${state.scan.removedCount ? `, ${state.scan.removedCount} removed` : ""}`;
    flashStatus(`Scan complete — ${summary}`);
  });

  es.addEventListener("error", (ev) => {
    let msg = "Scan stream error";
    try {
      const data = JSON.parse(ev.data || "{}");
      if (data.detail) msg = `Scan error: ${data.detail}`;
    } catch (_) { /* connection-level error, no payload */ }
    es.close();
    state.scan.es = null;
    showProgress(false);
    if (els.rescanBtn) {
      els.rescanBtn.disabled = false;
      els.rescanBtn.classList.remove("scanning");
    }
    els.status.textContent = msg;
  });
}

function showProgress(visible, label) {
  if (!els.scanProgress) return;
  if (visible) {
    els.scanProgress.classList.remove("hidden");
    if (label) setProgressLabel(label);
  } else {
    els.scanProgress.classList.add("hidden");
    els.progressFill.style.width = "0%";
    els.progressFill.classList.remove("indeterminate");
  }
}

function setProgressLabel(text) {
  if (els.progressText) els.progressText.textContent = text;
}

function setProgressBar(scanned, total) {
  if (!els.progressFill || !els.progressCounter) return;
  if (total > 0) {
    const pct = Math.min(100, (scanned / total) * 100);
    els.progressFill.classList.remove("indeterminate");
    els.progressFill.style.width = `${pct.toFixed(1)}%`;
    els.progressCounter.textContent = `${scanned} / ${total}`;
  } else {
    els.progressFill.classList.add("indeterminate");
    els.progressCounter.textContent = "";
  }
}

let _statusTimer = null;
function flashStatus(message) {
  els.status.textContent = message;
  if (_statusTimer) clearTimeout(_statusTimer);
  _statusTimer = setTimeout(updateStatus, 4000);
}

async function init() {
  setView(state.view);

  // Wire events
  els.viewTiles.addEventListener("click", () => setView("tiles"));
  els.viewList.addEventListener("click", () => setView("list"));
  els.settingsBtn.addEventListener("click", openSettings);
  els.modal.querySelectorAll("[data-close]").forEach((el) => {
    el.addEventListener("click", closeSettings);
  });
  els.saveSettings.addEventListener("click", saveSettings);
  els.libraryDirInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") saveSettings();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !els.modal.classList.contains("hidden")) closeSettings();
  });
  els.search.addEventListener("input", (e) => {
    state.filter = e.target.value;
    applyFilter();
  });

  // Click-through on album rows/tiles → open detail panel
  els.albums.addEventListener("click", (e) => {
    const target = e.target.closest("[data-id]");
    if (!target) return;
    openDetail(target.getAttribute("data-id"));
  });

  // Detail panel close
  document.querySelectorAll("[data-close-detail]").forEach((el) =>
    el.addEventListener("click", closeDetail)
  );
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && els.detail.classList.contains("open")) closeDetail();
  });

  // AI + apply buttons
  els.suggestMetadataBtn.addEventListener("click", suggestMetadata);
  els.suggestCoverBtn.addEventListener("click", suggestCover);
  els.applyBtn.addEventListener("click", applyChanges);
  els.discardBtn.addEventListener("click", discardSuggestions);

  // Rescan
  if (els.rescanBtn) {
    els.rescanBtn.addEventListener("click", async () => {
      if (state.scan && state.scan.es) return;  // already running
      try {
        await api("/api/scan/rescan", { method: "POST" });
      } catch (err) {
        flashStatus(`Rescan failed: ${err.message}`);
        return;
      }
      // Clear locally and start a forced stream
      setAlbums([]);
      applyFilter();
      startScanStream({ force: true });
    });
  }

  // Load current config + albums
  try {
    const cfg = await api("/api/config");
    state.libraryDir = cfg.library_dir;
    if (cfg.library_dir && cfg.exists) {
      await loadAlbums();
    } else {
      els.empty.classList.remove("hidden");
    }
  } catch (err) {
    els.status.textContent = `Error: ${err.message}`;
  }
}

// ---------- Detail panel ----------

const SUGGESTABLE_FIELDS = ["title", "album_artist", "artist", "year", "genre"];

function fmtTrack(seconds) {
  if (!seconds && seconds !== 0) return "";
  const m = Math.floor(seconds / 60);
  const s = Math.round(seconds % 60).toString().padStart(2, "0");
  return `${m}:${s}`;
}

async function openDetail(id) {
  const album = state.albums.find((a) => a.id === id);
  if (!album) return;
  state.selectedId = id;
  state.pendingMetadata = null;
  state.pendingCover = null;
  renderDetailShell(album);
  els.detail.classList.add("open");
  els.detail.setAttribute("aria-hidden", "false");
  els.detailOverlay.classList.remove("hidden");

  // Fetch tracks lazily
  try {
    const data = await api(`/api/albums/${album.id}/tracks`);
    renderDetailBody(album, data.tracks || []);
  } catch (err) {
    renderDetailBody(album, []);
    appendOutput(`<div class="error-msg">Couldn't load tracks: ${escapeHtml(err.message)}</div>`);
  }
}

function closeDetail() {
  state.selectedId = null;
  state.pendingMetadata = null;
  state.pendingCover = null;
  els.detail.classList.remove("open");
  els.detail.setAttribute("aria-hidden", "true");
  els.detailOverlay.classList.add("hidden");
  els.applyBtn.classList.add("hidden");
  els.discardBtn.classList.add("hidden");
}

function renderDetailShell(album) {
  els.detailHeading.textContent = album.title || album.dir_name;
  els.detailBody.innerHTML = `
    <div class="detail-cover">${detailCoverImg(album)}</div>
    <div class="detail-title">${escapeHtml(album.title || album.dir_name)}</div>
    <div class="detail-artist">${escapeHtml(album.album_artist || album.artist || "Unknown artist")}</div>
    <div class="cell-muted" style="text-align:center;">Loading tracks…</div>
    <div id="ai-output"></div>
  `;
  els.applyBtn.classList.add("hidden");
  els.discardBtn.classList.add("hidden");
  resetSuggestButtons();
}

function renderDetailBody(album, tracks) {
  const showTrackArtist = !!album.is_compilation;  // per-track artists only meaningful for comps
  const trackHtml = tracks.length
    ? tracks.map((t) => `
        <div class="track${showTrackArtist ? ' with-artist' : ''}">
          <div class="num">${escapeHtml(t.tracknumber || "")}</div>
          <div class="title">${escapeHtml(t.title || t.filename)}</div>
          ${showTrackArtist ? `<div class="track-artist cell-muted cell-truncate">${escapeHtml(t.artist || "")}</div>` : ""}
          <div class="dur">${escapeHtml(fmtTrack(t.duration))}</div>
        </div>
      `).join("")
    : `<div class="track"><div></div><div class="cell-muted">No tracks</div><div></div></div>`;

  // Compilation row (only when applicable) + parsed dir info for transparency.
  const compilationBadge = album.is_compilation
    ? `<span class="badge badge-compilation" title="Identified as a compilation by directory parse">Compilation</span>`
    : "";
  const parsedFromFolder = (album.dir_artist || album.dir_album)
    ? `${escapeHtml(album.dir_artist || (album.is_compilation ? "Various Artists" : "—"))} <span class="cell-muted">/</span> ${escapeHtml(album.dir_album || "—")}`
    : `<span class="cell-muted">unparsed</span>`;

  els.detailBody.innerHTML = `
    <div class="detail-cover">${detailCoverImg(album)}</div>
    <div class="detail-title">${escapeHtml(album.title || album.dir_name)} ${compilationBadge}</div>
    <div class="detail-artist">${escapeHtml(displayArtist(album))}</div>
    <dl class="kv">
      <dt>Year</dt><dd>${escapeHtml(album.year || "—")}</dd>
      <dt>Genre</dt><dd>${escapeHtml(album.genre || "—")}</dd>
      <dt>Tracks</dt><dd>${album.track_count}</dd>
      <dt>Length</dt><dd>${escapeHtml(fmtDuration(album.total_seconds))}</dd>
      <dt>Folder</dt><dd><code>${escapeHtml(album.dir_name)}</code></dd>
      <dt>From folder</dt><dd>${parsedFromFolder}</dd>
      <dt>Cover</dt><dd>${album.has_embedded_cover ? "embedded in FLAC" : (album.cover_file ? `<code>${escapeHtml(album.cover_file)}</code>` : '<span class="cell-muted">none</span>')}</dd>
    </dl>
    <div class="tracks">${trackHtml}</div>
    <div id="ai-output"></div>
  `;
}

function detailCoverImg(album) {
  if (!album.has_embedded_cover && !album.cover_file) {
    return `<span aria-hidden="true">&#9834;</span>`;
  }
  // Detail panel wants the full-resolution image, not the 384px thumb.
  return `<img src="/api/albums/${album.id}/cover?size=full" alt="" onerror="this.replaceWith(Object.assign(document.createElement('span'),{textContent:'♪'}));" />`;
}

function appendOutput(html) {
  let out = document.getElementById("ai-output");
  if (!out) {
    out = document.createElement("div");
    out.id = "ai-output";
    els.detailBody.appendChild(out);
  }
  out.insertAdjacentHTML("beforeend", html);
}

function clearOutput() {
  const out = document.getElementById("ai-output");
  if (out) out.innerHTML = "";
}

function resetSuggestButtons() {
  els.suggestMetadataBtn.disabled = false;
  els.suggestMetadataBtn.innerHTML = `<span class="btn-label">Suggest metadata fixes</span>`;
  els.suggestCoverBtn.disabled = false;
  els.suggestCoverBtn.innerHTML = `<span class="btn-label">Suggest cover art</span>`;
}

function maybeShowApply() {
  const hasChanges = state.pendingMetadata && Object.keys(state.pendingMetadata.suggestions || {}).length > 0;
  const hasCover = state.pendingCover && state.pendingCover.cover_url;
  if (hasChanges || hasCover) {
    els.applyBtn.classList.remove("hidden");
    els.discardBtn.classList.remove("hidden");
  } else {
    els.applyBtn.classList.add("hidden");
    els.discardBtn.classList.add("hidden");
  }
}

// ---------- AI calls ----------

async function suggestMetadata() {
  const album = state.albums.find((a) => a.id === state.selectedId);
  if (!album) return;
  els.suggestMetadataBtn.disabled = true;
  els.suggestMetadataBtn.innerHTML = `<span class="spinner"></span> Asking Claude…`;
  // Remove previous metadata block if present
  document.querySelectorAll("#ai-output .suggestions").forEach((el) => el.remove());
  try {
    const data = await api(`/api/ai/suggest-metadata/${album.id}`, { method: "POST" });
    state.pendingMetadata = {
      suggestions: data.suggestions || {},
      notes: data.notes || "",
      confidence: data.confidence || "?",
    };
    renderMetadataSuggestion(album, state.pendingMetadata);
    maybeShowApply();
  } catch (err) {
    appendOutput(`<div class="error-msg">${escapeHtml(err.message)}</div>`);
  } finally {
    els.suggestMetadataBtn.disabled = false;
    els.suggestMetadataBtn.innerHTML = `<span class="btn-label">Re-ask Claude</span>`;
  }
}

function renderMetadataSuggestion(album, pending) {
  const sug = pending.suggestions || {};
  const keys = SUGGESTABLE_FIELDS.filter((k) => sug[k] !== undefined && sug[k] !== null);
  if (keys.length === 0) {
    appendOutput(`<div class="suggestions">
      <h3><span>Claude's review</span><span class="cell-muted">${escapeHtml(pending.confidence)} confidence</span></h3>
      <div class="same">No high-confidence corrections — current metadata looks good.</div>
      ${pending.notes ? `<div class="notes">${escapeHtml(pending.notes)}</div>` : ""}
    </div>`);
    return;
  }
  const rows = keys.map((k) => {
    const oldV = album[k] || "—";
    const newV = sug[k];
    if (String(oldV) === String(newV)) {
      return `<div class="diff-row"><div class="label">${k}</div><div class="vals"><span class="same">${escapeHtml(String(oldV))} (no change)</span></div></div>`;
    }
    return `<div class="diff-row"><div class="label">${k}</div><div class="vals">
      <span class="old">${escapeHtml(String(oldV))}</span>
      <span class="new">${escapeHtml(String(newV))}</span>
    </div></div>`;
  }).join("");
  appendOutput(`<div class="suggestions">
    <h3><span>Claude's metadata suggestions</span><span class="cell-muted">${escapeHtml(pending.confidence)} confidence</span></h3>
    ${rows}
    ${pending.notes ? `<div class="notes">${escapeHtml(pending.notes)}</div>` : ""}
  </div>`);
}

async function suggestCover() {
  const album = state.albums.find((a) => a.id === state.selectedId);
  if (!album) return;
  els.suggestCoverBtn.disabled = true;
  els.suggestCoverBtn.innerHTML = `<span class="spinner"></span> Asking Claude…`;
  document.querySelectorAll("#ai-output .cover-suggest").forEach((el) => el.remove());
  try {
    const data = await api(`/api/ai/suggest-cover/${album.id}`, { method: "POST" });
    state.pendingCover = {
      cover_url: data.cover_url || null,
      source: data.source || "",
      notes: data.notes || "",
      confidence: data.confidence || "?",
    };
    renderCoverSuggestion(state.pendingCover);
    maybeShowApply();
  } catch (err) {
    appendOutput(`<div class="error-msg">${escapeHtml(err.message)}</div>`);
  } finally {
    els.suggestCoverBtn.disabled = false;
    els.suggestCoverBtn.innerHTML = `<span class="btn-label">Re-ask cover art</span>`;
  }
}

function renderCoverSuggestion(pending) {
  if (!pending.cover_url) {
    appendOutput(`<div class="suggestions">
      <h3><span>Cover art</span><span class="cell-muted">${escapeHtml(pending.confidence)} confidence</span></h3>
      <div class="same">Claude didn't find a confident cover-art match.</div>
      ${pending.notes ? `<div class="notes">${escapeHtml(pending.notes)}</div>` : ""}
    </div>`);
    return;
  }
  appendOutput(`<div class="cover-suggest">
    <img src="${escapeHtml(pending.cover_url)}" alt="proposed cover" onerror="this.style.display='none'" />
    <div class="meta">
      <div><strong>Proposed cover</strong> <span class="cell-muted">(${escapeHtml(pending.confidence)} confidence)</span></div>
      <div class="cell-muted" style="margin-top:4px;">${escapeHtml(pending.source)}</div>
      <div style="margin-top:6px;"><a href="${escapeHtml(pending.cover_url)}" target="_blank" rel="noopener">${escapeHtml(pending.cover_url)}</a></div>
      ${pending.notes ? `<div class="notes" style="margin-top:8px;">${escapeHtml(pending.notes)}</div>` : ""}
    </div>
  </div>`);
}

// ---------- Apply ----------

async function applyChanges() {
  const album = state.albums.find((a) => a.id === state.selectedId);
  if (!album) return;
  const sug = (state.pendingMetadata && state.pendingMetadata.suggestions) || {};
  const cover = state.pendingCover && state.pendingCover.cover_url;
  const summary = [
    Object.keys(sug).length ? `${Object.keys(sug).length} tag change${Object.keys(sug).length === 1 ? "" : "s"}` : null,
    cover ? "new cover art" : null,
  ].filter(Boolean).join(" + ");
  if (!confirm(`Apply ${summary} to all FLACs in:\n\n${album.dir_name}\n\nThis will modify your files.`)) return;

  els.applyBtn.disabled = true;
  els.applyBtn.innerHTML = `<span class="spinner"></span> Applying…`;
  try {
    const result = await api(`/api/albums/${album.id}/apply`, {
      method: "POST",
      body: JSON.stringify({ suggestions: sug, cover_url: cover, dry_run: false }),
    });
    const savedCount = (result.files || []).filter((f) => f.saved).length;
    appendOutput(`<div class="suggestions" style="border-color:#15803d;">
      <h3 style="color:#6ee7a3;">Applied to ${savedCount} file${savedCount === 1 ? "" : "s"}</h3>
      <div class="cell-muted">Reload the library to see updated tags.</div>
    </div>`);
    state.pendingMetadata = null;
    state.pendingCover = null;
    els.applyBtn.classList.add("hidden");
    els.discardBtn.classList.add("hidden");
    // Refresh the underlying data
    await loadAlbums();
  } catch (err) {
    appendOutput(`<div class="error-msg">Apply failed: ${escapeHtml(err.message)}</div>`);
  } finally {
    els.applyBtn.disabled = false;
    els.applyBtn.textContent = "Apply changes";
  }
}

function discardSuggestions() {
  state.pendingMetadata = null;
  state.pendingCover = null;
  clearOutput();
  els.applyBtn.classList.add("hidden");
  els.discardBtn.classList.add("hidden");
  resetSuggestButtons();
}

document.addEventListener("DOMContentLoaded", init);
