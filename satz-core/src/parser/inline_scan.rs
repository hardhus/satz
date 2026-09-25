use crate::model::block::BlockAnchor;
use crate::model::link::{Link, LinkKind};
use crate::model::range::ByteRange;
use crate::model::tag::Tag;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct InlineScanOutput {
    pub wiki_links: Vec<Link>,
    pub tags: Vec<Tag>,
    pub blocks: Vec<BlockAnchor>,
    /// Every `[^label]`-shaped occurrence found in the raw text, regardless of whether `label`
    /// has a matching definition -- unlike `structure::parse_structure()`'s `footnote_refs`
    /// (which pulldown-cmark only ever populates for a label that's already defined), this scan
    /// doesn't know or care about resolution. The caller (`parse_document`) cross-references
    /// against `structure.footnote_defs` to find the genuinely undefined ones.
    pub footnote_candidates: Vec<Link>,
}

/// Scans for wikilinks (`[[...]]`), embeds (`![[...]]`), and tags (`#tag`)
/// in non-code regions of the source text.
pub fn scan_inline(source: &str, code_spans: &[ByteRange]) -> InlineScanOutput {
    let mut output = InlineScanOutput::default();
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut si = 0usize;

    while i < len {
        // Skip code spans quickly
        while si < code_spans.len() && code_spans[si].end <= i {
            si += 1;
        }
        if si < code_spans.len() && code_spans[si].contains(i) {
            i = code_spans[si].end;
            continue;
        }

        // 1. Check for Embed `![[` or WikiLink `[[`
        if bytes[i] == b'!' && i + 2 < len && bytes[i + 1] == b'[' && bytes[i + 2] == b'[' {
            let start = i;
            if let Some((link, next_i)) = parse_wikilink(source, start, true) {
                let overlaps = si < code_spans.len() && code_spans[si].overlaps(&link.range);
                if !overlaps {
                    output.wiki_links.push(link);
                }
                i = next_i;
                continue;
            }
        } else if bytes[i] == b'[' && i + 1 < len && bytes[i + 1] == b'[' {
            let start = i;
            if let Some((link, next_i)) = parse_wikilink(source, start, false) {
                let overlaps = si < code_spans.len() && code_spans[si].overlaps(&link.range);
                if !overlaps {
                    output.wiki_links.push(link);
                }
                i = next_i;
                continue;
            }
        } else if bytes[i] == b'[' && i + 1 < len && bytes[i + 1] == b'^' {
            let start = i;
            if let Some((link, next_i)) = parse_footnote_candidate(source, start) {
                let overlaps = si < code_spans.len() && code_spans[si].overlaps(&link.range);
                if !overlaps {
                    output.footnote_candidates.push(link);
                }
                i = next_i;
                continue;
            }
        }

        // 2. Check for Tag `#tag`
        if bytes[i] == b'#' {
            let start = i;
            // Ensure `#` is not part of heading marker at start of line or after `\n` followed by space
            // And check preceding boundary
            let prev_char = if i > 0 {
                source[..i].chars().next_back()
            } else {
                None
            };

            let valid_prefix = match prev_char {
                None => true,
                Some(c) => {
                    c.is_whitespace() || matches!(c, '(' | '[' | '{' | '"' | '\'' | '<' | '—' | '–')
                }
            };

            if valid_prefix {
                let tag_opt = parse_tag(source, start);
                if let Some((tag, next_i)) = tag_opt {
                    let overlaps = si < code_spans.len() && code_spans[si].overlaps(&tag.range);
                    if !overlaps {
                        output.tags.push(tag);
                    }
                    i = next_i;
                    continue;
                }
            }
        }

        // 3. Check for Block Anchor `^block-id`
        if bytes[i] == b'^' {
            let start = i;
            let prev_char = if i > 0 {
                source[..i].chars().next_back()
            } else {
                None
            };

            let valid_prefix = match prev_char {
                None => true,
                Some(c) => c.is_whitespace(),
            };

            if valid_prefix && let Some((block, next_i)) = parse_block_anchor(source, start) {
                let overlaps = si < code_spans.len() && code_spans[si].overlaps(&block.range);
                if !overlaps {
                    output.blocks.push(block);
                }
                i = next_i;
                continue;
            }
        }

        // Advance by next char
        if let Some(c) = source[i..].chars().next() {
            i += c.len_utf8();
        } else {
            i += 1;
        }
    }

    output
}

