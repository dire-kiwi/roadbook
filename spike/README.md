# Roadbook spikes

Throwaway prototypes that tested the design in `docs/design/` before it was built. None of this is used by the real code. None is left: each was deleted once the real code replaced it, and is in the git history.

The first spike, a JavaScript compiler (`compile.mjs`) and diagram viewer (`viewer/`, `viz.mjs`, `out/`), was deleted once the Rust compiler (milestone 1) and `roadbook viz` with `packages/viewer` (milestone 2) replaced it. It is in the git history before milestone 2.

`api-ts`, a type-level sketch of the TypeScript API checked against `effect@4.1.5`, was deleted once `@dire-kiwi/roadbook-effect` and `roadbook gen` (milestone 3) implemented it; its negative cases live on as `tsc`-checked tests in `examples/shop/test/types.ts` and `packages/effect/test/types.ts`. `api-rust`, a hand-written version of what the Rust macro would generate, was deleted once `roadbook::include!` and the Rust driver (milestone 4) covered it, with its `Send` check and one struct implementing several charts' handlers as tests in `crates/roadbook/tests`. Both are in the git history before milestone 4.

`cross-crate`, a stand-in macro that tried four ways for `roadbook::include!` in one crate to find a chart another crate ships (`cargo metadata`, build scripts with `links`, a `roadbook.toml` map, and a hidden macro exported by the owning crate), was deleted once `roadbook::include!` implemented the last of them (milestone 5), which is what the spike recommended; `examples/inventory` and `examples/orders` are two real crates that use it. Its findings are summarised in `docs/design/actors.md` ("Across crates and packages"). It is in the git history before milestone 5's cross-crate imports.
