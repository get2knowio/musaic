/* Tiny inline icon set — lucide-style strokes, sized via parent */
const I = {
  Search: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><circle cx="11" cy="11" r="7"/><path d="m21 21-4.3-4.3"/></svg>,
  Grid: (p) => <svg viewBox="0 0 24 24" fill="currentColor" {...p}><rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/><rect x="14" y="14" width="7" height="7" rx="1"/></svg>,
  List: (p) => <svg viewBox="0 0 24 24" fill="currentColor" {...p}><rect x="3" y="5" width="18" height="2" rx="1"/><rect x="3" y="11" width="18" height="2" rx="1"/><rect x="3" y="17" width="18" height="2" rx="1"/></svg>,
  Refresh: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><path d="M3 12a9 9 0 0 1 15.5-6.4L21 8"/><path d="M21 3v5h-5"/><path d="M21 12a9 9 0 0 1-15.5 6.4L3 16"/><path d="M3 21v-5h5"/></svg>,
  Settings: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>,
  Close: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><path d="M18 6 6 18M6 6l12 12"/></svg>,
  Check: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" {...p}><path d="M20 6 9 17l-5-5"/></svg>,
  CheckCircle: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><circle cx="12" cy="12" r="10"/><path d="m9 12 2 2 4-4"/></svg>,
  Circle: (p) => <svg viewBox="0 0 24 24" fill="currentColor" {...p}><circle cx="12" cy="12" r="2.5"/></svg>,
  Loader: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" {...p}><path d="M21 12a9 9 0 1 1-6.219-8.56"/></svg>,
  XCircle: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><circle cx="12" cy="12" r="10"/><path d="m15 9-6 6M9 9l6 6"/></svg>,
  Sparkles: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><path d="M12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.1 2.1M16.3 16.3l2.1 2.1M5.6 18.4l2.1-2.1M16.3 7.7l2.1-2.1"/><circle cx="12" cy="12" r="2.5"/></svg>,
  Image: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="9" cy="9" r="1.5"/><path d="m21 15-3.5-3.5L9 19"/></svg>,
  Disc: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="3"/></svg>,
  ListMusic: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><path d="M21 15V6"/><path d="M18.5 18a2.5 2.5 0 1 1 0-5 2.5 2.5 0 0 1 0 5Z"/><path d="M12 12H3"/><path d="M16 6H3"/><path d="M12 18H3"/></svg>,
  Plex: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><path d="M22 12h-4l-3 9L9 3l-3 9H2"/></svg>,
  Activity: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><path d="M22 12h-4l-3 9L9 3l-3 9H2"/></svg>,
  Edit: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><path d="M12 20h9"/><path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4Z"/></svg>,
  History: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5"/><path d="M12 7v5l3 2"/></svg>,
  ChevronRight: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><path d="m9 18 6-6-6-6"/></svg>,
  ChevronLeft: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><path d="m15 18-6-6 6-6"/></svg>,
  ChevronDown: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><path d="m6 9 6 6 6-6"/></svg>,
  More: (p) => <svg viewBox="0 0 24 24" fill="currentColor" {...p}><circle cx="6" cy="12" r="1.6"/><circle cx="12" cy="12" r="1.6"/><circle cx="18" cy="12" r="1.6"/></svg>,
  AlertTriangle: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0Z"/><path d="M12 9v4"/><path d="M12 17h.01"/></svg>,
  Folder: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>,
  Sun: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M4.93 19.07l1.41-1.41M17.66 6.34l1.41-1.41"/></svg>,
  Moon: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z"/></svg>,
  Database: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><ellipse cx="12" cy="5" rx="9" ry="3"/><path d="M3 5v14a9 3 0 0 0 18 0V5"/><path d="M3 12a9 3 0 0 0 18 0"/></svg>,
  Cpu: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><rect x="4" y="4" width="16" height="16" rx="2"/><rect x="9" y="9" width="6" height="6"/><path d="M9 1v3M15 1v3M9 20v3M15 20v3M20 9h3M20 14h3M1 9h3M1 14h3"/></svg>,
  Info: (p) => <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>,
};
window.I = I;



