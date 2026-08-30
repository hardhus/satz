# satz

A fast Rust toolkit for Markdown knowledge-base vaults — Obsidian-style wikilinks, hierarchical tags, block/heading anchors, backlinks, and daily notes — shipped as both a scriptable **CLI** and a full-featured **Language Server (LSP)**.

Point it at a folder of `.md` files and it gives you: link/tag/backlink indexing, broken-link detection, graph export, and (through the LSP) go-to-definition, hover, rename, completion, diagnostics, and more in any LSP-capable editor.

## Features

**Vault indexing (shared by both CLI and LSP)**
- Wikilinks `[[note]]`, embeds `![[note]]`, standard Markdown links, and footnotes — all resolved against the vault.
- Aliases, titles, and hierarchical tags (`#parent/child`), resolved case- and Unicode-fold-insensitively.
- Heading and block (`^block-id`) anchors, with Unicode-aware (Turkish-correct) slugification.
- Backlink graph, orphan-note detection, and broken-link/broken-anchor detection.
- Fast: parallel parsing (`rayon`) and incremental re-indexing (no full rebuild on every edit).

**Deterministic formatter (shared by both CLI and LSP)**
- Same input always produces the same byte-for-byte output — no more vault drift from mixed `-`/`*` list markers or `*italic*` vs `_italic_`.
- GFM tables (column alignment, Unicode-width-aware padding), list markers and ordered-list renumbering, task-list checkboxes, emphasis/strong delimiters, thematic breaks, code-fence style, and blockquote spacing — every construct independently toggleable in `.satz.toml`.
- Whole-vault formatting from the CLI (`satz fmt --check`/`--write`) or the editor (`satz.formatWorkspace`), plus single-file `textDocument/formatting` — all producing minimal, line-scoped edits rather than replacing whole files.

**CLI (`satz`)**
- `index`, `stats` — vault-wide summary (docs, links, tags, orphans), human or JSON output.
- `list` — filter documents by tag, orphan status, or list every broken link.
- `resolve` — resolve a wikilink target to a file path from the shell.
- `daily` — open or create today's daily note.
- `fmt` — format the whole vault in place, or `--check` it in CI/pre-commit (tables, lists, emphasis, thematic breaks, code fences, blockquotes — deterministic, byte-for-byte).
- `graph` — export the link graph as Graphviz DOT or JSON.

**LSP (`satz-lsp`)**
- Go to definition, find references, hover previews, document/workspace symbols (fuzzy search, tag-scoped).
- Diagnostics: broken links/embeds/headings, duplicate heading slugs, missing frontmatter fields, orphan notes.
- Rename (headings and whole documents, including a file-rename operation), prepare-rename.
- Context-aware completion for links, headings, block anchors, footnotes, and tags.
- Code actions (create a missing note, add a missing heading, insert a frontmatter template, format the entire vault), code lens (backlink count), inlay hints (target metadata), semantic tokens, folding ranges, document links, document formatting.
- `satz.formatWorkspace` command — format every document in the vault in one `workspace/applyEdit`, with a result cache so repeat calls against an unchanged vault do no work.
- Live file-watching and `.satz.toml` hot-reload — no restart needed after editing config or after external file changes.

Full breakdowns: [`docs/cli.md`](docs/cli.md) and [`docs/lsp.md`](docs/lsp.md).

## Installation

