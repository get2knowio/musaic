/* Media Manager — main app (React + Babel) */
const { useState, useEffect, useRef, useMemo } = React;

const COVERS = window.MEDIA_DATA.COVERS;
const WALL_TRACKS = window.MEDIA_DATA.WALL_TRACKS;
const KOB_TRACKS = window.MEDIA_DATA.KOB_TRACKS;
const WALL_ALBUM_RG = window.MEDIA_DATA.WALL_ALBUM_RG;
const KOB_ALBUM_RG = window.MEDIA_DATA.KOB_ALBUM_RG;

/* ---------- TWEAKS ---------- */
const TWEAK_DEFAULTS = /*EDITMODE-BEGIN*/{
  "theme": "dark",
  "accentHue": 250,
  "density": "roomy",
  "tileMin": 184,
  "fontPairing": "iosevka",
  "aiVoice": "confident",
  "scenario": "library"
}/*EDITMODE-END*/;

const FONT_PAIRINGS = {
  "inter-jb": { sans: "'Inter', system-ui, sans-serif", mono: "'JetBrains Mono', ui-monospace, monospace", label: "Inter / JetBrains Mono" },
  "geist":    { sans: "'Geist', system-ui, sans-serif",  mono: "'Geist Mono', ui-monospace, monospace",     label: "Geist / Geist Mono" },
  "hanken":   { sans: "'Hanken Grotesk', system-ui, sans-serif", mono: "'JetBrains Mono', ui-monospace, monospace", label: "Hanken Grotesk / JB Mono" },
  "iosevka":  { sans: "'IBM Plex Sans', system-ui, sans-serif",  mono: "'IBM Plex Mono', ui-monospace, monospace", label: "IBM Plex Sans / Mono" },
};

const DENSITY = {
  compact:  { rowH: 24, padX: 12, padY: 8,  fsSm: 11.5, fsMd: 12.5, fsLg: 14 },
  standard: { rowH: 28, padX: 14, padY: 10, fsSm: 12,   fsMd: 13,   fsLg: 15 },
  roomy:    { rowH: 32, padX: 18, padY: 12, fsSm: 12.5, fsMd: 14,   fsLg: 16 },
};

/* ---------- HELPERS ---------- */
function CoverArt({ cover, className, style }) {
  return (
    <div className={"art " + (className||"")} style={{ background: cover.art, ...(style||{}) }}>
      {/* faint sheen */}
      <div style={{position:"absolute", inset:0, background:"linear-gradient(160deg, rgba(255,255,255,0.06), rgba(0,0,0,0.15))"}}/>
      {/* fake center label for that record-on-shelf feel */}
      <div style={{position:"absolute", inset:"34% 34%", borderRadius:"50%", background:"radial-gradient(circle at 30% 30%, rgba(255,255,255,0.15), transparent 60%)", border:"1px solid rgba(0,0,0,0.2)"}}/>
    </div>
  );
}

function Tile({ cover, onClick, selected, scanning }) {
  return (
    <article className={`tile ${selected?"selected":""} ${scanning?"tile-scanning":""}`} onClick={onClick}>
      <div className="tile-cover">
        <CoverArt cover={cover} />
        {cover.compilation && <span className="cover-badge compilation">VA</span>}
        {cover.needsArt && <span className="cover-badge warn">no art</span>}
        {cover.needsFix && <span className="cover-badge suggested">3 fixes</span>}
      </div>
      <div className="tile-info">
        <div className="tile-title">{cover.title}</div>
        <div className="tile-artist">{cover.artist}</div>
        <div className="tile-meta">
          <span>{cover.year}</span>
          <span>·</span>
          <span>{cover.tracks} tr</span>
          <span>·</span>
          <span>{cover.length}</span>
        </div>
      </div>
    </article>
  );
}

function SkeletonTile() {
  return (
    <article className="tile tile-skel">
      <div className="tile-cover" />
      <div className="skel-line" />
      <div className="skel-line short" />
      <div className="skel-line shorter" />
    </article>
  );
}

/* ---------- TITLEBAR + SIDEBAR ---------- */
function Titlebar({ tweaks, setTweak, onOpenSettings, onToggleTweaks }) {
  return (
    <header className="titlebar">
      <div className="tb-dots">
        <div className="tb-dot r"/><div className="tb-dot y"/><div className="tb-dot g"/>
      </div>
      <div className="tb-brand">
        <div className="tb-brand-mark">M</div>
        <span>Media Manager</span>
      </div>
      <div className="tb-path">
        <span className="sep">~/</span>
        <span>Music</span>
        <span className="sep">/</span>
        <span className="here">Library</span>
      </div>
      <div className="tb-spacer" />
      <div className="tb-search">
        <I.Search style={{width:13, height:13, color:"var(--fg-3)"}}/>
        <input placeholder="Filter albums, artists, tracks…"/>
        <span className="kbd">/</span>
      </div>
      <div className="tb-toggle">
        <button className="on"><I.Grid style={{width:12, height:12}}/></button>
        <button><I.List style={{width:12, height:12}}/></button>
      </div>
      <button className="tb-btn">
        <I.Refresh style={{width:13, height:13}}/>
        <span>Rescan</span>
      </button>
      <button className="tb-btn icon" onClick={() => setTweak('theme', tweaks.theme === 'dark' ? 'light' : 'dark')} title="Toggle theme">
        {tweaks.theme === 'dark' ? <I.Sun style={{width:13, height:13}}/> : <I.Moon style={{width:13, height:13}}/>}
      </button>
      <button className="tb-btn icon" onClick={onOpenSettings} title="Settings">
        <I.Settings style={{width:13, height:13}}/>
      </button>
    </header>
  );
}

