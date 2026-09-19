# Configuration reference

satz reads an optional `.satz.toml` file from the **vault root** (the directory you point the CLI or LSP at). If the file is missing, or a field is omitted, built-in defaults are used — you never need a config file to get started.

- The CLI (`satz fmt`, `satz daily`) reads `.satz.toml` once, synchronously, before running.
- The LSP server (`satz-lsp`) reads it once at startup and then **hot-reloads** it: editing and saving `.satz.toml` while the server is running re-parses it and applies the new values immediately, no restart needed. Deleting the file returns to the defaults.
- Only the `.satz.toml` in the **vault root** is read. A `.satz.toml` in a subfolder, or a file named `satz.toml`, is ignored.

All keys are optional. Unknown keys are rejected (TOML parsing is strict — there's no passthrough "extra" bucket at the config level, unlike frontmatter), so a typo such as `[formatter.wrap] enabled = true` (the key is `enable`) is reported instead of silently ignored.

String-valued formatter settings are checked too: `misc.hr_style`, `misc.code_fence_style`, `emphasis.italic_marker`, `emphasis.bold_marker`, `lists.marker` and `wrap.link_width_mode` must be one of the values listed in their tables, otherwise the file is invalid (`invalid formatter.misc.hr_style "====": expected one of "---", "***", "___"`) instead of the setting being silently ignored.

### When the file is invalid

A file that exists but can't be used — broken TOML, an unknown key, a value of the wrong type or out of range, an invalid `daily_note.format`, or a file that can't be read — is always reported, with the file name and (for TOML errors) the line and column. It never silently falls back to the defaults:

| Where | What happens |
|---|---|
| `satz fmt`, `satz daily` | Error on stderr, exit status `1`, **no file is changed or created**. |
| `satz-lsp` at startup | The vault is still indexed with the default settings, a warning message is shown in the editor, and **formatting is turned off** (format-on-save, *Format Document*, *Format entire vault*) until the file is valid. |
| `satz-lsp` on hot-reload | The previous settings stay in effect, the same warning is shown, and formatting is turned off until the file is valid. Fixing and saving the file turns it back on automatically. |

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
required_fields = []

[lsp]
reparse_debounce_ms = 200
reparse_max_wait_ms = 500
format_cache_capacity = 2000

[lsp.codelens]
enable = false

[lsp.inlay_hints]
enable = true

[lsp.semantic_tokens]
split_link_display = true

[hover]
preview_lines = 8

[diagnostics]
moc_tags = ["moc", "index"]
workspace = true

[formatter]
enabled = true
line_width = 80
blank_lines_around_headings = 1
final_newline = true
normalize_links = true

[formatter.tables]
enable = true
cell_padding = 1
min_column_width = 3

[formatter.lists]
enable = true
marker = "-"
renumber_ordered = true

[formatter.emphasis]
enable = true
italic_marker = "*"
bold_marker = "**"

[formatter.misc]
enable = true
hr_style = "---"
code_fence_style = "```"
blockquote_single_space = true

[formatter.wrap]
enable = false
link_width_mode = "raw"
```

This is exactly the built-in default configuration, spelled out (a test keeps it in sync with the code). You only need to include the keys you want to override.

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
| `format_cache_capacity` | integer | `2000` | Maximum number of (content hash → formatted text) entries the `satz.formatWorkspace` command caches, so a repeat call against an unchanged vault does no reformatting work. Not an LRU: once at capacity, new distinct hashes just aren't cached — existing entries keep serving hits. See [`docs/lsp.md`](lsp.md#format-the-whole-workspace). |

#### `[lsp.semantic_tokens]`

| Key | Type | Default | Effect |
|---|---|---|---|
| `split_link_display` | bool | `true` | For a `[[target\|display]]`/`![[target\|display]]` link, emits the `\|display` part as its own `linkDisplay` semantic token instead of lumping it into the same token as the target — lets your theme color the alias text differently from the target/heading. Links with no `\|display` are unaffected either way. |

### `[hover]`

| Key | Type | Default | Effect |
|---|---|---|---|
| `preview_lines` | integer | `8` | Maximum number of lines shown in a hover preview before it's truncated with a "… (N more lines)" footer. |

### `[diagnostics]`

| Key | Type | Default | Effect |
|---|---|---|---|
| `moc_tags` | list of strings | `["moc", "index"]` | Tag names (matched case/Unicode-folded, without `#`) that mark a note as a "Map of Content" — such notes are exempt from the `orphan-note` hint even if nothing links to them. |
| `workspace` | bool | `true` | **Reserved, not yet enforced.** Intended to toggle workspace-wide diagnostics; the `workspace/diagnostic` LSP request currently always computes diagnostics for every document regardless of this setting. Safe to leave unset. |

### `[formatter]` — deterministic Markdown formatting

Used by `satz fmt`, the LSP's "Format Document" request and `satz.formatWorkspace` command, and `satz_core::formatter::format_document` if embedded directly.

Deterministic, structure-aware Markdown formatting: the same input always produces the same byte-for-byte output. Every sub-table below can be disabled independently; `formatter.enabled = false` turns the whole thing off (both `satz fmt` and the LSP's format-on-request become a no-op).

The formatter changes how a document is *written*, never how it *renders*. Guarantees, all covered by tests that run the formatter over an adversarial set of documents:

- **Code is never touched.** Fenced blocks (any fence length, `` ` `` or `~`, nested, unterminated, inside a quote or list item), indented code blocks and HTML blocks keep their whitespace, blank lines and trailing spaces byte for byte. Fence styles can still be converted (see `code_fence_style`), but never in a way that changes where a block ends.
- **Line endings are preserved.** A file whose lines end in `\r\n` is formatted exactly like its `\n` twin and stays CRLF. A file mixing both is normalized to whichever is more common (LF on a tie).
- **Idempotent.** Formatting already-formatted text changes nothing.

| Key | Type | Default | Effect |
|---|---|---|---|
| `enabled` | bool | `true` | Master switch for the entire formatter. When `false`, `satz fmt` reports nothing to do and `textDocument/formatting` returns no edits. |
| `line_width` | integer | `80` | Target column width for paragraph reflow. Only takes effect when `[formatter.wrap] enable = true` — see below; otherwise unused. |
| `blank_lines_around_headings` | integer (0–255) | `1` | Number of blank lines forced before each heading (headings immediately after frontmatter always get exactly one). |
| `final_newline` | bool | `true` | Whether the formatted document must end with exactly one trailing newline. |
| `normalize_links` | bool | `true` | Whether `[[  target  \|  display  ]]`-style wikilinks get their whitespace trimmed down to `[[target\|display]]` on format. Wikilinks inside code (inline code, fenced or indented blocks) and inside frontmatter are literal text and are never touched. |

#### `[formatter.tables]`

| Key | Type | Default | Effect |
|---|---|---|---|
| `enable` | bool | `true` | Detect and realign GFM pipe tables (column widths, alignment markers). Cell content itself is reproduced verbatim — never re-parsed — so inline markdown/wikilinks inside cells survive untouched. A table with a row that has more cells than its header (usually an unescaped `\|` inside a wikilink or code span) is left exactly as you wrote it, because realigning it would delete the extra cells; escape the pipe (`\\|`) to have the table formatted. |
| `cell_padding` | integer | `1` | Minimum whitespace padding on each side of a cell's content. |
| `min_column_width` | integer | `3` | Minimum width (display columns) reserved for a column, even if its content is shorter. |

#### `[formatter.lists]`

| Key | Type | Default | Effect |
|---|---|---|---|
| `enable` | bool | `true` | Normalize unordered marker characters, renumber ordered lists, and canonicalize task-list checkboxes (`[ ]`/`[x]`). A marker (or `.`/`)` delimiter) change is what makes Markdown start a new list, so two lists that touch keep different markers (`- a

* b` stays two lists; the second keeps its own marker). When a marker changes width (`-   x` -> `- x`, `9.` -> `10.`) the item's continuation lines and nested content are re-indented by the same amount so nothing leaves the item; an item that cannot be re-indented safely (lazy or tab-indented continuation, inside a blockquote, containing a table) keeps its spacing. |
| `marker` | `"-"` \| `"*"` \| `"+"` | `"-"` | Character used for every unordered list marker in the vault. |
| `renumber_ordered` | bool | `true` | Renumber ordered lists sequentially (`1. 2. 3. ...`) from the list's own starting number, regardless of what each item was originally typed as. The `.`/`)` delimiter is always normalized to `.` regardless of this setting — when `false`, only the *numbers themselves* are left as originally written. |

#### `[formatter.emphasis]`

| Key | Type | Default | Effect |
|---|---|---|---|
| `enable` | bool | `true` | Normalize emphasis/strong delimiters. Only the delimiter characters are touched — content (including nested emphasis or `[[wikilinks]]`) is never altered. |
| `italic_marker` | `"*"` \| `"_"` | `"*"` | Delimiter used for single-emphasis (`*text*`/`_text_`). `_` is only written where it can work: an emphasis span inside a word (`a*b*c`) or containing a `_` keeps its `*`. |
| `bold_marker` | `"**"` \| `"__"` | `"**"` | Delimiter used for strong emphasis (`**text**`/`__text__`). |

#### `[formatter.misc]`

| Key | Type | Default | Effect |
|---|---|---|---|
| `enable` | bool | `true` | Enables thematic-break and code-fence style normalization (both governed by this one flag). |
| `hr_style` | `"---"` \| `"***"` \| `"___"` | `"---"` | Style used for every thematic break (horizontal rule). The YAML frontmatter's own `---` fences are never affected — they're a distinct construct, not a thematic break. |
| `code_fence_style` | `` "```" `` \| `"~~~"` | `` "```" `` | Style used for fenced code block delimiters. Fence length is preserved unless the code content itself contains a same-or-longer run of the target character, in which case the fence is lengthened just enough to stay unambiguous. An unterminated fence (cut off by EOF) has only its opening delimiter rewritten. |
| `blockquote_single_space` | bool | `true` | Normalize blockquote markers: consecutive markers are separated by exactly one space (`>>text` → `> > text`) and a marker followed directly by text gets one space (`>text` → `> text`). Extra spaces after the last marker are kept, because they are indentation that can be significant (indented code, nested lists: `>     code` stays code). Lazy-continuation lines (part of a blockquote but not themselves prefixed with `>`) are left untouched. |

#### `[formatter.wrap]`

Reflows top-level paragraphs to `[formatter] line_width`. **Off by default** — existing vaults
are never silently rewrapped; add `enable = true` to opt in. Scoped to top-level paragraphs only
(not list items or blockquote text, which would need indentation-aware continuation lines this
doesn't attempt). A wikilink (`[[...]]`/`![[...]]`), inline code span, or standard Markdown link
is **never split** across a line break, even if it alone exceeds `line_width` — rescuing an
unboundedly long line matters more than shaving a few columns off one unavoidably-long link, and
this isn't configurable.

| Key | Type | Default | Effect |
|---|---|---|---|
| `enable` | bool | `false` | Turns paragraph wrapping on. |
| `link_width_mode` | `"raw"` \| `"display"` | `"raw"` | How a wikilink's/link's width counts against `line_width`. `"raw"`: the full source text (`[[path#heading\|display]]`) counts, so the line you see in the editor never exceeds the limit. `"display"`: only the alias/display text (or the bare target if there's no alias) counts — matching what a rendered viewer would actually show, at the cost of the raw `.md` line sometimes running longer than `line_width`. |

Wrapping never changes what a paragraph *is*:
- A token that would start a different block if it began a line (`#`, `-`, `+`, `*`, `1.`, `>`,
  `---`, `===`, a code fence, an HTML tag, a `|` table row, `[^x]:`, `$$`) stays on the previous
  line even when that overruns `line_width`.
- A backslash hard break (`text\` followed by a newline) is kept, and a literal backslash is never
  followed by a newline (which would turn it into a hard break).
- A trailing-two-spaces hard break (`text␠␠` + newline) is kept exactly as written, both by wrapping and by the trailing-whitespace trim.
- Footnote definitions are not reflowed.
- As a final check, a paragraph is only rewritten if re-parsing the wrapped text still gives
  exactly one paragraph with the same words; otherwise it is left as you wrote it.

## See also

- [`docs/lsp.md`](lsp.md) for what each LSP-tunable field actually changes in editor behavior.
- [`docs/syntax.md`](syntax.md) for how tags, headings, and daily aliases are matched/folded.
- [`docs/cli.md`](cli.md) for how `daily_note` affects `satz daily`.
