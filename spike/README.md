# Roadbook spikes

Throwaway prototypes that tested the design in `docs/design/` before it was built. None of this is used by the real code.

- [`api-ts`](api-ts): the TypeScript API's types, checked against `effect@4.1.5`, including the negative cases.
- [`cross-crate`](cross-crate): how the Rust macro can find a chart that another crate ships.

The first spike, a JavaScript compiler (`compile.mjs`) and diagram viewer (`viewer/`, `viz.mjs`, `out/`), was deleted once the Rust compiler (milestone 1) and `roadbook viz` with `packages/viewer` (milestone 2) replaced it. It is in the git history before milestone 2. `api-rust`, a hand-written version of what the Rust macro would generate, was deleted once `roadbook::include!` and the Rust driver (milestone 4) covered it, with its `Send` check and one struct implementing several charts' handlers as tests in `crates/roadbook/tests`; it is in the git history before milestone 4.