// tweaks-panel.jsx
// Reusable Tweaks shell + form-control helpers.
//
// Owns the host protocol (listens for __activate_edit_mode / __deactivate_edit_mode,
// posts __edit_mode_available / __edit_mode_set_keys / __edit_mode_dismissed) so
// individual prototypes don't re-roll it. Ships a consistent set of controls so you
// don't hand-draw <input type="range">, segmented radios, steppers, etc.
//
// Usage (in an HTML file that loads React + Babel):
//
//   const TWEAK_DEFAULTS = /*EDITMODE-BEGIN*/{
//     "primaryColor": "#D97757",
//     "fontSize": 16,
//     "density": "regular",
//     "dark": false
//   }/*EDITMODE-END*/;
//
//   function App() {
//     const [t, setTweak] = useTweaks(TWEAK_DEFAULTS);
//     return (
//       <div style={{ fontSize: t.fontSize, color: t.primaryColor }}>
//         Hello
//         <TweaksPanel>
//           <TweakSection label="Typography" />
//           <TweakSlider label="Font size" value={t.fontSize} min={10} max={32} unit="px"
//                        onChange={(v) => setTweak('fontSize', v)} />
//           <TweakRadio  label="Density" value={t.density}
//                        options={['compact', 'regular', 'comfy']}
//                        onChange={(v) => setTweak('density', v)} />
//           <TweakSection label="Theme" />
//           <TweakColor  label="Primary" value={t.primaryColor}
//                        onChange={(v) => setTweak('primaryColor', v)} />
//           <TweakToggle label="Dark mode" value={t.dark}
//                        onChange={(v) => setTweak('dark', v)} />
//         </TweaksPanel>
//       </div>
//     );
//   }
//
// ─────────────────────────────────────────────────────────────────────────────

