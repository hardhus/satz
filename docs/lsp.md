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
| Document highlight | Same-document occurrences of whatever is under the cursor (the narrowest thing wins: a tag inside a heading is a tag): the tag family, a heading + links to it, a block + links to it, a footnote (every reference and its definition), or links to the same missing note. Links are resolved exactly like everywhere else (folder-relative Markdown paths, relative daily aliases). |
| Completion | Context-aware: note/alias completion inside `[[`, heading completion inside `[[doc#` (a heading that appears twice is offered once: links reach the first), block completion inside `[[doc#^` (the `^` you typed is replaced, never doubled), footnote label completion inside `[^`, tag completion after `#` (only where a tag can be typed — `# Heading text` is a heading marker — and with the spelling the vault uses most: `#Proje`, not the folded `#proje`). A `[[` already closed by `]]` earlier on the line is not a link being typed (so tags and footnotes still complete after it), and nothing is offered after a `|` (the display text). The word after the cursor is replaced too (`[[Ol|gu]]` becomes `[[doc-b]]`), a single following `]` is completed to `]]`, and candidates come in a stable order. When there are more than `lsp.completion_limit` (default 200) the list is first narrowed by what you have typed (exact match, then prefix, then fuzzy; case, `İ/ı` and NFC/NFD do not matter) and only then cut, and marked incomplete so the client asks again while you type; a list that fits is sent whole and the client filters it. Text after the cursor that is not part of the link (`x^2`, a later `#tag`, a table `|`) is never replaced. Supports `completionItem/resolve` for note previews (the note's first lines, not its frontmatter). |
| Document symbols | Heading outline, nested by level. Each symbol spans its whole section (children lie inside it); its selection is the heading line; an empty heading is named `(empty heading)`. |
| Workspace symbols | Fuzzy search (via `nucleo-matcher`) over titles, aliases, and headings across the whole vault; supports a `tag:<name> <query>` prefix to scope the search to a tag. |
| Rename / prepare rename | Heading renames and document renames (see [Rename](#rename) below). |
| Code actions | Create a missing note, add a missing heading to a target document, insert a frontmatter template. |
| Document links | Clickable ranges for every resolvable link, plus external `http(s)://` links. |
| Folding ranges | The frontmatter block the parser found, and each heading's section (nested by level; the last section ends at the last content line). |
| Code lens | "N backlinks" above the document; clicking it runs `satz.showBacklinks` (see [Show backlinks](#show-backlinks)). **Off by default** (`lsp.codelens.enable`). |
| Inlay hints | Inline note metadata after links (only for links inside the requested range); `⚠ not found`, `⚠ heading not found` or `⚠ block not found` for broken ones, also for links to a heading of the same note. **On by default** (`lsp.inlay_hints.enable`). |
| Semantic tokens | Full-document only (no range requests). Legend: `link`, `unresolvedLink`, `tag`, `heading`, `embed`, `blockAnchor`, `linkDisplay`. Footnote references (`[^label]`) get `link` when a matching definition exists, `unresolvedLink` when it doesn't (via a manual text scan — see the `broken-footnote` diagnostic below). A `[[target\|display]]`/`![[target\|display]]` link's `\|display` part gets its own `linkDisplay` token by default — see `lsp.semantic_tokens.split_link_display` in [`docs/configuration.md`](configuration.md#lsp--server-wide-lsp-tuning). Tokens never overlap and never span lines: inside a heading the link/tag/anchor tokens win and the heading colour covers the rest of the line, and a link that wraps across lines is coloured on each line. Refreshed automatically once initial vault indexing finishes, so a document opened before indexing completed gets recolored rather than staying stuck with mis-resolved links. Also refreshed after every debounced re-parse of an edited document (as is a pull client's diagnostics), so results are never one edit behind. |
| Document formatting | Deterministic, structure-aware Markdown formatting (tables, lists, emphasis, thematic breaks, code fences, blockquotes) — see [`docs/configuration.md`](configuration.md#formatter--deterministic-markdown-formatting). |
| Execute command | `satz.formatWorkspace` — formats every document in the vault in one `workspace/applyEdit`. See [Format the whole workspace](#format-the-whole-workspace). |

## Diagnostics

Pull diagnostics (`textDocument/diagnostic`, `workspace/diagnostic`) carry a `resultId`; a client that sends it back as `previousResultId` gets an `unchanged` report while nothing in the vault or the configuration has changed, instead of the full list. Any change invalidates every id (a diagnostic can depend on the whole index). `workspace/semanticTokens/refresh` is sent to every client, and `workspace/diagnostic/refresh` to every client that pulls diagnostics, whether or not the client announced `refreshSupport` for them: the colours and diagnostics of a note opened while the vault is still being indexed are computed from an incomplete index (its links look unresolved), and this request is what makes the client fetch them again once it is complete. A client that does not know the request just answers with an error, which is only logged at debug level.

Diagnostic codes you'll see in `diagnostic.code`:

| Code | Severity | Meaning |
|---|---|---|
| `broken-link` | Warning | A wikilink or markdown link's target document couldn't be resolved. |
| `broken-embed` | Warning | Same, for an `![[...]]` embed. |
| `broken-heading` | Warning | The target document (or current document, for a same-doc `#Heading`/`#^block` reference) exists, but the requested heading or block anchor doesn't. |
| `duplicate-heading` | Warning | Two headings in the same document slugify to the same value, making `#Heading` links to either of them ambiguous. |
| `missing-frontmatter-field` | Warning | A field listed in `frontmatter.required_fields` is missing (see [`docs/configuration.md`](configuration.md#frontmatter--used-by-the-lsps-diagnostics)). |
| `orphan-note` | Hint | Nothing links to this document, and it has at least one link or heading of its own (so brand-new empty notes don't get flagged). Suppressed for notes tagged with any of `diagnostics.moc_tags`. |
| `invalid-frontmatter` | Warning | The `---` frontmatter block is not valid YAML (or not a mapping). Its title, aliases and tags are ignored, so the message names the parse error. |
| `broken-footnote` | Warning | A `[^label]` reference has no matching `[^label]: ...` definition in the same document (footnotes, like standard Markdown, are always same-document). Found via a manual text scan independent of the structural parser, since pulldown-cmark itself never recognizes an undefined `[^label]` as a footnote reference at all. Labels match case-insensitively (`[^A]` refers to `[^a]:`), the same way pulldown-cmark resolves them. Known limit: prose that merely looks like a footnote reference outside a code span (for example a regex character class written as `[^a-z]`) is indistinguishable from one and will be flagged. |

## Code actions

Offered contextually depending on what's under the cursor/selection:

- **Create note** — offered on a broken wikilink/embed/markdown link; creates the target `.md` file (with a generated frontmatter + heading template) and opens it via a `workspace/applyEdit` create-then-edit operation. The target is only offered when it is a plain note name: `a/b/c` creates the folders, but `..`, `.`, drive letters (`C:x`), URL schemes (`mailto:`), names with a real file extension (`image.png`, `doc.pdf` — while `tlp/2.0121` is fine), Windows-forbidden characters (`* ? " < > |`), a trailing dot or a component over 255 bytes get no quick fix, so a file is never created outside the vault. An existing file is never overwritten. The title is written to YAML quoted when needed (`Q: what` becomes `title: "Q: what"`), so the frontmatter always stays valid.
- **Add heading** — offered on a broken `#Heading` reference where the target document exists; appends `## <Heading>` to the end of the target document.
- **Insert frontmatter template** — offered when the document has no `---` frontmatter block yet; inserts a title/date/aliases/tags template at the top.
- **Format entire vault** (`CodeActionKind::SOURCE`) — always offered (whenever `formatter.enabled` is true), regardless of cursor position; runs the same `satz.formatWorkspace` command described below. Some clients surface source actions in the code action menu more discoverably than a command palette entry, hence offering both.

## Workspaces

One server indexes one vault. With several workspace folders the first local folder is the vault; the others are named in a warning message and are not indexed (start another server for each). A folder listed twice counts once, and a folder that is not a local `file:` URI is skipped.

## Diagnostics you may not expect to be absent

A Markdown link's `#fragment` is checked against the note's headings, except `#top` and an HTML `id`/`name` the note defines itself (`<a id="x">`), which are anchors too. A `---` ... `---` pair at the top of a note that contains no `key:` line is read as horizontal rules and is not reported as broken frontmatter.

## Link resolution

A link target is tried in one fixed order: the exact path, the path plus `.md`, the path ignoring letter case, the file name (the last path component, without `.md`), then a note's title or alias (case- and Unicode-folded, matched whole). Because the file name decides, the folder part of a `[[wrong-folder/note]]` is a hint only: it still reaches `other/note.md` when that is the only `note.md` (the Obsidian "shortest path" habit). A dot in a name is part of the name, not an extension: `[[2.0121]]` is the note called `2.0121`. Names, titles, headings and tags are compared after Unicode normalization, so `dünya` typed precomposed matches a file or heading written with a combining diaeresis (what macOS file systems hand out).

## Daily-note aliases

`[[bugün]]`, `[[dün]]`, `[[yarın]]` (and the other `daily_note.aliases`) count as links to that day's note: go-to-definition, hover and diagnostics follow them, and so do backlinks and the `orphan-note` hint, so a daily note that is only reached through `[[bugün]]` is not an orphan. A note that really is named like an alias wins over the daily meaning. The date "today" is re-read when a request arrives after midnight or the `daily_note` settings change.

## Rename

Triggered from a heading definition or from a link:

Quick fixes name the diagnostic they answer. Rename and the "Add heading" quick fix return document edits: a file that is open carries the buffer version the edits were computed for, so the client refuses them if you typed in the meantime; files on disk carry no version.

- **Renaming keeps each link's syntax.** Wikilinks, embeds and Markdown links (`[t](a.md#Heading "title")`) are rewritten in their own form; only the heading fragment, or the last path component of the file name (folders and a `.md` extension are kept; spaces become `%20` in bare Markdown destinations), changes. A note rename rewrites only links that name the FILE — a link that reaches the note through its title or an alias still resolves and is left alone. Which note a link reaches is decided exactly as for go-to-definition: a Markdown link `[t](b.md)` in `sub/a.md` means `sub/b.md`, so renaming a `b.md` at the vault root never touches it. With duplicate headings, links belong to the first one (that is what they resolve to), so renaming a later duplicate touches no link. Edits are emitted in a stable order (files by URI, edits by position, the file rename last), and a rename that cannot produce a file location fails instead of half-applying.
- **Renaming a heading** replaces only the heading text in its document (the `#` level, a trailing `^block-id`, closing `#`s, the line ending and a setext underline are kept) and rewrites every same-document and cross-document link whose `#Heading` reference matches it (matched via [heading matching rules](syntax.md#heading-slugs--matching), so case/slug variants are all caught). The edit is scoped to the target document plus its known backlinks — it does not scan the entire vault.
- **Renaming via a link's target document** emits a `workspace/applyEdit` with a file-rename operation (`ResourceOp::Rename`) for the target file, plus text edits updating every in-scope link (again scoped to backlinks + the target itself, not a full-vault scan).

`textDocument/prepareRename` is implemented, so clients get the correct placeholder text (the current heading text, or the current link target) before you type a new name. The range it returns is exactly the heading text.

New names are validated and a bad one is reported to the client as an error instead of being silently ignored: heading names may not be empty or contain line breaks, `|`, `[[`, `]]`, `#` or start with `^`; note names may not be empty or contain `/  : * ? " < > | # [ ] ^`, control characters or end with `.`/space, must fit a 255-byte file name (`.md` is stripped once), and must not collide with an existing note in the same folder. Renaming from a link whose heading or note does not exist is an error too (it never falls back to renaming the document). The file rename never overwrites, and text edits are listed before the rename so they address the files by their current paths.

## Reference/highlight targets

`includeDeclaration: false` removes the declaration (the heading, block or note start) from the result — never the reference under the cursor. Duplicate headings: only the first owns the links that point at it.

Find References and Document Highlight both resolve "what's under the cursor" with the same priority order: **the narrowest thing covering the cursor** (a link, block anchor, heading, tag or footnote; when they nest, the innermost one — `# Title #tag` on the tag is the tag, `[see [[x]]](y.md)` on `[[x]]` is `x`). For tags, the search expands hierarchically (referencing `#parent` also surfaces `#parent/child` occurrences). For headings/blocks/documents, results include both the definition (if in the current/target document) and every link that resolves to it, scoped to that target's known backlinks.

## Format the whole workspace

Run the `satz.formatWorkspace` command (via `workspace/executeCommand`, or the "Format entire vault" source code action) to format every document in the vault in one shot, without leaving the editor:

1. The server computes each document's formatted output (for an open document from its live editor buffer, not from the last reparsed copy, so text typed a moment ago is what gets formatted) — checking a small in-memory cache keyed by content hash first (`lsp.format_cache_capacity`), so a repeat call against a vault that hasn't changed since the last one does no reformatting work at all — and skips any document whose result is identical to its current content; unchanged files never appear in the edit.
2. If anything needs to change, it sends one `workspace/applyEdit` request containing, per changed file, the *minimal* set of line-range `TextEdit`s (a line-based diff between the current and formatted text) rather than one edit replacing the whole document — scattered small changes stay small edits instead of one large blob.
3. Open documents are not touched by the server itself: the client applies the edit to its own buffer and reports it with `textDocument/didChange` (changing the server's copy first would apply the same edit twice). When the client supports `workspace.workspaceEdit.documentChanges`, the edit is sent as versioned document edits: an open document names the version the edits were computed against, so the client refuses them if you typed in the meantime (the command then reports `formatted: 0` and can simply be run again) instead of applying them to text they do not fit. Documents that aren't open are left for the client to persist — same as any other `workspace/applyEdit` — and the existing file watcher (see below) picks up the on-disk change normally.

Whether your editor exposes a convenient way to *trigger* `workspace/executeCommand` (a keybinding, a command palette entry) varies by client — this is a real LSP mechanism, not a satz-specific limitation, but the spec doesn't mandate any particular UI for it. If your client makes it awkward to discover, [`satz fmt --write`](cli.md#satz-fmt-path) from a terminal is the always-available equivalent.

The workspace-format cache is dropped whenever `.satz.toml` is reloaded, so a changed formatter setting takes effect on the next run.

`textDocument/formatting` (single-file formatting) uses the same minimal-diff approach, but never consults the workspace-format cache — you're actively editing that one file, so a cache would rarely help.

## Show backlinks

The backlink CodeLens runs `satz.showBacklinks` with the note's URI as its only argument. The server answers with a `Location[]`: one location per link, in the other notes that point at this one (the notes the lens counts; a link from the note to itself is not a backlink). An unknown note gives an empty list, a missing or non-URI argument an `InvalidParams` error. The client decides how to show the list (for example a references panel).

## Live reindexing & config hot-reload

A background file watcher (`notify`, polling every 500ms with a further 200ms debounce) keeps the in-memory index in sync without needing to restart the server:

- A note is a `.md` or `.markdown` file (any case); the initial scan and the watcher agree on that.
- Creating, modifying, or deleting a `.md` file outside the editor (e.g. `git checkout`, another tool writing to the vault) triggers a re-index of just that file — unless it's currently open in the client, in which case the editor's own buffer stays authoritative. Open documents are matched by their vault-relative, case-folded path (so a differently spelled path from the file system watcher still counts), and a briefly missing file (save-by-rename) never drops an open document from the index.
- Deleting, renaming or moving a FOLDER is followed too: the notes that were in it leave the index (open documents stay, their buffer is authoritative), and a folder that appears or is moved in is scanned and its notes are indexed.
- Watching starts before the first scan: what changes while the vault is being indexed is held back until the index is complete and then applied from what is on disk. If the first indexing fails (for example the folder does not exist) the server keeps working with the open documents, shows the reason, and the watcher still runs.
- Editing and saving the vault root's `.satz.toml` on disk reloads the whole configuration live; the client is notified to refresh diagnostics afterward. A `.satz.toml` in a subfolder, or a file named `satz.toml`, is ignored.
- If `.satz.toml` is invalid (at startup or after an edit) the editor shows a warning with the file name and line, and formatting (format-on-save, *Format Document*, the *Format entire vault* action and `satz.formatWorkspace`) is turned off until the file is valid again; see [When the file is invalid](configuration.md#when-the-file-is-invalid).
- Whether diagnostics are then pushed or the client is asked to re-pull depends on whether the client advertised diagnostic pull support during `initialize`.

## Document sync and closing

- Every request sees the live text of the open documents: if a debounced re-parse is still pending when a request arrives, the open documents are re-parsed first, so hover, go-to-definition, references, rename, folding and the like never work on text one edit behind.

- Positions follow the LSP definition: only `\n`, `\r\n` and `\r` end a line (U+2028, VT, FF and the like do not), and a column past the end of a line means the end of that line, never the next one.
- A `didChange` carrying an older document version than the buffer already has is ignored.
- Closing a document puts the index back to what is on disk (or drops the entry if there is no file), so unsaved edits that were discarded no longer shape links and diagnostics.
- The other open documents' diagnostics are refreshed whenever an edit changes what they depend on: the note's title/aliases/name, the notes it links to (orphan status), or its headings and block ids (anchor warnings) — not on every keystroke.

Two config fields control edit-triggered (as opposed to file-watcher-triggered) reparsing latency: `lsp.reparse_debounce_ms` and `lsp.reparse_max_wait_ms` — see [`docs/configuration.md`](configuration.md#lsp--server-wide-lsp-tuning).

## Logging

satz-lsp is silent by default — no log output at all, and no `RUST_LOG` env var is read. Verbosity is controlled entirely by a single `logLevel` field in the `initialize` request's `initializationOptions`, so it can be changed from your editor's own LSP config without recompiling or restarting anything beyond the language server itself.

`logLevel` accepts either a bare level (`"error"`, `"warn"`, `"info"`, `"debug"`, `"trace"`) applied to every module, or a full [`tracing-subscriber` `EnvFilter`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html) directive string for per-module control, e.g. `"satz_lsp=trace,satz_core=debug"`. Both forms go through the same field — there's no separate syntax to learn.

In Helix, set it under the language server's `config` table:

```toml
[language-server.satz]
command = "satz-lsp"

[language-server.satz.config]
logLevel = "debug"

[[language]]
name = "markdown"
language-servers = ["satz"]
```

Output goes to stderr, which most clients (including Helix, into `helix.log`) capture alongside the server's other diagnostic messages. Remove the `logLevel` line (or set it to `"off"`) to go back to silence — no rebuild needed either way.

Coverage is deliberately boundary-focused: the LSP lifecycle, every request handler, the file watcher, and index build/update all log at `debug` (a few very hot paths — per-link resolution misses, per-document diagnostics — log at `trace` instead, so `debug` stays readable). Parser and formatter internals are not instrumented: they re-run on every keystroke, and logging inside them would add real overhead for little diagnostic value over the boundary logs.

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