function Sidebar({ scenario, setScenario }) {
  const items = [
    { key: "library",    label: "Library",     icon: I.Disc, count: 312, active: true },
    { key: "playlists",  label: "Playlists",   icon: I.ListMusic, later: true },
    { key: "plex",       label: "Plex sync",   icon: I.Plex, later: true },
    { key: "history",    label: "Scan history",icon: I.History, later: true },
  ];
  return (
    <nav className="sidebar">
      <div className="sb-section">Workspace</div>
      {items.map(it => (
        <div key={it.key} className={`sb-item ${it.active?"active":""} ${it.later?"disabled":""}`}>
          <it.icon style={{width:13, height:13}}/>
          <span>{it.label}</span>
          {it.count !== undefined && <span className="count">{it.count}</span>}
          {it.later && <span className="later">v1.1</span>}
        </div>
      ))}

      <div className="sb-section">Filters</div>
      <div className="sb-chip"><span className="dot" style={{background:"var(--accent)"}}/>All albums<span className="count">312</span></div>
      <div className="sb-chip"><span className="dot" style={{background:"var(--warn)"}}/>Needs review<span className="count">14</span></div>
      <div className="sb-chip"><span className="dot" style={{background:"var(--err)"}}/>Missing art<span className="count">3</span></div>
      <div className="sb-chip"><span className="dot" style={{background:"var(--ok)"}}/>Compilations<span className="count">22</span></div>

      <div className="sb-section">Scenarios</div>
      {[
        ["library", "Library"],
        ["scanning", "Scanning"],
        ["detail-view", "Album detail"],
        ["detail-suggest", "AI suggestion"],
        ["detail-applying", "Applying"],
        ["detail-analyzing-bpm", "Detecting BPM"],
        ["detail-analyzing-rg", "Calc ReplayGain"],
        ["settings", "Settings"],
      ].map(([k, label]) => (
        <div key={k} className={`sb-chip ${scenario===k?"active":""}`}
             style={scenario===k?{background:"var(--accent-soft)", color:"var(--accent)"}:{}}
             onClick={() => setScenario(k)}>
          <span className="dot" style={{background: scenario===k?"var(--accent)":"var(--fg-4)"}}/>
          {label}
        </div>
      ))}

      <div className="sb-spacer"/>
      <div className="sb-section">Account</div>
      <div className="sb-item">
        <I.Settings style={{width:13, height:13}}/>
        <span>Settings</span>
        <span className="count">,</span>
      </div>
    </nav>
  );
}

/* ---------- FILTER BAR ---------- */
function FilterBar({ scanning }) {
  return (
    <div className="filterbar">
      <span className="fb-stat"><b>312</b> albums</span>
      <span className="muted mono-small">·</span>
      <span className="fb-stat mono">~/Music</span>
      <span className="muted mono-small">·</span>
      <span className="fb-stat">last scan <b className="mono">14:22</b></span>

      <div style={{flex:1}}/>

      <span className="fb-pill on">Year: 1970s <span className="x">×</span></span>
      <span className="fb-pill">+ filter</span>
      <button className="fb-sort">
        <span style={{color:"var(--fg-4)"}}>Sort</span>
        <span>Artist ↑</span>
        <I.ChevronDown style={{width:12, height:12, color:"var(--fg-3)"}}/>
      </button>
    </div>
  );
}

/* ---------- SCAN RIBBON ---------- */
function ScanRibbon() {
  const [pct, setPct] = useState(15);
  const [count, setCount] = useState(47);
  const [activeFile, setActiveFile] = useState("Pink Floyd - The Wall/06 Mother.flac");
  const sample = [
    "Pink Floyd - The Wall/06 Mother.flac",
    "Pink Floyd - The Wall/07 Goodbye Blue Sky.flac",
    "Pink Floyd - The Wall/08 Empty Spaces.flac",
    "Joni Mitchell - Blue/03 Little Green.flac",
    "Joni Mitchell - Blue/04 Carey.flac",
    "Radiohead/OK Computer/05 Let Down.flac",
    "Daft Punk - Discovery/02 Aerodynamic.flac",
  ];
  useEffect(() => {
    let i = 0;
    const t = setInterval(() => {
      i = (i+1) % sample.length;
      setActiveFile(sample[i]);
      setCount(c => Math.min(c+3, 312));
      setPct(p => Math.min(p+0.9, 100));
    }, 900);
    return () => clearInterval(t);
  }, []);
  return (
    <div className="scan-ribbon">
      <div className="left">
        <span className="pulse"/>
        <span style={{fontWeight:500}}>Scanning library…</span>
        <span className="meta tnum">{count} <span style={{color:"var(--fg-4)"}}>of</span> 312</span>
      </div>
      <div>
        <div className="progress">
          <div className="bar"><span style={{width: pct+"%"}}/></div>
          <div className="meta tnum">{pct.toFixed(0)}% · ~{Math.max(5, Math.round((100-pct)*1.4))}s</div>
        </div>
        <div className="live" style={{marginTop:4}}>
          <span style={{color:"var(--fg-4)"}}>›</span> reading <b>{activeFile}</b>
        </div>
      </div>
      <div className="right">
        <button className="btn sm ghost">Pause</button>
        <button className="btn sm">Cancel</button>
      </div>
    </div>
  );
}

/* ---------- ALBUM DETAIL SHEET ---------- */
function AlbumHero({ cover, badges }) {
  return (
    <div className="album-hero">
      <div className="album-hero-cover"><CoverArt cover={cover}/></div>
      <div className="album-hero-info">
        <div className="eyebrow">
          <span>Album</span>
          {cover.compilation && <span className="tag compilation">Compilation</span>}
          {cover.needsFix && <span className="tag warn">3 issues</span>}
          {!cover.needsFix && !cover.needsArt && <span className="tag ok">verified</span>}
        </div>
        <h2>{cover.title}</h2>
        <div className="artist">{cover.artist}</div>
        <div className="stats">
          <span><b>{cover.year}</b></span>
          <span><b className="tnum">{cover.tracks}</b> tracks</span>
          <span className="tnum">{cover.length}</span>
          <span>{cover.genre}</span>
        </div>
      </div>
    </div>
  );
}