satz is not published to crates.io (`publish = false` — see [License](#license)). Build it from source:

```sh
git clone https://github.com/hardhus/satz.git
cd satz
cargo build --release
```

This builds the workspace's three crates and produces two binaries:

- `target/release/satz` — the CLI.
- `target/release/satz-lsp` — the language server.

Add `target/release/` to your `PATH`, or reference the binaries by their full path from your editor config.

## Quick start: CLI

```sh
satz index .                          # one-shot summary
satz stats --vault . --json           # scriptable stats
satz list --vault . --tag project     # notes tagged #project (and #project/*)
satz list --vault . --broken          # every broken link, with file:line
satz resolve --vault . "[[My Note]]"  # -> path/to/my-note.md
satz daily .                          # open/create today's daily note
satz fmt . --check                    # CI check: exit 1 if anything needs formatting
satz graph --vault . -f dot -o graph.dot && dot -Tsvg graph.dot -o graph.svg
```

Full command reference, every flag, and exact output formats: **[`docs/cli.md`](docs/cli.md)**.

## Quick start: LSP

Launch `satz-lsp` over stdio for Markdown files; it discovers the vault root from your editor's workspace folder.

**Helix** (`languages.toml`, project or user config):

```toml
[language-server.satz]
command = "satz-lsp"

[[language]]
name = "markdown"
language-servers = ["satz"]
```

**Neovim** (built-in LSP client, no plugin required beyond `nvim-lspconfig` optionally):

```lua
vim.api.nvim_create_autocmd("FileType", {
  pattern = "markdown",
  callback = function(args)
    vim.lsp.start({
      name = "satz",
      cmd = { "satz-lsp" },
      root_dir = vim.fs.root(args.buf, { ".satz.toml", ".git" }) or vim.fn.getcwd(),
    })
  end,
})
```

Full capability list, diagnostics codes, rename/code-action semantics, and more editor snippets (generic clients): **[`docs/lsp.md`](docs/lsp.md)**.

## Configuration

Drop an optional `.satz.toml` in your vault root to override defaults — daily note folder/format, required frontmatter fields, hover/diagnostics/formatter behavior, and LSP tuning (debounce, code lens, inlay hints). Nothing is required; every field has a sensible default.

```toml
[daily_note]
folder = "journal"
format = "%Y/%m/%d"

[frontmatter]
required_fields = ["title", "date"]
```

The LSP hot-reloads this file — edit and save it while the server is running and changes apply immediately. Full field-by-field reference, types, and defaults: **[`docs/configuration.md`](docs/configuration.md)**.

## Vault syntax

satz understands wikilinks, embeds, hierarchical tags, block/heading anchors, footnotes, and YAML frontmatter, plus relative daily-note aliases like `[[bugün]]`/`[[today]]` — and, for the formatter, GFM tables, task-list checkboxes, and standard list/emphasis/thematic-break/blockquote/code-fence syntax. See **[`docs/syntax.md`](docs/syntax.md)** for the exact rules (what counts as a tag, how link resolution priority works, heading slugification, etc.).

## Documentation

| File | Covers |
|---|---|
| [`docs/cli.md`](docs/cli.md) | Every `satz` subcommand, every flag, exact output formats and exit codes. |
| [`docs/lsp.md`](docs/lsp.md) | Full LSP capability list, diagnostics codes, rename/code-action behavior, editor setup snippets. |
| [`docs/configuration.md`](docs/configuration.md) | Complete `.satz.toml` schema — every key, type, default, and what actually reads it. |
| [`docs/syntax.md`](docs/syntax.md) | The Markdown dialect: wikilinks, tags, anchors, frontmatter, link resolution order. |
| [`docs/architecture.md`](docs/architecture.md) | Crate layout and how to embed `satz-core`/`satz-cli` as libraries. |

## Project layout

- `satz-core` — the parsing/indexing engine (Markdown parser, `Index`, `VaultGraph`, formatter, config). Pure library, no CLI/LSP dependencies.
- `satz-cli` — the `satz` binary, plus a `satz_cli` library crate for embedding its commands directly.
- `satz-lsp` — the `satz-lsp` binary (built on `tower-lsp-server`).

See [`docs/architecture.md`](docs/architecture.md) if you want to use `satz-core` (or `satz-cli`) as a dependency in your own tool instead of shelling out to the binaries.

## License

satz is **source-available**, not open source. Per [`LICENSE`](LICENSE): you may inspect, compile, run, and modify it for personal or internal use, but you may **not** redistribute it (source or binary), publicly display it, or create/publish a fork or continuation of it. The copyright notice must be preserved in any copy. The software is provided as-is, without warranty. Read the full [`LICENSE`](LICENSE) file for the exact terms.
