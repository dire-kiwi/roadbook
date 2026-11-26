// The viewer's stylesheet, as strings so the same rules reach the browser bundle, static SVG
// made in Node and the tests without a CSS loader. Every colour is a custom property; the dark
// values follow the system, and <html data-theme="light|dark"> overrides it.

export const LIGHT = `
  --rb-bg: #f3f4f6;
  --rb-canvas: #fafbfc;
  --rb-surface: #ffffff;
  --rb-container: rgba(30, 41, 59, 0.045);
  --rb-fg: #181b20;
  --rb-muted: #5a6370;
  --rb-border: #cfd4db;
  --rb-box: #6a7381;
  --rb-edge: #3b424d;
  --rb-ok: #0b7a22;
  --rb-err: #c22a2b;
  --rb-accent: #1f62c9;
  --rb-accent-bg: #e4edfb;
  --rb-msg: #7a35d6;
  --rb-match: #fff1a8;
  --rb-live: #a84f00;
  --rb-live-bg: #fdf0e2;
  --rb-label-bg: #ffffff;
  --rb-shadow: 0 1px 2px rgba(15, 23, 42, 0.08);
  --rb-add: #0a7d34;
  --rb-add-bg: #e2f4e7;
  --rb-del: #b3261e;
  --rb-del-bg: #fbe8e6;
  --rb-chg: #8f5b00;
  --rb-chg-bg: #fcf1d6;
  --rb-cov: #1f62c9;
  --rb-never: #c22a2b;
`

export const DARK = `
  --rb-bg: #111317;
  --rb-canvas: #15181c;
  --rb-surface: #20242b;
  --rb-container: rgba(255, 255, 255, 0.04);
  --rb-fg: #eceef2;
  --rb-muted: #a2aab6;
  --rb-border: #353b45;
  --rb-box: #8b95a3;
  --rb-edge: #c3c9d2;
  --rb-ok: #4ccb62;
  --rb-err: #ff8482;
  --rb-accent: #75adf7;
  --rb-accent-bg: #1c2b45;
  --rb-msg: #c39bff;
  --rb-match: #5a4a00;
  --rb-live: #ffad5c;
  --rb-live-bg: #3a2614;
  --rb-label-bg: #15181c;
  --rb-shadow: 0 1px 2px rgba(0, 0, 0, 0.4);
  --rb-add: #5fd38a;
  --rb-add-bg: #15301f;
  --rb-del: #ff8a80;
  --rb-del-bg: #3a1a18;
  --rb-chg: #f0c060;
  --rb-chg-bg: #362a10;
  --rb-cov: #75adf7;
  --rb-never: #ff8482;
`

export const FONTS = `
:root, .rb-svg {
  --rb-sans: system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", "Noto Sans", sans-serif;
  --rb-mono: ui-monospace, "SF Mono", SFMono-Regular, Menlo, Consolas, "Liberation Mono", "DejaVu Sans Mono", monospace;
}
`

