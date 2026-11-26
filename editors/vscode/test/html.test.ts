// The preview page: its Content-Security-Policy, which scripts it runs, and how it carries the IR.

import { describe, expect, it } from 'vitest'
import { contentSecurityPolicy, jsonScript, makeNonce, previewHtml } from '../src/preview/html.ts'
import type { PageOptions } from '../src/preview/html.ts'

const CSP_SOURCE = 'https://file+.vscode-resource.vscode-cdn.net'

function page(over: Partial<PageOptions> = {}): string {
  return previewHtml({
    cspSource: CSP_SOURCE,
    nonce: 'n0nce+/=',
    viewerUri: 'https://file+.vscode-resource.vscode-cdn.net/ext/dist/roadbook-viewer.js',
    bridgeUri: 'https://file+.vscode-resource.vscode-cdn.net/ext/dist/bridge.js',
    title: 'Preview checkout.road',
    entry: 'examples/shop/checkout.road',
    irText: '{"irVersion":"0.2","sources":[],"machines":{"M":{"doc":"</script><script>alert(1)</script><!--"}}}',
    config: { theme: 'dark', view: { machine: 'M', selection: null }, uri: 'file:///x/checkout.road' },
    ...over,
  })
}

/** Every `<script …>` start tag. */
const scripts = (html: string) => [...html.matchAll(/<script\b([^>]*)>/gi)].map((m) => m[1] ?? '')

describe('the Content-Security-Policy', () => {
  it('runs only scripts with the nonce, and loads nothing from the network', () => {
    const csp = contentSecurityPolicy(CSP_SOURCE, 'abc')
    const directives = Object.fromEntries(csp.split(';').map((d) => d.trim().split(/\s+/)).map(([k, ...v]) => [k, v]))
    expect(directives['default-src']).toEqual(["'none'"])
    expect(directives['script-src']).toEqual(["'nonce-abc'"])
    expect(csp).not.toMatch(/unsafe-eval|\*|https?:\/\/(?!file\+)/)
    expect(directives['style-src']).toEqual([CSP_SOURCE, "'unsafe-inline'"])
    expect(directives['img-src']).toEqual([CSP_SOURCE, 'data:'])
    expect(directives['base-uri']).toEqual(["'none'"])
  })

  it('is the first thing in the head, before any script or style', () => {
    const html = page()
    const at = html.indexOf('http-equiv="Content-Security-Policy"')
    expect(at).toBeGreaterThan(0)
    expect(at).toBeLessThan(html.indexOf('<script'))
    expect(at).toBeLessThan(html.indexOf('<style'))
    expect(html).toContain(`content="default-src 'none'; script-src 'nonce-n0nce+/='`)
  })
})

describe('the page', () => {
  it('runs the viewer and then the bridge, each with the nonce, and no inline script', () => {
    const html = page()
    const tags = scripts(html)
    const run = tags.filter((t) => !t.includes('type="application/json"'))
    expect(run).toEqual([
      ' nonce="n0nce+/=" src="https://file+.vscode-resource.vscode-cdn.net/ext/dist/roadbook-viewer.js"',
      ' nonce="n0nce+/=" src="https://file+.vscode-resource.vscode-cdn.net/ext/dist/bridge.js"',
    ])
    expect(html).not.toMatch(/\son[a-z]+=/i)
  })

  it('carries the IR as the viewer reads it, and nothing in it can close its element', () => {
    const html = page()
    const m = /<script type="application\/json" id="rb-data">([\s\S]*?)<\/script>/.exec(html)
    expect(m).not.toBeNull()
    const data = JSON.parse(m![1]!)
    expect(data).toMatchObject({ kind: 'chart', mode: 'explore', entry: 'examples/shop/checkout.road', index: null, trace: null })
    expect(data.ir.machines.M.doc).toBe('</script><script>alert(1)</script><!--')
    expect(m![1]).not.toContain('<')
    // The page's four script elements: data, config, viewer, bridge.
    expect(scripts(html)).toHaveLength(4)
  })

  it('follows the theme it is given and passes the view to restore', () => {
    const html = page({ config: { theme: 'light', view: null, uri: 'file:///a.road' } })
    expect(html).toContain('<html lang="en" data-theme="light">')
    const m = /<script type="application\/json" id="rbx-config">([\s\S]*?)<\/script>/.exec(html)
    expect(JSON.parse(m![1]!)).toEqual({ theme: 'light', view: null, uri: 'file:///a.road' })
    expect(html).toContain('#rb-theme { display: none !important; }')
  })

  it('without IR is the bridge alone, which shows the problems', () => {
    const html = page({ irText: null })
    expect(html).not.toContain('id="rb-data"')
    expect(html).not.toContain('roadbook-viewer.js')
    expect(html).toContain('bridge.js')
  })

  it('escapes the title and the URIs', () => {
    const html = page({ title: 'Preview <b>&"x".road', bridgeUri: 'https://h/"><script>x</script>' })
    expect(html).toContain('<title>Preview &lt;b&gt;&amp;"x".road</title>')
    expect(html).toContain('src="https://h/&quot;&gt;&lt;script&gt;x&lt;/script&gt;"')
  })
})

describe('helpers', () => {
  it('makes a fresh nonce each time', () => {
    const a = makeNonce()
    expect(a).toMatch(/^[A-Za-z0-9+/]{22}==$/)
    expect(makeNonce()).not.toBe(a)
  })

  it('escapes every < in JSON', () => {
    expect(jsonScript('x', '"<!--</script>"')).toBe('<script type="application/json" id="x">"\\u003c!--\\u003c/script>"</script>')
  })
})
