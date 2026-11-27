import * as Core from "@dire-kiwi/roadbook-core"
import type { Effect, Scope } from "effect"
import { Codec, type ErrorClass } from "./internal/codec.ts"
import { type AnyHandlers, Program, restore, resume, run, spawn } from "./internal/driver.ts"
import type { Actor, Implemented, Machine, MachineDef } from "./Machine.ts"

/**
 * The typed handle for machine `name` of the compiled chart `ir`. The generated `.road.ts`
 * module calls this for each machine it declares, with the module's error classes, so that the
 * runtime can hand host code and callers instances of them. Throws if the IR's version is not
 * the one this runtime reads, or it has no machine `name`.
 */
export const make = <D extends MachineDef>(
  ir: unknown,
  name: D["name"],
  errors: Readonly<Record<string, ErrorClass>> = {}
): Machine<D> => {
  const checked = Core.loadIr(ir)
  if (!Object.hasOwn(checked.machines, name)) throw new Core.IrError(`the IR has no machine ${JSON.stringify(name)}`)
  const codec = new Codec(checked, errors)
  const machine: Machine<D> = {
    name,
    ir: checked,
    implement: (handlers) => {
      const program = new Program(codec, name, handlers as AnyHandlers)
      return {
        def: machine,
        run: (input: D["input"]) => run(program, input),
        spawn: (input: D["input"]) => spawn<D>(program, input) as Effect.Effect<Actor<D>, never, Scope.Scope>,
        restore: (saved, options) => restore<D>(program, saved, options),
        resume: (saved, event, options) => resume<D>(program, saved, event, options)
      } as Implemented<D, any>
    }
  }
  return machine
}