const __TWEAKS_STYLE = `
  .twk-panel{position:fixed;right:16px;bottom:16px;z-index:2147483646;width:280px;
    max-height:calc(100vh - 32px);display:flex;flex-direction:column;
    background:rgba(250,249,247,.78);color:#29261b;
    -webkit-backdrop-filter:blur(24px) saturate(160%);backdrop-filter:blur(24px) saturate(160%);
    border:.5px solid rgba(255,255,255,.6);border-radius:14px;
    box-shadow:0 1px 0 rgba(255,255,255,.5) inset,0 12px 40px rgba(0,0,0,.18);
    font:11.5px/1.4 ui-sans-serif,system-ui,-apple-system,sans-serif;overflow:hidden}
  .twk-hd{display:flex;align-items:center;justify-content:space-between;
    padding:10px 8px 10px 14px;cursor:move;user-select:none}
  .twk-hd b{font-size:12px;font-weight:600;letter-spacing:.01em}
  .twk-x{appearance:none;border:0;background:transparent;color:rgba(41,38,27,.55);
    width:22px;height:22px;border-radius:6px;cursor:default;font-size:13px;line-height:1}
  .twk-x:hover{background:rgba(0,0,0,.06);color:#29261b}
  .twk-body{padding:2px 14px 14px;display:flex;flex-direction:column;gap:10px;
    overflow-y:auto;overflow-x:hidden;min-height:0;
    scrollbar-width:thin;scrollbar-color:rgba(0,0,0,.15) transparent}
  .twk-body::-webkit-scrollbar{width:8px}
  .twk-body::-webkit-scrollbar-track{background:transparent;margin:2px}
  .twk-body::-webkit-scrollbar-thumb{background:rgba(0,0,0,.15);border-radius:4px;
    border:2px solid transparent;background-clip:content-box}
  .twk-body::-webkit-scrollbar-thumb:hover{background:rgba(0,0,0,.25);
    border:2px solid transparent;background-clip:content-box}
  .twk-row{display:flex;flex-direction:column;gap:5px}
  .twk-row-h{flex-direction:row;align-items:center;justify-content:space-between;gap:10px}
  .twk-lbl{display:flex;justify-content:space-between;align-items:baseline;
    color:rgba(41,38,27,.72)}
  .twk-lbl>span:first-child{font-weight:500}
  .twk-val{color:rgba(41,38,27,.5);font-variant-numeric:tabular-nums}

  .twk-sect{font-size:10px;font-weight:600;letter-spacing:.06em;text-transform:uppercase;
    color:rgba(41,38,27,.45);padding:10px 0 0}
  .twk-sect:first-child{padding-top:0}

  .twk-field{appearance:none;width:100%;height:26px;padding:0 8px;
    border:.5px solid rgba(0,0,0,.1);border-radius:7px;
    background:rgba(255,255,255,.6);color:inherit;font:inherit;outline:none}
  .twk-field:focus{border-color:rgba(0,0,0,.25);background:rgba(255,255,255,.85)}
  select.twk-field{padding-right:22px;
    background-image:url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='10' height='6' viewBox='0 0 10 6'><path fill='rgba(0,0,0,.5)' d='M0 0h10L5 6z'/></svg>");
    background-repeat:no-repeat;background-position:right 8px center}

  .twk-slider{appearance:none;-webkit-appearance:none;width:100%;height:4px;margin:6px 0;
    border-radius:999px;background:rgba(0,0,0,.12);outline:none}
  .twk-slider::-webkit-slider-thumb{-webkit-appearance:none;appearance:none;
    width:14px;height:14px;border-radius:50%;background:#fff;
    border:.5px solid rgba(0,0,0,.12);box-shadow:0 1px 3px rgba(0,0,0,.2);cursor:default}
  .twk-slider::-moz-range-thumb{width:14px;height:14px;border-radius:50%;
    background:#fff;border:.5px solid rgba(0,0,0,.12);box-shadow:0 1px 3px rgba(0,0,0,.2);cursor:default}

  .twk-seg{position:relative;display:flex;padding:2px;border-radius:8px;
    background:rgba(0,0,0,.06);user-select:none}
  .twk-seg-thumb{position:absolute;top:2px;bottom:2px;border-radius:6px;
    background:rgba(255,255,255,.9);box-shadow:0 1px 2px rgba(0,0,0,.12);
    transition:left .15s cubic-bezier(.3,.7,.4,1),width .15s}
  .twk-seg.dragging .twk-seg-thumb{transition:none}
  .twk-seg button{appearance:none;position:relative;z-index:1;flex:1;border:0;
    background:transparent;color:inherit;font:inherit;font-weight:500;min-height:22px;
    border-radius:6px;cursor:default;padding:4px 6px;line-height:1.2;
    overflow-wrap:anywhere}

  .twk-toggle{position:relative;width:32px;height:18px;border:0;border-radius:999px;
    background:rgba(0,0,0,.15);transition:background .15s;cursor:default;padding:0}
  .twk-toggle[data-on="1"]{background:#34c759}
  .twk-toggle i{position:absolute;top:2px;left:2px;width:14px;height:14px;border-radius:50%;
    background:#fff;box-shadow:0 1px 2px rgba(0,0,0,.25);transition:transform .15s}
  .twk-toggle[data-on="1"] i{transform:translateX(14px)}

  .twk-num{display:flex;align-items:center;height:26px;padding:0 0 0 8px;
    border:.5px solid rgba(0,0,0,.1);border-radius:7px;background:rgba(255,255,255,.6)}
  .twk-num-lbl{font-weight:500;color:rgba(41,38,27,.6);cursor:ew-resize;
    user-select:none;padding-right:8px}
  .twk-num input{flex:1;min-width:0;height:100%;border:0;background:transparent;
    font:inherit;font-variant-numeric:tabular-nums;text-align:right;padding:0 8px 0 0;
    outline:none;color:inherit;-moz-appearance:textfield}
  .twk-num input::-webkit-inner-spin-button,.twk-num input::-webkit-outer-spin-button{
    -webkit-appearance:none;margin:0}
  .twk-num-unit{padding-right:8px;color:rgba(41,38,27,.45)}

  .twk-btn{appearance:none;height:26px;padding:0 12px;border:0;border-radius:7px;
    background:rgba(0,0,0,.78);color:#fff;font:inherit;font-weight:500;cursor:default}
  .twk-btn:hover{background:rgba(0,0,0,.88)}
  .twk-btn.secondary{background:rgba(0,0,0,.06);color:inherit}
  .twk-btn.secondary:hover{background:rgba(0,0,0,.1)}

  .twk-swatch{appearance:none;-webkit-appearance:none;width:56px;height:22px;
    border:.5px solid rgba(0,0,0,.1);border-radius:6px;padding:0;cursor:default;
    background:transparent;flex-shrink:0}
  .twk-swatch::-webkit-color-swatch-wrapper{padding:0}
  .twk-swatch::-webkit-color-swatch{border:0;border-radius:5.5px}
  .twk-swatch::-moz-color-swatch{border:0;border-radius:5.5px}
`;

// ── useTweaks ───────────────────────────────────────────────────────────────
// Single source of truth for tweak values. setTweak persists via the host
// (__edit_mode_set_keys → host rewrites the EDITMODE block on disk).
function useTweaks(defaults) {
  const [values, setValues] = React.useState(defaults);
  // Accepts either setTweak('key', value) or setTweak({ key: value, ... }) so a
  // useState-style call doesn't write a "[object Object]" key into the persisted
  // JSON block.
  const setTweak = React.useCallback((keyOrEdits, val) => {
    const edits = typeof keyOrEdits === 'object' && keyOrEdits !== null
      ? keyOrEdits : { [keyOrEdits]: val };
    setValues((prev) => ({ ...prev, ...edits }));
    window.parent.postMessage({ type: '__edit_mode_set_keys', edits }, '*');
  }, []);
  return [values, setTweak];
}

