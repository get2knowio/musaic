# Handoff: Media Manager (FLAC Library Index + Metadata Editor)

## Overview

A desktop-style **music collection manager** for an existing directory of FLAC files. The user points the app at a music root (e.g. `~/Music`), it scans subfolders into albums, and provides a dense, "pro-tool" interface for browsing the library, fixing metadata problems, and running audio analysis (BPM detection, ReplayGain).

This handoff covers the **immediate v1 scope**: a local index of an existing FLAC directory tree. A future phase will allow adding albums by photographing vinyl records (no audio files); the design is informed by that direction but does **not** implement it yet.

The interface vibe is **Linear / Sublime / Beekeeper**: small type, monospace accents, neutral palette, low chrome, lots-on-screen density. Light and dark themes both fully supported.

## About the Design Files

The files bundled in this handoff are **design references created in HTML/JSX** — interactive prototypes showing intended look and behavior. They are **not production code to copy directly**.

Your task is to **recreate these designs in the target application's environment** (likely Electron + React + Tailwind/CSS Modules, or a native desktop framework like Tauri or SwiftUI) using the codebase's established patterns and libraries.

If no environment exists yet, **Electron + React + TypeScript** is a reasonable default for this kind of file-system-heavy desktop app, with a Rust/Node backend for FLAC tag I/O and audio analysis.

## Fidelity

**High-fidelity.** The mocks specify exact colors (via OKLCH-derived custom properties), typography, spacing, layout, and interaction states. Recreate the UI pixel-perfectly using the codebase's component primitives. Every value referenced below is locked in `styles.css` — treat it as the spec.

The mock data (album list, tracks, sizes, BPM values) is illustrative — your implementation reads real values from disk via FLAC tag libraries (e.g. `node-taglib`, `mutagen` via a Python sidecar, or `lofty` in Rust).

---

## Screens / Views

The prototype is a **single-page application** with a persistent shell (titlebar + sidebar + main canvas). State is driven by a `scenario` enum that flips between sub-views; see "State Management" below for the canonical list.

### 1. Application shell

**Persistent across all scenarios.**

