# roadbook-lsp

The Roadbook language server. `roadbook lsp` runs it on standard input and output; this
crate is the library behind it.

## Features

- **Diagnostics** on open, change and save: the compiler's errors and warnings, with codes,
  `help` lines, secondary labels as related information, and every machine-applicable fix
  as a quick fix (code action). A change compiles the files it can affect, unsaved buffers
  laid over the file system: every file connected to it by imports, either way (what it
  imports, what imports it, and so on), as `roadbook check` would check them together. Each of those files gets
  its diagnostics published, opened or not, and an empty list once it is clean. Changes
  that arrive while a compilation runs are taken in together, so typing fast compiles far
  fewer times than it changes the text.
- **Files of other packages** (in `node_modules`, or a `[packages]` directory) belong to
  their package, as they do for `roadbook check`: they are never compiled as entries of the
  project, but as the project's files that import them reach them, so their imports resolve
  from the package (a `/` path from the package's root) and only their errors are reported,
  never warnings. A package's file that no file of the project imports gets no diagnostics.
- **Untitled documents** (any URI that is not a `file:` one) are compiled on their own, as a
  single file in memory with no project and no options; they can import nothing. Everything
  below works in them.
- **Go to definition** and **find references** for every kind of name: states (in targets,
  each segment of a dotted target, and `initial`), types, aliases, enums and their variants,
  errors (in `!` lists, `fail` outcomes and values), extern types, fields (in reads, record
  values and event values), events and emitted events (in `on`, `on name.Event`, `send` and
  `emit`), functions, actions, tasks and machines (at calls, `do` and `invoke`), parameters
  and variables, `invoke … as` names, names in import lists, and `ref` types. Across files:
  an imported name leads to its declaration in the other file.
- **Rename**, with `prepareRename`. The new name is checked: the right case for the kind of
  name, no `_`, not a reserved word, and no clash with a name it would share a namespace
  with (sibling states; a machine's parameters, variables and `as` names; a machine's events;
  the fields of a record or event; the variants of an enum; an event and a type). Then the
  files are compiled again with the edit applied: every use must resolve to the same thing
  as before (so a renamed state cannot capture a target that meant a state further out), and
  no error may appear (two declarations with one name among files compiled together).
  Declarations are renamed in every file that imports or uses them. A rename that would edit
  a file of another package (a declaration or a use in one) is refused, saying which file.
  `invoke … as` names follow the compiler's rule: two states side by side may use the same
  one, but not a state and a state inside it.
- **Hover**: the declaration's signature (`task chargeCard(order: Order, user: User) ->
  Receipt ! CardDeclined | GatewayUnavailable`, `var attempts: int = 0`, `final state
  Checkout.Charged`), what it belongs to or which file declares it, and its `///` comment.
  The built-in names have one too, read from the syntax tree: the enum `Ended` and its
  variants, and in an `on name.ended` transition the word `ended`, `event` and
  `event.status`.
- **Completion** after a dot, also while the file does not parse: `ended` and the peer's
  emitted events after `on name.` for a `ref` parameter or variable (`ref M`, `ref M?`,
  `ref M[]` or an alias of one), the variants after `Ended.`, and `status` after `event.` in
  an `on name.ended` transition.
- **Formatting** with the canonical formatter (`roadbook fmt`). A file with syntax errors is
  left alone.
- **Document symbols**: declarations, and in a machine its parameters, variables, events and
  states, nested as written. Built from the syntax tree, so the outline is there while the
  file does not compile.

Positions are in UTF-16 code units, the protocol's default, or in bytes when the client
offers `utf-8` in `general.positionEncodings`. Documents are synchronised in full.

The answers about names come from the compiler's symbol index
(`roadbook_compiler::index`, `Compilation::index`), built during name resolution. It holds
what the compiler resolved, so a file with syntax errors, whose declarations the compiler
leaves out, has no definitions or references until it parses again (its diagnostics and
outline still work).

## The `roadbook/ir` request

A request of the server's own, for editors that show the diagram of the chart being edited
(the VS Code extension's preview): the document compiled as an entry, as `roadbook export
ir <file>` compiles it, but with every open document's text in place of the file on disk.
So the IR follows unsaved edits to the chart and to the files it imports, and the chart is
compiled under its own project path with the project's options (`allow-unassigned`), as
the command line does.

```ts
// params
{ textDocument: { uri: string } }
// result
{
  uri: string,
  version: number | null,     // the document's version compiled; null if it is not open
  root: string | null,        // the project's root directory; null for an untitled document
  entry: string,              // the document's project path, as the IR's `sources` name it
  ir: string | null,          // the IR as `roadbook export ir` prints it; null on errors
  diagnostics: Diagnostic[],  // errors and warnings, as `roadbook check --json` prints them
}
```

A file that is not open is read from disk. The request fails (`RequestFailed`, with a
message) for a file of another package, which is not an entry of the project, and for a
URI the server cannot read.

## Running it

```sh
roadbook lsp            # or: roadbook lsp --stdio
```

The server reads and writes JSON-RPC messages with `Content-Length` headers on standard input
and output, and exits after `shutdown` and `exit`. It does not depend on its working
directory: each file's project is the nearest directory above it with a `roadbook.toml`,
read as the other commands read it (`exclude`, `allow-unassigned`, `[packages]`, and
packages in `node_modules`); a file in no project is a project of its own directory.

## Editors

An editor starts `roadbook lsp` as a child process over stdio for files with the `.road`
extension, language id `roadbook`. With `vscode-languageclient`:

```ts
const server = { command: "roadbook", args: ["lsp"], transport: TransportKind.stdio }
const client = new LanguageClient("roadbook", "Roadbook", server, {
  documentSelector: [
    { scheme: "file", language: "roadbook" },
    { scheme: "untitled", language: "roadbook" },
  ],
  synchronize: { fileEvents: workspace.createFileSystemWatcher("**/*.road") },
})
```

Watching `*.road` files is optional: `workspace/didChangeWatchedFiles` makes the server see
files created, deleted or changed outside the editor; saves are seen either way.

Neovim (`vim.lsp.start({ name = "roadbook", cmd = { "roadbook", "lsp" }, root_dir = … })`)
and Helix (`[language-server.roadbook] command = "roadbook"`, `args = ["lsp"]`) work the same
way.

## In code

```rust,ignore
roadbook_lsp::run_stdio(Box::new(|file: &std::path::Path| {
    Ok(roadbook_lsp::ProjectConfig::standalone(file.parent().unwrap()))
}))?;
```

`run` takes any `lsp_server::Connection`; the tests drive it in-process with
`Connection::memory()` (`tests/server.rs`), and `crates/roadbook-cli/tests/lsp.rs` drives
the `roadbook lsp` binary over stdio (an editor session, and many changes sent at once
compiled together).
