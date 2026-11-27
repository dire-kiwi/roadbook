# Roadbook for VS Code

Support for Roadbook statecharts (`.road` files) in VS Code: highlighting, everything the
language server `roadbook lsp` offers, and a live diagram preview. Private to this repository;
it is not published to the Marketplace.

## Features

- **The `roadbook` language** for `*.road`: a TextMate grammar (`syntaxes/roadbook.tmLanguage.json`)
  that tells keywords, declarations, states, events and types (upper-case names) from values
  (lower-case names), calls, strings and their escapes, numbers, durations (`30s`), operators,
  and `///` doc comments from `//` comments. Comment toggling, bracket matching and
  auto-closing, indentation after `{` and `(`, `// region` folding markers, and `///`
  continued on Enter.
- **The language server.** The extension runs `roadbook lsp` over standard input and output, so
  diagnostics, go to definition, references, rename, hover, formatting, document symbols and
  code actions (the compiler's fixes) come from the compiler itself, in saved files and in
  untitled editors (compiled on their own, with no project). **Roadbook: Restart Language
  Server** restarts it; so does changing `roadbook.path`, and a restart asked for while the
  server is still starting waits for it and then starts one with the new setting. If the
  command line is older than the language server, the extension says so and the rest still
  works.
- **Live preview.** **Roadbook: Open Preview to the Side** (`Ctrl+K V`, `Cmd+K V` on macOS, or
  the preview button in the editor's title bar) opens the diagram viewer that `roadbook viz`
  writes, for the chart in the editor:
  - It updates when the file is saved and, after a short pause, as you type, in the chart or in
    any other open chart (one it imports, say). The language server compiles it, with the
    unsaved text of every open editor (its `roadbook/ir` request, see
    `crates/roadbook-lsp/README.md`), as `roadbook export ir` would compile the files: under
    the chart's own project path and with the project's options, so the preview and the
    command line agree. Untitled charts can be previewed too. Without the language server
    (an older command line), the preview runs `roadbook export ir` on the saved file, so it
    shows the chart as last saved.
  - When the chart does not compile, the last diagram that did stays, with the errors over it;
    click one to go to it. With no diagram yet, the preview is the list of errors.
  - It follows VS Code's light or dark theme.
  - When the chart changes, the page is replaced, and the machine you had open and the state or
    transition you had selected are selected again. Zoom, pan and collapsed states start
    afresh.
  - Clicking a state or a transition in the diagram shows its line in the editor. The page can
    ask to open only the chart and the `.road` files inside its project (an error's file);
    anything else it names, such as a path out of the project, is not opened.
  - The page runs only the viewer and the extension's bridge script, under a
    Content-Security-Policy with a fresh nonce per page: no inline scripts, no `eval`, nothing
    loaded from the network.

## Setup

The extension needs the `roadbook` command line. From this repository:

```sh
cargo build -p roadbook-cli          # target/debug/roadbook
```

then put it on your `PATH`, or point `roadbook.path` at it. Highlighting works without it.

## Settings

| Setting | Default | |
| --- | --- | --- |
| `roadbook.path` | `roadbook` | The command line. A bare name is looked up on `PATH`; a relative path is taken from the first workspace folder; `${workspaceFolder}` and a leading `~` are expanded. Changing it restarts the language server. Restricted: see below. |
| `roadbook.preview.updateOnChange` | `true` | Update the preview as you type, not only on save. |
| `roadbook.preview.debounce` | `300` | Milliseconds to wait after the last change before updating. |
| `roadbook.preview.revealSource` | `true` | Clicking a state or transition in the preview reveals its line. |
| `roadbook.trace.server` | `off` | Log the language server's messages (`messages`, `verbose`) in the Roadbook output channel. |

Since `roadbook.path` names a program the extension runs, a workspace's own value (in its
`.vscode/settings.json`) is used only in a trusted workspace. The extension declares the
setting in `capabilities.untrustedWorkspaces.restrictedConfigurations`, so in Restricted Mode
VS Code ignores the workspace's value and the extension runs the command line from your user
settings or `PATH`; trusting the workspace restarts the server with its value. The setting
stays `machine-overridable` rather than `machine`, so that a trusted project can point at its
own build (`${workspaceFolder}/target/debug/roadbook`).

## Running from source

```sh
pnpm install                          # at the repository root
pnpm --filter roadbook-vscode build   # dist/extension.js, dist/bridge.js, dist/roadbook-viewer.js
```

Open this folder (`editors/vscode`) in VS Code and press `F5`: a second window opens on
`examples/` with the extension loaded from source (`.vscode/launch.json` builds first). Set
`roadbook.path` there to your `target/debug/roadbook` if it is not on your `PATH`.

`build` bundles the extension with esbuild, with the language client inside, and copies the
committed viewer bundle (`crates/roadbook-cli/assets/roadbook-viewer.js`) into `dist/`, so the
preview shows exactly what `roadbook viz` does. After changing the viewer, rebuild it
(`pnpm --filter @dire-kiwi/roadbook-viewer build`) and then this package.

## Tests

```sh
pnpm --filter roadbook-vscode typecheck
pnpm --filter roadbook-vscode test
```

`test` runs:

- the grammar against snapshots (`test/snapshots/*.snap`: the shop and upload examples and
  `test/grammar/edge-cases.road.txt`, every token with its scopes) and a few targeted
  scope checks; `pnpm exec vitest run -u` rewrites the snapshots, so review the diff;
- unit tests of the preview page (its Content-Security-Policy, its scripts, how the IR is
  embedded), of reading the language server's `roadbook/ir` answers, of the files the page may
  open, of starting and stopping in order, and of compiling a saved chart with the real
  command line when one is built;
- an integration test in a real VS Code (`test/integration`): it opens
  `examples/shop/checkout.road` and checks its language, opens the preview and waits for the
  viewer to draw it, reveals a state's and a transition's line, waits for the language
  server's diagnostics on a broken copy (skipped, saying so, if `roadbook lsp` is not
  available), checks that the preview shows that copy's errors, follows unsaved edits (to the
  chart, and to `types.road`, which it imports) and restores the selection, opens only the
  project's charts, previews an untitled chart and shows its diagnostics, runs restarts and
  stops asked for in a row in order, and that the page broke no Content-Security-Policy rule.

The integration test skips, saying why, unless it can run. It needs the command line built
(`ROADBOOK_BIN`, or `debug/roadbook` under `CARGO_TARGET_DIR` or `target/`) and a VS Code:
`ROADBOOK_VSCODE_TEST=1` downloads the latest stable (about 150 MB, or
`ROADBOOK_VSCODE_VERSION`) into `ROADBOOK_VSCODE_CACHE` (default `editors/vscode/.vscode-test`),
and later runs reuse it. On Linux VS Code runs with Chromium's headless Ozone platform, so no
display is needed; `ROADBOOK_VSCODE_HEADLESS=0` uses the display, or `xvfb-run` if there is
none. `ROADBOOK_VSCODE_TEST=0` always skips.

## Packaging a `.vsix` locally

Do not publish it. To install the extension in your own VS Code:

```sh
pnpm --filter roadbook-vscode build
cd editors/vscode
pnpm dlx @vscode/vsce package --no-dependencies --allow-missing-repository --skip-license
code --install-extension roadbook-vscode-0.1.0.vsix
```

`--no-dependencies` is right because `dist/extension.js` bundles everything it needs;
`.vscodeignore` keeps the package to `dist/`, the grammar, the language configuration and
this README.
