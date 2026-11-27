## Catalog

`spec/conformance/charts/catalog.road:14`

```text
Searches its stock for the items it is asked about.
```

```mermaid
stateDiagram-v2
  Open: on search / reply call done inStock(event.stock, event.page)
  [*] --> Open
```

## Shopper

`spec/conformance/charts/catalog.road:24`

```text
Asks the catalog which of the items it wants are in stock.
```

```mermaid
stateDiagram-v2
  Asking: invoke catalog.search(stock, wanted)
  Found: output found
  [*] --> Asking
  Asking --> Found: done / set found = result
  Asking --> Found: fail Unanswered
  Found --> [*]
```
