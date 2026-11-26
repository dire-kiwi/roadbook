// The preview's HTML: the page `roadbook viz` writes for a chart (its data as JSON, read by the
// viewer script; see crates/roadbook-cli/src/commands/viz/page.rs), with a strict
// Content-Security-Policy, the bridge script that connects it to the extension, and the colours
// of VS Code's theme. No `vscode` import.

import { randomBytes } from 'node:crypto'
import type { BridgeConfig } from './protocol.ts'

export interface PageOptions {
  /** `webview.cspSource`: the origin the webview loads extension files from. */
  cspSource: string
  /** A fresh nonce per page; the only scripts that run are the two that carry it. */
  nonce: string
  /** Webview URIs of dist/roadbook-viewer.js and dist/bridge.js. */
  viewerUri: string
  bridgeUri: string
  title: string
  /** The chart's project path, which the viewer shows in its header. */
  entry: string
  /** The IR as JSON, or null when nothing has compiled yet (the bridge shows the problems). */
  irText: string | null
  config: BridgeConfig
}

/** A nonce: 128 random bits, base64. */
export function makeNonce(): string {
  return randomBytes(16).toString('base64')
}

/**
 * The policy: nothing but the two scripts with the nonce runs (no inline handlers, no eval);
 * styles may be inline, since the viewer injects its stylesheet and styles SVG attributes;
 * images and fonts only from the extension or data URIs; no network, frames, forms or workers.
 */
export function contentSecurityPolicy(cspSource: string, nonce: string): string {
  return [
    "default-src 'none'",
    `script-src 'nonce-${nonce}'`,
    `style-src ${cspSource} 'unsafe-inline'`,
    `img-src ${cspSource} data:`,
    `font-src ${cspSource} data:`,
    "base-uri 'none'",
    "form-action 'none'",
  ].join('; ')
}

/** JSON inside a `<script>` element: every `<` escaped, so nothing in it can close the element. */
export function jsonScript(id: string, json: string): string {
  return `<script type="application/json" id="${attr(id)}">${json.replace(/</g, '\\u003c')}</script>`
}

export function previewHtml(o: PageOptions): string {
  const data =
    o.irText === null
      ? ''
      : jsonScript('rb-data', `{"kind":"chart","mode":"explore","entry":${JSON.stringify(o.entry)},"index":null,"trace":null,"ir":${o.irText}}`) + '\n'
  const viewer = o.irText === null ? '' : `<script nonce="${attr(o.nonce)}" src="${attr(o.viewerUri)}"></script>\n`
  return (
    '<!doctype html>\n<html lang="en"' +
    ` data-theme="${o.config.theme}">\n<head>\n<meta charset="utf-8">\n` +
    `<meta http-equiv="Content-Security-Policy" content="${attr(contentSecurityPolicy(o.cspSource, o.nonce))}">\n` +
    '<meta name="viewport" content="width=device-width, initial-scale=1">\n' +
    `<title>${text(o.title)}</title>\n<style>${STYLE}</style>\n</head>\n<body>\n` +
    data +
    jsonScript('rbx-config', JSON.stringify(o.config)) +
    '\n' +
    viewer +
    `<script nonce="${attr(o.nonce)}" src="${attr(o.bridgeUri)}"></script>\n</body>\n</html>\n`
  )
}

function text(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}

function attr(s: string): string {
  return text(s).replace(/"/g, '&quot;')
}

/**
 * The extension's own styles: the viewer's theme toggle is hidden (the preview follows VS
 * Code's theme), the page fills the editor, and the problems box.
 */
const STYLE = `
html, body { height: 100%; margin: 0; }
#rb-theme { display: none !important; }
#rbx-problems { position: fixed; z-index: 50; left: 12px; right: 12px; bottom: 12px; max-height: 40vh; overflow: auto;
  font: 13px/1.45 var(--vscode-font-family, system-ui, sans-serif); color: var(--vscode-foreground, inherit);
  background: var(--vscode-editorWidget-background, var(--rb-surface, #fff));
  border: 1px solid var(--vscode-inputValidation-errorBorder, #c22a2b); border-left-width: 4px; border-radius: 6px;
  box-shadow: 0 4px 18px rgba(0, 0, 0, 0.25); padding: 8px 12px; }
#rbx-problems[hidden] { display: none; }
#rbx-problems.rbx-alone { position: static; max-height: none; margin: 16px; box-shadow: none; }
#rbx-problems h2 { font-size: 13px; margin: 0 0 6px; font-weight: 650; }
#rbx-problems ul { list-style: none; margin: 0; padding: 0; display: grid; gap: 6px; }
#rbx-problems button { all: unset; cursor: pointer; display: block; width: 100%; }
#rbx-problems button:hover .rbx-where, #rbx-problems button:focus-visible .rbx-where { text-decoration: underline; }
#rbx-problems button:focus-visible { outline: 1px solid var(--vscode-focusBorder, #1f62c9); }
#rbx-problems .rbx-where { font-family: var(--vscode-editor-font-family, monospace); opacity: 0.8; }
#rbx-problems .rbx-code { font-family: var(--vscode-editor-font-family, monospace); font-weight: 650; color: var(--vscode-errorForeground, #c22a2b); }
#rbx-problems .rbx-help { display: block; opacity: 0.8; }
#rbx-problems pre { margin: 0; white-space: pre-wrap; font: 12px/1.4 var(--vscode-editor-font-family, monospace); }
`
