// Small helpers the pages share: the stylesheet, the theme toggle, text measurement.

import { STYLE } from '../style.ts'
import type { Measure } from '../model.ts'

export const $ = <T extends HTMLElement = HTMLElement>(root: ParentNode, sel: string): T => {
  const el = root.querySelector<T>(sel)
  if (!el) throw new Error(`missing ${sel}`)
  return el
}

export function injectStyle() {
  const style = document.createElement('style')
  style.textContent = STYLE
  document.head.appendChild(style)
}

const THEMES = ['auto', 'light', 'dark'] as const
type Theme = (typeof THEMES)[number]
const KEY = 'roadbook-theme'

function stored(): Theme {
  try {
    const t = localStorage.getItem(KEY)
    return t === 'light' || t === 'dark' ? t : 'auto'
  } catch {
    return 'auto' // storage can be unavailable on file:// pages
  }
}

function apply(theme: Theme, button: HTMLElement | null) {
  if (theme === 'auto') delete document.documentElement.dataset.theme
  else document.documentElement.dataset.theme = theme
  if (button) {
    button.textContent = `Theme: ${theme}`
    button.title = 'Colour scheme: follow the system, light or dark'
  }
}

/** Follows the system until the user picks a theme with the button; remembers the choice. */
export function setupTheme(button: HTMLElement) {
  apply(stored(), button)
  button.addEventListener('click', () => {
    const next = THEMES[(THEMES.indexOf((document.documentElement.dataset.theme as Theme | undefined) ?? 'auto') + 1) % THEMES.length]!
    apply(next, button)
    try {
      if (next === 'auto') localStorage.removeItem(KEY)
      else localStorage.setItem(KEY, next)
    } catch {
      /* not remembered, still applied */
    }
  })
}

/** Measures text in the diagram's monospace font, so boxes fit their text exactly. */
export function canvasMeasure(): Measure {
  const ctx = document.createElement('canvas').getContext('2d')
  const mono = getComputedStyle(document.documentElement).getPropertyValue('--rb-mono').trim() || 'monospace'
  const cache = new Map<string, number>()
  return (text, size, bold) => {
    if (!ctx) return text.length * size * 0.62
    const key = `${size}|${bold ? 1 : 0}|${text}`
    let w = cache.get(key)
    if (w === undefined) {
      ctx.font = `${bold ? '650' : '400'} ${size}px ${mono}`
      w = ctx.measureText(text).width + 1
      cache.set(key, w)
    }
    return w
  }
}