// ── TweaksPanel ─────────────────────────────────────────────────────────────
// Floating shell. Registers the protocol listener BEFORE announcing
// availability — if the announce ran first, the host's activate could land
// before our handler exists and the toolbar toggle would silently no-op.
// The close button posts __edit_mode_dismissed so the host's toolbar toggle
// flips off in lockstep; the host echoes __deactivate_edit_mode back which
// is what actually hides the panel.
function TweaksPanel({ title = 'Tweaks', children }) {
  const [open, setOpen] = React.useState(false);
  const dragRef = React.useRef(null);
  const offsetRef = React.useRef({ x: 16, y: 16 });
  const PAD = 16;

  const clampToViewport = React.useCallback(() => {
    const panel = dragRef.current;
    if (!panel) return;
    const w = panel.offsetWidth, h = panel.offsetHeight;
    const maxRight = Math.max(PAD, window.innerWidth - w - PAD);
    const maxBottom = Math.max(PAD, window.innerHeight - h - PAD);
    offsetRef.current = {
      x: Math.min(maxRight, Math.max(PAD, offsetRef.current.x)),
      y: Math.min(maxBottom, Math.max(PAD, offsetRef.current.y)),
    };
    panel.style.right = offsetRef.current.x + 'px';
    panel.style.bottom = offsetRef.current.y + 'px';
  }, []);

  React.useEffect(() => {
    if (!open) return;
    clampToViewport();
    if (typeof ResizeObserver === 'undefined') {
      window.addEventListener('resize', clampToViewport);
      return () => window.removeEventListener('resize', clampToViewport);
    }
    const ro = new ResizeObserver(clampToViewport);
    ro.observe(document.documentElement);
    return () => ro.disconnect();
  }, [open, clampToViewport]);

  React.useEffect(() => {
    const onMsg = (e) => {
      const t = e?.data?.type;
      if (t === '__activate_edit_mode') setOpen(true);
      else if (t === '__deactivate_edit_mode') setOpen(false);
    };
    window.addEventListener('message', onMsg);
    window.parent.postMessage({ type: '__edit_mode_available' }, '*');
    return () => window.removeEventListener('message', onMsg);
  }, []);

  const dismiss = () => {
    setOpen(false);
    window.parent.postMessage({ type: '__edit_mode_dismissed' }, '*');
  };

  const onDragStart = (e) => {
    const panel = dragRef.current;
    if (!panel) return;
    const r = panel.getBoundingClientRect();
    const sx = e.clientX, sy = e.clientY;
    const startRight = window.innerWidth - r.right;
    const startBottom = window.innerHeight - r.bottom;
    const move = (ev) => {
      offsetRef.current = {
        x: startRight - (ev.clientX - sx),
        y: startBottom - (ev.clientY - sy),
      };
      clampToViewport();
    };
    const up = () => {
      window.removeEventListener('mousemove', move);
      window.removeEventListener('mouseup', up);
    };
    window.addEventListener('mousemove', move);
    window.addEventListener('mouseup', up);
  };

  if (!open) return null;
  return (
    <>
      <style>{__TWEAKS_STYLE}</style>
      <div ref={dragRef} className="twk-panel" data-noncommentable=""
           style={{ right: offsetRef.current.x, bottom: offsetRef.current.y }}>
        <div className="twk-hd" onMouseDown={onDragStart}>
          <b>{title}</b>
          <button className="twk-x" aria-label="Close tweaks"
                  onMouseDown={(e) => e.stopPropagation()}
                  onClick={dismiss}>✕</button>
        </div>
        <div className="twk-body">{children}</div>
      </div>
    </>
  );
}

// ── Layout helpers ──────────────────────────────────────────────────────────

function TweakSection({ label, children }) {
  return (
    <>
      <div className="twk-sect">{label}</div>
      {children}
    </>
  );
}

function TweakRow({ label, value, children, inline = false }) {
  return (
    <div className={inline ? 'twk-row twk-row-h' : 'twk-row'}>
      <div className="twk-lbl">
        <span>{label}</span>
        {value != null && <span className="twk-val">{value}</span>}
      </div>
      {children}
    </div>
  );
}

// ── Controls ────────────────────────────────────────────────────────────────

function TweakSlider({ label, value, min = 0, max = 100, step = 1, unit = '', onChange }) {
  return (
    <TweakRow label={label} value={`${value}${unit}`}>
      <input type="range" className="twk-slider" min={min} max={max} step={step}
             value={value} onChange={(e) => onChange(Number(e.target.value))} />
    </TweakRow>
  );
}