- **Layout**: CSS grid `[titlebar 36px] [body 1fr]`. Body is grid `[sidebar 200px (collapsed: 56px)] [main 1fr]`.
- **Titlebar (`.titlebar`)**:
  - Height: 36px. Background `var(--titlebar)`. Bottom border `1px solid var(--line)`.
  - Left: app glyph (small disc icon) + path crumbs `~/Music ▸ Library` in monospace 11px.
  - Center: search input — 36px tall pill, monospace placeholder `Filter albums, artists, tracks…`, with a `/` keyboard hint badge on the right edge.
  - Right cluster: view-mode toggle (Grid / List icon buttons), Settings cog, Tweaks toggle (only visible when host's `__edit_mode_available` toggle is on), profile chip.
- **Sidebar (`.sidebar`)**:
  - Width: 200px, vertical scroll. Background `var(--panel)`. Right border `1px solid var(--line)`.
  - Sections (each preceded by an uppercase 9.5px monospace label `var(--fg-3)` with `letter-spacing: 0.08em`):
    - **Library** — `Library` (active, count 312), `Recently added` (12), `Favorites` (24), `Playlists` (3)
    - **Filters** — `All albums` (312), `Needs review` (14, warn dot), `Missing art` (3, error dot)
    - **Genres** — `Jazz` (47), `Rock` (124), `Electronic` (38), `Classical` (29), `Hip-Hop` (22), `Other` (52)
    - **Sources** — `~/Music` (root, active), `External SSD` (offline, muted)
  - Items: `.sb-chip` — 26px tall, 8px gap from icon to label, count number right-aligned in tabular-num monospace.
  - Active item: background `var(--accent-soft)`, color `var(--accent)`, left border 2px `var(--accent)`.
- **Main canvas (`.main`)**: scrollable, contains the filter bar + tile grid (or whatever the current scenario displays).

### 2. Library — tile grid (default scenario)

The "record wall" — covers dominate, this is where the user spends most of their time.

- **Filter bar (`.filterbar`)** at top of `.main`:
  - Sticky, 36px tall, padding `0 16px`, border-bottom `1px solid var(--line)`.
  - Left stats in monospace 11px: `**312** albums · ~/Music · 142 GB · last scan 2h ago`.
  - Right: filter chip dropdowns — `Sort: Artist ▾`, `Filter: All ▾`, `Year: Any ▾`. Each is a 24px-tall pill with a tiny chevron.
- **Tile grid (`.grid`)**:
  - CSS grid: `grid-template-columns: repeat(auto-fill, minmax(var(--tile-min), 1fr))`.
  - `--tile-min` defaults to 168px. User-tunable 120–240px.
  - Gap: 12px (compact) / 16px (cozy) / 20px (spacious).
  - Padding: 16px around the grid.
- **Tile (`.tile`)**:
  - Aspect-ratio 1/1 cover art via gradient `background` (in production: real cover image with `object-fit: cover`).
  - Below cover: 2-line text block padding 8px 2px:
    - Line 1: album title — `font-weight: 500`, 12.5px, color `var(--fg)`, `text-overflow: ellipsis`.
    - Line 2: artist — 11px, color `var(--fg-3)`, ellipsis.
  - Hover: scale `1.02`, transition `transform .15s`. Cursor `pointer`.
  - Selected: 2px solid `var(--accent)` outline (offset 2px), ring shadow `0 0 0 4px var(--accent-soft)`.
  - **State badges** (top-right corner, 4px inset):
    - `Needs review` → 14px round dot `var(--warn)`
    - `Missing art` → 14px round dot `var(--err)` with `?` glyph
    - `Compilation` → small 10px monospace pill `VA`
  - **Scanning state** (`.tile-scanning`): cover dimmed 40%, indeterminate progress shimmer `linear-gradient(90deg, transparent, rgba(255,255,255,0.08), transparent)` animating left→right over 1.4s.
- **Skeleton tile (`.tile-skel`)**: same dimensions, gradient placeholder pulsing 1.5s opacity 0.4 → 1.0 → 0.4.

### 3. Album detail sheet (the primary editing surface)

Slides in from the right when a tile is clicked. **480px wide**, full-height of the main canvas. Backdrop `rgba(0,0,0,0.35)` covers the rest of `.main`. Transition `transform .22s cubic-bezier(.32,.72,.34,1)`.

CSS grid layout: `grid-template-rows: auto 1fr auto` — head / body (scrolls) / foot.

#### 3a. Sheet head (`.sheet-head`)
- 44px tall, padding `0 14px`, border-bottom `1px solid var(--line)`.
- Left: ◀ back button + breadcrumb `Library ▸ <album title>`.
- Right: close (×) button.

#### 3b. Sheet body (`.sheet-body`) — scrollable

**Album hero (`.album-hero`)** — never collapsed, identity of the album:
- Padding: `14px`. Flex row, gap 14px.
- Left: 96×96 cover art square, border-radius 4px, subtle 1px inset shadow.
- Right: vertical block — title 17px/600, artist 13px `var(--fg-2)`, year · genre · track-count · duration in 11px monospace `var(--fg-3)`.
- Optional badge row below: `Needs review`, `Missing art`, etc — 11px pills.

**Three collapsible sections** follow the hero. Each is a `<Section>` component:

- Header: 32px tall, full-width clickable, padding `8px 14px`. Hover background `var(--bg-2)`.
  - Left: 11px chevron right (rotates 90° when open, transition 0.15s).
  - Title: monospace 9.5px UPPERCASE, letter-spacing 0.08em, color `var(--fg-3)` (becomes `var(--fg-2)` when section open).
  - Right meta: monospace 10.5px, color `var(--fg-4)`, ellipsis-clipped.
- Body: padding determined by inner content's own padding.
- Border-bottom `1px solid var(--line)` between sections.
- **Open state persisted to localStorage** under `mm.section.<id>` (values `"0"` / `"1"`).

The three sections, in order:

1. **Album info** (id `album-info`, **closed** by default):
   - Header meta (when collapsed): `FLAC · 44.1 kHz · 16-bit · 312.4 MB`
   - Body: `<dl class="props">` definition list — 100px label column, value column. Rows:
     - `folder` — folder name as inline `<code>` (monospace 11px, background `var(--bg-2)`, 2px 5px padding, 3px radius)
     - `format` — `FLAC · 44.1 kHz · 16-bit · stereo`
     - `size` — total album size, monospace
     - `modified` — last filesystem mtime, monospace
     - `cover art` — `1500 × 1500 · JPEG · 24-bit · 842 KB` (monospace, tabular-num)
     - `cover source` — `embedded in FLAC (also cover.jpg present)` — note that the parenthetical is muted-mono-small.

2. **Audio analysis** (id `audio-analysis`, **closed** by default, auto-opens during a run):
   - Header meta: `[BPM ✓] [RG ✓]` style status pills (or `BPM —` / `RG —` if not yet run). Add a `[Detecting…]` accent pill while a run is active.
   - Body: `.analysis-block` panel containing two run cards (BPM + ReplayGain), each with:
     - Icon + label + brief description (`librosa, 22 kHz mono` / `pyloudnorm, −18 LUFS, EBU R128`)
     - State indicator — count of analyzed tracks, OR album_gain inline if RG done
     - Action button — `Detect BPM` / `Re-detect` (or `Calculate ReplayGain` / `Recalculate`)
   - When ReplayGain has been calculated, expands a small key-value strip under the card showing `album_gain` and `album_peak`.

3. **Tracks** (id `tracks`, **open** by default):
   - Header meta: `<n> tracks · <length>` (e.g. `26 · 1:21:30`).
   - Body: `.tracks` table.
   - Track row template (`.track-row`):
     - CSS grid columns: `[# 24px] [title 1fr] [bpm 56px*] [rg 64px*] [duration 56px]` — BPM/RG columns only added when `showBpm` / `showRg` is true.
     - Compilation albums add an `[artist 1fr]` column between title and bpm (template becomes `24px 1fr 1fr 56px 64px 56px`).
     - Track number: monospace 11px tabular-num, `var(--fg-4)`, right-aligned.
     - Title: 12.5px regular, `var(--fg)`. Truncates with ellipsis.
     - BPM value (when present): monospace 11px tabular-num, e.g. `132.0`, `var(--fg-2)`.
     - RG track gain: monospace 11px tabular-num, e.g. `−2.18 dB`, `var(--fg-2)` (use Unicode minus `−` not hyphen).
     - Duration: monospace 11px tabular-num, `var(--fg-3)`, right-aligned.
   - Hover: background `var(--bg-2)`.
   - Active row (during analysis or apply): subtle `var(--accent-soft)` background, accent left-border 2px.

#### 3c. Sheet foot (`.sheet-foot`)
- Sticky to bottom of sheet (always visible regardless of body scroll, because of the grid `1fr` middle row).
- 48px tall, padding `0 12px`, border-top `1px solid var(--line)`, background `var(--panel-2)`. Flex row, gap 8px.
- Default content (view mode):
  - Left: ghost button `Find cover art` (with magnifying glass + image icon)
  - Left: primary button `Suggest fixes` (with sparkles icon — runs the AI suggestion flow)
  - `<flex spacer>`
  - Right: ghost icon button `⋯` (More) with title `More` — toggles popover menu
- AI suggestion mode foot:
  - Ghost `Discard`, muted "3 fields · 12 files" label, ghost `Edit before apply`, primary `Apply 3 changes` (with check icon)
- Apply-in-progress mode foot:
  - Muted `Writing tags · keep this panel open`, flex spacer, disabled ghost `Cancel`

#### 3d. More menu popover (`.more-menu`)

Opens **above** the ⋯ button, anchored to its right edge. Closes on outside-click or Escape.

- Width: 240px min. Background `var(--panel)`, border `1px solid var(--line-2)`, border-radius 8px.
- Shadow: `0 8px 24px -4px rgba(0,0,0,0.35), 0 2px 6px rgba(0,0,0,0.18)`.
- Padding 4px. z-index 50.
- Entry animation: opacity 0→1 + translateY(2px→0) over 0.1s ease-out.
- Items (`.more-item`):
  - 30px tall, 9px gap, padding `6px 9px`, border-radius 5px.
  - Icon (13×13, color `var(--fg-3)` → `var(--fg-2)` on hover) + label + optional `kbd` shortcut on the right.
  - Hover: background `var(--bg-2)`, color `var(--fg)`.
- Three items:
  1. ✏️ `Rename folder…` — shortcut `⌘R` — opens the rename dialog
  2. 🔄 `Re-scan this album` — shortcut `⌘⇧R` — flips scenario to `"scanning"`
  3. ⚡ `Force re-analyze` — clears prior BPM/RG state and starts a fresh BPM analysis run

### 4. AI suggestion mode (scenario `detail-suggest`)

Shown inside the sheet body in place of the `<PropTable>` (album info). Rest of body unchanged.

- Block header: `AI suggestions` (monospace label) + voice mode chip — `Confident assistant` is the default ("clear before/after, confidence chips, low chrome").
- 3 suggestion rows (the mock uses these for *The Wall*):
  - `title` : ~~The Wall (Remastered)~~ → **The Wall** [confidence 98%]
  - `year` : ~~1979 (Remastered 2011)~~ → **1979** [confidence 92%]
  - `genre` : (empty) → **Progressive Rock** [confidence 88%]
- Row template:
  - Field name in monospace label (left, 80px column)
  - Before value: strikethrough, color `var(--fg-3)`
  - Arrow `→` in `var(--fg-4)`
  - After value: weight 500, color `var(--fg)`
  - Confidence chip: 18px tall pill, monospace 9.5px, background `color-mix(in oklch, var(--ok) 14%, transparent)`, color `var(--ok)`.
  - Per-row affordance: small ✓ accept / ✕ reject hover icons on the right.
- Footer (already covered above): Discard / Edit before apply / Apply 3 changes.

### 5. Apply progress (scenario `detail-applying`)

Replaces both album-info and tracks sections. `<AlbumHero>` stays at top.

- Top-of-body progress card (`<ApplyProgress>`):
  - Big monospace counter `7 / 12` writing tag updates.
  - Progress bar — 4px tall, accent fill, smooth transition.
  - Sub-label: `Writing FLAC tags · ETA 4s`.
- Below it, a track list (`<ApplyingTracks>`) where each row shows write status:
  - `done` — green check icon, row faded
  - `active` — accent spinner icon, row highlighted with accent background
  - `pending` — empty circle icon, default styling
  - `error` — red X icon, error background

### 6. Scanning state (scenario `scanning`)

Shown in the main canvas. Replaces some of the tile grid with a "scan ribbon" + skeleton tiles.

- **Scan ribbon (`.scan-ribbon`)** at top of main:
  - 56px tall, accent-tinted background `var(--accent-soft)`, monospace text.
  - Left: spinner icon + label `Scanning ~/Music…`.
  - Center: rolling file-path display (monospace 10.5px, ellipsis from left): `…Daft Punk - Discovery/02 Aerodynamic.flac` — cycles through file paths every 250ms.
  - Right: count `47 / 312 albums · 15%` + 4px progress bar across the very bottom of the ribbon.
- Tile grid below: first 4 cells show real tiles (already scanned), next 2 are `tile-scanning` (currently being processed), rest are `<SkeletonTile>` placeholders (12 of them).

### 7. Settings dialog (scenario `settings`)

Modal overlay. Width 720px, centered. `rgba(0,0,0,0.45)` backdrop.

- Head: `Settings / <active-tab>` breadcrumb + close button.
- Left tabs (sidebar within the dialog):
  - `Library` (folder icon)
  - `AI` (sparkles icon)
  - `Storage` (database icon)
  - `About` (info icon)
- Right content panel — see prototype for tab-specific layouts. Library tab shows: library directory input with browse button, scan-on-launch toggle, file-watch toggle, ignore-patterns textarea, supported-formats chips. Storage tab shows db path / size / album-count / track-count, cache sizes, and a "Clear caches" destructive button.
- Foot: ghost `Cancel` + primary `Save changes`.

### 8. Rename folder dialog

Opens when "Rename folder…" is clicked from the More menu. 560px wide, modal.

- Head: `Rename folder` title + close button.
- Body:
  - Helper text: `Renames the folder on disk. Tags and library references update automatically.`
  - `From` label (monospace 9.5px uppercase) + read-only `<code>` block showing current folder.
  - `To` label + editable monospace `<input>` pre-filled with current folder name. Auto-focused and selected on mount.
  - `Use suggested:` inline button below the input — accent-tinted dashed border, sparkles icon, fills the input with `${artist} - ${title} (${year}) [FLAC]`.
- Foot: ghost `Cancel` + primary `Rename` (with check icon, disabled if name unchanged or empty).
- Closes on backdrop click, Escape, or Cancel.

### 9. Tweaks panel

The host environment exposes a "Tweaks" toggle. When activated, a draggable panel appears with:

- Theme: Light / Dark radio
- Accent hue: 0–360° slider
- Density: Compact / Cozy / Spacious radio
- Tile size: 120–240 px slider
- Font pairing: Inter+JetBrains, Geist+Geist Mono, IBM Plex, Hanken+JetBrains
- AI voice: Confident / Cautious / Friendly radio
- Scenario picker: dropdown to jump between all the scenarios listed below

These are **prototype-only tools** — your implementation should pick a single set of values (the defaults below) unless theming is a real product requirement.

---

## Interactions & Behavior

### Navigation flow

```
[Library tile grid] —click tile→ [Album detail sheet] —Suggest fixes→ [AI suggestion mode]
                                                       —Apply 3→ [Apply progress] —done→ [view]
                                                       —⋯ → Rename / Re-scan / Force re-analyze
                                                       —close→ [Library]
[Library] —⌘, or Settings cog→ [Settings dialog]
```

### Animations and transitions

- **Sheet slide-in**: `transform: translateX(100% → 0)`, `0.22s cubic-bezier(.32,.72,.34,1)`. Backdrop fades in `0.18s ease`.
- **Tile hover**: `transform: scale(1.02)`, `0.15s ease-out`.
- **Section chevron**: `transform: rotate(0 → 90deg)`, `0.15s`.
- **More menu entry**: opacity + translateY, `0.1s ease-out`.
- **Scanning shimmer**: `1.4s linear infinite`, `background-position: -200% → 200%`.
- **Skeleton pulse**: opacity `0.4 → 1.0 → 0.4`, `1.5s ease-in-out infinite`.
- **Analysis row complete**: brief green flash on the row — `background-color` from `transparent → var(--ok-soft) → transparent` over 0.6s.
- **Progress bar fills**: `width` transition `0.3s ease`.

### Hover states

- All buttons: subtle background lift (e.g. `var(--bg-2)`).
- All clickable rows: background `var(--bg-2)`.
- Icon buttons: color `var(--fg-3) → var(--fg)`.

### Loading states

- **Initial library scan** (no albums yet): full-canvas skeleton grid, scan ribbon active.
- **Album detail loading** (rare — local FS is fast): brief shimmer over the body, no full skeleton.
- **Cover art loading**: image swap from gradient placeholder to real image with 0.2s opacity fade.

### Error states

- **Missing cover art**: error dot badge on tile, "Find cover art" CTA in detail sheet foot.
- **Tag write error during apply**: red X icon on the offending track row, error message in a toast at top of sheet body, partial-success summary in foot.
- **FLAC file unreadable**: tile rendered with diagonal-stripe overlay, hover tooltip explaining the error.

### Form validation

- **Rename folder input**: must be non-empty, no `/` or `:` characters, max 255 chars. Disable Rename button when invalid. Show validation message below input in `var(--err)` if user types invalid chars.

### Keyboard shortcuts

- `/` — focus search
- `Esc` — close sheet / dialog / menu
- `⌘R` — rename folder (when sheet open)
- `⌘⇧R` — re-scan album
- `⌘,` — open settings
- `←` / `→` — prev/next album within sheet (not yet implemented in mock, but design assumes it)
- `Space` — preview play (future, when audio playback is added)

### Responsive behavior

This is a desktop app — assume **min-width 1024px**. The sheet is fixed at 480px, sidebar fixed at 200px, main fills the rest. Below 1024px: sidebar collapses to 56px (icons only). The sheet covers the full main on small windows.

---

## State Management

### Global state

- `scenario`: enum — `"library"`, `"detail-view"`, `"detail-suggest"`, `"detail-applying"`, `"detail-analyzing-bpm"`, `"detail-analyzing-rg"`, `"settings"`, `"scanning"`.
- `selectedAlbumId`: string | null — drives which album the detail sheet shows.
- `tweaks`: theme/density/etc — not part of v1, but the architecture should allow user-pref overrides.

### Persistent state (per-user, e.g. SQLite or `electron-store`)

- Library root path
- Per-section open/closed state in the detail sheet (key: `mm.section.<section-id>`)
- View mode (grid / list) — currently grid only
- Window size / position
- Selected sidebar filter

### Per-album derived state

- `hasBpm`: boolean — true when at least one track has a BPM tag in the FLAC file
- `hasRg`: boolean — true when ReplayGain tags are present (`REPLAYGAIN_ALBUM_GAIN`, `REPLAYGAIN_TRACK_GAIN`)
- `analyzing`: `"bpm"` | `"rg"` | null — drives the in-progress UI
- `analysisDone`: number — count of tracks completed in the current analysis run

### Data fetching

- **Initial scan**: walk library root, for each subfolder containing `.flac` files, treat as an album. Read tags via FLAC tag library. Cache results in SQLite.
- **Cover art**: prefer embedded `METADATA_BLOCK_PICTURE`, fall back to `cover.jpg` / `folder.jpg` / `front.jpg` in folder. Generate 200px thumbnail, cache in app-data dir.
- **Re-scan album**: re-read tags + cover for one folder, update SQLite row, refresh the UI.
- **BPM analysis**: spawn `librosa.beat.beat_track` on each track (Python sidecar) or use a Rust BPM library. Write `BPM` tag to FLAC.
- **ReplayGain**: `pyloudnorm` (or `loudgain` / `rsgain` CLI). Compute album_gain across all tracks plus per-track gain. Write `REPLAYGAIN_*` tags.

---

## Design Tokens

All tokens are CSS custom properties on `:root` and `.dark`. Use OKLCH for any new color additions.

### Colors — light theme

```css
--bg:        #fafafa;   /* app background */
--bg-2:      #f3f3f3;   /* hover surfaces, subtle fills */
--bg-3:      #e8e8e8;   /* depressed surfaces, kbd backgrounds */
--panel:     #ffffff;   /* cards, sheets, dialogs */
--panel-2:   #fbfbfb;   /* sheet/dialog footers */
--titlebar:  #f5f5f5;
--line:      #e2e2e2;   /* hairlines */
--line-2:    #d4d4d4;   /* stronger borders, e.g. menu */
--fg:        #1a1a1a;   /* primary text */
--fg-2:      #3a3a3a;   /* secondary */
--fg-3:      #6e6e6e;   /* tertiary, mono labels */
--fg-4:      #9a9a9a;   /* quaternary, disabled */
--accent:    oklch(64% 0.13 var(--accent-h, 250));
--accent-soft: color-mix(in oklch, var(--accent) 12%, transparent);
--ok:        oklch(60% 0.14 145);
--warn:      oklch(72% 0.16 65);
--err:       oklch(60% 0.20 25);
```

### Colors — dark theme (`.dark`)

```css
--bg:        #0e0e0f;
--bg-2:      #18181a;
--bg-3:      #232325;
--panel:     #131316;
--panel-2:   #18181b;
--titlebar:  #0f0f10;
--line:      #26262a;
--line-2:    #34343a;
--fg:        #ededee;
--fg-2:      #c4c4c8;
--fg-3:      #8a8a92;
--fg-4:      #5c5c64;
--accent:    oklch(72% 0.15 var(--accent-h, 250));
```

### Typography

```css
--font-sans: "Inter", system-ui, sans-serif;       /* default UI */
--font-mono: "JetBrains Mono", ui-monospace, monospace; /* labels, paths, numbers */
```

Font pairings the user can switch between (provide as theme presets if relevant):
- `inter-jb` — Inter + JetBrains Mono (default)
- `geist-geistmono` — Geist + Geist Mono
- `plex` — IBM Plex Sans + IBM Plex Mono
- `hanken-jb` — Hanken Grotesk + JetBrains Mono

Type scale (px):
- `9.5` — micro labels (uppercase mono section headers, kbd hints)
- `10.5` — meta info, footnotes
- `11` — secondary mono (paths, durations, counts)
- `11.5` — small UI text
- `12` — buttons, body small
- `12.5` — primary body, tile titles
- `13` — secondary headers
- `15` — section header (Settings tabs etc.)
- `17` — album hero title

Apply `font-variant-numeric: tabular-nums` to all numeric monospace values (durations, sizes, BPM, RG dB).

Use `text-wrap: pretty` on multi-line copy.

### Spacing

Follow a 4px base. Density modes scale gaps and padding:

| Token | Compact | Cozy | Spacious |
|---|---|---|---|
| `--row-gap` | 2px | 4px | 6px |
| `--grid-gap` | 12px | 16px | 20px |
| `--pad-1` | 6px | 8px | 10px |
| `--pad-2` | 10px | 14px | 18px |

### Border radius

```css
--r-1: 3px;   /* code blocks, kbd */
--r-2: 5px;   /* buttons, inputs, menu items */
--r-3: 6px;   /* tiles, panels */
--r-4: 8px;   /* dialogs, popovers */
```

### Shadows

```css
--shadow-sm:    0 1px 2px rgba(0,0,0,0.06);
--shadow-menu:  0 8px 24px -4px rgba(0,0,0,0.35), 0 2px 6px rgba(0,0,0,0.18);
--shadow-sheet: -8px 0 24px rgba(0,0,0,0.18);
--shadow-dialog: 0 16px 48px rgba(0,0,0,0.30);
```

---

## Assets

- **Icons**: lucide-style stroked SVGs at 24×24 viewBox, `stroke-width: 2`, `stroke-linecap: round`, `stroke-linejoin: round`. Color via `currentColor`. See `icons.jsx` for the inline set used in the prototype — the production app should consume `lucide-react` (or equivalent) directly.
- **Cover art**: in the prototype, mocked as CSS gradients. In production, real album art from FLAC embedded picture blocks or sidecar `cover.jpg`. Generate 200px thumbnails for tile grid, 96px for hero, and keep originals for full-screen view (future).
- **Fonts**: load Inter, JetBrains Mono, Geist, Geist Mono, Hanken Grotesk, IBM Plex Sans, IBM Plex Mono via Google Fonts (or self-host for offline).

---

## Files

The bundled prototype source files (in this handoff folder under `prototype/`):

- `Media Manager.html` — entry HTML, loads React + Babel + the JSX bundle
- `app.jsx` — main app component tree (App, Titlebar, Sidebar, FilterBar, Tile grid, DetailSheet with all sub-components, Settings, etc.)
- `icons.jsx` — inline SVG icon set
- `tweaks-panel.jsx` — prototype-only theme/density/scenario controls
- `data.js` — mock album + track data (Pink Floyd *The Wall* with full tracklist; Miles Davis *Kind of Blue* with full tracklist; plus ~10 other albums for the wall)
- `styles.css` — all design tokens and component styles
- `combined.jsx` — concatenation of icons + tweaks + app, used by the runtime loader

**For implementation**: focus on `app.jsx` for component structure and `styles.css` for the design tokens. The JSX uses inline event handlers and React hooks (`useState`, `useEffect`, `useMemo`, `useRef`); these patterns translate cleanly to a real codebase but should be expressed via the target framework's idioms.

The mock animation timings (e.g. `setInterval` driving `analysisDone` from 0 → tracks.length) exist purely so the prototype demos all states without backend wiring; replace these with real subprocess/IPC events in production.
