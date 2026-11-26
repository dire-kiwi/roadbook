/**
 * `@roadbook/effect`: run Roadbook machines on Effect v4. The generated `.road.ts` modules call
 * {@link make}; applications call `implement` on the handles they export, then `run` or `spawn`.
 */
export type * as Core from "@roadbook/core"
export { Defect, isDefect } from "./Defect.ts"
export type { ErrorClass } from "./internal/codec.ts"
export { CurrentInvoke, type InvokeContext, type CancelToken, type Port } from "./internal/driver.ts"
export type {
  Actor,
  Callable,
  Handle,
  Handlers,
  Implemented,
  Machine,
  MachineDef,
  NoExtras,
  Requirements,
  Runnable,
  Snapshot
} from "./Machine.ts"
export { make } from "./make.ts"
export {
  ClockTimer,
  type RunHandle,
  RootRunId,
  RunObserver,
  type TimerRequest,
  Timer,
  TraceSink
} from "./References.ts"
