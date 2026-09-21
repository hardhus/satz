# Architecture & embedding

This page is for anyone who wants to use satz's pieces as a Rust library rather than just running the binaries.

## Workspace layout

```
satz-core/   library only — parsing, indexing, graph, formatter, config, no I/O beyond walking the filesystem and reading `.satz.toml`
satz-cli/    binary `satz` + lib `satz_cli` — thin command layer on top of satz-core
satz-lsp/    binary `satz-lsp` — tower-lsp-server implementation on top of satz-core
```

`satz-core` has no dependency on either `satz-cli` or `satz-lsp`, and no async runtime dependency — it's a plain synchronous library, safe to pull into any Rust project (CLI tool, GUI app, build script, etc.) that wants to understand a folder of Markdown files.

## `satz-core`: the embedding path

The typical flow, mirroring what both `satz-cli` and `satz-lsp` do internally:

```rust
use satz_core::{walk_vault, Index, VaultGraph};

let docs = walk_vault(vault_root)?;      // Vec<Document> — parses every .md file in parallel
let index = Index::build(docs);          // resolves links, builds backlink/tag maps

index.resolve_link("some-note");         // -> Option<&DocId>
index.docs_with_tag("project");          // -> impl Iterator<Item = &Document>
index.backlinks_of(&doc_id);             // -> impl Iterator<Item = &DocId>
index.stats();                           // -> IndexStats

let graph = VaultGraph::build(&index);   // petgraph-backed
graph.export_json()?;
graph.export_dot();
```

Key types:

- **`parse_document(source: &str, path: &Path) -> Document`** — the single-file entry point. Never panics; a document with malformed YAML frontmatter just gets `Frontmatter::default()` instead of erroring. Useful if you're feeding it in-memory buffers (an editor's unsaved content, for instance) rather than reading from disk.
- **`Document`** — everything extracted from one file: `id` (`DocId`, a vault-relative path string), `title`, `frontmatter`, `headings`, `links`, `tags`, `footnotes`, `blocks`, `line_index` (UTF-16-safe position conversion, useful if you're building LSP-adjacent tooling of your own), `content_hash`.
- **`Index`** — the whole-vault view: document lookup by id/path, link resolution, tag queries, backlinks, orphan/broken-link queries, and incremental updates via `replace_doc`/`remove_doc` (what the LSP uses on every keystroke instead of rebuilding from scratch). An edit that leaves a document's title, aliases and file stem unchanged only refreshes that document's own links and tags; adding, removing, or changing the identity of a document rebuilds the derived lookups and backlinks so links elsewhere in the vault (for example, a `[[new-note]]` that was broken until `new-note.md` was created) resolve correctly. Both paths are tested to produce exactly what a fresh `Index::build` would.
- **`VaultGraph`** — a `petgraph::DiGraph` wrapper for exporting the link graph; see [`docs/cli.md`](cli.md#satz-graph) for what the JSON/DOT shapes look like.
- **`VaultConfig`** — parses `.satz.toml`; see [`docs/configuration.md`](configuration.md) for the full schema (and which fields are currently load-bearing vs. reserved).
- **`formatter::format_document(source: &str, config: &FormatterConfig) -> String`** — the deterministic Markdown formatter (tables, lists, emphasis, thematic breaks, code fences, blockquotes, optional paragraph wrapping; see [`docs/configuration.md`](configuration.md#formatter--deterministic-markdown-formatting)). This is the same function `satz fmt` and the LSP's formatting requests call — embedding it directly gets you the formatter without going through either binary. `formatter::diff::line_diff(old, new)` is the minimal line-based diff helper the LSP uses to turn a `format_document` result into small edits instead of a whole-document replace.

## `satz-cli`: using it as a library

`satz-cli` builds both a binary (`satz`) and a library crate (`satz_cli`), specifically so a host application can embed the CLI's command implementations (`satz_cli::commands::*`) directly — calling `commands::stats_cmd::run(args)` in-process, for example — instead of shelling out to the `satz` binary and parsing its stdout. Each command also has `run_with_output(args, &mut writer)`, which writes what the command prints to the writer you give it (a `Vec<u8>`, for instance) instead of stdout; `run` writes to stdout and lets a reader that went away (`| head`) end the output quietly. `resolve` does not end the process when the target is unknown: like `fmt` it returns an `Outcome` (`Found` / `NotFound`) and leaves the exit status to the caller. The library exposes the same `Cli`/`Commands` clap types as the binary, so argument parsing behaves identically either way.

## `satz-lsp`

Built on [`tower-lsp-server`](https://docs.rs/tower-lsp-server) with a `tokio` async runtime. Not designed to be embedded as a library the way the other two crates are — it's a standalone server process. Its internal module layout (`backend.rs` for the `LanguageServer` trait impl, `handlers/` for one file per LSP request, `state.rs` for the shared in-memory `SatzState`, `watcher.rs` for the file-watching/hot-reload loop) is a reasonable reference if you're building a different editor integration on top of `satz-core` directly instead of speaking LSP.

## See also

- [`docs/configuration.md`](configuration.md)
- [`docs/syntax.md`](syntax.md)