function PropTable({ cover }) {
  // Per-album cover-art metadata (mock but plausible).
  const coverMeta = {
    dsotm: { dims: "1500 × 1500", size: "842 KB", fmt: "JPEG", depth: "24-bit" },
    kob:   { dims: "1400 × 1400", size: "1.2 MB",  fmt: "PNG",  depth: "24-bit" },
    wall:  { dims: "3000 × 3000", size: "2.4 MB",  fmt: "JPEG", depth: "24-bit" },
    abbey: { dims: "1600 × 1600", size: "1.0 MB",  fmt: "JPEG", depth: "24-bit" },
  };
  const cm = coverMeta[cover.id] || { dims: "1200 × 1200", size: "640 KB", fmt: "JPEG", depth: "24-bit" };
  return (
    <dl className="props">
      <dt>folder</dt><dd><code>{cover.folder}</code></dd>
      <dt>format</dt><dd>FLAC · 44.1 kHz · 16-bit · stereo</dd>
      <dt>size</dt><dd className="mono">312.4 MB</dd>
      <dt>modified</dt><dd className="mono">2024-08-12 · 14:02</dd>
      <dt>cover art</dt><dd className="mono tnum">{cm.dims} · {cm.fmt} · {cm.depth} · {cm.size}</dd>
      <dt>cover source</dt><dd>embedded in FLAC <span className="muted mono-small">(also cover.jpg present)</span></dd>
    </dl>
  );
}

function TrackRow({ n, title, dur, artist, status, compilation, applying, active, bpm, rgGain, analysisMode }) {
  let icon = null;
  if (applying) {
    if (status === "done") icon = <I.CheckCircle style={{width:13, height:13}}/>;
    else if (status === "active") icon = <I.Loader style={{width:13, height:13}} className="spin"/>;
    else if (status === "error") icon = <I.XCircle style={{width:13, height:13}}/>;
    else icon = <I.Circle style={{width:8, height:8}}/>;
  }
  // Analysis-mode in-progress states
  let analysisIcon = null;
  if (analysisMode) {
    if (status === "done") analysisIcon = <I.CheckCircle style={{width:12, height:12, color:"var(--ok)"}}/>;
    else if (status === "active") analysisIcon = <I.Loader style={{width:12, height:12, color:"var(--accent)"}} className="spin"/>;
    else analysisIcon = <I.Circle style={{width:7, height:7, color:"var(--fg-4)"}}/>;
  }
  return (
    <div className={`track-row ${compilation?"compilation":""} ${applying?"applying":""} ${analysisMode?"analyzing":""} ${status||""} ${active?"active":""}`}>
      {applying && <span className={`status-icon ${status||"pending"}`}>{icon}</span>}
      {analysisMode && <span className={`status-icon ${status||"pending"}`}>{analysisIcon}</span>}
      <span className="num tnum">{n}</span>
      <span className="title">{title}</span>
      {compilation && <span className="track-artist">{artist}</span>}
      {bpm !== undefined && (
        <span className={`tk-bpm tnum ${bpm===null?"empty":""}`}>{bpm===null ? "—" : bpm.toFixed(1)}</span>
      )}
      {rgGain !== undefined && (
        <span className={`tk-rg tnum ${rgGain===null?"empty":""}`}>{rgGain===null ? "—" : rgGain}</span>
      )}
      <span className="duration tnum">{dur}</span>
    </div>
  );
}

function TrackList({ tracks, compilation, showBpm, showRg, analysisMode, analysisDone }) {
  // Determine column template via class on tracks
  const cols = [
    "num",
    "title",
    compilation && "artist",
    showBpm && "bpm",
    showRg && "rg",
    "dur",
  ].filter(Boolean).join("-");

  return (
    <div className={`tracks tracks-cols-${cols} ${analysisMode?"in-analysis":""}`}>
      <div className={`tracks-head ${compilation?"compilation":""}`}>
        {analysisMode && <span/>}
        <span style={{textAlign:"right"}}>#</span>
        <span>Title</span>
        {compilation && <span>Artist</span>}
        {showBpm && <span className="col-bpm">BPM</span>}
        {showRg && <span className="col-rg">RG track</span>}
        <span className="duration">Length</span>
      </div>
      {tracks.map((t, i) => {
        // tracks shape: [title, dur, bpm, rgGain, rgPeak]
        let trackBpm = showBpm ? (t[2] ?? null) : undefined;
        let trackRg  = showRg  ? (t[3] ?? null) : undefined;
        // Hide unanalyzed values during analysis until that row's "done"
        let status;
        if (analysisMode) {
          if (i < analysisDone) status = "done";
          else if (i === analysisDone) status = "active";
          else status = "pending";
          if (status !== "done") {
            if (analysisMode === "bpm") trackBpm = null;
            if (analysisMode === "rg") trackRg = null;
          }
        }
        return (
          <TrackRow
            key={i}
            n={i+1}
            title={t[0]}
            dur={t[1]}
            artist={t[2]||""}
            compilation={compilation}
            bpm={trackBpm}
            rgGain={trackRg}
            analysisMode={!!analysisMode}
            status={status}
          />
        );
      })}
    </div>
  );
}