/// Attempts to parse a wikilink starting at `start`.
/// Returns `(Link, next_index)`.
fn parse_wikilink(source: &str, start: usize, is_embed: bool) -> Option<(Link, usize)> {
    let prefix_len = if is_embed { 3 } else { 2 };
    let inner_start = start + prefix_len;

    // Find the closing `]]` on the same line only: searching further would make every unclosed
    // `[[` scan to the end of the document.
    let rest = &source[inner_start..];
    let line = &rest[..rest.find(['\n', '\r']).unwrap_or(rest.len())];
    let end_bracket = line.find("]]")?;
    let inner_slice = &line[..end_bracket];

    let full_end = inner_start + end_bracket + 2;
    let range = ByteRange::new(start, full_end);
    let kind = if is_embed {
        LinkKind::Embed
    } else {
        LinkKind::WikiLink
    };

    // Parse target and display. Inside a table cell the separator is written `\|`; that one
    // backslash belongs to the separator, not to the target.
    let (target_raw, display) = match inner_slice.find('|') {
        Some(pipe) => {
            let target = &inner_slice[..pipe];
            let target = target.strip_suffix('\\').unwrap_or(target);
            (
                target.trim(),
                Some(inner_slice[pipe + 1..].trim().to_string()),
            )
        }
        None => (inner_slice.trim(), None),
    };

    // Parse heading or block anchor inside target_raw
    let (target_doc, target_heading, target_block) =
        if let Some((doc, block)) = target_raw.split_once("#^") {
            (doc.trim().to_string(), None, Some(block.trim().to_string()))
        } else if let Some((doc, heading)) = target_raw.split_once('#') {
            (
                doc.trim().to_string(),
                Some(heading.trim().to_string()),
                None,
            )
        } else {
            (target_raw.to_string(), None, None)
        };

    // An empty heading or block is no heading or block.
    let target_heading = target_heading.filter(|h| !h.is_empty());
    let target_block = target_block.filter(|b| !b.is_empty());
    let link = Link::new(
        kind,
        target_doc,
        target_heading,
        target_block,
        display,
        range,
    );

    // Nothing to point at (`[[]]`, `[[|x]]`, `[[#]]`): plain text, not a link.
    if link.is_degenerate() {
        return None;
    }
    Some((link, full_end))
}

/// Attempts to parse a `[^label]`-shaped footnote reference candidate starting at `start`, where
/// `source[start..start+2] == "[^"`. Doesn't check whether `label` has a matching definition --
/// that's left to the caller. Fails (no match) if no `]` is found before a newline or EOF.
fn parse_footnote_candidate(source: &str, start: usize) -> Option<(Link, usize)> {
    let label_start = start + 2;
    let rest = &source[label_start..];
    let line = &rest[..rest.find(['\n', '\r']).unwrap_or(rest.len())];
    let end_bracket = line.find(']')?;
    let label = &line[..end_bracket];
    // pulldown-cmark never treats a label containing `[` as a footnote (even when "defined"), and
    // an empty or whitespace-padded one (`[^]`, `[^ x]`, `[^x ]`) is prose, not a reference.
    // A space in the middle is valid (`[^a b]`).
    if label.is_empty()
        || label != label.trim()
        || label.contains('[')
        || label.contains('\n')
        || label.contains('\r')
    {
        return None;
    }

    let full_end = label_start + end_bracket + 1;
    let range = ByteRange::new(start, full_end);
    Some((
        Link::new(
            LinkKind::Footnote,
            String::new(),
            None,
            None,
            Some(label.to_string()),
            range,
        ),
        full_end,
    ))
}

