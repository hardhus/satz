use crate::model::range::ByteRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum LinkKind {
    /// `[[target]]` or `[[target#heading]]` or `[[target|display]]`
    WikiLink,
    /// `[text](url)` or `[text](url#heading)`
    Markdown,
    /// `[^label]` reference
    Footnote,
    /// `![[target]]` or `![[target#heading]]`
    Embed,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Link {
    pub kind: LinkKind,
    /// Target document path/name, or empty string `""` for same-document links
    pub target_doc: String,
    /// Target heading if specified (`#heading`)
    pub target_heading: Option<String>,
    /// Target block anchor if specified (`#^block-id`)
    pub target_block: Option<String>,
    /// Custom display text / alias (e.g. `[[target|display]]` or `[display](target)`)
    pub display: Option<String>,
    /// Byte range of the entire link syntax
    pub range: ByteRange,
}

impl Link {
    pub fn new(
        kind: LinkKind,
        target_doc: String,
        target_heading: Option<String>,
        target_block: Option<String>,
        display: Option<String>,
        range: ByteRange,
    ) -> Self {
        Self {
            kind,
            target_doc,
            target_heading,
            target_block,
            display,
            range,
        }
    }
}

/// Schemes that are followed by no `//` (`mailto:a@b.c`); any other scheme needs the `://`.
const OPAQUE_SCHEMES: &[&str] = &[
    "mailto",
    "tel",
    "sms",
    "geo",
    "data",
    "javascript",
    "urn",
    "magnet",
];

/// Whether a link target points outside the vault: `scheme://...` (`https://`, `obsidian://`,
/// `file:///`) or one of the opaque schemes (`mailto:`, `tel:`). A single-letter scheme is a
/// Windows drive (`C:\notes\x.md`) and a note may legitimately have a colon in its name
/// (`Project:Alpha`), so both stay internal.
pub fn is_external_target(target: &str) -> bool {
    let Some((scheme, rest)) = target.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    let scheme_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme.len() >= 2
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
    if !scheme_ok {
        return false;
    }
    rest.starts_with("//")
        || OPAQUE_SCHEMES
            .iter()
            .any(|s| s.eq_ignore_ascii_case(scheme))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_with_a_scheme_are_external() {
        for t in [
            "http://a.b",
            "https://a.b/c#frag",
            "HTTPS://A.B",
            "mailto:a@b.c",
            "tel:+90555",
            "ftp://host/x",
            "obsidian://open?vault=x",
            "file:///x",
            "git+ssh://h/r",
            "sms:123",
            "javascript:void(0)",
            "urn:isbn:123",
        ] {
            assert!(is_external_target(t), "{t:?}");
        }
    }

    #[test]
    fn note_names_and_paths_are_not_external() {
        for t in [
            "",
            "note",
            "folder/note",
            "note.md",
            "C:\\dir\\x.md",
            "C:/dir/x.md",
            "a:b",
            "1abc:x",
            ":x",
            "a b:c",
            "note: with colon",
            "Project:Alpha",
            "Note:Title",
            "sub/mailto:x",
            "./x:y",
        ] {
            assert!(!is_external_target(t), "{t:?}");
        }
    }
}
