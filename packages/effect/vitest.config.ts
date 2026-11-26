import { fileURLToPath } from "node:url"
import { defineConfig } from "vitest/config"

// The examples are not packages: map the two packages their generated modules import to this
// package's, as tsconfig.json does for `tsc`, so that test/examples.test.ts can run them.
export default defineConfig({
  resolve: {
    alias: [
      { find: /^effect$/, replacement: fileURLToPath(import.meta.resolve("effect")) },
      { find: /^@roadbook\/effect\/durable$/, replacement: fileURLToPath(new URL("./src/entry-durable.ts", import.meta.url)) },
      { find: /^@roadbook\/effect$/, replacement: fileURLToPath(new URL("./src/index.ts", import.meta.url)) }
    ]
  }
})
