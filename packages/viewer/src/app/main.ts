// The browser entry point. `roadbook viz` writes pages that hold their data in
// <script type="application/json" id="rb-data"> and load this script (shared, or inlined with
// --self-contained). It reads the data and mounts the page it describes.

import ElkBundled from 'elkjs/lib/elk.bundled.js'
import type { ElkLike } from '../layout.ts'
import { IR_VERSION } from '../ir.ts'
import type { PageData } from '../project.ts'
import { mountChart } from './chart.ts'
import { canvasMeasure, injectStyle } from './dom.ts'
import { mountIndex } from './index-page.ts'

function start() {
  injectStyle()
  const el = document.getElementById('rb-data')
  if (!el) return
  const data = JSON.parse(el.textContent ?? 'null') as PageData
  if (data.kind === 'chart' && data.ir.irVersion !== IR_VERSION) {
    document.body.textContent = `This page holds IR version ${data.ir.irVersion}; this viewer reads version ${IR_VERSION}. Rebuild it with the same roadbook.`
    return
  }
  // elk.bundled.js is CommonJS; its default export is the constructor.
  const elk = new (ElkBundled as unknown as new () => ElkLike)()
  const measure = canvasMeasure()
  if (data.kind === 'index') void mountIndex(data.index, elk, measure)
  else mountChart(data, elk, measure)
}

if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start)
else start()
