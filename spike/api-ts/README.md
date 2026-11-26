# TypeScript API sketch

The proposed `@roadbook/effect` API as types only, checked against `effect@4.1.5`.

- `roadbook.ts`: the library surface. Every value is `declare`d; nothing runs.
- `shop.road.ts`: what `roadbook gen` would write for `examples/shop`, written by hand.
- `app.ts`: what a developer writes, including an Express handler, a test stub for a
  child machine, `@ts-expect-error` cases for the mistakes the types must catch, and
  assertions that the inferred `Effect` types are exact.

```
npm install && npm run check
```
