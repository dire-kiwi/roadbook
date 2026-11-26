# spec

The specification that every Roadbook tool is built against, kept separate from the design documents so that it can be versioned and tested. It holds the IR's JSON Schema ([`ir.schema.json`](ir.schema.json)) and its field-by-field reference ([`ir.md`](ir.md)), which the compiler, the viewer, both runtimes and the code generators all read; the trace schema and the conformance cases (charts, scripts and expected traces that both runtimes must reproduce byte for byte) come next, alongside the language reference.
