# Language server reference

`satz-lsp` is a standard [LSP](https://microsoft.github.io/language-server-protocol/) server communicating over stdio. Point any LSP-capable editor at the `satz-lsp` binary for Markdown files and it will pick up the vault root from the client's `workspaceFolders` (or the deprecated `rootUri` as a fallback).

Vault indexing happens in the background right after `initialize` — a big vault won't block the editor from opening, but navigation/diagnostics for cross-file links may lag slightly behind indexing completion on first load.

## Capabilities at a glance

| Capability | Notes |
|---|---|
| Text sync | Incremental (`TextDocumentSyncKind::INCREMENTAL`), UTF-16 position semantics per the LSP spec. |
| Diagnostics | Both pull (`textDocument/diagnostic`) and push (`textDocument/publishDiagnostics`), auto-detected from client capabilities. Also supports `workspace/diagnostic`. |
| Go to definition | Wikilinks, embeds, markdown links, footnote references. |
| Find references | Documents, headings, block anchors, tags — see [Reference/highlight targets](#referencehighlight-targets). |
| Hover | Preview of the link target (section/paragraph-scoped when the link has a heading/block anchor); footnote hover shows the footnote body. |
| Document highlight | Same-document occurrences of whatever's under the cursor (tag family, heading + links to it, block + links to it). |
| Completion | Context-aware: note/alias completion inside `[[`, heading completion inside `[[doc#`, block completion inside `[[doc#^`, footnote label completion inside `[^`, tag completion after `#`. Supports `completionItem/resolve` for note previews. |
| Document symbols | Heading outline, nested by level. |
| Workspace symbols | Fuzzy search (via `nucleo-matcher`) over titles, aliases, and headings across the whole vault; supports a `tag:<name> <query>` prefix to scope the search to a tag. |
| Rename / prepare rename | Heading renames and document renames (see [Rename](#rename) below). |
| Code actions | Create a missing note, add a missing heading to a target document, insert a frontmatter template. |
| Document links | Clickable ranges for every resolvable link, plus external `http(s)://` links. |
| Folding ranges | Frontmatter block, and each heading's section (nested by level). |
| Code lens | "N backlinks" above the document. **Off by default** (`lsp.codelens.enable`). |
| Inlay hints | Inline note metadata after links. **On by default** (`lsp.inlay_hints.enable`). |
| Semantic tokens | Full-document only (no range requests). Legend: `link`, `unresolvedLink`, `tag`, `heading`, `embed`, `blockAnchor`. |
| Document formatting | Whitespace/blank-line/link normalization — see [`docs/configuration.md`](configuration.md#formatter--used-by-the-lsps-format-document-and-by-satz_coreformatterformat_document-if-embedded-directly). |

## Diagnostics

Diagnostic codes you'll see in `diagnostic.code`:

| Code | Severity | Meaning |
|---|---|---|
| `broken-link` | Warning | A wikilink or markdown link's target document couldn't be resolved. |
| `broken-embed` | Warning | Same, for an `![[...]]` embed. |
| `broken-heading` | Warning | The target document (or current document, for a same-doc `#Heading`/`#^block` reference) exists, but the requested heading or block anchor doesn't. |
| `duplicate-heading` | Warning | Two headings in the same document slugify to the same value, making `#Heading` links to either of them ambiguous. |
| `missing-frontmatter-field` | Warning | A field listed in `frontmatter.required_fields` is missing (see [`docs/configuration.md`](configuration.md#frontmatter--used-by-the-lsps-diagnostics)). |
| `orphan-note` | Hint | Nothing links to this document, and it has at least one link or heading of its own (so brand-new empty notes don't get flagged). Suppressed for notes tagged with any of `diagnostics.moc_tags`. |

## Code actions

Offered contextually depending on what's under the cursor/selection:

- **Create note** — offered on a broken wikilink/embed/markdown link; creates the target `.md` file (with a generated frontmatter + heading template) and opens it via a `workspace/applyEdit` create-then-edit operation.
- **Add heading** — offered on a broken `#Heading` reference where the target document exists; appends `## <Heading>` to the end of the target document.
- **Insert frontmatter template** — offered when the document has no `---` frontmatter block yet; inserts a title/date/aliases/tags template at the top.

## Rename

Triggered from a heading definition or from a link:

- **Renaming a heading** rewrites the `#` line in its document and rewrites every same-document and cross-document link whose `#Heading` reference matches it (matched via [heading matching rules](syntax.md#heading-slugs--matching), so case/slug variants are all caught). The edit is scoped to the target document plus its known backlinks — it does not scan the entire vault.
- **Renaming via a link's target document** emits a `workspace/applyEdit` with a file-rename operation (`ResourceOp::Rename`) for the target file, plus text edits updating every in-scope link (again scoped to backlinks + the target itself, not a full-vault scan).

`textDocument/prepareRename` is implemented, so clients get the correct placeholder text (the current heading text, or the current link target) before you type a new name.

## Reference/highlight targets

Find References and Document Highlight both resolve "what's under the cursor" with the same priority order: **link → block anchor → heading → tag → whole document**. For tags, the search expands hierarchically (referencing `#parent` also surfaces `#parent/child` occurrences). For headings/blocks/documents, results include both the definition (if in the current/target document) and every link that resolves to it, scoped to that target's known backlinks.

## Code lens caveat

The backlink-count CodeLens's command is `satz.showBacklinks`. The server does not implement `workspace/executeCommand`, so clicking it does nothing unless your editor/extension separately binds that command to some client-side action (e.g. opening a references panel). Treat it as an informational count unless you've wired up a client-side handler.

## Live reindexing & config hot-reload

A background file watcher (`notify`, polling every 500ms with a further 200ms debounce) keeps the in-memory index in sync without needing to restart the server:

- Creating, modifying, or deleting a `.md` file outside the editor (e.g. `git checkout`, another tool writing to the vault) triggers a re-index of just that file — unless it's currently open in the client, in which case the editor's own buffer stays authoritative.
- Editing and saving `.satz.toml` on disk reloads the whole configuration live; the client is notified to refresh diagnostics afterward.
- Whether diagnostics are then pushed or the client is asked to re-pull depends on whether the client advertised diagnostic pull support during `initialize`.

Two config fields control edit-triggered (as opposed to file-watcher-triggered) reparsing latency: `lsp.reparse_debounce_ms` and `lsp.reparse_max_wait_ms` — see [`docs/configuration.md`](configuration.md#lsp--server-wide-lsp-tuning).

## Editor setup

satz doesn't ship an editor extension — configure your client's generic/manual LSP support to launch `satz-lsp` for Markdown files.

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

**Helix** (`languages.toml`, project or user config):

```toml
[language-server.satz]
command = "satz-lsp"

[[language]]
name = "markdown"
language-servers = ["satz"]
```

**Any other client**: point its "custom/manual language server" configuration at the `satz-lsp` executable with stdio transport and `markdown` as the language ID; no command-line arguments are needed.

## See also

- [`docs/configuration.md`](configuration.md) — every `[lsp]`, `[hover]`, `[diagnostics]`, `[formatter]` field.
- [`docs/syntax.md`](syntax.md) — the link/tag/anchor/frontmatter conventions all of the above operate on.