/* ---------- AI SUGGESTION ---------- */
function AISuggestion({ voice }) {
  const diffs = [
    { field: "title",  before: "The Wall (Remastered)", after: "The Wall" },
    { field: "year",   before: "2011-09", after: "1979" },
    { field: "genre",  before: "Rock", after: "Progressive Rock" },
  ];
  if (voice === "neutral") {
    return (
      <div className="ai-block voice-neutral">
        <div className="ai-head">
          <span className="icon"><I.Sparkles style={{width:14, height:14}}/></span>
          <span className="label">3 changes proposed</span>
          <div className="meta">
            <span className="confidence high">conf 0.92</span>
            <span>·</span>
            <span>haiku-4-5 · 1.4s</span>
          </div>
        </div>
        <div className="diffs">
          {diffs.map(d => (
            <div className="diff" key={d.field}>
              <div className="field">{d.field}</div>
              <div className="vals">
                <div className="before">{d.before}</div>
                <div className="after">{d.after}</div>
              </div>
            </div>
          ))}
        </div>
        <div className="ai-notes">
          Verified via MusicBrainz release <code>f3b87a85-d5fc-4c12-a9f2-3a5b1e8d7c6e</code> — original 1979 Harvest release. Track count and titles match.
        </div>
      </div>
    );
  }
  if (voice === "conversational") {
    return (
      <div className="ai-block voice-conversational">
        <div className="ai-head">
          <span className="icon"><I.Sparkles style={{width:14, height:14}}/></span>
          <span className="label">Claude</span>
          <div className="meta">
            <span className="confidence high">High confidence</span>
          </div>
        </div>
        <div className="speech">
          This looks like the original 1979 Harvest release of <b style={{fontStyle:"normal"}}>The Wall</b>, not the 2011 remaster — the track count and titles match the original. I'd suggest correcting the year and dropping "(Remastered)" from the title; "Progressive Rock" is also a more accurate genre than just "Rock."
        </div>
        <div className="diffs">
          {diffs.map(d => (
            <div className="diff" key={d.field}>
              <div className="field">{d.field}</div>
              <div className="vals">
                <div className="before">{d.before}</div>
                <div className="after">{d.after}</div>
              </div>
            </div>
          ))}
        </div>
        <div className="ai-notes">
          Source: MusicBrainz <code>f3b87a85-d5fc-…</code>
        </div>
      </div>
    );
  }
  // confident (default)
  return (
    <div className="ai-block">
      <div className="ai-head">
        <span className="icon"><I.Sparkles style={{width:14, height:14}}/></span>
        <span className="label">3 metadata fixes</span>
        <div className="meta">
          <span className="confidence high">high · 0.92</span>
        </div>
      </div>
      <div className="diffs">
        {diffs.map(d => (
          <div className="diff" key={d.field}>
            <div className="field">{d.field}</div>
            <div className="vals">
              <div className="before">{d.before}</div>
              <div className="after">{d.after}</div>
            </div>
          </div>
        ))}
      </div>
      <div className="ai-notes">
        <b>Reasoning:</b> Track count and titles match MusicBrainz release <code>f3b87a85-d5fc-…</code> — the 1979 Harvest original, not the 2011 remaster.
      </div>
    </div>
  );
}

/* ---------- APPLY PROGRESS ---------- */
function ApplyProgress({ done, total }) {
  const pct = Math.round((done/total)*100);
  return (
    <div className="apply-block">
      <div className="apply-line">
        <I.CheckCircle style={{width:14, height:14, color:"var(--ok)"}}/>
        <span className="label">Cover art</span>
        <span className="meta">downloaded from <code style={{fontFamily:"var(--font-mono)", fontSize:10, padding:"1px 4px", background:"var(--bg-3)", borderRadius:2}}>coverartarchive.org</code> · 212 KB</span>
        <span className="badge done">done</span>
      </div>
      <div className="apply-line">
        <I.Loader style={{width:14, height:14, color:"var(--accent)"}} className="spin"/>
        <span className="label">Writing tags</span>
        <span className="meta tnum">{done} of {total} files</span>
        <span className="badge active">{pct}%</span>
      </div>
      <div className="apply-progress">
        <div className="bar"><span style={{width: pct+"%"}}/></div>
      </div>
      <div className="file-strip" title="Per-file status">
        {Array.from({length: total}).map((_, i) => {
          let cls = "cell";
          if (i < done) cls += " done";
          else if (i === done) cls += " active";
          return <div key={i} className={cls} title={`Track ${i+1}`}/>
        })}
      </div>
    </div>
  );
}

function ApplyingTracks({ done, total, tracks }) {
  return (
    <div className="tracks">
      <div className="tracks-head applying">
        <span/>
        <span style={{textAlign:"right"}}>#</span>
        <span>Title</span>
        <span className="duration">Length</span>
      </div>
      {tracks.map((t, i) => {
        let status = "pending";
        if (i < done) status = "done";
        else if (i === done) status = "active";
        return <TrackRow key={i} n={i+1} title={t[0]} dur={t[1]} status={status} applying/>;
      })}
    </div>
  );
}

/* ---------- COLLAPSIBLE SECTION ---------- */
function Section({ id, title, meta, defaultOpen, children }) {
  const storageKey = "mm.section." + id;
  const [open, setOpen] = useState(() => {
    try {
      const v = localStorage.getItem(storageKey);
      if (v === "1") return true;
      if (v === "0") return false;
    } catch (e) {}
    return !!defaultOpen;
  });
  useEffect(() => {
    try { localStorage.setItem(storageKey, open ? "1" : "0"); } catch (e) {}
  }, [open, storageKey]);
  return (
    <section className={`mm-section ${open?"open":"closed"}`}>
      <button className="mm-section-head" onClick={() => setOpen(o => !o)} aria-expanded={open}>
        <I.ChevronRight className={`mm-section-caret ${open?"down":""}`} style={{width:11, height:11}}/>
        <span className="mm-section-title">{title}</span>
        {meta && <span className="mm-section-meta">{meta}</span>}
      </button>
      {open && <div className="mm-section-body">{children}</div>}
    </section>
  );
}

/* ---------- ANALYSIS PANEL (BPM + ReplayGain) ---------- */
function AnalysisPanel({ cover, albumRg, hasBpm, hasRg, onRunBpm, onRunRg, analyzing }) {
  const isBpm = analyzing === "bpm";
  const isRg = analyzing === "rg";
  return (
    <div className="analysis-block">
      <div className="analysis-head">
        <I.Activity style={{width:13, height:13, color:"var(--fg-3)"}}/>
        <span className="label">Audio analysis</span>
        <span className="muted mono-small">deterministic · local</span>
      </div>

      <div className="analysis-row">
        <div className="analysis-meta">
          <div className="title-line">
            <span className="proc">BPM</span>
            <span className="muted mono-small">librosa · 22 kHz mono</span>
          </div>
          <div className="sub mono-small">
            writes <code>bpm</code> Vorbis tag · per track
          </div>
        </div>
        <div className="analysis-stat">
          {hasBpm
            ? <span className="ok-pill"><I.CheckCircle style={{width:11, height:11}}/> {cover.tracks} tracks tagged</span>
            : <span className="muted mono-small">not analyzed</span>}
        </div>
        <button className={`btn small ${isBpm?"running":""}`} onClick={onRunBpm} disabled={!!analyzing}>
          {isBpm
            ? <><I.Loader style={{width:11, height:11}} className="spin"/> Detecting…</>
            : (hasBpm ? "Re-detect" : "Detect BPM")}
        </button>
      </div>

      <div className="analysis-row">
        <div className="analysis-meta">
          <div className="title-line">
            <span className="proc">ReplayGain 2.0</span>
            <span className="muted mono-small">pyloudnorm · −18 LUFS · EBU R128</span>
          </div>
          <div className="sub mono-small">
            writes <code>replaygain_track_gain</code> · <code>track_peak</code> · <code>album_gain</code> · <code>album_peak</code>
          </div>
        </div>
        <div className="analysis-stat">
          {hasRg
            ? <span className="ok-pill"><I.CheckCircle style={{width:11, height:11}}/> album <span className="tnum">{albumRg.gain}</span></span>
            : <span className="muted mono-small">not analyzed</span>}
        </div>
        <button className={`btn small ${isRg?"running":""}`} onClick={onRunRg} disabled={!!analyzing}>
          {isRg
            ? <><I.Loader style={{width:11, height:11}} className="spin"/> Measuring…</>
            : (hasRg ? "Recalculate" : "Calculate ReplayGain")}
        </button>
      </div>

      {hasRg && !isRg && (
        <dl className="rg-album">
          <dt>album_gain</dt><dd className="mono tnum">{albumRg.gain}</dd>
          <dt>album_peak</dt><dd className="mono tnum">{albumRg.peak}</dd>
        </dl>
      )}
    </div>
  );
}

