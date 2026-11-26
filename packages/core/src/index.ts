/**
 * `@roadbook/core`: the Roadbook interpreter. Pure, synchronous and dependency-free, so it runs
 * in Node and in the browser alike. `start` and `step` implement `spec/execution.md`; a driver
 * (such as `@roadbook/effect`) carries out the commands they return.
 */
export * from "./ir.ts"
export * from "./json.ts"
export * from "./types.ts"
export { compareCodePoints, SETTLE_LIMIT, start, step } from "./interpreter.ts"
