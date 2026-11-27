// The CLI embeds crates/roadbook-cli/assets/roadbook-viewer.js; it must be this package, built.
import fs from 'node:fs'
import { expect, it } from 'vitest'

it('the committed viewer bundle is up to date (rebuild: pnpm --filter @dire-kiwi/roadbook-viewer build)', async () => {
  const build = (await import('../build.mjs' as string)) as { bundle(): Promise<string>; ASSET: string }
  const committed = fs.existsSync(build.ASSET) ? fs.readFileSync(build.ASSET, 'utf8') : ''
  const fresh = await build.bundle()
  expect(committed.split('\n')[0]).toBe(fresh.split('\n')[0])
  expect(committed === fresh).toBe(true)
}, 60000)
