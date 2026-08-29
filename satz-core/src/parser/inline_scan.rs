use crate::model::block::BlockAnchor;
use crate::model::link::{Link, LinkKind};
use crate::model::range::ByteRange;
use crate::model::tag::Tag;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct InlineScanOutput {
    pub wiki_links: Vec<Link>,
    pub tags: Vec<Tag>,
    pub blocks: Vec<BlockAnchor>,
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

    // Find closing `]]` on the same line
    let rest = &source[inner_start..];
    let end_bracket = rest.find("]]")?;

    // Must not span multiple lines
    let inner_slice = &rest[..end_bracket];
    if inner_slice.contains('\n') || inner_slice.contains('\r') {
        return None;
    }

    let full_end = inner_start + end_bracket + 2;
    let range = ByteRange::new(start, full_end);
    let kind = if is_embed {
        LinkKind::Embed
    } else {
        LinkKind::WikiLink
    };

    // Parse target and display
    let (target_raw, display) = if let Some((target, disp)) = inner_slice.split_once('|') {
        (target.trim(), Some(disp.trim().to_string()))
    } else {
        (inner_slice.trim(), None)
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

    Some((
        Link::new(
            kind,
            target_doc,
            target_heading,
            target_block,
            display,
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

    // Check trailing character: must be end of string, whitespace, or punctuation
    if let Some(next_ch) = source[full_end..].chars().next()
        && !next_ch.is_whitespace()
        && !matches!(next_ch, '.' | ',' | ';' | ':' | ')' | ']' | '}')
    {
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
}
