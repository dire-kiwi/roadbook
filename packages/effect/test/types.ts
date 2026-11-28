/**
 * Type-level tests of `make` and `implement` with a hand-written definition, checked by `tsc`
 * (`pnpm typecheck`), not run. The same guarantees against generated code are tested in
 * examples/shop/test/types.ts.
 */
import { Context, Effect, Schema } from "effect"
import * as Roadbook from "../src/index.ts"

class Oops extends Schema.TaggedError<Oops>()("Oops", {}) {}
class Other extends Schema.TaggedError<Other>()("Other", {}) {}

interface NestDef extends Roadbook.MachineDef {
  readonly name: "Nest"
  readonly input: {}
  readonly output: void
  readonly error: never
  readonly event: { readonly _tag: "Again" } | { readonly _tag: "Leave" }
  readonly state: "Nest" | "Nest.Outer" | "Nest.Away"
  readonly fns: {}
  readonly actions: { readonly log: { readonly what: string; readonly n: number } }
  readonly tasks: { readonly work: { readonly args: { readonly n: number }; readonly success: number; readonly error: Oops } }
  readonly machines: {}
}

class Log extends Context.Service<Log, { line(s: string): Effect.Effect<void> }>()("test/Log") {}

declare const ir: unknown
const Nest = Roadbook.make<NestDef>(ir, "Nest")

export const NestLive = Nest.implement({
  actions: { log: ({ what }) => Log.use((log) => log.line(what)) },
  tasks: { work: ({ n }) => (n > 1 ? Effect.succeed(n) : Effect.fail(new Oops())) }
})

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2) ? true : false
export const exact: Equal<ReturnType<typeof NestLive.run>, Effect.Effect<void, never, Log>> = true

export const mistakes = () => {
  Nest.implement({
    actions: { log: () => Effect.void },
    // @ts-expect-error  work declares only Oops
    tasks: { work: () => Effect.fail(new Other()) }
  })
  Nest.implement({
    // @ts-expect-error  an action cannot fail
    actions: { log: () => Effect.fail(new Oops()) },
    tasks: { work: () => Effect.succeed(1) }
  })
  // @ts-expect-error  the actions are missing
  Nest.implement({ tasks: { work: () => Effect.succeed(1) } })
  Nest.implement({
    actions: { log: () => Effect.void },
    tasks: { work: () => Effect.succeed(1) },
    // @ts-expect-error  Nest calls no functions
    fns: { extra: () => 1 }
  })
  // @ts-expect-error  a state Nest does not have
  Effect.map(Effect.scoped(NestLive.spawn({})), (actor) => Effect.map(actor.snapshot, (s) => s.matches("Nest.Inner")))
}

interface Progress {
  readonly _tag: "Progress"
  readonly percent: number
}

interface FeedDef extends Roadbook.MachineDef {
  readonly name: "Feed"
  readonly input: {}
  readonly output: void
  readonly error: never
  readonly event: never
  readonly state: "Feed"
  readonly fns: {}
  readonly actions: {}
  readonly tasks: {
    readonly upload: {
      readonly args: { readonly size: number }
      readonly success: number
      readonly error: never
      readonly emits: Progress
    }
    readonly ping: { readonly args: {}; readonly success: void; readonly error: never }
  }
  readonly machines: {}
}

const Feed = Roadbook.make<FeedDef>(ir, "Feed")

// A task that emits is given its emitter; one that ignores it, or emits nothing, needs no change.
export const FeedLive = Feed.implement({
  tasks: {
    upload: ({ size }, emit) => Effect.as(emit({ _tag: "Progress", percent: 50 }), size),
    ping: () => Effect.void
  }
})

export const emitMistakes = () => {
  Feed.implement({
    tasks: {
      // @ts-expect-error  upload emits only Progress
      upload: (_, emit) => Effect.as(emit({ _tag: "Stage", name: "x" }), 1),
      ping: () => Effect.void
    }
  })
  Feed.implement({
    tasks: {
      upload: () => Effect.succeed(1),
      // @ts-expect-error  ping emits nothing: its emitter takes `never`
      ping: (_, emit) => emit({ _tag: "Progress", percent: 1 })
    }
  })
}
