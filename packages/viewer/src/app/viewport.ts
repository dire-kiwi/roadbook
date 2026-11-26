// Pan, zoom and fit for an SVG inside a container, by rewriting the SVG's viewBox.

import type { Box } from '../layout.ts'

export class Viewport {
  private svg: SVGSVGElement | null = null
  private content = { width: 1, height: 1 }
  private view = { x: 0, y: 0, w: 1, h: 1 }
  /** True until the user pans or zooms: a resize then fits again. */
  private fitted = true
  private drag: { x: number; y: number; vx: number; vy: number; moved: boolean } | null = null
  /** Set for one tick after a drag, so the click that ends it does not select. */
  suppressClick = false

  constructor(private readonly el: HTMLElement) {
    el.addEventListener(
      'wheel',
      (ev) => {
        ev.preventDefault()
        const r = el.getBoundingClientRect()
        this.zoomBy(Math.exp(-ev.deltaY * (ev.deltaMode === 1 ? 0.05 : 0.0015)), ev.clientX - r.left, ev.clientY - r.top)
      },
      { passive: false },
    )
    el.addEventListener('pointerdown', (ev) => {
      if (ev.button !== 0) return
      this.drag = { x: ev.clientX, y: ev.clientY, vx: this.view.x, vy: this.view.y, moved: false }
    })
    window.addEventListener('pointermove', (ev) => {
      const d = this.drag
      if (!d) return
      const dx = ev.clientX - d.x
      const dy = ev.clientY - d.y
      if (!d.moved && Math.hypot(dx, dy) < 4) return
      d.moved = true
      el.classList.add('rb-dragging')
      const k = this.view.w / this.size().w
      this.view.x = d.vx - dx * k
      this.view.y = d.vy - dy * k
      this.fitted = false
      this.apply()
    })
    window.addEventListener('pointerup', () => {
      if (this.drag?.moved) {
        this.suppressClick = true
        setTimeout(() => (this.suppressClick = false), 0)
      }
      this.drag = null
      el.classList.remove('rb-dragging')
    })
    new ResizeObserver(() => {
      if (!this.svg) return
      if (this.fitted) return this.fit()
      const { w, h } = this.size()
      const scale = w / this.view.w
      this.view.h = h / scale
      this.apply()
    }).observe(el)
  }

  /** Shows a new drawing. `keep`: keep the current zoom and centre instead of fitting. */
  attach(svg: SVGSVGElement, width: number, height: number, keep = false) {
    this.svg = svg
    svg.removeAttribute('width')
    svg.removeAttribute('height')
    this.content = { width, height }
    if (keep && !this.fitted) this.apply()
    else this.open()
  }

  /**
   * The first view of a drawing: all of it if it is readable that way, else its top at a
   * readable size (the reader zooms out or presses Fit for the whole).
   */
  private open() {
    this.fit()
    const { w, h } = this.size()
    if (w / this.view.w >= 0.5) return
    const scale = 0.75
    this.view = { w: w / scale, h: h / scale, x: 0, y: -12 }
    this.view.x = (this.content.width - this.view.w) / 2
    this.fitted = false
    this.apply()
  }

  private size() {
    return { w: Math.max(this.el.clientWidth, 50), h: Math.max(this.el.clientHeight, 50) }
  }

  private apply() {
    const v = this.view
    this.svg?.setAttribute('viewBox', `${v.x} ${v.y} ${v.w} ${v.h}`)
  }

  fit() {
    const { w, h } = this.size()
    const margin = 28
    const scale = Math.min((w - 2 * margin) / this.content.width, (h - 2 * margin) / this.content.height, 1.25)
    this.view = { w: w / scale, h: h / scale, x: 0, y: 0 }
    this.view.x = (this.content.width - this.view.w) / 2
    this.view.y = (this.content.height - this.view.h) / 2
    this.fitted = true
    this.apply()
  }

  zoomBy(factor: number, cx = this.size().w / 2, cy = this.size().h / 2) {
    const { w, h } = this.size()
    const v = this.view
    const scale = Math.min(Math.max((w / v.w) * factor, 0.05), 5)
    const px = v.x + (cx / w) * v.w
    const py = v.y + (cy / h) * v.h
    v.w = w / scale
    v.h = h / scale
    v.x = px - (cx / w) * v.w
    v.y = py - (cy / h) * v.h
    this.fitted = false
    this.apply()
  }

  /** Scrolls a box into view, zooming in to a readable size first if the whole chart is shown tiny. */
  reveal(box: Box | undefined) {
    if (!box) return
    const { w } = this.size()
    const v = this.view
    if (w / v.w < 0.6) this.zoomBy(0.6 / (w / v.w))
    const pad = 20
    const inside = box.x >= v.x + pad && box.y >= v.y + pad && box.x + box.width <= v.x + v.w - pad && box.y + box.height <= v.y + v.h - pad
    if (inside) return
    v.x = box.x + box.width / 2 - v.w / 2
    v.y = box.y + box.height / 2 - v.h / 2
    this.fitted = false
    this.apply()
  }
}
