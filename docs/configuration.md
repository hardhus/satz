# Configuration reference

satz reads an optional `.satz.toml` file from the **vault root** (the directory you point the CLI or LSP at). If the file is missing, or a field is omitted, built-in defaults are used — you never need a config file to get started.

- The CLI (`satz daily`) reads `.satz.toml` once, synchronously, before running.
- The LSP server (`satz-lsp`) reads it once at startup and then **hot-reloads** it: editing and saving `.satz.toml` while the server is running re-parses it and applies the new values immediately, no restart needed.

All keys are optional. Unknown keys are rejected (TOML parsing is strict — there's no passthrough "extra" bucket at the config level, unlike frontmatter).

## Full example

```toml
id_scheme = "path"
turkish_i_folding = false

[daily_note]
folder = "daily"
format = "%Y-%m-%d"

[daily_note.aliases]
today = ["bugün", "bugun", "today"]
yesterday = ["dün", "dun", "yesterday"]
tomorrow = ["yarın", "yarin", "tomorrow"]

[frontmatter]
required_fields = ["title", "date"]

[lsp]
reparse_debounce_ms = 200
reparse_max_wait_ms = 500

[lsp.codelens]
enable = false

[lsp.inlay_hints]
enable = true

[hover]
preview_lines = 8

[diagnostics]
moc_tags = ["moc", "index"]
workspace = true

[formatter]
line_width = 80
blank_lines_around_headings = 1
final_newline = true
normalize_links = true
```

This is exactly the built-in default configuration, spelled out. You only need to include the keys you want to override.

## Field reference

### Top level

| Key | Type | Default | Effect |
|---|---|---|---|
| `id_scheme` | `"path"` \| `"hierarchical"` | `"path"` | **Reserved, not yet enforced.** Intended to select how document identity/resolution works; currently every document's identity is always derived from its vault-relative path regardless of this setting. Safe to leave unset. |
| `turkish_i_folding` | bool | `false` | **Reserved, not yet wired up.** The underlying folding function (`fold_key_ext`) supports an extra mode that folds ASCII `I`/`ı` together with `İ`/`i` for case-insensitive title/alias/tag lookups, but nothing in the indexer or LSP currently reads this field to enable it — folding always behaves as if this were `false`. Safe to leave unset. |

### `[daily_note]` — used by `satz daily` (CLI) and by `[[bugün]]`/`[[dün]]`/`[[yarın]]`-style relative links (LSP hover/definition/diagnostics)

| Key | Type | Default | Effect |
|---|---|---|---|
| `folder` | string | `"daily"` | Subfolder (relative to vault root) where daily notes live. Empty string means "vault root". |
| `format` | [chrono strftime string](https://docs.rs/chrono/latest/chrono/format/strftime/index.html) | `"%Y-%m-%d"` | Format used both for the daily note's filename (`.md` appended automatically if not already present) and as the target of relative daily aliases. Can include `/` to place notes in date-based subfolders, e.g. `"%Y/%m/%d"`. |
| `aliases.today` | list of strings | `["bugün", "bugun", "today"]` | Words that, used as a wikilink target (e.g. `[[bugün]]`), resolve to today's daily note. Matching is Unicode-folded and case-insensitive. |
| `aliases.yesterday` | list of strings | `["dün", "dun", "yesterday"]` | Same, resolving to yesterday's daily note. |
| `aliases.tomorrow` | list of strings | `["yarın", "yarin", "tomorrow"]` | Same, resolving to tomorrow's daily note. |

> Relative daily aliases only resolve through the LSP's context-aware resolution path (hover, go-to-definition, diagnostics). The CLI's `satz resolve` command does not resolve them — pass an explicit date or note name instead.

### `[frontmatter]` — used by the LSP's diagnostics

| Key | Type | Default | Effect |
|---|---|---|---|
| `required_fields` | list of strings | `[]` | Field names that must be present in a document's YAML frontmatter, or the LSP raises a `missing-frontmatter-field` warning. `title`, `date`, `tags`, and `aliases`/`alias` are checked against their typed fields (non-empty required); any other name is checked against the frontmatter's passthrough `extra` map. |

### `[lsp]` — server-wide LSP tuning

| Key | Type | Default | Effect |
|---|---|---|---|
| `reparse_debounce_ms` | integer (ms) | `200` | After you stop typing, how long the server waits before re-parsing the document and refreshing diagnostics. |
| `reparse_max_wait_ms` | integer (ms) | `500` | Upper bound on how long reparsing can be delayed while you keep typing continuously — guarantees a reparse happens at least this often even under constant edits. |
| `codelens.enable` | bool | `false` | Turns on the "N backlinks" CodeLens shown above each document. Off by default because satz is terminal/CLI-first. |
| `inlay_hints.enable` | bool | `true` | Turns on inline hints after links showing the target note's tags (or title, or a "⚠ not found" marker for broken links). |

### `[hover]`

| Key | Type | Default | Effect |
|---|---|---|---|
| `preview_lines` | integer | `8` | Maximum number of lines shown in a hover preview before it's truncated with a "… (N satır daha)" ("… (N more lines)") footer. |

### `[diagnostics]`

| Key | Type | Default | Effect |
|---|---|---|---|
| `moc_tags` | list of strings | `["moc", "index"]` | Tag names (matched case/Unicode-folded, without `#`) that mark a note as a "Map of Content" — such notes are exempt from the `orphan-note` hint even if nothing links to them. |
| `workspace` | bool | `true` | **Reserved, not yet enforced.** Intended to toggle workspace-wide diagnostics; the `workspace/diagnostic` LSP request currently always computes diagnostics for every document regardless of this setting. Safe to leave unset. |

### `[formatter]` — used by the LSP's "Format Document" and by `satz_core::formatter::format_document` if embedded directly

| Key | Type | Default | Effect |
|---|---|---|---|
| `line_width` | integer | `80` | **Reserved, not yet enforced.** The formatter does not currently wrap or rewrap prose to this width. Safe to leave unset. |
| `blank_lines_around_headings` | integer (0–255) | `1` | Number of blank lines forced before each heading (headings immediately after frontmatter always get exactly one). |
| `final_newline` | bool | `true` | Whether the formatted document must end with exactly one trailing newline. |
| `normalize_links` | bool | `true` | Whether `[[  target  \|  display  ]]`-style wikilinks get their whitespace trimmed down to `[[target\|display]]` on format. |

## See also

- [`docs/lsp.md`](lsp.md) for what each LSP-tunable field actually changes in editor behavior.
- [`docs/syntax.md`](syntax.md) for how tags, headings, and daily aliases are matched/folded.
- [`docs/cli.md`](cli.md) for how `daily_note` affects `satz daily`.
