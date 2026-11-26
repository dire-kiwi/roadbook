// Static SVG of one machine, laid out and drawn in Node, with the stylesheet inside:
//
//   node packages/viewer/scripts/svg.ts <chart.ir.json> <Machine> [light|dark] > out.svg
//
// Text is measured by estimate, not by a browser. The pictures in docs/design/images are this
// output turned into PNG: `rsvg-convert -z 1.5 out.svg -o out.png`.

import fs from 'node:fs'
import { createRequire } from 'node:module'
import type { ElkLike } from '../src/layout.ts'
import { defaultCollapsed, layoutMachine } from '../src/layout.ts'
import { renderSvg } from '../src/render.ts'
import type { Ir } from '../src/ir.ts'
import { own } from '../src/ir.ts'

const [file, machine, theme] = process.argv.slice(2)
if (!file || !machine) {
  console.error('usage: node scripts/svg.ts <chart.ir.json> <Machine> [light|dark]')
  process.exit(2)
}
const ir = JSON.parse(fs.readFileSync(file, 'utf8')) as Ir
const m = own(ir.machines, machine)
if (!m) {
  console.error(`no machine ${machine} (found: ${Object.keys(ir.machines).join(', ')})`)
  process.exit(2)
}
const Elk = createRequire(import.meta.url)('elkjs/lib/elk.bundled.js') as new () => ElkLike
const collapsed = defaultCollapsed(m)
const layout = await layoutMachine(ir, machine, new Elk(), { collapsed })
process.stdout.write(renderSvg(ir, machine, layout, { collapsed, standalone: true, theme: theme === 'dark' ? 'dark' : 'light' }) + '\n')
