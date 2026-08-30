# Vault syntax reference

satz parses plain Markdown files plus a small set of Obsidian-compatible conventions layered on top. This page is the exhaustive reference for what's recognized.

## Wikilinks

```
[[note]]
[[note|Display Text]]
[[note#Heading]]
[[note#^block-id]]
[[#Heading]]        (same-document heading link)
```

- `note` is resolved against the vault (see [Resolution order](#link-and-id-resolution-order) below); it does **not** need a `.md` extension or full path.
- `|Display Text` sets custom display text; it has no effect on resolution.
- `#Heading` targets a specific heading in the target document (or the current document, if `note` is omitted).
- `#^block-id` targets a specific [block anchor](#block-anchors).
- Wikilinks must open and close on the same line; a `[[` with no matching `]]` on that line is not treated as a link.
- Anything inside inline code spans (`` `...` ``) or fenced code blocks is never scanned for links, tags, or block anchors.

## Embeds

```
![[image.png]]
![[note#Heading]]
```

Same target/heading/block syntax as wikilinks, just prefixed with `!`. Parsed as a distinct `Embed` link kind (useful for graph export and semantic highlighting), but resolved exactly like a wikilink.

## Standard Markdown links

```
[Display text](relative/path.md#Heading)
[External](https://example.com)
```

Markdown-style links are also indexed. If the destination isn't `http://` or `https://`, it's resolved against the vault the same way a wikilink target is (including an optional `#Heading` suffix). External links are recognized but never checked for brokenness and never become graph edges.

## Footnotes

```
Some claim[^1].

[^1]: The footnote text.
```

Standard Markdown footnote syntax. References and definitions are matched by label within the **same document only** — footnotes don't link across files.

## Tags

```
#tag
#parent/child
```

- Inline tags: a `#` immediately followed by at least one letter, optionally continuing with letters/digits/`_`/`-`/`/`. Must be preceded by whitespace, an opening bracket/quote, or be at the start of the text — `word#notatag` is not a tag.
- Hierarchical tags use `/` as a separator. Querying a parent tag (`felsefe`) also matches everything nested under it (`felsefe/mantık`) via prefix matching — both in `satz list --tag` and in the LSP's tag references/highlighting.
- Frontmatter tags: `tags: [a, b]` or `tags: a` (a comma-separated string also works) or the singular `tag:` key. A leading `#` in a frontmatter tag value is stripped automatically.
- Tag matching is Unicode-fold-insensitive (`#Rust` and `#rust` are the same tag).

## Block anchors

```
This is the paragraph you want to reference. ^my-block-id
```

A block anchor is `^` immediately followed by alphanumerics/hyphens, at the end of a line (or followed by whitespace/punctuation), preceded by whitespace or start-of-line. Reference it from anywhere with `[[note#^my-block-id]]`.

## Frontmatter

```yaml
---
title: "My Note"
aliases: [short-name, "Alternate Title"]
tags: [rust, lsp]
date: "2026-08-30"
custom_field: 42
---
```

- Must be YAML, delimited by `---` on the first line and a closing `---`.
- **Only `---` fences are parsed as frontmatter.** The formatter (`satz` LSP's "Format Document") also tolerates a `+++` fence for whitespace-normalization purposes, but the parser never extracts YAML data from a `+++` block — don't use `+++` if you want `title`/`tags`/`aliases`/`date` actually recognized.
- Recognized fields: `title` (or falls back to the first `# H1` heading, or falls back to the filename stem, or `"Untitled"`), `aliases` (or singular `alias`; both a YAML list and a single string are accepted), `tags` (or singular `tag`; list, comma-separated string, or single string), `date`.
- Any other key is preserved verbatim in a passthrough map rather than causing an error.
- Malformed YAML never crashes the parser — the document just gets an empty frontmatter instead.

## GFM tables & task lists

```
| Column A | Column B |
|:---------|---------:|
| left     |    right |

- [ ] todo
- [x] done
```

Recognized via `pulldown-cmark`'s GFM extensions, primarily for the **formatter** rather than indexing: pipe tables (with alignment markers `:---`/`---:`/`:---:`) and `- [ ]`/`- [x]` task-list checkboxes. A wikilink, tag, or footnote inside a table cell or list item is still indexed normally. See [`docs/configuration.md`](configuration.md#formatter--deterministic-markdown-formatting) for exactly how the formatter realigns tables, normalizes list markers/renumbers ordered lists, and canonicalizes checkboxes, and [`docs/cli.md`](cli.md#satz-fmt-path)/[`docs/lsp.md`](lsp.md#format-the-whole-workspace) for how to run it.

## Link and ID resolution order

Given a raw target string (from a wikilink, markdown link, or `satz resolve`), `Index::resolve_link` tries, in order:

1. **Exact vault-relative path** match (e.g. `books/tractatus.md`).
2. The same path with **`.md` appended** (e.g. `books/tractatus` → `books/tractatus.md`).
3. **Filename stem** match, case/Unicode-fold-insensitive (e.g. `tractatus`).
4. **Title or alias** match, case/Unicode-fold-insensitive, against the document's frontmatter `title`/`aliases` (or its resolved title if no frontmatter title is set).

If the target contains no `/`, `\`, or `.`, steps 3 and 4 are tried before falling back to a literal path lookup — this is what makes bare targets like `[[TLP]]` or `[[my note]]` resolve straight to an alias or title without needing the full path.

## Heading slugs & matching

Heading slugs (used for `#Heading` anchor matching and for stable heading IDs) are generated by lower-casing with Unicode awareness, replacing non-alphanumeric characters with `-`, and collapsing/trimming dashes — e.g. `"Merhaba Dünya! (2024)"` → `merhaba-dünya-2024`. Turkish `İ` is folded to `i` specially (dotless-i correctness). A link's `#Heading` reference matches a heading if any of the following hold: the reference's slug equals the heading's slug, the reference text case-insensitively equals the heading text, or slugifying the reference text produces the heading's slug.

## Relative daily-note aliases

`[[bugün]]`, `[[dün]]`, `[[yarın]]` (plus their configured English/ASCII variants — see [`docs/configuration.md`](configuration.md#daily_note--used-by-satz-daily-cli-and-by-bugündünyarın-style-relative-links-lsp-hoverdefinitiondiagnostics)) resolve to today's/yesterday's/tomorrow's daily note based on `[daily_note]` config. This resolution only happens through the LSP's context-aware resolution path (hover, go-to-definition, diagnostics) — the plain `Index::resolve_link` used by `satz resolve` does not know about relative dates.

## See also

- [`docs/lsp.md`](lsp.md) — how diagnostics/completion/hover use all of the above.
- [`docs/cli.md`](cli.md) — commands that query links, tags, and resolution.
