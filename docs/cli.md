# CLI reference

The `satz` binary exposes seven subcommands. Run `satz --help` or `satz <command> --help` at any time for the same information from `clap`.

General notes that apply to every command:

- Logging goes to **stderr** via `tracing`, default level `WARN`. Set `RUST_LOG=info` (or `debug`) before running to see more.
- Any `--vault`/`-v` or positional `path` argument defaults to `.` (the current directory) unless noted otherwise.
- Vault walking respects `.gitignore` and always skips `.git`, `.obsidian`, `node_modules`, `.trash`, `.stversions`, `.svn`, `.hg` regardless of ignore files.

## `satz index [path]`

Indexes the vault and prints a one-shot summary. Useful as a fast sanity check or in CI.

```
satz index .
```

```
Indexing vault: .
✓ 42 documents indexed in 12ms
  Links:        118 total, 3 broken
  Tags:         27 unique
  Orphans:      5 documents (no backlinks)
```

If `broken_links > 0`, a hint is printed to stderr pointing at `satz list --broken`.

| Arg | Default | Meaning |
|---|---|---|
| `path` (positional) | `.` | Vault root to index. |

## `satz stats`

Prints the same summary statistics `index` computes, either as a human-readable table or as JSON (handy for scripting/CI).

```
satz stats --vault . --json
```

```json
{
  "doc_count": 42,
  "total_links": 118,
  "broken_links": 3,
  "unique_tags": 27,
  "orphan_docs": 5,
  "total_headings": 96,
  "total_words": 8420
}
```

| Flag | Default | Meaning |
|---|---|---|
| `-v, --vault <path>` | `.` | Vault root. |
| `--json` | off | Emit `IndexStats` as pretty-printed JSON instead of a table. |

## `satz list`

Lists documents, optionally filtered. Filters can combine (`--tag` + `--orphans`), except `--broken` which takes over the entire output.

```
satz list --vault . --tag felsefe --tag wittgenstein   # AND of both tags
satz list --vault . --orphans
satz list --vault . --broken
```

`--broken` output format (one line per broken link occurrence):

```
books/tractatus.md:14	[[missing-note]]	— dosya bulunamadı
notes/index.md:3	[[other#Section]]	— dosya var, başlık yok
```

Fields are tab-separated: `path:line`, the raw link text as it appears in the source, and a reason. The reason strings are currently emitted in Turkish:
- `dosya bulunamadı` — the target document could not be found at all.
- `dosya var, başlık yok` — the target document exists, but the requested `#heading` or `#^block` anchor doesn't.

