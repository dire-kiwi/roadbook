/**
 * Saved snapshots at the API: `Saved`, the plain JSON a store keeps (execution.md, "The saved
 * snapshot"), and `RestoreError`, why `restore` or `resume` refused one.
 */
import * as Core from "@dire-kiwi/roadbook-core"
import { Schema } from "effect"

/**
 * A saved snapshot: plain JSON matching `spec/snapshot.schema.json`, the same document the
 * Rust runtime saves. The values inside (`input`, `vars`) are checked against the IR's types
 * when it is restored.
 */
export type Saved = Core.Saved

/**
 * The schema of a saved snapshot, for a store to decode with: it checks the document's shape
 * and version, not that it fits a machine, which `restore` checks.
 */
export const Saved: Schema.declare<Core.Saved> = Schema.declare<Core.Saved>(Core.isSaved, {
  title: "Roadbook.Saved",
  description: "A saved snapshot of a Roadbook run (spec/snapshot.schema.json)"
})

/**
 * Why a saved snapshot was refused, before any run started: `invalid` (not a saved snapshot of
 * this version, or it does not fit the machine), `machine` (of another machine), `ir` (another
 * hash and no migration, or a migration that did not give the current one), `migration` (the
 * migration threw), `ended` (the run had ended) or `ref` (the machine holds a `ref`).
 */
export class RestoreError extends Schema.TaggedError<RestoreError>()("RestoreError", {
  reason: Schema.Literals(["invalid", "machine", "ir", "migration", "ended", "ref"]),
  message: Schema.String
}) {}