/** The rules of the diagram itself: what static SVG needs. */
export const DIAGRAM_CSS = `
.rb-svg { font-family: var(--rb-mono); }
.rb-svg text { fill: var(--rb-fg); }

.rb-box { fill: var(--rb-surface); stroke: var(--rb-box); stroke-width: 1.4; }
.rb-container > .rb-box { fill: var(--rb-container); stroke-width: 1.2; }
.rb-region > .rb-box { stroke-dasharray: 5 4; }
.rb-box-inner { fill: none; stroke: var(--rb-box); stroke-width: 1.1; }
.rb-box-stack { fill: var(--rb-box); }
.rb-kind-final > .rb-box { stroke-width: 1.6; }
.rb-collapsed > .rb-box { stroke-width: 1.8; }
.rb-sep { stroke: var(--rb-border); stroke-width: 1; }
/* a history state: a small circle marked H, or H* for a deep one */
.rb-history > .rb-box { stroke-width: 1.5; }
.rb-history-mark { font-size: 12px; font-weight: 750; fill: var(--rb-fg); }
.rb-legend-swatch .rb-history-mark { font-size: 10px; }

.rb-title { font-size: 13px; font-weight: 650; }
.rb-tag { font-size: 11px; fill: var(--rb-muted) !important; }
.rb-row { font-size: 11.5px; }
.rb-kw { fill: var(--rb-muted); }
.rb-row.rb-k-fail, .rb-row.rb-k-fail .rb-kw { fill: var(--rb-err); }
.rb-row.rb-k-done .rb-kw { fill: var(--rb-ok); }
.rb-row.rb-k-collapsed { fill: var(--rb-muted); font-style: italic; }

.rb-pill { fill: var(--rb-accent-bg); stroke: var(--rb-accent); stroke-width: 1; }
.rb-pill-text, .rb-pill-text .rb-kw { fill: var(--rb-accent) !important; }
.rb-pill-text .rb-kw { font-weight: 650; }
.rb-pill-chevron { fill: none; stroke: var(--rb-accent); stroke-width: 1.5; stroke-linecap: round; stroke-linejoin: round; }

.rb-toggle rect { fill: var(--rb-surface); stroke: var(--rb-box); stroke-width: 1; }
.rb-toggle path { fill: none; stroke: var(--rb-fg); stroke-width: 1.5; stroke-linecap: round; }

.rb-edge-line { fill: none; stroke: var(--rb-edge); stroke-width: 1.5; stroke-linejoin: round; }
.rb-edge-hit { fill: none; stroke: transparent; stroke-width: 14; }
.rb-arrow path { fill: var(--rb-edge); stroke: none; }
.rb-k-done .rb-edge-line { stroke: var(--rb-ok); }
.rb-k-done.rb-arrow path { fill: var(--rb-ok); }
.rb-k-fail .rb-edge-line { stroke: var(--rb-err); }
.rb-k-fail.rb-arrow path { fill: var(--rb-err); }
.rb-k-after .rb-edge-line { stroke-dasharray: 7 4; }
.rb-k-always .rb-edge-line { stroke-dasharray: 0.1 4.5; stroke-linecap: round; stroke-width: 2; }
.rb-initial { fill: var(--rb-edge); }
.rb-port { fill: var(--rb-canvas); stroke: var(--rb-edge); stroke-width: 1.5; }
.rb-k-done .rb-port { stroke: var(--rb-ok); }
.rb-k-fail .rb-port { stroke: var(--rb-err); }

.rb-label-bg { fill: var(--rb-label-bg); stroke: var(--rb-border); stroke-width: 1; }
.rb-label-trigger { font-size: 12px; font-weight: 600; }
.rb-label-effect { font-size: 11px; fill: var(--rb-muted) !important; }
.rb-guard { font-weight: 400; }
/* messaging: events of other machines, send and emit */
.rb-k-message .rb-edge-line { stroke: var(--rb-msg); }
.rb-k-message.rb-arrow path { fill: var(--rb-msg); }
.rb-k-message .rb-port { stroke: var(--rb-msg); }
.rb-svg .rb-msg { fill: var(--rb-msg) !important; }
.rb-msg-kw { font-weight: 700; }
.rb-alias { font-weight: 650; font-style: italic; }

/* the project index's edges: invoke solid, ref dashed, messages in the message colour with open arrows, emitted ones dotted */
.rb-i-ref .rb-edge-line { stroke-dasharray: 7 4; }
.rb-i-send .rb-edge-line, .rb-i-emit .rb-edge-line { stroke: var(--rb-msg); stroke-width: 1.4; }
.rb-i-emit .rb-edge-line { stroke-dasharray: 0.1 4.5; stroke-linecap: round; stroke-width: 2.2; }
.rb-arrow.rb-i-send path, .rb-arrow.rb-i-emit path { fill: none; stroke: var(--rb-msg); stroke-width: 1.6; stroke-linecap: round; stroke-linejoin: round; }
.rb-ilabel-text { font-size: 11px; }
.rb-ilabel .rb-ikw { font-weight: 700; }
.rb-ilabel.rb-i-send .rb-ikw, .rb-ilabel.rb-i-emit .rb-ikw { fill: var(--rb-msg); }
.rb-label.rb-k-done .rb-label-trigger { fill: var(--rb-ok); }
.rb-label.rb-k-fail .rb-label-trigger { fill: var(--rb-err); }
.rb-label .rb-guard { fill: var(--rb-fg); }

/* what diff and coverage mark: a badge on a box's or a label's corner, a glyph before a row */
.rb-badge rect { fill: var(--rb-surface); stroke: var(--rb-fg); stroke-width: 1.2; }
.rb-svg .rb-badge text { font-size: 10px; font-weight: 750; }
.rb-glyph { font-size: 11.5px; font-weight: 800; }

/* diff: added heavy and solid (+), removed dashed, faded and struck through (−), changed heavy (~) */
.rb-diff-added > .rb-box { stroke: var(--rb-add); stroke-width: 2.6; stroke-dasharray: none; }
.rb-diff-added.rb-leaf > .rb-box { fill: var(--rb-add-bg); }
.rb-diff-removed { opacity: 0.6; }
.rb-diff-removed > .rb-box, .rb-diff-removed > .rb-box-inner { stroke: var(--rb-del); stroke-dasharray: 4 3; stroke-width: 1.8; }
.rb-diff-removed.rb-leaf > .rb-box { fill: var(--rb-del-bg); }
.rb-diff-removed > .rb-title, .rb-row.rb-diff-removed, .rb-label.rb-diff-removed text { text-decoration: line-through; }
.rb-diff-changed > .rb-box { stroke: var(--rb-chg); stroke-width: 2.6; }
.rb-diff-changed.rb-leaf > .rb-box { fill: var(--rb-chg-bg); }
.rb-diff-inside > .rb-box { stroke: var(--rb-chg); stroke-width: 2.2; }
.rb-edge.rb-diff-added .rb-edge-line { stroke: var(--rb-add); stroke-width: 3; }
.rb-edge.rb-diff-removed .rb-edge-line { stroke: var(--rb-del); stroke-dasharray: 4 3; }
.rb-edge.rb-diff-changed .rb-edge-line { stroke: var(--rb-chg); stroke-width: 3; }
.rb-label.rb-diff-added .rb-label-bg { stroke: var(--rb-add); stroke-width: 2.2; }
.rb-label.rb-diff-removed .rb-label-bg { stroke: var(--rb-del); stroke-dasharray: 3 2; }
.rb-label.rb-diff-changed .rb-label-bg { stroke: var(--rb-chg); stroke-width: 2.2; }
.rb-diff-added > .rb-badge rect { stroke: var(--rb-add); }
.rb-diff-removed > .rb-badge rect { stroke: var(--rb-del); }
.rb-diff-changed > .rb-badge rect, .rb-diff-inside > .rb-badge rect { stroke: var(--rb-chg); }
.rb-svg .rb-diff-added > .rb-badge text, .rb-svg .rb-glyph.rb-diff-added { fill: var(--rb-add); }
.rb-svg .rb-diff-removed > .rb-badge text, .rb-svg .rb-glyph.rb-diff-removed { fill: var(--rb-del); }
.rb-svg .rb-diff-changed > .rb-badge text, .rb-svg .rb-diff-inside > .rb-badge text, .rb-svg .rb-glyph.rb-diff-changed { fill: var(--rb-chg); }

/* coverage: heavier and darker the more often taken (×N), never taken dotted in the error colour ("never") */
.rb-edge.rb-cov-1 .rb-edge-line { stroke: var(--rb-cov); stroke-width: 2; }
.rb-edge.rb-cov-2 .rb-edge-line { stroke: var(--rb-cov); stroke-width: 3; }
.rb-edge.rb-cov-3 .rb-edge-line { stroke: var(--rb-cov); stroke-width: 4.2; }
.rb-edge.rb-cov-4 .rb-edge-line { stroke: var(--rb-cov); stroke-width: 5.6; }
.rb-edge.rb-cov-never .rb-edge-line { stroke: var(--rb-never); stroke-dasharray: 1.5 3.5; stroke-width: 2; stroke-linecap: round; }
.rb-label.rb-cov-never .rb-label-bg { stroke: var(--rb-never); stroke-dasharray: 2 2; stroke-width: 1.6; }
.rb-label.rb-cov-never .rb-badge rect, .rb-cov-never > .rb-badge rect { stroke: var(--rb-never); }
.rb-svg .rb-cov-never > .rb-badge text, .rb-svg .rb-glyph.rb-cov-never { fill: var(--rb-never); }
.rb-cov-1.rb-leaf > .rb-box { fill: color-mix(in srgb, var(--rb-cov) 10%, var(--rb-surface)); }
.rb-cov-2.rb-leaf > .rb-box { fill: color-mix(in srgb, var(--rb-cov) 20%, var(--rb-surface)); }
.rb-cov-3.rb-leaf > .rb-box { fill: color-mix(in srgb, var(--rb-cov) 31%, var(--rb-surface)); }
.rb-cov-4.rb-leaf > .rb-box { fill: color-mix(in srgb, var(--rb-cov) 42%, var(--rb-surface)); }
.rb-cov-never > .rb-box { stroke: var(--rb-never); stroke-dasharray: 2 3; stroke-width: 2; }
`