| Flag | Default | Meaning |
|---|---|---|
| `-v, --vault <path>` | `.` | Vault root. |
| `--tag <name>` | none | Only show documents with this tag (hierarchical prefix match, e.g. `--tag project` also matches `project/sub`). Repeatable; repeated flags are AND'd together. |
| `--orphans` | off | Only show documents with no incoming backlinks (self-links don't count). |
| `--broken` | off | Instead of listing documents, list every broken wikilink/embed/markdown link occurrence across the vault. Takes precedence over `--tag`/`--orphans`. |

## `satz resolve <target>`

Resolves a single link target the same way the indexer would, and prints where it points. Useful for scripting "does this link work" checks or jumping to a note from a shell alias.

```
satz resolve --vault . "[[TLP]]"
# books/tractatus.md

satz resolve --vault . "note#Some Heading"
# notes/note.md:14
```

Accepts the target with or without surrounding `[[` `]]`, and with or without a `#heading` suffix. Resolution order matches `Index::resolve_link`: exact relative path → path with `.md` appended → filename stem → title/alias (case- and Unicode-fold-insensitive). If a `#heading` suffix is given and a matching heading is found, output is `path:line` (1-indexed); otherwise just `path`.

If the target can't be resolved at all, `satz` exits with status `1` and prints `not found: <target>` to stderr. Note that relative daily aliases (`[[bugün]]` etc., see [`docs/configuration.md`](configuration.md)) are **not** resolved by this command — only by the LSP.

| Arg | Default | Meaning |
|---|---|---|
| `-v, --vault <path>` | `.` | Vault root. |
| `target` (positional) | — | Required. The wikilink target to resolve. |

## `satz daily [path]`

Prints the path to today's daily note, creating it (with frontmatter + an `# <date>` heading) if it doesn't already exist.

```
satz daily .
# /abs/path/to/vault/daily/2026-08-30.md
```

Behavior:
1. Reads `.satz.toml` from `path` if present (see [`[daily_note]`](configuration.md#daily_note--used-by-satz-daily-cli-and-by-bugündünyarın-style-relative-links-lsp-hoverdefinitiondiagnostics)); otherwise uses defaults (`folder = "daily"`, `format = "%Y-%m-%d"`).
2. Formats today's date with `daily_note.format`, appends `.md` if the formatted string doesn't already end with it.
3. If `--create` is true (the default) and the file doesn't exist, creates `daily_note.folder` (and any parent directories) and writes an initial document generated from the filename-derived title and today's date.
4. Prints the resulting absolute path to stdout.

| Flag/Arg | Default | Meaning |
|---|---|---|
| `path` (positional) | `.` | Vault root. |
| `-c, --create <bool>` | `true` | Whether to create the file if missing, e.g. `satz daily . --create false` to only report the path without creating anything. |

## `satz fmt [path]`

Formats every Markdown file in the vault in place, or checks whether they're already formatted — the guaranteed, editor-independent way to keep a vault consistently formatted (see [`docs/configuration.md`](configuration.md#formatter--deterministic-markdown-formatting) for everything `[formatter]` controls: tables, lists, emphasis, thematic breaks, code fences, blockquotes).

```
satz fmt . --check   # list files that would change; exit 1 if any would (CI/pre-commit)
satz fmt . --write   # format in place (also the default if neither flag is given)
```

```
$ satz fmt . --check
notes/messy-table.md
notes/mixed-emphasis.md
✗ 2 file(s) need formatting, 41 file(s) already clean (18ms)
```

Runs in parallel (`rayon`) over every parsed document; a file whose formatted output is byte-identical to its current content is never written (no unnecessary I/O or mtime churn) in `--write` mode. If `formatter.enabled = false` in `.satz.toml`, the command prints a notice and exits successfully without touching anything.

| Flag/Arg | Default | Meaning |
|---|---|---|
| `path` (positional) | `.` | Vault root. |
| `--check` | off | Report which files would change (one relative path per line, sorted) without writing anything. Exits with status `1` if any file would change — usable directly as a CI or pre-commit check. Mutually exclusive with `--write`. |
| `--write` | on (default) | Format files in place. This is what happens when neither flag is passed; passing it explicitly is only for clarity in scripts. Mutually exclusive with `--check`. |

## `satz graph`

Exports the vault's link graph (one node per document, one edge per resolvable link) as Graphviz DOT or structured JSON — useful for visualizing your vault or feeding it into other tooling.

```
satz graph --vault . --format dot --output graph.dot
dot -Tsvg graph.dot -o graph.svg

satz graph --vault . --format json | jq '.nodes | length'
```

JSON shape:

```json
{
  "nodes": [
    { "id": "books/tractatus.md", "title": "Tractatus Logico-Philosophicus", "path": "books/tractatus.md", "tags": ["felsefe", "wittgenstein"] }
  ],
  "edges": [
    { "source": "notes/index.md", "target": "books/tractatus.md", "kind": "wikilink", "label": null }
  ]
}
```

`kind` is one of `wikilink`, `embed`, `markdown`, `footnote`. `label` carries the `#heading` or `#^block` anchor when the link targets one. Only links that successfully resolve to another indexed document become edges; external `http(s)://` links and unresolved links are omitted from the graph.

| Flag | Default | Meaning |
|---|---|---|
| `-v, --vault <path>` | `.` | Vault root. |
| `-f, --format <dot\|json>` | `json` | Output format. |
| `-o, --output <path>` | none (stdout) | Write output to a file instead of stdout. When set, a one-line summary (`Graph exported to ... (N nodes, M edges)`) is printed to stderr. |

## See also

- [`docs/configuration.md`](configuration.md) — everything `satz daily` reads from `.satz.toml`.
- [`docs/syntax.md`](syntax.md) — what counts as a link/tag/anchor in the first place.
