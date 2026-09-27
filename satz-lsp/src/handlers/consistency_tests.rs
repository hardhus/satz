//! The same link must be judged the same way by every handler: diagnostics, document links,
//! inlay hints and highlights all resolve links through one path (folder-relative Markdown
//! paths, relative daily aliases from the config, anchor checks).
// Test states are built field by field so each test shows exactly what it sets up.
#![allow(clippy::field_reassign_with_default)]

use crate::handlers::{
    code_action::code_action, diagnostics::compute_diagnostics,
    document_highlight::document_highlight, document_link::document_link, inlay_hint::inlay_hint,
};
use crate::state::{OpenDocument, SatzState};
use satz_core::{Index, VaultConfig, parse_document};
use std::path::Path;
use tower_lsp_server::ls_types::{
    CodeActionOrCommand, CodeActionParams, DocumentHighlightParams, DocumentLinkParams,
    InlayHintLabel, InlayHintParams, Position, Range, TextDocumentIdentifier,
    TextDocumentPositionParams,
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
    /// "Create note" code actions offered with the cursor on the first link.
    create_note: usize,
}

fn judge(open: &str, files: &[(&str, &str)], config: VaultConfig) -> Verdict {
    let mut state = SatzState::default();
    state.index = Index::build(
        files
            .iter()
            .map(|(p, t)| parse_document(t, &crate::convert::native_path(p)))
            .collect(),
    );
    state.config = config;
    let root = if cfg!(windows) {
        Path::new("C:\\vault").to_path_buf()
    } else {
        Path::new("/vault").to_path_buf()
    };
    state.set_vault_root(Some(root.clone()));
    let text = files.iter().find(|(p, _)| *p == open).unwrap().1;
    let uri = crate::convert::path_to_uri(&root.join(crate::convert::native_path(open)))
        .unwrap()
        .as_str()
        .to_string();
    state.open_docs.insert(
        uri.clone(),
        OpenDocument::new(&uri, root.join(crate::convert::native_path(open)), text, 1),
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

    let create_note = code_action(
        CodeActionParams {
            text_document: TextDocumentIdentifier {
                uri: uri.parse().unwrap(),
            },
            range: Range::new(
                Position::new(start.line, start.character + 1),
                Position::new(start.line, start.character + 1),
            ),
            context: Default::default(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
        &state,
    )
    .unwrap_or_default()
    .into_iter()
    .filter(|a| match a {
        CodeActionOrCommand::CodeAction(a) => a.title.starts_with("Create note"),
        CodeActionOrCommand::Command(_) => false,
    })
    .count();

    Verdict {
        diagnostics,
        link_targets,
        hints,
        highlights,
        create_note,
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
    assert_eq!(v.create_note, 0);
}

#[test]
fn a_link_that_leaves_the_vault_is_broken_everywhere() {
    let files = [("sub/a.md", "[t](../../out.md)\n"), ("out.md", "# out\n")];
    let v = judge("sub/a.md", &files, VaultConfig::default());
    assert_eq!(v.diagnostics, vec!["broken-link"]);
    assert!(v.link_targets.is_empty(), "{:?}", v.link_targets);
    assert_eq!(v.hints, vec![" ⚠ not found"]);
    // Broken, but a note outside the vault must not be offered for creation either.
    assert_eq!(v.create_note, 0);
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
    assert_eq!(v.create_note, 0, "a resolved daily alias needs no new note");
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

#[test]
fn a_really_missing_note_is_reported_and_offered_for_creation() {
    for link in ["[[ghost]]", "[t](ghost.md)"] {
        let files = [("a.md", format!("{link}\n"))];
        let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
        let v = judge("a.md", &refs, daily_config());
        assert_eq!(v.diagnostics, vec!["broken-link"], "{link}");
        assert_eq!(v.create_note, 1, "{link}");
        assert_eq!(v.hints, vec![" ⚠ not found"], "{link}");
    }
}

// ---- the link garden: every kind of link, every handler, one judgement (6.1) ----
//
// A note (`a.md`) holds one link of every kind, one to a line, and every handler is asked about each
// of them with the cursor inside it. What the resolution says about the link (`Truth`) decides what
// every handler must answer; the few places where a handler answers differently on purpose are
// named in `disagreements` and pinned there.

use crate::handlers::{
    definition::goto_definition,
    diagnostics::as_the_user_sees_it,
    hover::hover,
    references::find_references,
    rename::rename,
    semantic_tokens::{TOKEN_TYPES, semantic_tokens_full},
};
use satz_core::{LinkKind, LinkResolution};
use tower_lsp_server::ls_types::{
    GotoDefinitionParams, GotoDefinitionResponse, HoverParams, ReferenceContext, ReferenceParams,
    RenameParams, SemanticTokensParams, SemanticTokensResult,
};

fn vault_root() -> std::path::PathBuf {
    if cfg!(windows) {
        std::path::PathBuf::from("C:\\vault")
    } else {
        std::path::PathBuf::from("/vault")
    }
}

/// A state over `files`, with `open` opened as an editor would have it (its buffer is what the
/// index holds).
fn state_over(open: &str, files: &[(&str, &str)], config: VaultConfig) -> (SatzState, String) {
    let mut state = SatzState::default();
    state.index = Index::build(
        files
            .iter()
            .map(|(p, t)| parse_document(t, &crate::convert::native_path(p)))
            .collect(),
    );
    state.config = config;
    let root = vault_root();
    state.set_vault_root(Some(root.clone()));
    let text = files.iter().find(|(p, _)| *p == open).unwrap().1;
    let uri = crate::convert::path_to_uri(&root.join(crate::convert::native_path(open)))
        .unwrap()
        .as_str()
        .to_string();
    state.open_docs.insert(
        uri.clone(),
        OpenDocument::new(&uri, root.join(crate::convert::native_path(open)), text, 1),
    );
    (state, uri)
}

/// What the resolution says about a link: the ground truth every handler is compared with.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Truth {
    Resolved(String),
    AnchorMissing(String),
    DocMissing,
}

fn truth_of(state: &SatzState, doc: &satz_core::Document, link: &satz_core::Link) -> Truth {
    match state.resolve(link, doc) {
        LinkResolution::Resolved { doc, .. } => Truth::Resolved(doc.id.as_str().to_string()),
        LinkResolution::AnchorMissing { doc } => Truth::AnchorMissing(doc.id.as_str().to_string()),
        LinkResolution::DocMissing => Truth::DocMissing,
    }
}

/// What every handler says about one link, asked with the cursor inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Probe {
    text: String,
    kind: LinkKind,
    /// The link names no note (`[[#Heading]]`, `[t](#top)`, `[t]()`).
    same_note: bool,
    /// The link has a heading or a block.
    has_anchor: bool,
    /// The link has a block (and no heading matters to what a rename does).
    has_block: bool,
    external: bool,
    /// It points at nothing at all (`[t]()`).
    degenerate: bool,
    truth: Truth,
    /// What the user is told about it: the truth, but a real anchor that is no heading (`#top`) is
    /// no missing anchor.
    seen: Truth,
    /// Diagnostic codes whose range starts where the link starts.
    diagnostics: Vec<String>,
    /// The file a document link on the link points to.
    document_link: Option<String>,
    /// The inlay hint at the end of the link, if any.
    hint: Option<String>,
    /// The type of the semantic token that starts where the link starts.
    token: Option<&'static str>,
    /// The start of what hover shows, `None` if it shows nothing.
    hover: Option<String>,
    /// The file go-to-definition leads to.
    definition: Option<String>,
    /// How many places references are found in (`None`: no answer).
    references: Option<usize>,
    /// `Ok(number of edits)` or `Err(the message)` for a rename to a fresh name.
    rename: Result<usize, String>,
    /// "Create note" offered.
    create_note: usize,
}

fn file_name_of(uri: &str) -> String {
    uri.rsplit('/').next().unwrap_or(uri).to_string()
}

/// The file name a URI of the note `id` ends in (a name with letters outside ASCII is encoded).
fn uri_name_of(id: &str) -> String {
    file_name_of(
        crate::convert::path_to_uri(&vault_root().join(crate::convert::native_path(id)))
            .unwrap()
            .as_str(),
    )
}

fn probe_link(state: &SatzState, uri: &str, link_at: usize) -> Probe {
    let doc = state.doc_for_uri(uri).unwrap().1;
    let link = &doc.links[link_at];
    let range = crate::convert::byte_range_to_lsp(link.range, &doc.line_index);
    let (start, end) = (range.start, range.end);
    let inside = Position::new(start.line, start.character + 1);
    let ident = TextDocumentIdentifier {
        uri: uri.parse().unwrap(),
    };
    let text = doc.line_index.source()[link.range.start..link.range.end].to_string();

    let mut diagnostics: Vec<String> = compute_diagnostics(doc, &state.index, &state.config)
        .into_iter()
        .filter(|d| d.range.start == start)
        .filter_map(|d| match d.code {
            Some(tower_lsp_server::ls_types::NumberOrString::String(s)) => Some(s),
            _ => None,
        })
        .collect();
    diagnostics.sort();

    let document_link = document_link(
        DocumentLinkParams {
            text_document: ident.clone(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
        state,
    )
    .unwrap_or_default()
    .into_iter()
    .filter(|l| l.range.start == start)
    .find_map(|l| l.target.map(|u| file_name_of(u.as_str())));

    let hint = inlay_hint(
        InlayHintParams {
            text_document: ident.clone(),
            range: Range::new(Position::new(0, 0), Position::new(u32::MAX, 0)),
            work_done_progress_params: Default::default(),
        },
        state,
    )
    .unwrap_or_default()
    .into_iter()
    .filter(|h| h.position == end)
    .find_map(|h| match h.label {
        InlayHintLabel::String(s) => Some(s),
        InlayHintLabel::LabelParts(_) => None,
    });

    let token = semantic_tokens_full(
        SemanticTokensParams {
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            text_document: ident.clone(),
        },
        state,
    )
    .and_then(|r| match r {
        SemanticTokensResult::Tokens(t) => Some(t.data),
        SemanticTokensResult::Partial(_) => None,
    })
    .and_then(|data| {
        let (mut line, mut col) = (0u32, 0u32);
        for t in data {
            line += t.delta_line;
            col = if t.delta_line == 0 {
                col + t.delta_start
            } else {
                t.delta_start
            };
            if line == start.line && col == start.character {
                return TOKEN_TYPES.get(t.token_type as usize).copied();
            }
        }
        None
    });

    let position = TextDocumentPositionParams {
        text_document: ident.clone(),
        position: inside,
    };
    let hover = hover(
        HoverParams {
            text_document_position_params: position.clone(),
            work_done_progress_params: Default::default(),
        },
        state,
    )
    .map(|h| match h.contents {
        tower_lsp_server::ls_types::HoverContents::Markup(m) => m.value,
        _ => "(other)".to_string(),
    });

    let definition = goto_definition(
        GotoDefinitionParams {
            text_document_position_params: position.clone(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
        state,
    )
    .and_then(|r| match r {
        GotoDefinitionResponse::Scalar(l) => Some(file_name_of(l.uri.as_str())),
        _ => None,
    });

    let references = find_references(
        ReferenceParams {
            text_document_position: position.clone(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: ReferenceContext {
                include_declaration: true,
            },
        },
        state,
    )
    .map(|l| l.len());

    let renamed = rename(
        RenameParams {
            text_document_position: position,
            new_name: "zz fresh".to_string(),
            work_done_progress_params: Default::default(),
        },
        state,
    );
    let rename = match renamed {
        Ok(None) => Err("nothing".to_string()),
        Ok(Some(edit)) => {
            let plain: usize = edit
                .changes
                .map(|c| c.values().map(Vec::len).sum())
                .unwrap_or(0);
            let versioned: usize = match edit.document_changes {
                Some(tower_lsp_server::ls_types::DocumentChanges::Edits(e)) => {
                    e.iter().map(|d| d.edits.len()).sum()
                }
                Some(tower_lsp_server::ls_types::DocumentChanges::Operations(operations)) => {
                    operations
                        .iter()
                        .map(|operation| match operation {
                            tower_lsp_server::ls_types::DocumentChangeOperation::Edit(e) => {
                                e.edits.len()
                            }
                            tower_lsp_server::ls_types::DocumentChangeOperation::Op(_) => 1,
                        })
                        .sum()
                }
                None => 0,
            };
            Ok(plain + versioned)
        }
        Err(message) => Err(message),
    };

    let create_note = code_action(
        CodeActionParams {
            text_document: ident,
            range: Range::new(inside, inside),
            context: Default::default(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
        state,
    )
    .unwrap_or_default()
    .into_iter()
    .filter(|a| match a {
        CodeActionOrCommand::CodeAction(a) => a.title.starts_with("Create note"),
        CodeActionOrCommand::Command(_) => false,
    })
    .count();

    let truth = truth_of(state, doc, link);
    let seen = match as_the_user_sees_it(link, state.resolve(link, doc)) {
        LinkResolution::Resolved { doc, .. } => Truth::Resolved(doc.id.as_str().to_string()),
        LinkResolution::AnchorMissing { doc } => Truth::AnchorMissing(doc.id.as_str().to_string()),
        LinkResolution::DocMissing => Truth::DocMissing,
    };
    Probe {
        text,
        kind: link.kind,
        same_note: link.target_doc.is_empty(),
        has_anchor: link.target_heading.is_some() || link.target_block.is_some(),
        has_block: link.target_block.is_some(),
        external: satz_core::model::link::is_external_target(&link.target_doc),
        degenerate: link.is_degenerate(),
        truth,
        seen,
        diagnostics,
        document_link,
        hint,
        token,
        hover,
        definition,
        references,
        rename,
        create_note,
    }
}

const BODY_OF_B: &str = "# B\n\n## Real\n\ntext ^blk\n\n<a id=\"custom\"></a>\n";

/// The notes the garden links to.
fn garden_files() -> Vec<(&'static str, String)> {
    vec![
        ("b.md", BODY_OF_B.to_string()),
        ("sub/c.md", "# C\n\n## Sec\n".to_string()),
        (
            "alias.md",
            "---\naliases: [Other Name]\n---\n# Alias note\n".to_string(),
        ),
        ("daily/today-note.md", "# the daily note\n".to_string()),
        ("İş.md", "# İş\n".to_string()),
        ("Çalışma.md", "# Çalışma\n".to_string()),
    ]
}

/// Every kind of link, in a note (`a.md`) that has a heading `# A` and a block `^self`.
const GARDEN_LINKS: &[&str] = &[
    // wikilinks: the note
    "[[b]]",
    "[[B]]",
    "[[ghost]]",
    "[[Other Name]]",
    "[[alias]]",
    "[[sub/c]]",
    "[[c]]",
    "[[today]]",
    "[[iş]]",
    "[[İş]]",
    "[[Çalışma]]",
    "[[b|shown]]",
    "[[b|one|two]]",
    // wikilinks: a heading or a block
    "[[b#Real]]",
    "[[b#real]]",
    "[[b#Yok]]",
    "[[b#^blk]]",
    "[[b#^nope]]",
    "[[c#Sec]]",
    "[[b#Real|shown]]",
    "[[ghost#Real]]",
    // wikilinks: into the note itself
    "[[#A]]",
    "[[#Nope]]",
    "[[#^self]]",
    "[[#^nothing]]",
    // embeds
    "![[b]]",
    "![[ghost]]",
    "![[pic.png]]",
    "![[b#Real]]",
    "![[b#Yok]]",
    "![[b|300]]",
    "![[https://example.com/x.png]]",
    // Markdown links
    "[t](b.md)",
    "[t](b.md#Real)",
    "[t](b.md#Yok)",
    "[t](b.md#top)",
    "[t](b.md#custom)",
    "[t](#A)",
    "[t](#top)",
    "[t](#Nope)",
    "[t](ghost.md)",
    "[t](sub/c.md)",
    "[t](sub/c.md#Sec)",
    "[t](../out.md)",
    "[t](Çalışma.md)",
    // in a table
    "| [[b]] | [[ghost]] |\n|---|---|",
    // outside, empty
    "[t](https://example.com/x#frag)",
    "[t](mailto:a@b.c)",
    "[t]()",
    // footnotes
    "[^1]",
];

fn garden_note() -> String {
    let mut text = String::from("# A\n\ntext ^self\n\n");
    for link in GARDEN_LINKS {
        text.push_str(link);
        text.push_str("\n\n");
    }
    text.push_str("[^1]: the footnote\n");
    text
}

/// The whole garden, judged: one `Probe` per link of `a.md`.
fn garden() -> Vec<Probe> {
    let note = garden_note();
    let owned = garden_files();
    let mut files: Vec<(&str, &str)> = owned.iter().map(|(p, t)| (*p, t.as_str())).collect();
    files.push(("a.md", note.as_str()));
    let (state, uri) = state_over("a.md", &files, daily_config());
    let count = state.doc_for_uri(&uri).unwrap().1.links.len();
    (0..count).map(|i| probe_link(&state, &uri, i)).collect()
}

/// Where a handler's answer is not what the resolution says about the link. The places where a
/// handler answers differently ON PURPOSE are the ones with a comment.
fn disagreements(p: &Probe) -> Vec<String> {
    let mut bad: Vec<String> = Vec::new();
    let mut expect = |what: &str, ok: bool| {
        if !ok {
            bad.push(what.to_string());
        }
    };
    let has_no_warning = |hint: &Option<String>| hint.as_deref().is_none_or(|h| !h.contains('⚠'));

    // An address elsewhere is nobody's business here: nothing is reported, coloured or looked up.
    if p.external {
        expect("no diagnostic", p.diagnostics.is_empty());
        expect("no hint", p.hint.is_none());
        // (an embed of an address keeps the embed colour: there is nothing to resolve)
        let colour = (p.kind == LinkKind::Embed).then_some("embed");
        expect("token", p.token == colour);
        expect("no hover", p.hover.is_none());
        expect("no definition", p.definition.is_none());
        expect("no references", p.references.is_none());
        expect("no create note", p.create_note == 0);
        return bad;
    }
    // A footnote reference (it has its definition: an undefined one is not a link at all).
    if p.kind == LinkKind::Footnote {
        expect("no diagnostic", p.diagnostics.is_empty());
        expect("no document link", p.document_link.is_none());
        expect("token link", p.token == Some("link"));
        expect("hover shows the definition", p.hover.is_some());
        expect(
            "definition in the note",
            p.definition.as_deref() == Some("a.md"),
        );
        expect("no rename", p.rename == Err("nothing".to_string()));
        return bad;
    }
    // `[t]()` points at nothing: it is not broken and it is not a backlink, and it is coloured,
    // hovered and followed as the note it is in (the resolution's answer, see `Link::is_degenerate`),
    // while references, highlights and renames find nothing to work on.
    if p.degenerate {
        expect("no diagnostic", p.diagnostics.is_empty());
        expect("no hint", p.hint.is_none());
        expect("token link", p.token == Some("link"));
        expect("hover", p.hover.is_some());
        expect(
            "definition in the note",
            p.definition.as_deref() == Some("a.md"),
        );
        expect("no references", p.references.is_none());
        expect("no rename", p.rename == Err("nothing".to_string()));
        expect("no create note", p.create_note == 0);
        return bad;
    }

    // What the user is told (a diagnostic, a colour, a hint) follows `seen`; what the link is (a
    // document link, hover, definition, references, rename) follows the plain resolution.
    let embed = p.kind == LinkKind::Embed;
    match &p.seen {
        Truth::Resolved(_) => {
            expect("no diagnostic", p.diagnostics.is_empty());
            expect("no warning in the hint", has_no_warning(&p.hint));
            // A link into the note itself has nothing to say about the note it already is.
            expect(
                "hint only for another note",
                p.hint.is_some() != p.same_note,
            );
            expect(
                "colour of a resolved link",
                p.token == Some(if embed { "embed" } else { "link" }),
            );
        }
        Truth::AnchorMissing(_) => {
            expect("broken-heading", p.diagnostics == ["broken-heading"]);
            expect(
                "hint says the anchor is missing",
                p.hint.as_deref().is_some_and(|h| h.contains("not found")),
            );
            expect(
                "colour of an unresolved link",
                p.token == Some("unresolvedLink"),
            );
        }
        Truth::DocMissing => {
            let code = if embed { "broken-embed" } else { "broken-link" };
            expect("broken note", p.diagnostics == [code]);
            expect(
                "hint says not found",
                p.hint.as_deref() == Some(" ⚠ not found"),
            );
            expect(
                "colour of an unresolved link",
                p.token == Some("unresolvedLink"),
            );
        }
    }
    match &p.truth {
        Truth::Resolved(target) | Truth::AnchorMissing(target) => {
            let file = uri_name_of(target);
            expect(
                "document link to the note",
                p.document_link.as_deref() == Some(&file),
            );
            expect("hover", p.hover.is_some());
            expect(
                "definition in the note",
                p.definition.as_deref() == Some(&file),
            );
            expect("references", p.references.is_some_and(|n| n >= 1));
            expect("no create note", p.create_note == 0);
        }
        Truth::DocMissing => {
            expect("no document link", p.document_link.is_none());
            expect("no hover", p.hover.is_none());
            expect("no definition", p.definition.is_none());
            expect("no references", p.references.is_none());
            expect(
                "rename says the note does not exist",
                p.rename
                    .as_ref()
                    .err()
                    .is_some_and(|e| e.contains("does not exist")),
            );
            // Offered for a name that is safe to make a file of, and for nothing else.
            expect(
                "create note only for a name that can be one",
                p.create_note == usize::from(p.text.contains("ghost")),
            );
        }
    }
    // What a rename does follows the plain resolution too: a link with a heading renames the
    // heading (and fails when there is none), any other link renames the note, and the note a
    // link to a block names is renamed as well (there is no block to rename).
    if !matches!(p.truth, Truth::DocMissing) {
        if p.same_note && p.has_block {
            expect("nothing to rename", p.rename == Err("nothing".to_string()));
        } else if p.has_anchor && !p.has_block {
            match p.truth {
                Truth::Resolved(_) => expect("heading renamed", p.rename.is_ok()),
                _ => expect(
                    "no such heading to rename",
                    p.rename
                        .as_ref()
                        .err()
                        .is_some_and(|e| e.contains("not found")),
                ),
            }
        } else {
            expect("note renamed", p.rename.as_ref().is_ok_and(|n| *n >= 1));
        }
    }
    bad
}

#[test]
fn every_handler_agrees_with_the_resolution_on_every_kind_of_link() {
    let probes = garden();
    let mut all_bad = Vec::new();
    for p in &probes {
        let bad = disagreements(p);
        if !bad.is_empty() {
            all_bad.push(format!("{}: {bad:?}\n    {p:?}", p.text));
        }
    }
    assert!(all_bad.is_empty(), "\n{}", all_bad.join("\n"));

    // The garden is not vacuous: every kind of answer is asked about.
    let count = |f: &dyn Fn(&Probe) -> bool| probes.iter().filter(|p| f(p)).count();
    assert!(count(&|p| matches!(p.truth, Truth::Resolved(_)) && !p.external) >= 24);
    assert!(count(&|p| matches!(p.truth, Truth::AnchorMissing(_))) >= 10);
    assert!(count(&|p| matches!(p.truth, Truth::DocMissing) && !p.external) >= 7);
    assert!(count(&|p| p.kind == LinkKind::Embed) >= 7);
    assert!(count(&|p| p.kind == LinkKind::Markdown) >= 12);
    assert!(
        count(&|p| p.truth != p.seen) >= 3,
        "the real anchors that are no headings"
    );
    let (external, degenerate) = (
        count(&|p| p.external),
        count(&|p| p.degenerate && p.kind != LinkKind::Footnote),
    );
    assert!(
        external >= 3 && degenerate == 1,
        "{external} external, {degenerate} empty"
    );
}

#[test]
fn a_link_is_told_apart_from_what_the_user_is_told_only_for_a_real_anchor() {
    // `truth` and `seen` differ in exactly one way: a Markdown link whose fragment is no heading but
    // a real anchor is a missing anchor for what the link is, and resolved for what is said of it.
    for p in garden() {
        if p.truth != p.seen {
            assert_eq!(p.kind, LinkKind::Markdown, "{}", p.text);
            assert!(matches!(p.truth, Truth::AnchorMissing(_)), "{}", p.text);
            assert!(matches!(p.seen, Truth::Resolved(_)), "{}", p.text);
            assert!(
                p.text.to_lowercase().contains("#top") || p.text.contains("#custom"),
                "{}",
                p.text
            );
        }
    }
}

// ---- random vaults: the tables of the index and the answers of the handlers agree ----

struct Xorshift(u64);

impl Xorshift {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
    fn pick<'a>(&mut self, of: &[&'a str]) -> &'a str {
        of[self.below(of.len())]
    }
}

const NAMES: &[&str] = &[
    "alpha",
    "Beta",
    "gamma",
    "Delta",
    "épsilon",
    "İş",
    "çalışma",
    "zeta",
];
const FOLDERS: &[&str] = &["", "", "sub/", "deep/x/"];
const ALIASES: &[&str] = &["Ali", "Bob", "Zed Note"];

/// A vault of a few notes with titles, aliases, headings, blocks and every kind of link, some of
/// them to notes that are not there. Every note is open (as an editor would have it).
fn random_vault(rng: &mut Xorshift) -> SatzState {
    let mut paths: Vec<String> = Vec::new();
    for _ in 0..4 + rng.below(7) {
        let path = format!("{}{}.md", rng.pick(FOLDERS), rng.pick(NAMES));
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    let mut files: Vec<(String, String)> = Vec::new();
    for path in &paths {
        let mut text = String::new();
        if rng.below(3) == 0 {
            text.push_str(&format!("---\naliases: [{}]\n---\n", rng.pick(ALIASES)));
        }
        text.push_str(&format!(
            "# {}\n\n## H1\n\nsome text ^b1\n\n## H2\n\n",
            rng.pick(NAMES)
        ));
        for _ in 0..rng.below(8) {
            let n = if rng.below(6) == 0 {
                "ghost"
            } else {
                rng.pick(NAMES)
            };
            let h = rng.pick(&["H1", "H2", "Nope"]);
            let b = rng.pick(&["b1", "nope"]);
            let link = match rng.below(13) {
                0 => format!("[[{n}]]"),
                1 => format!("[[{n}#{h}]]"),
                2 => format!("[[{n}#^{b}]]"),
                3 => format!("![[{n}]]"),
                4 => format!("[t]({n}.md)"),
                5 => format!("[t](../{n}.md)"),
                6 => format!("[t](sub/{n}.md)"),
                7 => format!("[t]({n}.md#{h})"),
                8 => format!("[[#{h}]]"),
                9 => format!("[t](#{h})"),
                10 => "[t]()".to_string(),
                11 => format!("[[{}]]", rng.pick(ALIASES)),
                _ => "[t](https://example.com/x)".to_string(),
            };
            text.push_str(&link);
            text.push_str("\n\n");
        }
        files.push((path.clone(), text));
    }
    let mut state = SatzState::default();
    state.index = Index::build(
        files
            .iter()
            .map(|(p, t)| parse_document(t, &crate::convert::native_path(p)))
            .collect(),
    );
    state.config = daily_config();
    state.config.lsp.codelens.enable = true;
    let root = vault_root();
    state.set_vault_root(Some(root.clone()));
    for (path, text) in &files {
        let uri = crate::convert::path_to_uri(&root.join(crate::convert::native_path(path)))
            .unwrap()
            .as_str()
            .to_string();
        state.open_docs.insert(
            uri.clone(),
            OpenDocument::new(&uri, root.join(crate::convert::native_path(path)), text, 1),
        );
    }
    state
}

fn uri_of_note(id: &str) -> String {
    crate::convert::path_to_uri(&vault_root().join(crate::convert::native_path(id)))
        .unwrap()
        .as_str()
        .to_string()
}

fn cursor_in(doc: &satz_core::Document, link: &satz_core::Link) -> Position {
    let range = crate::convert::byte_range_to_lsp(link.range, &doc.line_index);
    Position::new(range.start.line, range.start.character + 1)
}

#[test]
fn the_backlinks_of_the_index_the_code_lens_and_the_references_count_what_the_handlers_link_to() {
    use std::collections::{BTreeMap, BTreeSet};
    let mut rng = Xorshift(0x6161_0000_0000_0001);
    let (mut checked_links, mut self_links) = (0, 0);
    for round in 0..200 {
        let state = random_vault(&mut rng);
        // Who links to whom, by what the handlers use (`link_target_doc`).
        let mut sources: BTreeMap<String, BTreeSet<String>> = Default::default();
        let mut links_to: BTreeMap<String, usize> = Default::default();
        // The same, not counting the links a note has to itself.
        let mut links_from_others: BTreeMap<String, usize> = Default::default();
        // The links that point at their own note in the index's tables: everything but a Markdown
        // link with no note in it (`[t](#H)`), which the tables never count as a link at all.
        let mut self_edges: BTreeSet<String> = Default::default();
        for doc in state.index.documents() {
            for link in &doc.links {
                let Some(target) = state.link_target_doc(doc, link) else {
                    continue;
                };
                sources
                    .entry(target.as_str().to_string())
                    .or_default()
                    .insert(doc.id.as_str().to_string());
                *links_to.entry(target.as_str().to_string()).or_default() += 1;
                if *target != doc.id {
                    *links_from_others
                        .entry(target.as_str().to_string())
                        .or_default() += 1;
                }
                checked_links += 1;
                if *target == doc.id
                    && !(link.kind == LinkKind::Markdown && link.target_doc.is_empty())
                {
                    self_edges.insert(doc.id.as_str().to_string());
                }
            }
        }
        for doc in state.index.documents() {
            let id = doc.id.as_str().to_string();
            let mut from_index: BTreeSet<String> = state
                .index
                .backlinks_of(&doc.id)
                .map(|d| d.as_str().to_string())
                .collect();
            let mut from_handlers = sources.get(&id).cloned().unwrap_or_default();
            // A note that links to itself: the tables and the handlers agree on when it does.
            assert_eq!(
                from_index.contains(&id),
                self_edges.contains(&id),
                "round {round}: {id} and itself"
            );
            self_links += usize::from(from_index.contains(&id));
            from_index.remove(&id);
            from_handlers.remove(&id);
            assert_eq!(
                from_index, from_handlers,
                "round {round}: who links to {id}"
            );

            // The code lens counts the other notes that link here.
            let lens = crate::handlers::codelens::code_lens(
                tower_lsp_server::ls_types::CodeLensParams {
                    text_document: TextDocumentIdentifier {
                        uri: uri_of_note(&id).parse().unwrap(),
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                },
                &state,
            )
            .and_then(|l| l.into_iter().next())
            .and_then(|l| l.command)
            .map(|c| c.title)
            .expect("a code lens for an open note");
            let counted: usize = lens.split(' ').next().unwrap().parse().unwrap();
            assert_eq!(
                counted,
                from_handlers.len(),
                "round {round}: code lens of {id}"
            );

            // `satz.showBacklinks` lists every link of those notes, and no other.
            let shown = crate::handlers::execute_command::show_backlinks(
                &state,
                &[serde_json::json!(uri_of_note(&id))],
            )
            .expect("a note that is open");
            assert_eq!(
                shown.len(),
                links_from_others.get(&id).copied().unwrap_or(0),
                "round {round}: backlinks listed for {id}"
            );
            assert_eq!(
                shown
                    .iter()
                    .map(|l| l.uri.as_str().to_string())
                    .collect::<BTreeSet<_>>(),
                from_handlers.iter().map(|n| uri_of_note(n)).collect(),
                "round {round}: the notes that backlinks are listed in for {id}"
            );
        }
        // References and highlights from a link that names a note (no anchor) count the links
        // that reach that note, everywhere and in this note.
        for doc in state.index.documents() {
            let uri = uri_of_note(doc.id.as_str());
            for link in &doc.links {
                if link.target_heading.is_some()
                    || link.target_block.is_some()
                    || link.target_doc.is_empty()
                    || link.kind == LinkKind::Footnote
                {
                    continue;
                }
                let Some(target) = state.link_target_doc(doc, link) else {
                    continue;
                };
                let at = cursor_in(doc, link);
                let position = TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: uri.parse().unwrap(),
                    },
                    position: at,
                };
                let refs = find_references(
                    ReferenceParams {
                        text_document_position: position.clone(),
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                        context: ReferenceContext {
                            include_declaration: false,
                        },
                    },
                    &state,
                )
                .map(|l| l.len());
                assert_eq!(
                    refs,
                    links_to.get(target.as_str()).copied(),
                    "round {round}: references from `{}` in {}",
                    &doc.line_index.source()[link.range.start..link.range.end],
                    doc.id
                );
                let highlights = document_highlight(
                    DocumentHighlightParams {
                        text_document_position_params: position,
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                    },
                    &state,
                )
                .map_or(0, |h| h.len());
                let here = doc
                    .links
                    .iter()
                    .filter(|l| state.link_target_doc(doc, l) == Some(target))
                    .count();
                assert_eq!(
                    highlights,
                    here,
                    "round {round}: highlights from `{}` in {}",
                    &doc.line_index.source()[link.range.start..link.range.end],
                    doc.id
                );
            }
        }
    }
    assert!(
        checked_links > 1500 && self_links > 30,
        "{checked_links} links, {self_links} self"
    );
}

#[test]
fn every_name_the_workspace_symbols_offer_is_a_name_a_link_can_use() {
    use tower_lsp_server::ls_types::{WorkspaceSymbolParams, WorkspaceSymbolResponse};
    let mut rng = Xorshift(0x6161_0000_0000_0002);
    let (mut names, mut headings, mut skipped) = (0, 0, 0);
    for round in 0..200 {
        let state = random_vault(&mut rng);
        let Some(WorkspaceSymbolResponse::Flat(symbols)) =
            crate::handlers::workspace_symbol::workspace_symbol(
                WorkspaceSymbolParams {
                    query: String::new(),
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                },
                &state,
            )
        else {
            panic!("symbols expected");
        };
        // The names a note answers to (title, aliases, file name), and who answers to each.
        let mut owners: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
            Default::default();
        for doc in state.index.documents() {
            let stem = doc
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            for name in std::iter::once(doc.title.clone())
                .chain(doc.frontmatter.aliases.iter().cloned())
                .chain(std::iter::once(stem))
            {
                owners
                    .entry(satz_core::fold_key(&name))
                    .or_default()
                    .insert(doc.id.as_str().to_string());
            }
        }
        for symbol in symbols {
            let Some(doc) = state
                .index
                .documents()
                .find(|d| uri_of_note(d.id.as_str()) == symbol.location.uri.as_str())
            else {
                panic!("round {round}: a symbol for no note: {symbol:?}");
            };
            // The kind says what the symbol is: the note by its title, an alias, a heading.
            let name = match symbol.kind {
                tower_lsp_server::ls_types::SymbolKind::FILE => Some(symbol.name.clone()),
                tower_lsp_server::ls_types::SymbolKind::KEY => {
                    symbol.name.strip_suffix(" (alias)").map(str::to_string)
                }
                _ => None,
            };
            if let Some(name) = name {
                // A name another note answers to as well is a clash, decided by its own rule.
                if owners[&satz_core::fold_key(&name)].len() != 1 {
                    skipped += 1;
                    continue;
                }
                names += 1;
                assert_eq!(
                    state.index.resolve_link(&name).map(|d| d.as_str()),
                    Some(doc.id.as_str()),
                    "round {round}: the name {name:?} of {}",
                    doc.id
                );
            } else {
                // A heading: `[[folder/note#heading]]` reaches it (the first of equal headings).
                headings += 1;
                let target = doc.id.as_str().trim_end_matches(".md").to_string();
                let link = satz_core::Link::new(
                    LinkKind::WikiLink,
                    target,
                    Some(symbol.name.clone()),
                    None,
                    None,
                    satz_core::ByteRange::new(0, 1),
                );
                assert!(
                    matches!(state.resolve(&link, doc), LinkResolution::Resolved { .. }),
                    "round {round}: the heading {:?} of {}",
                    symbol.name,
                    doc.id
                );
            }
        }
    }
    assert!(
        names > 400 && headings > 800,
        "{names} names, {headings} headings, {skipped} clashes"
    );
}