/** The rules of the pages around the diagram. */
export const APP_CSS = `
* { box-sizing: border-box; }
html, body { margin: 0; }
body { font: 14px/1.45 var(--rb-sans); color: var(--rb-fg); background: var(--rb-bg); }
code, .rb-mono { font-family: var(--rb-mono); font-size: 0.92em; }
[hidden] { display: none !important; }

.rb-signature { display: block; font-size: 13px; overflow-wrap: anywhere; }
.rb-doc { margin: 4px 0 0; color: var(--rb-muted); max-width: 80ch; }
.rb-t-done { color: var(--rb-ok); }
.rb-t-fail { color: var(--rb-err); }
.rb-t-msg { color: var(--rb-msg); }

.rb-legend-list { list-style: none; margin: 0; padding: 0; display: grid; gap: 5px; font-size: 12.5px; }
.rb-legend-list li { display: flex; align-items: center; gap: 9px; color: var(--rb-muted); }
.rb-legend-list code { color: var(--rb-fg); font-weight: 600; }
.rb-legend-list code.rb-t-done { color: var(--rb-ok); }
.rb-legend-list code.rb-t-fail { color: var(--rb-err); }
.rb-legend-list code.rb-t-msg { color: var(--rb-msg); }
.rb-legend-swatch { flex: none; font-family: var(--rb-mono); }
.rb-legend-swatch .rb-legend-guard { font-size: 9.5px; }

/* the page */
body.rb-app { height: 100vh; display: grid; grid-template-rows: auto auto minmax(0, 1fr); overflow: hidden; }
.rb-top { display: flex; align-items: center; gap: 14px; padding: 8px 14px; background: var(--rb-surface); border-bottom: 1px solid var(--rb-border); flex-wrap: wrap; }
.rb-brand { font-weight: 650; letter-spacing: 0.01em; white-space: nowrap; color: inherit; text-decoration: none; }
.rb-brand small { font-weight: 400; color: var(--rb-muted); margin-left: 6px; font-family: var(--rb-mono); font-size: 12px; }
.rb-tabs { display: flex; gap: 4px; flex-wrap: wrap; flex: 1; align-items: center; }
.rb-tabs-label { color: var(--rb-muted); font-size: 12.5px; margin-right: 4px; }
.rb-btn, .rb-tab {
  font: inherit; color: var(--rb-fg); background: transparent; border: 1px solid var(--rb-border);
  border-radius: 6px; padding: 3px 10px; cursor: pointer; white-space: nowrap; text-decoration: none;
}
.rb-tab { font-family: var(--rb-mono); font-size: 13px; }
.rb-btn:hover, .rb-tab:hover { border-color: var(--rb-box); }
.rb-tab[aria-selected="true"] { background: var(--rb-accent-bg); border-color: var(--rb-accent); color: var(--rb-accent); font-weight: 650; text-decoration: underline; text-underline-offset: 3px; }
.rb-btn:focus-visible, .rb-tab:focus-visible, .rb-link:focus-visible, .rb-search input:focus-visible { outline: 2px solid var(--rb-accent); outline-offset: 1px; }
.rb-tools { display: flex; gap: 4px; align-items: center; }

.rb-search { position: relative; }
.rb-search input { font: 13px var(--rb-mono); color: var(--rb-fg); background: var(--rb-bg); border: 1px solid var(--rb-border); border-radius: 6px; padding: 4px 8px; width: 21em; max-width: 40vw; }
.rb-results { position: absolute; right: 0; top: calc(100% + 4px); z-index: 10; width: 26em; max-height: 60vh; overflow: auto; margin: 0; padding: 4px; list-style: none; background: var(--rb-surface); border: 1px solid var(--rb-border); border-radius: 8px; box-shadow: 0 6px 24px rgba(0, 0, 0, 0.18); }
.rb-results li button { display: block; width: 100%; text-align: left; font: 12.5px var(--rb-mono); color: var(--rb-fg); background: none; border: 0; border-radius: 5px; padding: 4px 8px; cursor: pointer; overflow-wrap: anywhere; }
.rb-results li button:hover, .rb-results li button[aria-selected="true"] { background: var(--rb-accent-bg); }
.rb-results .rb-what { color: var(--rb-muted); font-family: var(--rb-sans); font-size: 11.5px; margin-right: 6px; }
.rb-results .rb-none { color: var(--rb-muted); padding: 4px 8px; font-size: 12.5px; }

.rb-machine-head { display: flex; align-items: flex-start; gap: 12px; padding: 9px 14px 10px; background: var(--rb-surface); border-bottom: 1px solid var(--rb-border); }
.rb-trail { display: flex; flex-wrap: wrap; align-items: center; gap: 4px; font: 12.5px var(--rb-mono); margin-bottom: 4px; }
.rb-trail .rb-sep-text { color: var(--rb-muted); }
.rb-trail [aria-current] { font-weight: 650; }

.rb-main { display: grid; grid-template-columns: minmax(0, 1fr) 372px; min-height: 0; }
.rb-canvas { position: relative; overflow: hidden; background: var(--rb-canvas); cursor: grab; touch-action: none; user-select: none; }
.rb-canvas.rb-dragging { cursor: grabbing; }
.rb-canvas > .rb-svg { display: block; width: 100%; height: 100%; }
.rb-canvas .rb-state, .rb-canvas .rb-edge, .rb-canvas .rb-label, .rb-canvas .rb-machine-link, .rb-canvas .rb-row[data-transition], .rb-canvas .rb-toggle { cursor: pointer; }
.rb-canvas .rb-state:hover > .rb-box { stroke: var(--rb-fg); }
.rb-canvas [data-event] { cursor: pointer; }
.rb-canvas [data-event]:hover { text-decoration: underline; }
.rb-canvas .rb-msg.rb-selected { text-decoration: underline; font-weight: 700; }
.rb-canvas .rb-toggle:hover rect { stroke: var(--rb-accent); stroke-width: 2; }
.rb-canvas .rb-machine-link:hover .rb-pill { stroke-width: 2; }
.rb-canvas .rb-edge:hover .rb-edge-line, .rb-canvas .rb-edge.rb-hover .rb-edge-line { stroke-width: 2.6; }
.rb-canvas .rb-label:hover .rb-label-bg, .rb-canvas .rb-label.rb-hover .rb-label-bg { stroke: var(--rb-box); }
.rb-busy { position: absolute; left: 12px; top: 10px; font-size: 12px; color: var(--rb-muted); pointer-events: none; }
.rb-error { position: absolute; inset: 20px; color: var(--rb-err); font: 13px var(--rb-mono); white-space: pre-wrap; }

.rb-selected > .rb-box { stroke: var(--rb-accent) !important; stroke-width: 3; }
.rb-selected > .rb-box-inner { stroke: var(--rb-accent); }
.rb-edge.rb-selected .rb-edge-line { stroke-width: 3.2; }
.rb-canvas .rb-label.rb-selected .rb-label-bg { stroke: var(--rb-accent); stroke-width: 2.4; }
.rb-row.rb-selected { text-decoration: underline; }
.rb-match > .rb-box, .rb-label.rb-match .rb-label-bg { fill: var(--rb-match) !important; stroke-dasharray: none; }
.rb-match > .rb-box { stroke-width: 2.4; }

.rb-stage { display: grid; grid-template-rows: minmax(0, 1fr) auto; min-width: 0; min-height: 0; }
.rb-legend { background: var(--rb-surface); border-top: 1px solid var(--rb-border); padding: 7px 14px; }
.rb-legend .rb-legend-list { display: flex; flex-wrap: wrap; gap: 4px 20px; font-size: 12px; }
.rb-legend .rb-legend-list li { gap: 6px; }
.rb-hint { position: absolute; right: 12px; bottom: 8px; font-size: 12px; color: var(--rb-muted); pointer-events: none; }

.rb-panel { background: var(--rb-surface); border-left: 1px solid var(--rb-border); overflow: auto; padding: 14px 16px 28px; }
.rb-panel h2 { font: 650 16px/1.3 var(--rb-mono); margin: 0; overflow-wrap: anywhere; }
.rb-panel h3 { font-size: 11.5px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--rb-muted); margin: 18px 0 6px; font-weight: 600; }
.rb-panel-head { display: flex; align-items: flex-start; gap: 8px; }
.rb-panel-head h2 { flex: 1; }
.rb-chip { display: inline-block; font-size: 11.5px; border: 1px solid var(--rb-border); border-radius: 999px; padding: 0 8px; color: var(--rb-muted); white-space: nowrap; }
.rb-path { font-family: var(--rb-mono); font-size: 12px; color: var(--rb-muted); margin: 2px 0 0; overflow-wrap: anywhere; }
.rb-panel .rb-doc { margin: 10px 0 0; color: var(--rb-fg); }
.rb-code { font-family: var(--rb-mono); font-size: 12.5px; background: var(--rb-bg); border: 1px solid var(--rb-border); border-radius: 6px; padding: 7px 9px; margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; }
.rb-kv { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 3px 12px; margin: 0; font-size: 13px; }
.rb-kv dt { color: var(--rb-muted); }
.rb-kv dd { margin: 0; font-family: var(--rb-mono); font-size: 12.5px; overflow-wrap: anywhere; }
.rb-list { list-style: none; margin: 0; padding: 0; display: grid; gap: 4px; }
.rb-list li { font-family: var(--rb-mono); font-size: 12.5px; overflow-wrap: anywhere; }
.rb-list .rb-sub { display: block; color: var(--rb-muted); padding-left: 14px; }
.rb-list .rb-note { font-family: var(--rb-sans); color: var(--rb-muted); }
.rb-link { font: inherit; color: var(--rb-accent); background: none; border: 0; padding: 0; cursor: pointer; text-align: left; text-decoration: underline; text-decoration-color: transparent; }
.rb-link:hover { text-decoration-color: currentColor; }
.rb-link.rb-mono { font-family: var(--rb-mono); font-size: 12.5px; }
.rb-tlink { display: block; width: 100%; font-family: var(--rb-mono); font-size: 12.5px; color: var(--rb-fg); background: none; border: 1px solid var(--rb-border); border-radius: 6px; padding: 5px 8px; cursor: pointer; text-align: left; overflow-wrap: anywhere; }
.rb-tlink:hover { border-color: var(--rb-accent); }
.rb-tlink .rb-msg-kw, .rb-list .rb-msg-kw { color: var(--rb-msg); font-weight: 700; }
.rb-signature .rb-ref { font-size: inherit; }
.rb-talks { list-style: none; margin: 0; padding: 0; display: grid; gap: 4px; font-size: 12.5px; }
.rb-talks li { font-family: var(--rb-mono); overflow-wrap: anywhere; }
.rb-talks .rb-rel { font-family: var(--rb-sans); color: var(--rb-muted); }
.rb-talks .rb-rel.rb-t-msg { color: var(--rb-msg); font-weight: 600; }
.rb-tlink .rb-arrow-text { color: var(--rb-muted); }
.rb-tlink .rb-sub { display: block; color: var(--rb-muted); font-size: 12px; }
.rb-empty { color: var(--rb-muted); font-size: 13px; margin: 0; }
.rb-code + .rb-empty, .rb-code + .rb-kv, .rb-code + .rb-path, .rb-code + .rb-code, .rb-path + .rb-code, .rb-empty + .rb-tlink, .rb-tlink + .rb-empty, .rb-code + p { margin-top: 7px; }
.rb-tlink + .rb-tlink { margin-top: 5px; }
.rb-tree { list-style: none; margin: 0; padding: 0; }
.rb-tree ul { list-style: none; margin: 0; padding-left: 14px; border-left: 1px solid var(--rb-border); margin-left: 4px; }
.rb-tree li { font-family: var(--rb-mono); font-size: 12.5px; padding: 1px 0; }

/* the project index */
body.rb-index { min-height: 100vh; }
.rb-index-main { padding: 18px 22px 40px; display: grid; gap: 22px; }
.rb-index-main h2 { font-size: 11.5px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--rb-muted); margin: 0 0 8px; font-weight: 600; }
.rb-graph { background: var(--rb-canvas); border: 1px solid var(--rb-border); border-radius: 8px; overflow: auto; padding: 10px; }
.rb-graph .rb-svg { display: block; margin: 0 auto; max-width: 100%; height: auto; }
.rb-graph .rb-mnode { cursor: pointer; }
.rb-graph .rb-mnode:hover .rb-box { stroke: var(--rb-accent); stroke-width: 2.4; }
.rb-graph .rb-mnode .rb-entry-mark { font-size: 10.5px; fill: var(--rb-muted); }
.rb-index-legend { display: flex; flex-wrap: wrap; gap: 4px 20px; margin: 8px 2px 0; }
.rb-table { width: 100%; border-collapse: collapse; font-size: 13px; background: var(--rb-surface); border: 1px solid var(--rb-border); border-radius: 8px; }
.rb-table th, .rb-table td { text-align: left; padding: 7px 10px; border-bottom: 1px solid var(--rb-border); vertical-align: top; }
.rb-table th { font-size: 11.5px; text-transform: uppercase; letter-spacing: 0.05em; color: var(--rb-muted); font-weight: 600; }
.rb-table td code { overflow-wrap: anywhere; }
.rb-table a { color: var(--rb-accent); font-family: var(--rb-mono); font-weight: 650; }
.rb-table .rb-doc { margin: 2px 0 0; }

/* simulate and replay: a solid ring around active states, a dashed one around the states the
   current record names, heavy lines for the transitions taken; all readable in greyscale */
.rb-ring { fill: none; pointer-events: none; }
.rb-ring-active { stroke: var(--rb-live); stroke-width: 3.2; }
.rb-ring-named { stroke: var(--rb-fg); stroke-width: 2; stroke-dasharray: 6 4; }
.rb-active > .rb-box { stroke: var(--rb-live); stroke-width: 2; }
.rb-active.rb-leaf > .rb-box { fill: var(--rb-live-bg); }
.rb-active > .rb-title { font-weight: 800; }
.rb-edge.rb-taken .rb-edge-line { stroke-width: 4.4; }
.rb-canvas .rb-label.rb-taken .rb-label-bg { stroke: var(--rb-live); stroke-width: 3; }
.rb-label.rb-taken .rb-label-trigger { font-weight: 800; text-decoration: underline; }
.rb-row.rb-taken { font-weight: 800; text-decoration: underline; }
.rb-mode-status { position: absolute; left: 12px; top: 10px; max-width: calc(100% - 24px); font: 12px var(--rb-mono); color: var(--rb-fg); background: var(--rb-surface); border: 1px solid var(--rb-border); border-radius: 6px; padding: 3px 8px; pointer-events: none; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; box-shadow: var(--rb-shadow); }

.rb-modes { display: flex; border: 1px solid var(--rb-border); border-radius: 7px; overflow: hidden; }
.rb-mode-btn { font: inherit; font-size: 13px; color: var(--rb-fg); background: transparent; border: 0; border-left: 1px solid var(--rb-border); padding: 3px 11px; cursor: pointer; }
.rb-mode-btn:first-child { border-left: 0; }
.rb-mode-btn:hover { background: var(--rb-bg); }
.rb-mode-btn[aria-pressed="true"] { background: var(--rb-accent-bg); color: var(--rb-accent); font-weight: 650; text-decoration: underline; text-underline-offset: 3px; }
.rb-mode-btn:focus-visible { outline: 2px solid var(--rb-accent); outline-offset: -2px; }

.rb-mode-panel { margin-bottom: 14px; }
.rb-details-below { border-top: 1px solid var(--rb-border); padding-top: 12px; }
.rb-mode-head { display: flex; align-items: center; gap: 8px; }
.rb-mode-head h2 { flex: 1; }
.rb-mode-note { color: var(--rb-muted); font-size: 12.5px; margin: 6px 0 0; }
.rb-panel h4 { font-size: 12.5px; margin: 12px 0 5px; font-weight: 650; }
.rb-h-note { font-weight: 400; color: var(--rb-muted); text-transform: none; letter-spacing: 0; }
.rb-status-running { border-color: var(--rb-accent); color: var(--rb-accent); }
.rb-status-done { border-color: var(--rb-ok); color: var(--rb-ok); }
.rb-status-failed, .rb-status-defect { border-color: var(--rb-err); color: var(--rb-err); font-weight: 650; }
.rb-refusal { margin: 10px 0 0; padding: 8px 10px; border: 2px solid var(--rb-err); border-radius: 6px; font-size: 13px; background: var(--rb-bg); }
.rb-alert { margin: 10px 0 0; padding: 6px 9px; border: 1px dashed var(--rb-err); border-radius: 6px; font-size: 13px; }
.rb-ask { margin: 12px 0 0; padding: 4px 11px 11px; border: 2px solid var(--rb-accent); border-radius: 8px; background: var(--rb-accent-bg); }
.rb-ask h3 { color: var(--rb-fg); }
.rb-ask p { margin: 4px 0; font-size: 13px; }
.rb-steps { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; margin: 12px 0 0; }
.rb-step-text { flex-basis: 100%; order: -1; font-size: 12.5px; }
.rb-clock { font: 12.5px var(--rb-mono); margin: 6px 0 0; }
.rb-choice { margin: 5px 0; display: flex; flex-wrap: wrap; align-items: center; gap: 4px 8px; }
details.rb-choice { display: block; }
details.rb-choice > summary { cursor: pointer; font: 12.5px var(--rb-mono); border: 1px solid var(--rb-border); border-radius: 6px; padding: 3px 9px; display: inline-block; }
details.rb-choice > summary:hover { border-color: var(--rb-box); }
details.rb-choice[open] > summary { border-style: dashed; margin-bottom: 5px; }
.rb-choice .rb-btn, .rb-row-btns .rb-btn { font-family: var(--rb-mono); font-size: 12.5px; }
.rb-cnote { color: var(--rb-muted); font-size: 12px; }
.rb-task-note { margin: 0 0 4px; }
.rb-row-btns { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; margin-top: 8px; }
.rb-form { display: grid; gap: 5px; margin: 6px 0; }
.rb-form .rb-go { justify-self: start; }
.rb-btn:disabled { opacity: 0.5; cursor: not-allowed; text-decoration: line-through; }
.rb-json { width: 100%; font: 12px/1.4 var(--rb-mono); color: var(--rb-fg); background: var(--rb-bg); border: 1px solid var(--rb-border); border-radius: 6px; padding: 6px 8px; resize: vertical; }
.rb-json[aria-invalid="true"] { border: 2px dashed var(--rb-err); }
.rb-form-errors { margin: 0; padding: 0 0 0 16px; color: var(--rb-err); font-size: 12px; }
.rb-form-errors:empty { display: none; }
.rb-ms { width: 7em; font: 12.5px var(--rb-mono); color: var(--rb-fg); background: var(--rb-bg); border: 1px solid var(--rb-border); border-radius: 5px; padding: 2px 5px; }
.rb-advance label { font: 12.5px var(--rb-mono); }
.rb-active-path { margin: 0; font-size: 12.5px; }
.rb-vars .rb-vnote { color: var(--rb-muted); font-family: var(--rb-sans); font-size: 11.5px; }
.rb-file { display: inline-block; }
.rb-problems summary { cursor: pointer; color: var(--rb-err); font-size: 13px; margin-top: 8px; }
/* diff and coverage panels */
.rb-diff-chip-added { border-color: var(--rb-add); color: var(--rb-add); font-weight: 650; }
.rb-diff-chip-removed { border-color: var(--rb-del); color: var(--rb-del); border-style: dashed; }
.rb-diff-chip-changed { border-color: var(--rb-chg); color: var(--rb-chg); font-weight: 650; }
.rb-diff-chip-error { border: 2px solid var(--rb-err); color: var(--rb-err); font-weight: 650; }
.rb-diff-sign { display: inline-block; width: 1.2em; text-align: center; font: 800 13px var(--rb-mono); }
.rb-diff-sign-added { color: var(--rb-add); }
.rb-diff-sign-removed { color: var(--rb-del); }
.rb-diff-sign-changed { color: var(--rb-chg); }
.rb-diff-list li.rb-current, .rb-cov-list li.rb-current { outline: 2px solid var(--rb-accent); outline-offset: 1px; border-radius: 3px; }
.rb-changes { width: 100%; border-collapse: collapse; font-size: 12px; margin: 6px 0; table-layout: fixed; }
.rb-changes th, .rb-changes td { text-align: left; vertical-align: top; padding: 4px 5px; border-bottom: 1px solid var(--rb-border); }
.rb-changes thead th { font-size: 11px; color: var(--rb-muted); font-weight: 600; }
.rb-changes tbody th { width: 22%; font-weight: 650; font-family: var(--rb-mono); overflow-wrap: anywhere; }
.rb-changes .rb-code { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; }
.rb-changes td.rb-before .rb-code { border-left: 3px dashed var(--rb-del); text-decoration: line-through; text-decoration-color: var(--rb-del); }
.rb-changes td.rb-after .rb-code { border-left: 3px solid var(--rb-add); }
.rb-diff-details, .rb-cov-details { margin: 0 0 10px; padding: 6px 9px; border-left: 4px solid var(--rb-chg); background: var(--rb-bg); }
.rb-cov-details { border-left-color: var(--rb-cov); }
.rb-cov-table { border-collapse: collapse; font-size: 12px; }
.rb-cov-table td { padding: 2px 6px; vertical-align: top; }
.rb-cov-n { font: 700 12px var(--rb-mono); text-align: right; white-space: nowrap; }
.rb-cov-all summary { cursor: pointer; font-size: 13px; margin-top: 10px; }
.rb-diagnostics { white-space: pre; overflow-x: auto; }
.rb-diff-summary { margin-bottom: 18px; }

.rb-now { margin: 10px 0 0; padding: 6px 9px; border-left: 4px solid var(--rb-live); background: var(--rb-bg); }
.rb-now p { margin: 0; }
.rb-now-text { font: 12.5px var(--rb-mono); overflow-wrap: anywhere; }
.rb-records { list-style: none; margin: 0; padding: 0; max-height: 46vh; overflow: auto; border: 1px solid var(--rb-border); border-radius: 6px; font: 11.5px/1.35 var(--rb-mono); }
.rb-records li { border-bottom: 1px solid var(--rb-border); }
.rb-records li:last-child { border-bottom: 0; }
.rb-rec { display: flex; gap: 8px; width: 100%; padding: 2px 6px; text-align: left; font: inherit; color: inherit; background: none; border: 0; }
button.rb-rec { cursor: pointer; }
.rb-records button:hover { background: var(--rb-bg); }
.rb-records .rb-rt { color: var(--rb-muted); min-width: 3em; text-align: right; flex: none; }
.rb-records .rb-rrun { color: var(--rb-muted); flex: none; }
.rb-records .rb-rtext { overflow-wrap: anywhere; min-width: 0; }
.rb-records li.rb-recent { background: var(--rb-live-bg); border-left: 3px solid var(--rb-live); }
.rb-records li.rb-current { background: var(--rb-live-bg); outline: 2px solid var(--rb-live); outline-offset: -2px; font-weight: 700; }
.rb-records .rb-rk-transition .rb-rtext { font-weight: 650; }
.rb-records .rb-rk-failed .rb-rtext, .rb-records .rb-rk-defect .rb-rtext { color: var(--rb-err); }
body[data-mode="simulate"] .rb-hint, body[data-mode="replay"] .rb-hint { display: none; }

@media (max-width: 860px) {
  .rb-main { grid-template-columns: minmax(0, 1fr); grid-template-rows: minmax(0, 3fr) minmax(0, 2fr); }
  .rb-panel { border-left: 0; border-top: 1px solid var(--rb-border); }
}
`

/** The whole stylesheet of the viewer pages. */
export const STYLE =
  `:root { color-scheme: light dark; ${LIGHT} }\n` +
  `@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) { ${DARK} } }\n` +
  `:root[data-theme="dark"] { color-scheme: dark; ${DARK} }\n` +
  `:root[data-theme="light"] { color-scheme: light; }\n` +
  FONTS +
  DIAGRAM_CSS +
  APP_CSS