/* ---------- SHEET CONTAINER ---------- */
function DetailSheet({ scenario, cover, onClose, onSetScenario }) {
  const isApply = scenario === "detail-applying";
  const isSuggest = scenario === "detail-suggest";
  const isAnalyzeBpm = scenario === "detail-analyzing-bpm";
  const isAnalyzeRg = scenario === "detail-analyzing-rg";
  const analyzing = isAnalyzeBpm ? "bpm" : isAnalyzeRg ? "rg" : null;

  const tracks = cover.id === "kob" ? KOB_TRACKS : WALL_TRACKS;
  const albumRg = cover.id === "kob" ? KOB_ALBUM_RG : WALL_ALBUM_RG;
  // Show pre-analyzed values only on "verified" album (Kind of Blue) by default
  const [hasBpm, setHasBpm] = useState(cover.id === "kob");
  const [hasRg, setHasRg]   = useState(cover.id === "kob");
  const [moreOpen, setMoreOpen] = useState(false);
  const [renameOpen, setRenameOpen] = useState(false);

  const [done, setDone] = useState(7);
  const [analysisDone, setAnalysisDone] = useState(0);

  // Apply progress
  useEffect(() => {
    if (!isApply) return;
    setDone(7);
    const t = setInterval(() => {
      setDone(d => (d >= tracks.length ? 7 : d+1));
    }, 1100);
    return () => clearInterval(t);
  }, [isApply, scenario]);

  // Analysis progress
  useEffect(() => {
    if (!analyzing) return;
    setAnalysisDone(0);
    const t = setInterval(() => {
      setAnalysisDone(d => {
        if (d >= tracks.length) {
          clearInterval(t);
          if (analyzing === "bpm") setHasBpm(true);
          if (analyzing === "rg") setHasRg(true);
          // Return to view mode after a beat
          setTimeout(() => onSetScenario && onSetScenario("detail-view"), 700);
          return d;
        }
        return d + 1;
      });
    }, 350);
    return () => clearInterval(t);
  }, [analyzing, scenario]);

  return (
    <>
      <div className={`sheet-backdrop open`} onClick={onClose}/>
      <aside className={`sheet open`}>
        <div className="sheet-head">
          <button className="btn icon ghost" onClick={onClose}><I.ChevronRight style={{width:14, height:14}}/></button>
          <div className="crumb">
            <span>Library</span>
            <I.ChevronRight style={{width:10, height:10}}/>
            <span className="here">{cover.title}</span>
          </div>
          <div className="nav">
            <button className="btn icon ghost" title="Previous"><I.ChevronLeft style={{width:14, height:14}}/></button>
            <button className="btn icon ghost" title="Next"><I.ChevronRight style={{width:14, height:14}}/></button>
            <button className="btn icon ghost" onClick={onClose} title="Close"><I.Close style={{width:14, height:14}}/></button>
          </div>
        </div>
        <div className="sheet-body">
          {/* AlbumHero stays uncollapsed — it's the identity of the album. */}
          <AlbumHero cover={cover}/>
          {isSuggest && <AISuggestion voice={window._tweakAiVoice||"confident"}/>}
          {isApply && <ApplyProgress done={done} total={tracks.length}/>}

          {!isApply && (
            <Section
              id="album-info"
              title="Album info"
              defaultOpen={false}
              meta={<span className="mono">FLAC · 44.1 kHz · 16-bit · 312.4 MB</span>}
            >
              <PropTable cover={cover}/>
            </Section>
          )}

          {!isApply && !isSuggest && (
            <Section
              id="audio-analysis"
              title="Audio analysis"
              defaultOpen={!!analyzing}
              meta={
                <span className="mono section-meta-pills">
                  <span className={`pill ${hasBpm?"on":""}`}>BPM {hasBpm ? "✓" : "—"}</span>
                  <span className={`pill ${hasRg?"on":""}`}>RG {hasRg ? "✓" : "—"}</span>
                  {analyzing && <span className="pill running">{analyzing==="bpm"?"Detecting…":"Measuring…"}</span>}
                </span>
              }
            >
              <AnalysisPanel
                cover={cover}
                albumRg={albumRg}
                hasBpm={hasBpm}
                hasRg={hasRg}
                analyzing={analyzing}
                onRunBpm={() => onSetScenario && onSetScenario("detail-analyzing-bpm")}
                onRunRg={() => onSetScenario && onSetScenario("detail-analyzing-rg")}
              />
            </Section>
          )}

          {isApply
            ? <ApplyingTracks done={done} total={tracks.length} tracks={tracks}/>
            : (
              <Section
                id="tracks"
                title="Tracks"
                defaultOpen={true}
                meta={<span className="mono tnum">{tracks.length} · {cover.length}</span>}
              >
                <TrackList
                  tracks={tracks}
                  compilation={cover.compilation}
                  showBpm={hasBpm || isAnalyzeBpm}
                  showRg={hasRg || isAnalyzeRg}
                  analysisMode={analyzing}
                  analysisDone={analysisDone}
                />
              </Section>
            )}
        </div>
        <div className="sheet-foot">
          {isSuggest ? (
            <>
              <button className="btn ghost">Discard</button>
              <span className="muted mono-small" style={{marginLeft:"auto"}}>3 fields · 12 files</span>
              <button className="btn">Edit before apply</button>
              <button className="btn primary">
                <I.Check style={{width:13, height:13}}/>
                Apply 3 changes
              </button>
            </>
          ) : isApply ? (
            <>
              <span className="muted mono-small">Writing tags · keep this panel open</span>
              <span style={{flex:1}}/>
              <button className="btn ghost" disabled>Cancel</button>
            </>
          ) : (
            <>
              <button className="btn primary">
                <I.Sparkles style={{width:13, height:13}}/>
                Suggest fixes
              </button>
              <button className="btn">
                <I.Image style={{width:13, height:13}}/>
                Find cover art
              </button>
              <span style={{flex:1}}/>
              <div className="more-wrap">
                <button
                  className={`btn icon ghost ${moreOpen?"active":""}`}
                  title="More"
                  onClick={() => setMoreOpen(o => !o)}
                  aria-haspopup="menu"
                  aria-expanded={moreOpen}
                >
                  <I.More style={{width:14, height:14}}/>
                </button>
                {moreOpen && (
                  <MoreMenu
                    onClose={() => setMoreOpen(false)}
                    onRename={() => { setMoreOpen(false); setRenameOpen(true); }}
                    onRescan={() => { setMoreOpen(false); onSetScenario && onSetScenario("scanning"); }}
                    onForceAnalyze={() => {
                      setMoreOpen(false);
                      // Reset analysis state and start a fresh BPM run
                      setHasBpm(false); setHasRg(false);
                      onSetScenario && onSetScenario("detail-analyzing-bpm");
                    }}
                  />
                )}
              </div>
            </>
          )}
        </div>
      </aside>
      {renameOpen && (
        <RenameDialog cover={cover} onClose={() => setRenameOpen(false)}/>
      )}
    </>
  );
}