function TweakToggle({ label, value, onChange }) {
  return (
    <div className="twk-row twk-row-h">
      <div className="twk-lbl"><span>{label}</span></div>
      <button type="button" className="twk-toggle" data-on={value ? '1' : '0'}
              role="switch" aria-checked={!!value}
              onClick={() => onChange(!value)}><i /></button>
    </div>
  );
}

function TweakRadio({ label, value, options, onChange }) {
  const trackRef = React.useRef(null);
  const [dragging, setDragging] = React.useState(false);
  const opts = options.map((o) => (typeof o === 'object' ? o : { value: o, label: o }));
  const idx = Math.max(0, opts.findIndex((o) => o.value === value));
  const n = opts.length;

  // The active value is read by pointer-move handlers attached for the lifetime
  // of a drag — ref it so a stale closure doesn't fire onChange for every move.
  const valueRef = React.useRef(value);
  valueRef.current = value;

  const segAt = (clientX) => {
    const r = trackRef.current.getBoundingClientRect();
    const inner = r.width - 4;
    const i = Math.floor(((clientX - r.left - 2) / inner) * n);
    return opts[Math.max(0, Math.min(n - 1, i))].value;
  };

  const onPointerDown = (e) => {
    setDragging(true);
    const v0 = segAt(e.clientX);
    if (v0 !== valueRef.current) onChange(v0);
    const move = (ev) => {
      if (!trackRef.current) return;
      const v = segAt(ev.clientX);
      if (v !== valueRef.current) onChange(v);
    };
    const up = () => {
      setDragging(false);
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  };

  return (
    <TweakRow label={label}>
      <div ref={trackRef} role="radiogroup" onPointerDown={onPointerDown}
           className={dragging ? 'twk-seg dragging' : 'twk-seg'}>
        <div className="twk-seg-thumb"
             style={{ left: `calc(2px + ${idx} * (100% - 4px) / ${n})`,
                      width: `calc((100% - 4px) / ${n})` }} />
        {opts.map((o) => (
          <button key={o.value} type="button" role="radio" aria-checked={o.value === value}>
            {o.label}
          </button>
        ))}
      </div>
    </TweakRow>
  );
}

function TweakSelect({ label, value, options, onChange }) {
  return (
    <TweakRow label={label}>
      <select className="twk-field" value={value} onChange={(e) => onChange(e.target.value)}>
        {options.map((o) => {
          const v = typeof o === 'object' ? o.value : o;
          const l = typeof o === 'object' ? o.label : o;
          return <option key={v} value={v}>{l}</option>;
        })}
      </select>
    </TweakRow>
  );
}

function TweakText({ label, value, placeholder, onChange }) {
  return (
    <TweakRow label={label}>
      <input className="twk-field" type="text" value={value} placeholder={placeholder}
             onChange={(e) => onChange(e.target.value)} />
    </TweakRow>
  );
}

function TweakNumber({ label, value, min, max, step = 1, unit = '', onChange }) {
  const clamp = (n) => {
    if (min != null && n < min) return min;
    if (max != null && n > max) return max;
    return n;
  };
  const startRef = React.useRef({ x: 0, val: 0 });
  const onScrubStart = (e) => {
    e.preventDefault();
    startRef.current = { x: e.clientX, val: value };
    const decimals = (String(step).split('.')[1] || '').length;
    const move = (ev) => {
      const dx = ev.clientX - startRef.current.x;
      const raw = startRef.current.val + dx * step;
      const snapped = Math.round(raw / step) * step;
      onChange(clamp(Number(snapped.toFixed(decimals))));
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  };
  return (
    <div className="twk-num">
      <span className="twk-num-lbl" onPointerDown={onScrubStart}>{label}</span>
      <input type="number" value={value} min={min} max={max} step={step}
             onChange={(e) => onChange(clamp(Number(e.target.value)))} />
      {unit && <span className="twk-num-unit">{unit}</span>}
    </div>
  );
}

function TweakColor({ label, value, onChange }) {
  return (
    <div className="twk-row twk-row-h">
      <div className="twk-lbl"><span>{label}</span></div>
      <input type="color" className="twk-swatch" value={value}
             onChange={(e) => onChange(e.target.value)} />
    </div>
  );
}

function TweakButton({ label, onClick, secondary = false }) {
  return (
    <button type="button" className={secondary ? 'twk-btn secondary' : 'twk-btn'}
            onClick={onClick}>{label}</button>
  );
}

Object.assign(window, {
  useTweaks, TweaksPanel, TweakSection, TweakRow,
  TweakSlider, TweakToggle, TweakRadio, TweakSelect,
  TweakText, TweakNumber, TweakColor, TweakButton,
});


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
