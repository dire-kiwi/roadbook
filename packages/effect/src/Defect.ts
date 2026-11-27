import type * as Core from "@dire-kiwi/roadbook-core"

const describe = (defect: Core.Defect): string => {
  switch (defect.kind) {
    case "host":
      return `host code failed: ${defect.message}`
    case "unassigned":
      return `variable ${defect.var} was read before it had a value`
    case "overflow":
      return `arithmetic overflow in ${defect.expr}`
    case "undeclared":
      return `failed with error ${defect.error}, which it does not declare`
    case "limit":
      return "settling took more than 1000 microsteps"
  }
}

/**
 * What a run that ends with a defect dies with (`Effect.die(new Defect(…))`). `defect` is the
 * defect object of `spec/execution.md`, as the trace's `defect` record has it. A child run's
 * defect reaches its parent, and the parent's caller, as the same object.
 */
export class Defect extends Error {
  readonly _tag = "RoadbookDefect"
  readonly defect: Core.Defect

  constructor(defect: Core.Defect) {
    super(`Roadbook defect: ${describe(defect)}`)
    this.name = "RoadbookDefect"
    this.defect = defect
  }
}

export const isDefect = (u: unknown): u is Defect => u instanceof Defect

/**
 * The defect object for something host code threw or died with: a `Defect` is passed on
 * unchanged, an `Error` gives its `message` and any other value `String(value)`.
 */
export const hostDefect = (cause: unknown): Core.Defect => {
  if (cause instanceof Defect) return cause.defect
  if (cause instanceof Error) return { kind: "host", message: cause.message }
  return { kind: "host", message: String(cause) }
}