/* ---------- MORE MENU (album overflow) ---------- */
function MoreMenu({ onClose, onRename, onRescan, onForceAnalyze }) {
  const ref = useRef(null);
  useEffect(() => {
    function handle(e) {
      if (ref.current && !ref.current.contains(e.target)) onClose();
    }
    function key(e) { if (e.key === "Escape") onClose(); }
    document.addEventListener("mousedown", handle);
    document.addEventListener("keydown", key);
    return () => {
      document.removeEventListener("mousedown", handle);
      document.removeEventListener("keydown", key);
    };
  }, [onClose]);
  return (
    <div ref={ref} className="more-menu" role="menu">
      <button className="more-item" role="menuitem" onClick={onRename}>
        <I.Edit style={{width:13, height:13}}/>
        <span>Rename folder…</span>
        <span className="kbd">⌘R</span>
      </button>
      <button className="more-item" role="menuitem" onClick={onRescan}>
        <I.Refresh style={{width:13, height:13}}/>
        <span>Re-scan this album</span>
        <span className="kbd">⌘⇧R</span>
      </button>
      <button className="more-item" role="menuitem" onClick={onForceAnalyze}>
        <I.Activity style={{width:13, height:13}}/>
        <span>Force re-analyze</span>
      </button>
    </div>
  );
}

/* ---------- RENAME DIALOG ---------- */
function RenameDialog({ cover, onClose }) {
  const [name, setName] = useState(cover.folder);
  const inputRef = useRef(null);
  useEffect(() => {
    inputRef.current && inputRef.current.focus();
    inputRef.current && inputRef.current.select();
    function key(e) { if (e.key === "Escape") onClose(); }
    document.addEventListener("keydown", key);
    return () => document.removeEventListener("keydown", key);
  }, [onClose]);
  const suggested = `${cover.artist} - ${cover.title} (${cover.year}) [FLAC]`;
  const changed = name !== cover.folder;
  return (
    <div className="dialog-backdrop" onClick={onClose}>
      <div className="dialog rename-dialog" onClick={e=>e.stopPropagation()}>
        <div className="dialog-head">
          <h3>Rename folder</h3>
          <span style={{flex:1}}/>
          <button className="btn icon ghost" onClick={onClose}><I.Close style={{width:14, height:14}}/></button>
        </div>
        <div className="rename-body">
          <p className="muted mono-small" style={{marginBottom:10}}>
            Renames the folder on disk. Tags and library references update automatically.
          </p>
          <label className="rename-label">From</label>
          <div className="rename-from mono"><code>{cover.folder}</code></div>
          <label className="rename-label">To</label>
          <input
            ref={inputRef}
            className="input mono"
            value={name}
            onChange={e=>setName(e.target.value)}
            spellCheck={false}
          />
          <button
            className="rename-suggest"
            onClick={() => setName(suggested)}
            disabled={name === suggested}
          >
            <I.Sparkles style={{width:11, height:11}}/>
            Use suggested: <code>{suggested}</code>
          </button>
        </div>
        <div className="dialog-foot">
          <button className="btn ghost" onClick={onClose}>Cancel</button>
          <span style={{flex:1}}/>
          <button className="btn primary" disabled={!changed || !name.trim()} onClick={onClose}>
            <I.Check style={{width:13, height:13}}/>
            Rename
          </button>
        </div>
      </div>
    </div>
  );
}

