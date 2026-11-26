# tree-sitter-roadbook

A [tree-sitter](https://tree-sitter.github.io) grammar for Roadbook `.road` files, with
queries for highlighting, locals, folds and indents. It follows the grammar in
[`docs/design/language.md`](../../docs/design/language.md#grammar). The reference parser
is still `crates/roadbook-syntax`, and the compiler's diagnostics are the real check. This
grammar is for editors.

```
grammar.js            the grammar
src/                  the generated parser (committed; do not edit)
queries/              highlights.scm, locals.scm, folds.scm, indents.scm
test/corpus/          corpus tests, including inputs that must not parse
check.sh              every check below
```

## Build and test

You need the tree-sitter CLI 0.26 (`npm install -g tree-sitter-cli@0.26`, or
`cargo install tree-sitter-cli`) and a C compiler.

```sh
cd editors/tree-sitter-roadbook
tree-sitter generate          # after editing grammar.js; commit src/ with it
tree-sitter test              # the corpus
tree-sitter parse path/to/chart.road
tree-sitter highlight path/to/chart.road
./check.sh                    # everything below
```

`check.sh` fails if `src/` is not what `tree-sitter generate` produces from `grammar.js`.
It runs the corpus, parses every `.road` file in the repository and fails on any `ERROR`
or `MISSING` node, then loads every query and highlights the shop example. The script
skips the fixtures that fail parsing on purpose: those whose `// expect:` lines name a
syntax code (`RB00xx`). `scripts/check-all.sh` runs it when `tree-sitter` is installed.

The grammar accepts a little more than the compiler does. It applies the rules that
`language.md` lists under "Rules the grammar does not express" to the parse tree only
where an editor benefits:

- a parameter or variable named `result` parses. The compiler rejects it (`RB0013`). In
  an expression, `result` is read as the implicit name.
- a `///` comment parses anywhere, not only before something it documents.
- a final state may hold any state item, and `output`/`fail` may appear in any state.
- there is no limit on nesting depth or on the size of a number, and `T??` parses.

Like the compiler, it rejects chained comparisons, `not not x`, an unparenthesised record
value as an `after` delay, a branch block that does not start with `if`, a transition that
does nothing, and a machine-only or state-only item in the wrong body.

## Neovim

Neovim 0.10 or later needs only the compiled parser and the queries on its runtime path.
You do not need nvim-treesitter:

```sh
cd editors/tree-sitter-roadbook
site=~/.local/share/nvim/site
mkdir -p "$site/parser" "$site/queries/roadbook"
cc -shared -fPIC -O2 -I src src/parser.c -o "$site/parser/roadbook.so"
cp queries/*.scm "$site/queries/roadbook/"
```

```lua
vim.filetype.add({ extension = { road = "roadbook" } })
vim.api.nvim_create_autocmd("FileType", {
  pattern = "roadbook",
  callback = function()
    vim.treesitter.start()
    vim.bo.commentstring = "// %s"
    vim.wo.foldmethod = "expr"
    vim.wo.foldexpr = "v:lua.vim.treesitter.foldexpr()"
  end,
})
```

The capture names follow nvim-treesitter's (`@keyword.conditional`, `@function.call`,
`@comment.documentation`, …). `indents.scm` uses nvim-treesitter's indent captures.

## Helix

In `~/.config/helix/languages.toml`:

```toml
[[language]]
name = "roadbook"
scope = "source.roadbook"
file-types = ["road"]
roots = ["roadbook.toml"]
comment-token = "//"
indent = { tab-width = 2, unit = "  " }

[[grammar]]
name = "roadbook"
source = { path = "/path/to/this/repo/editors/tree-sitter-roadbook" }
```

Then run `hx --grammar build`, and copy `highlights.scm` and `locals.scm` to
`~/.config/helix/runtime/queries/roadbook/`. Helix names a few scopes differently, and a
capture it does not know is left uncoloured. Rename these in the copy: `@number` to
`@constant.numeric`, `@boolean` to `@constant.builtin.boolean`, `@keyword.modifier` to
`@keyword.storage.modifier`, `@comment.documentation` to `@comment.block.documentation`.
Do not copy `indents.scm`: Helix uses different indent captures.

## Zed

Zed loads grammars through an extension. A minimal dev extension (Extensions → Install Dev
Extension) has this layout:

```toml
# extension.toml
id = "roadbook"
name = "Roadbook"
version = "0.1.0"
schema_version = 1

[grammars.roadbook]
repository = "file:///path/to/this/repo"
rev = "<commit>"
path = "editors/tree-sitter-roadbook"
```

```toml
# languages/roadbook/config.toml
name = "Roadbook"
grammar = "roadbook"
path_suffixes = ["road"]
line_comments = ["// "]
tab_size = 2
```

Copy `queries/highlights.scm` to `languages/roadbook/`.