/// Attempts to parse a `#tag` starting at `start` where `source[start] == '#'`.
fn parse_tag(source: &str, start: usize) -> Option<(Tag, usize)> {
    let after_hash = start + 1;
    if after_hash >= source.len() {
        return None;
    }

    let mut end = after_hash;
    let mut has_alphabetic = false;
    for (idx, c) in source[after_hash..].char_indices() {
        if c.is_alphabetic() {
            has_alphabetic = true;
            end = after_hash + idx + c.len_utf8();
        } else if c.is_numeric() || c == '_' || c == '-' || c == '/' {
            end = after_hash + idx + c.len_utf8();
        } else {
            break;
        }
    }

    // Must have at least one alphabetic character (disallows pure numbers like #123)
    if !has_alphabetic {
        return None;
    }

    let tag_name = source[after_hash..end]
        .trim_end_matches(['/', '-', '_'])
        .to_string();
    if tag_name.is_empty() {
        return None;
    }

    let final_end = after_hash + tag_name.len();
    Some((
        Tag::new(tag_name, ByteRange::new(start, final_end)),
        final_end,
    ))
}

/// Attempts to parse a block anchor `^block-id` starting at `start`.
/// Valid characters in block-id are alphanumeric and hyphens `[a-zA-Z0-9-]`.
/// Must be followed by whitespace, newline, punctuation, or end of string.
fn parse_block_anchor(source: &str, start: usize) -> Option<(BlockAnchor, usize)> {
    let rest = &source[start + 1..];
    let mut end = 0;

    for (idx, ch) in rest.char_indices() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            end = idx + ch.len_utf8();
        } else {
            break;
        }
    }

    if end == 0 {
        return None;
    }

    let id = &rest[..end];
    let full_end = start + 1 + end;

    // An anchor ends its line (optionally followed by one sentence punctuation mark): `2 ^3 power`
    // is arithmetic, not a block.
    let after = &source[full_end..];
    let tail = &after[..after.find(['\n', '\r']).unwrap_or(after.len())];
    let tail = match tail.chars().next() {
        Some('.' | ',' | ';' | ':' | ')' | ']' | '}') => &tail[1..],
        _ => tail,
    };
    if !tail.trim().is_empty() {
        return None;
    }

    Some((
        BlockAnchor::new(id, ByteRange::new(start, full_end)),
        full_end,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wikilinks_scan() {
        let text = "Check [[note]] and [[doc#heading]] and [[doc#^block]] and [[doc|alias]].";
        let output = scan_inline(text, &[]);
        assert_eq!(output.wiki_links.len(), 4);

        assert_eq!(output.wiki_links[0].kind, LinkKind::WikiLink);
        assert_eq!(output.wiki_links[0].target_doc, "note");
        assert_eq!(output.wiki_links[0].target_heading, None);
        assert_eq!(output.wiki_links[0].display, None);

        assert_eq!(output.wiki_links[1].target_doc, "doc");
        assert_eq!(
            output.wiki_links[1].target_heading.as_deref(),
            Some("heading")
        );

        assert_eq!(output.wiki_links[2].target_doc, "doc");
        assert_eq!(output.wiki_links[2].target_block.as_deref(), Some("block"));

        assert_eq!(output.wiki_links[3].target_doc, "doc");
        assert_eq!(output.wiki_links[3].display.as_deref(), Some("alias"));
    }

    #[test]
    fn test_embed_scan() {
        let text = "Here is ![[image.png]] embedded.";
        let output = scan_inline(text, &[]);
        assert_eq!(output.wiki_links.len(), 1);
        assert_eq!(output.wiki_links[0].kind, LinkKind::Embed);
        assert_eq!(output.wiki_links[0].target_doc, "image.png");
    }

    #[test]
    fn test_tags_scan() {
        let text = "Tags: #felsefe #wittgenstein/tractatus #test_123 (#nested) but not #123 and not word#notatag.";
        let output = scan_inline(text, &[]);
        let names: Vec<&str> = output.tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["felsefe", "wittgenstein/tractatus", "test_123", "nested"]
        );
    }

    #[test]
    fn test_block_anchor_scan() {
        let text = "This is a paragraph with a block reference. ^p1-ref\n\nAnother one ^my-block.";
        let output = scan_inline(text, &[]);
        assert_eq!(output.blocks.len(), 2);
        assert_eq!(output.blocks[0].id, "p1-ref");
        assert_eq!(output.blocks[1].id, "my-block");
    }

    #[test]
    fn test_ignore_code_spans() {
        let text = "Real [[link]] and `inline [[fake-link]]` and #real-tag and `#fake-tag` and `^fake-block` ^real-block.";
        let code_spans = vec![
            ByteRange::new(18, 40), // `inline [[fake-link]]`
            ByteRange::new(59, 70), // `#fake-tag`
            ByteRange::new(75, 89), // `^fake-block`
        ];
        let output = scan_inline(text, &code_spans);
        assert_eq!(output.wiki_links.len(), 1);
        assert_eq!(output.wiki_links[0].target_doc, "link");

        assert_eq!(output.tags.len(), 1);
        assert_eq!(output.tags[0].name, "real-tag");

        assert_eq!(output.blocks.len(), 1);
        assert_eq!(output.blocks[0].id, "real-block");
    }

    #[test]
    fn test_footnote_candidate_scan() {
        // scan_inline doesn't check resolution -- both a defined-elsewhere and an undefined
        // label show up identically as candidates; the caller decides which are broken.
        let text = "Ref one [^a] and ref two [^b].";
        let output = scan_inline(text, &[]);
        assert_eq!(output.footnote_candidates.len(), 2);
        assert_eq!(output.footnote_candidates[0].kind, LinkKind::Footnote);
        assert_eq!(output.footnote_candidates[0].display.as_deref(), Some("a"));
        assert_eq!(output.footnote_candidates[1].display.as_deref(), Some("b"));
    }

    #[test]
    fn test_footnote_candidate_rejects_bracket_and_padded_labels() {
        // `[` inside the label is never a footnote; a label with leading/trailing whitespace
        // (`[^ x]`, `[^x ]`) is far more likely prose than a footnote reference.
        for text in ["a [^x[y] b", "a [^ x] b", "a [^x ] b", "a [^] b"] {
            let output = scan_inline(text, &[]);
            assert!(
                output.footnote_candidates.is_empty(),
                "{text:?} -> {:?}",
                output.footnote_candidates
            );
        }
        // A space in the middle is fine: pulldown-cmark accepts `[^a b]`.
        let output = scan_inline("a [^a b] c", &[]);
        assert_eq!(output.footnote_candidates.len(), 1);
    }

    #[test]
    fn test_footnote_candidate_ignores_code_spans() {
        let text = "Real [^a] but `inline [^fake]` code.";
        let code_spans = vec![ByteRange::new(14, 30)]; // `inline [^fake]`
        let output = scan_inline(text, &code_spans);
        assert_eq!(output.footnote_candidates.len(), 1);
        assert_eq!(output.footnote_candidates[0].display.as_deref(), Some("a"));
    }

    type Parts = (String, Option<String>, Option<String>, Option<String>);

    /// `(target_doc, heading, block, display)` of every wikilink in `text`.
    fn wiki(text: &str) -> Vec<Parts> {
        scan_inline(text, &[])
            .wiki_links
            .into_iter()
            .map(|l| (l.target_doc, l.target_heading, l.target_block, l.display))
            .collect()
    }

    fn some(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    // ---- F01-02: `\|` is the alias separator inside tables ----

    #[test]
    fn an_escaped_pipe_separates_target_and_alias_like_a_plain_one() {
        assert_eq!(
            wiki("| [[note\\|alias]] |"),
            vec![("note".into(), None, None, some("alias"))]
        );
        assert_eq!(
            wiki("[[a#h\\|x]]"),
            vec![("a".into(), some("h"), None, some("x"))]
        );
        assert_eq!(
            wiki("[[a#^b\\|x]]"),
            vec![("a".into(), None, some("b"), some("x"))]
        );
        assert_eq!(
            wiki("[[note\\|  spaced alias ]]"),
            vec![("note".into(), None, None, some("spaced alias"))]
        );
        // The plain pipe is unchanged.
        assert_eq!(
            wiki("[[note|alias]]"),
            vec![("note".into(), None, None, some("alias"))]
        );
    }

    #[test]
    fn a_backslash_that_does_not_escape_a_pipe_stays_in_the_target() {
        // Two backslashes: the first is a literal one, the second escapes the pipe.
        assert_eq!(
            wiki("[[note\\\\|alias]]"),
            vec![("note\\".into(), None, None, some("alias"))]
        );
        // No pipe at all: nothing to strip.
        assert_eq!(
            wiki("[[note\\]]"),
            vec![("note\\".into(), None, None, None)]
        );
    }

    // ---- F01-09: unclosed brackets do not scan past their own line ----

    #[test]
    fn brackets_never_close_on_a_later_line() {
        for text in ["[[a\nb]]", "[[a\r\nb]]", "![[a\nb]]", "[[a\n\n]]", "[[a"] {
            assert!(wiki(text).is_empty(), "{text:?}");
        }
        for text in ["[^a\nb]", "[^a\r\nb]", "[^a"] {
            assert!(
                scan_inline(text, &[]).footnote_candidates.is_empty(),
                "{text:?}"
            );
        }
        // A closed one on the next line is still found.
        assert_eq!(wiki("[[a\n[[b]]").len(), 1);
        assert_eq!(wiki("[[a\r\n[[b]]")[0].0, "b");
    }

    #[test]
    fn many_unclosed_openers_scan_in_linear_time() {
        let text = "[[unclosed link and [^unclosed note\n".repeat(20_000);
        let start = std::time::Instant::now();
        let out = scan_inline(&text, &[]);
        assert!(out.wiki_links.is_empty() && out.footnote_candidates.is_empty());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "took {:?}",
            start.elapsed()
        );
    }

    // ---- F01-19: links with nothing to point at are not links ----

    #[test]
    fn degenerate_wikilinks_are_not_links() {
        for text in [
            "[[]]",
            "[[ ]]",
            "[[|x]]",
            "[[ | x ]]",
            "[[#]]",
            "[[#^]]",
            "![[]]",
            "![[|x]]",
            "[[\\|x]]",
        ] {
            assert!(wiki(text).is_empty(), "{text:?} -> {:?}", wiki(text));
        }
        // Same-note anchors are real links.
        assert_eq!(wiki("[[#h]]"), vec![("".into(), some("h"), None, None)]);
        assert_eq!(wiki("[[#^b]]"), vec![("".into(), None, some("b"), None)]);
        // Neighbours are unaffected.
        assert_eq!(wiki("[[]] [[a]]"), vec![("a".into(), None, None, None)]);
        assert_eq!(wiki("[[a]] [[]] [[b]]").len(), 2);
    }

    // ---- F01-12: a block anchor ends its line ----

    fn blocks(text: &str) -> Vec<String> {
        scan_inline(text, &[])
            .blocks
            .into_iter()
            .map(|b| b.id)
            .collect()
    }

    #[test]
    fn a_caret_word_inside_a_sentence_is_not_a_block_anchor() {
        for text in [
            "2 ^3 power",
            "x ^a b",
            "a ^b c ^d e",
            "^a and more",
            "^a_b",
            "see ^a\u{00e9}",
        ] {
            assert!(blocks(text).is_empty(), "{text:?} -> {:?}", blocks(text));
        }
    }

    #[test]
    fn a_trailing_caret_word_is_a_block_anchor() {
        for (text, id) in [
            ("text ^id", "id"),
            ("text ^id  ", "id"),
            ("text ^id\t", "id"),
            ("text ^id\nnext", "id"),
            ("text ^id\r\nnext", "id"),
            ("^id", "id"),
            ("## Title ^blk", "blk"),
            ("- item ^it-1", "it-1"),
            ("End of sentence ^my-block.", "my-block"),
        ] {
            assert_eq!(blocks(text), vec![id.to_string()], "{text:?}");
        }
        assert_eq!(blocks("a ^b c ^d"), vec!["d"]);
        assert_eq!(blocks("first ^one\nsecond ^two"), vec!["one", "two"]);
    }

    // ---- what a wikilink points at, worked out again the way it was (4.3) ----

    /// `(target_doc, heading, block, display)` of the wikilink whose text between the brackets is
    /// `inner_slice`, or `None` when it points at nothing: the extraction of `parse_wikilink` as it
    /// was, kept as the reference.
    fn reference_wikilink_parts(inner_slice: &str) -> Option<Parts> {
        // Parse target and display. Inside a table cell the separator is written `\|`; that one
        // backslash belongs to the separator, not to the target.
        let (target_raw, display) = match inner_slice.find('|') {
            Some(pipe) => {
                let target = &inner_slice[..pipe];
                let target = target.strip_suffix('\\').unwrap_or(target);
                (
                    target.trim(),
                    Some(inner_slice[pipe + 1..].trim().to_string()),
                )
            }
            None => (inner_slice.trim(), None),
        };
        let (target_doc, target_heading, target_block) =
            if let Some((doc, block)) = target_raw.split_once("#^") {
                (doc.trim().to_string(), None, Some(block.trim().to_string()))
            } else if let Some((doc, heading)) = target_raw.split_once('#') {
                (
                    doc.trim().to_string(),
                    Some(heading.trim().to_string()),
                    None,
                )
            } else {
                (target_raw.to_string(), None, None)
            };
        let target_heading = target_heading.filter(|h| !h.is_empty());
        let target_block = target_block.filter(|b| !b.is_empty());
        if target_doc.is_empty() && target_heading.is_none() && target_block.is_none() {
            return None;
        }
        Some((target_doc, target_heading, target_block, display))
    }

    #[test]
    fn a_wikilink_points_at_what_it_always_did_and_nothing_else_is_a_link() {
        let targets = ["", " ", "a", "a b", "İş", "a/b", "a.md", "\t"];
        let anchors = [
            "", "#", "# ", "#h", "# h ", "#^", "#^b", "#^ b ", "#h#^b", "#^b#h", "##", "#\t",
            "#^\t",
        ];
        let displays = ["", "|", "|x", "| x ", "\\|x", "|x|y", "| "];
        let pads = ["", " ", "\t"];
        let (mut cases, mut links, mut dropped) = (0, 0, 0);
        for target in targets {
            for anchor in anchors {
                for display in displays {
                    for pad in pads {
                        for embed in ["", "!"] {
                            let inner = format!("{pad}{target}{anchor}{display}{pad}");
                            let text = format!("{embed}[[{inner}]]");
                            let wanted = reference_wikilink_parts(&inner);
                            let got = wiki(&text);
                            for link in scan_inline(&text, &[]).wiki_links {
                                assert!(!link.is_degenerate(), "{text:?}: {link:?}");
                            }
                            match &wanted {
                                Some(parts) => assert_eq!(got, vec![parts.clone()], "{text:?}"),
                                None => assert!(got.is_empty(), "{text:?}: {got:?}"),
                            }
                            cases += 1;
                            links += usize::from(wanted.is_some());
                            dropped += usize::from(wanted.is_none());
                        }
                    }
                }
            }
        }
        assert_eq!(cases, 8 * 13 * 7 * 3 * 2);
        assert!(
            links > 3000 && dropped > 100,
            "{links} links, {dropped} dropped"
        );
    }
}