/* ---------- SETTINGS DIALOG ---------- */
function SettingsDialog({ onClose }) {
  const [tab, setTab] = useState("library");
  return (
    <div className="dialog-backdrop" onClick={onClose}>
      <div className="dialog" onClick={e=>e.stopPropagation()}>
        <div className="dialog-head">
          <h3>Settings</h3>
          <span className="crumb">/ {tab}</span>
          <span style={{flex:1}}/>
          <button className="btn icon ghost" onClick={onClose}><I.Close style={{width:14, height:14}}/></button>
        </div>
        <div className="dialog-body">
          <div className="dialog-tabs">
            {[
              ["library", "Library", I.Folder],
              ["ai",      "AI",      I.Sparkles],
              ["storage", "Storage", I.Database],
              ["about",   "About",   I.Info],
            ].map(([k,label,Ic]) => (
              <button key={k} className={tab===k?"active":""} onClick={()=>setTab(k)}>
                <Ic style={{width:13, height:13}}/>
                {label}
              </button>
            ))}
          </div>
          <div className="dialog-content">
            {tab === "library" && (
              <>
                <div className="field-group">
                  <h4>Library directory</h4>
                  <p>Absolute path to the folder containing your album directories. Subfolders with FLAC files are treated as albums.</p>
                  <div className="field-row">
                    <input className="input" defaultValue="/Users/paul/Music"/>
                    <button className="btn">Browse…</button>
                  </div>
                </div>
                <div className="field-group">
                  <h4>Folder convention</h4>
                  <p>How to interpret folders. Affects "from path" parsing in the detail panel.</p>
                  <div className="field-row" style={{gap:14}}>
                    <label className="row" style={{fontSize:12}}><input type="radio" name="conv" defaultChecked/> Artist / Album</label>
                    <label className="row" style={{fontSize:12}}><input type="radio" name="conv"/> Album only</label>
                    <label className="row" style={{fontSize:12}}><input type="radio" name="conv"/> Custom regex…</label>
                  </div>
                </div>
                <div className="field-group">
                  <h4>Auto rescan</h4>
                  <p>Watch the library directory and rescan when files change.</p>
                  <label className="row" style={{fontSize:12}}><input type="checkbox" defaultChecked/> Rescan on filesystem changes</label>
                </div>
              </>
            )}
            {tab === "ai" && (
              <>
                <div className="field-group">
                  <h4>Credentials</h4>
                  <dl className="kv">
                    <dt>provider</dt><dd>Anthropic <span className="tag ok" style={{marginLeft:6}}>connected</span></dd>
                    <dt>source</dt><dd className="mono">.env · ANTHROPIC_API_KEY</dd>
                    <dt>model</dt><dd className="mono">claude-haiku-4-5</dd>
                  </dl>
                </div>
                <div className="field-group">
                  <h4>Suggestion behavior</h4>
                  <p>Default voice for Claude's metadata suggestions. (You can change this on the fly in the Tweaks panel.)</p>
                  <div className="field-row" style={{gap:14}}>
                    <label className="row" style={{fontSize:12}}><input type="radio" name="voice" defaultChecked/> Confident assistant</label>
                    <label className="row" style={{fontSize:12}}><input type="radio" name="voice"/> Conversational</label>
                    <label className="row" style={{fontSize:12}}><input type="radio" name="voice"/> Neutral diff</label>
                  </div>
                </div>
              </>
            )}
            {tab === "storage" && (
              <>
                <div className="field-group">
                  <h4>Database</h4>
                  <dl className="kv">
                    <dt>db file</dt><dd className="mono">~/Library/Application Support/MediaManager/library.sqlite</dd>
                    <dt>size</dt><dd className="mono tnum">3.42 MB</dd>
                    <dt>albums</dt><dd className="mono tnum">312</dd>
                    <dt>tracks</dt><dd className="mono tnum">3,847</dd>
                  </dl>
                </div>
                <div className="field-group">
                  <h4>Caches</h4>
                  <dl className="kv">
                    <dt>thumbs</dt><dd className="mono tnum">142.1 MB · 312 covers</dd>
                    <dt>scan cache</dt><dd className="mono tnum">8.2 MB</dd>
                  </dl>
                  <div className="field-row" style={{marginTop:10}}>
                    <button className="btn">Clear thumb cache</button>
                    <button className="btn">Clear scan cache</button>
                    <button className="btn danger">Reset database</button>
                  </div>
                </div>
              </>
            )}
            {tab === "about" && (
              <div className="field-group">
                <h4>Media Manager</h4>
                <dl className="kv">
                  <dt>version</dt><dd className="mono">0.4.0-beta · build 142</dd>
                  <dt>backend</dt><dd className="mono">FastAPI · SQLite · SSE</dd>
                  <dt>frontend</dt><dd className="mono">React · TanStack Query</dd>
                </dl>
              </div>
            )}
          </div>
        </div>
        <div className="dialog-foot">
          <span className="left">paths and DB locations are read-only at runtime</span>
          <span style={{flex:1}}/>
          <button className="btn ghost" onClick={onClose}>Cancel</button>
          <button className="btn primary">Save &amp; rescan</button>
        </div>
      </div>
    </div>
  );
}

