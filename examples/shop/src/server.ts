/** `pnpm --filter @roadbook/example-shop start`: serve the shop on `PORT` (default 3000). */
import { ManagedRuntime } from "effect"
import { makeApp } from "./app.js"
import { FakeServices } from "./services.js"

const runtime = ManagedRuntime.make(FakeServices)
const port = Number(process.env["PORT"] ?? 3000)
const server = makeApp(runtime).listen(port, () => {
  console.log(`shop listening on http://localhost:${port}`)
})

const shutdown = () => {
  server.close()
  void runtime.dispose()
}
process.once("SIGINT", shutdown)
process.once("SIGTERM", shutdown)
