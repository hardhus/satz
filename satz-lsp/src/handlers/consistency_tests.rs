//! The same link must be judged the same way by every handler: diagnostics, document links,
//! inlay hints and highlights all resolve links through one path (folder-relative Markdown
//! paths, relative daily aliases from the config, anchor checks).
#![allow(clippy::field_reassign_with_default)]

use crate::handlers::{
    diagnostics::compute_diagnostics, document_highlight::document_highlight,
    document_link::document_link, inlay_hint::inlay_hint,
};
use crate::state::{OpenDocument, SatzState};
use satz_core::{Index, VaultConfig, parse_document};
use std::path::Path;
use tower_lsp_server::ls_types::{
    DocumentHighlightParams, DocumentLinkParams, InlayHintLabel, InlayHintParams, Position, Range,
    TextDocumentIdentifier, TextDocumentPositionParams,
};

/// What each handler says about the first link of the opened note.
#[derive(Debug, PartialEq, Eq)]
struct Verdict {
    /// Diagnostic codes on the note.
    diagnostics: Vec<String>,
    /// Target file names the note's document links point to.
    link_targets: Vec<String>,
    /// Inlay hint labels.
    hints: Vec<String>,
    /// Number of highlights when the cursor is on the first link.
    highlights: usize,
}

fn judge(open: &str, files: &[(&str, &str)], config: VaultConfig) -> Verdict {
    let mut state = SatzState::default();
    state.index = Index::build(
        files
            .iter()
            .map(|(p, t)| parse_document(t, Path::new(p)))
            .collect(),
    );
    state.config = config;
    let root = if cfg!(windows) {
        Path::new("C:\\vault").to_path_buf()
    } else {
        Path::new("/vault").to_path_buf()
    };
    state.vault_root = Some(root.clone());
    let text = files.iter().find(|(p, _)| *p == open).unwrap().1;
    let uri = crate::convert::path_to_uri(&root.join(open))
        .unwrap()
        .as_str()
        .to_string();
    state.open_docs.insert(
        uri.clone(),
        OpenDocument::new(&uri, root.join(open), text, 1),
    );
    let doc = state
        .index
        .documents()
        .find(|d| d.path == Path::new(open))
        .unwrap();
    let first = doc.links.first().expect("the note has a link");
    let start = doc.line_index.byte_to_position(first.range.start);

    let mut diagnostics: Vec<String> = compute_diagnostics(doc, &state.index, &state.config)
        .into_iter()
        .filter_map(|d| match d.code {
            Some(tower_lsp_server::ls_types::NumberOrString::String(s)) => Some(s),
            _ => None,
        })
        .collect();
    diagnostics.retain(|c| c != "orphan-note"); // unrelated to link resolution
    diagnostics.sort();

    let ident = TextDocumentIdentifier {
        uri: uri.parse().unwrap(),
    };
    let link_targets = document_link(
        DocumentLinkParams {
            text_document: ident.clone(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
        &state,
    )
    .unwrap_or_default()
    .into_iter()
    .filter_map(|l| l.target)
    .map(|u| u.as_str().rsplit('/').next().unwrap().to_string())
    .collect();

    let hints = inlay_hint(
        InlayHintParams {
            text_document: ident.clone(),
            range: Range::new(Position::new(0, 0), Position::new(u32::MAX, 0)),
            work_done_progress_params: Default::default(),
        },
        &state,
    )
    .unwrap_or_default()
    .into_iter()
    .map(|h| match h.label {
        InlayHintLabel::String(s) => s,
        InlayHintLabel::LabelParts(_) => panic!("string labels expected"),
    })
    .collect();

    let highlights = document_highlight(
        DocumentHighlightParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: ident,
                position: Position::new(start.line, start.character + 1),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
        &state,
    )
    .map_or(0, |h| h.len());

    Verdict {
        diagnostics,
        link_targets,
        hints,
        highlights,
    }
}

fn daily_config() -> VaultConfig {
    let mut config = VaultConfig::default();
    config.daily_note.folder = "daily".into();
    config.daily_note.format = "today-note".into(); // no specifiers: a fixed file name
    config
}

#[test]
fn a_folder_relative_markdown_link_is_resolved_by_every_handler() {
    let files = [
        ("sub/a.md", "[t](../b.md) and [again](../b.md)\n"),
        ("b.md", "# root b\n"),
        ("sub/b.md", "# sub b\n"),
    ];
    let v = judge("sub/a.md", &files, VaultConfig::default());
    assert_eq!(v.diagnostics, Vec::<String>::new());
    assert_eq!(v.link_targets, vec!["b.md", "b.md"]);
    assert_eq!(v.hints.len(), 2);
    assert!(v.hints.iter().all(|h| !h.contains('⚠')), "{:?}", v.hints);
    assert_eq!(v.highlights, 2, "both links to b.md are highlighted");
}

#[test]
fn a_link_that_leaves_the_vault_is_broken_everywhere() {
    let files = [("sub/a.md", "[t](../../out.md)\n"), ("out.md", "# out\n")];
    let v = judge("sub/a.md", &files, VaultConfig::default());
    assert_eq!(v.diagnostics, vec!["broken-link"]);
    assert!(v.link_targets.is_empty(), "{:?}", v.link_targets);
    assert_eq!(v.hints, vec![" ⚠ not found"]);
}

#[test]
fn a_relative_daily_alias_is_resolved_by_every_handler() {
    let files = [
        ("a.md", "[[today]] and [[today]]\n"),
        ("daily/today-note.md", "# the daily note\n"),
    ];
    let v = judge("a.md", &files, daily_config());
    assert_eq!(v.diagnostics, Vec::<String>::new());
    assert_eq!(v.link_targets, vec!["today-note.md", "today-note.md"]);
    assert_eq!(v.hints.len(), 2);
    assert!(v.hints.iter().all(|h| !h.contains('⚠')), "{:?}", v.hints);
    assert_eq!(v.highlights, 2);
}

#[test]
fn a_missing_heading_in_a_markdown_link_is_reported_like_in_a_wikilink() {
    let target = "# B\n\n## Real\n\ntext ^blk\n";
    for (link, expect) in [
        ("[t](b.md#Real)", Vec::<&str>::new()),
        ("[t](b.md#Yok)", vec!["broken-heading"]),
        ("[[b#Yok]]", vec!["broken-heading"]),
        ("[t](#Yok)", vec!["broken-heading"]),
    ] {
        let files = [
            ("a.md", format!("# A\n\n{link}\n")),
            ("b.md", target.to_string()),
        ];
        let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
        let v = judge("a.md", &refs, VaultConfig::default());
        assert_eq!(v.diagnostics, expect, "{link}");
    }
}

#[test]
fn external_and_empty_targets_are_never_reported() {
    for link in [
        "[t](https://example.com/x#frag)",
        "[t](mailto:a@b.c)",
        "[t]()",
        "[t](tel:+90)",
    ] {
        let files = [("a.md", format!("{link}\n"))];
        let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
        let v = judge("a.md", &refs, VaultConfig::default());
        assert!(v.diagnostics.is_empty(), "{link}: {:?}", v.diagnostics);
        assert!(
            v.hints.iter().all(|h| !h.contains('⚠')),
            "{link}: {:?}",
            v.hints
        );
    }
}

#[test]
fn the_same_file_name_in_two_folders_is_told_apart_by_every_handler() {
    let files = [
        ("sub/a.md", "[t](b.md) and [u](../b.md)\n"),
        ("sub/b.md", "# sub b\n"),
        ("b.md", "# root b\n"),
    ];
    let v = judge("sub/a.md", &files, VaultConfig::default());
    assert_eq!(v.diagnostics, Vec::<String>::new());
    assert_eq!(v.link_targets, vec!["b.md", "b.md"]);
    assert_eq!(v.highlights, 1, "only the link to sub/b.md itself");
}