/* ---------- TWEAKS PANEL ---------- */
function MMTweaksPanel() {
  const [tweaks, setTweak] = useTweaks(TWEAK_DEFAULTS);

  // Apply theme + tokens
  useEffect(() => {
    document.documentElement.classList.toggle("dark", tweaks.theme === "dark");
  }, [tweaks.theme]);

  useEffect(() => {
    document.documentElement.style.setProperty("--accent-h", tweaks.accentHue);
  }, [tweaks.accentHue]);

  useEffect(() => {
    const d = DENSITY[tweaks.density];
    if (!d) return;
    const r = document.documentElement.style;
    r.setProperty("--row-h", d.rowH+"px");
    r.setProperty("--pad-x", d.padX+"px");
    r.setProperty("--pad-y", d.padY+"px");
    r.setProperty("--fs-sm", d.fsSm+"px");
    r.setProperty("--fs-md", d.fsMd+"px");
    r.setProperty("--fs-lg", d.fsLg+"px");
  }, [tweaks.density]);

  useEffect(() => {
    document.documentElement.style.setProperty("--tile-min", tweaks.tileMin + "px");
  }, [tweaks.tileMin]);

  useEffect(() => {
    const f = FONT_PAIRINGS[tweaks.fontPairing] || FONT_PAIRINGS["inter-jb"];
    document.documentElement.style.setProperty("--font-sans", f.sans);
    document.documentElement.style.setProperty("--font-mono", f.mono);
  }, [tweaks.fontPairing]);

  useEffect(() => {
    window._tweakAiVoice = tweaks.aiVoice;
    // Force a sheet rerender when AI voice changes:
    window.dispatchEvent(new CustomEvent("mm-aivoice", { detail: tweaks.aiVoice }));
  }, [tweaks.aiVoice]);

  // Expose scenario to parent
  useEffect(() => {
    window._tweakScenario = tweaks.scenario;
    window.dispatchEvent(new CustomEvent("mm-scenario", { detail: tweaks.scenario }));
  }, [tweaks.scenario]);

  return (
    <TweaksPanel title="Tweaks">
      <TweakSection label="Theme">
        <TweakRadio label="Mode" value={tweaks.theme} onChange={v=>setTweak('theme',v)}
          options={[{value:"light",label:"Light"},{value:"dark",label:"Dark"}]}/>
        <TweakSlider label="Accent hue" min={0} max={360} value={tweaks.accentHue}
          onChange={v=>setTweak('accentHue', v)}
          unit="°"/>
        <div style={{display:"grid", gridTemplateColumns:"repeat(6, 1fr)", gap:6, marginTop:6}}>
          {[10, 60, 150, 200, 250, 305].map(h => (
            <button key={h} onClick={()=>setTweak('accentHue', h)}
              style={{
                height: 22, borderRadius: 4, border: "1px solid var(--line)",
                background: `oklch(0.68 0.16 ${h})`, cursor:"pointer",
                outline: tweaks.accentHue===h ? "2px solid var(--accent-fg)" : "none",
              }} title={`hue ${h}°`}/>
          ))}
        </div>
      </TweakSection>

      <TweakSection label="Layout">
        <TweakRadio label="Density" value={tweaks.density} onChange={v=>setTweak('density',v)}
          options={[{value:"compact",label:"Compact"},{value:"standard",label:"Standard"},{value:"roomy",label:"Roomy"}]}/>
        <TweakSlider label="Tile size" min={120} max={280} step={4} value={tweaks.tileMin}
          onChange={v=>setTweak('tileMin', v)} unit="px"/>
      </TweakSection>

      <TweakSection label="Type">
        <TweakSelect label="Font pairing" value={tweaks.fontPairing} onChange={v=>setTweak('fontPairing',v)}
          options={Object.entries(FONT_PAIRINGS).map(([k,v]) => ({value:k, label:v.label}))}/>
      </TweakSection>

      <TweakSection label="AI">
        <TweakRadio label="Suggestion voice" value={tweaks.aiVoice} onChange={v=>setTweak('aiVoice',v)}
          options={[{value:"confident",label:"Confident"},{value:"neutral",label:"Neutral diff"},{value:"conversational",label:"Conversational"}]}/>
      </TweakSection>

      <TweakSection label="Preview">
        <TweakSelect label="Scenario" value={tweaks.scenario} onChange={v=>setTweak('scenario',v)}
          options={[
            {value:"library",label:"Library"},
            {value:"scanning",label:"Scanning library"},
            {value:"detail-view",label:"Album detail"},
            {value:"detail-suggest",label:"AI suggestion"},
            {value:"detail-applying",label:"Applying changes"},
            {value:"detail-analyzing-bpm",label:"Detecting BPM"},
            {value:"detail-analyzing-rg",label:"Calc ReplayGain"},
            {value:"settings",label:"Settings dialog"},
          ]}/>
      </TweakSection>
    </TweaksPanel>
  );
}

/* ---------- ROOT APP ---------- */
function App() {
  const [scenario, setScenarioState] = useState(TWEAK_DEFAULTS.scenario);
  const [aiVoice, setAiVoice] = useState(TWEAK_DEFAULTS.aiVoice);
  const [showSettings, setShowSettings] = useState(false);

  useEffect(() => {
    const onScn = (e) => setScenarioState(e.detail);
    const onVoice = (e) => setAiVoice(e.detail);
    window.addEventListener("mm-scenario", onScn);
    window.addEventListener("mm-aivoice", onVoice);
    return () => {
      window.removeEventListener("mm-scenario", onScn);
      window.removeEventListener("mm-aivoice", onVoice);
    };
  }, []);

  // Pick the album shown in the sheet based on scenario
  const sheetCover = useMemo(() => {
    if (scenario === "detail-view") return COVERS.find(c=>c.id==="kob");
    return COVERS.find(c=>c.id==="wall");
  }, [scenario]);

  const showSheet = scenario.startsWith("detail-");
  const showRibbon = scenario === "scanning";
  const showSettingsScene = scenario === "settings" || showSettings;

  return (
    <div className="app" data-screen-label={scenario}>
      <Titlebar
        tweaks={{ theme: document.documentElement.classList.contains("dark") ? "dark":"light" }}
        setTweak={(k,v) => {
          if (k === "theme") document.documentElement.classList.toggle("dark", v==="dark");
          window.parent.postMessage({type:"__edit_mode_set_keys", edits:{[k]:v}}, "*");
        }}
        onOpenSettings={() => setShowSettings(true)}
      />
      <div className="body">
        <Sidebar scenario={scenario} setScenario={setScenarioState}/>
        <main className="main">
          {showRibbon && <ScanRibbon/>}
          {!showRibbon && <FilterBar/>}
          <div className="grid-scroll">
            <div className="grid">
              {showRibbon
                ? <>
                    {COVERS.slice(0, 4).map(c => <Tile key={c.id} cover={c}/>)}
                    <Tile cover={COVERS[4]} scanning/>
                    <Tile cover={COVERS[5]} scanning/>
                    {Array.from({length: 6}).map((_, i) => <SkeletonTile key={"s"+i}/>)}
                  </>
                : COVERS.map(c => <Tile key={c.id} cover={c}/>)}
            </div>
          </div>
          {showSheet && <DetailSheet scenario={scenario} cover={sheetCover} onClose={()=>setScenarioState("library")} onSetScenario={setScenarioState} key={scenario.split("-analyzing-")[0]+"-"+aiVoice}/>}
          {showSettingsScene && <SettingsDialog onClose={()=>{ setShowSettings(false); if (scenario==="settings") setScenarioState("library"); }}/>}
        </main>
      </div>
    </div>
  );
}

ReactDOM.createRoot(document.getElementById("root")).render(
  <>
    <App/>
    <MMTweaksPanel/>
  </>
);
