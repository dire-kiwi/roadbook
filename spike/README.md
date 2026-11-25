# Waymark spike

Throwaway prototype of the pipeline `.way source -> IR JSON -> self-contained HTML`, with no AI in the loop.
It exists to test the language design in `docs/design/`; the real compiler is planned in Rust. Plain Node ESM (Node 26), one dependency (`elkjs`).

```sh
cd spike && npm install && cd ..
node spike/compile.mjs examples/shop/place_order.way > /tmp/place_order.ir.json   # IR on stdout, diagnostics on stderr, exit 1 on errors
node spike/viz.mjs examples/shop/place_order.way -o spike/out/shop.html           # interactive viewer, one file, works from file://
node spike/viz.mjs examples/shop/place_order.way --static --machine Checkout -o spike/out/shop-Checkout.static.html   # script-free page
(cd spike && npm run viz)                                                         # regenerate everything in out/
```

- `compile.mjs`: lexer, recursive-descent parser for the whole grammar, imports, name and target resolution, IR emission.
- `viewer/`: `model.js` (text of rows and labels), `layout.js` (ELK graph), `render.js` (`renderSvg(ir, machine, layout, opts) -> string`, DOM-free), `app.js` (browser interaction), `viewer.css`, `index.html`.
- `viz.mjs`: inlines elkjs, the viewer and the IR into one HTML file; `--static` runs ELK in Node instead.
- `out/`: generated pages, committed on purpose.

Implemented checks: unknown or ambiguous transition target, missing or invalid `initial`, unknown type / task / machine / fn / action / variable / event,
`invoke` that misses a declared error, the same trigger twice in a state, a branch shadowed by `else`, `output`/`fail` outside a top-level final state.

Not implemented: type checking, definite assignment, warnings, `always`-cycle and recursion checks, `waymark fmt`, `--json` diagnostics, error recovery
(the parser stops at the first syntax error in a file), code generation and the runtimes.
