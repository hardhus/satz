// Test states are built field by field so each test shows exactly what it sets up.
#![allow(clippy::field_reassign_with_default)]

use super::*;
use satz_core::{Index, parse_document};
use std::path::Path;
use tower_lsp_server::ls_types::{
    CodeActionContext, CreateFileOptions, Position, TextDocumentIdentifier,
};
use tower_lsp_server::ls_types::{Diagnostic, NumberOrString};

#[test]
fn test_code_action_create_missing_note() {
    let abs_a = if cfg!(windows) {
        Path::new("C:\\doc-a.md")
    } else {
        Path::new("/doc-a.md")
    };
    let rel_a = Path::new("doc-a.md");

    let doc_a = parse_document(
        "---\ntitle: Doc A\n---\n\nLink to [[missing-note]] here",
        rel_a,
    );

    let mut state = SatzState::default();
    state.index = Index::build(vec![doc_a]);
    state.set_vault_root(Some(if cfg!(windows) {
        Path::new("C:\\").to_path_buf()
    } else {
        Path::new("/").to_path_buf()
    }));

    let uri_a_str = if cfg!(windows) {
        "file:///C:/doc-a.md"
    } else {
        "file:///doc-a.md"
    };

    state.open_docs.insert(
        uri_a_str.to_string(),
        crate::state::OpenDocument::new(
            uri_a_str,
            abs_a.to_path_buf(),
            "---\ntitle: Doc A\n---\n\nLink to [[missing-note]] here",
            1,
        ),
    );

    // Selection range covering the link partially
    let params = CodeActionParams {
        text_document: TextDocumentIdentifier {
            uri: uri_a_str.parse().unwrap(),
        },
        range: Range::new(Position::new(4, 5), Position::new(4, 20)),
        context: CodeActionContext::default(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let response = code_action(params, &state).expect("CodeAction response expected");
    // One quickfix for the broken link, plus the always-offered "Format entire vault" source
    // action.
    assert_eq!(response.len(), 2);

    let quickfix = response
        .iter()
        .find_map(|a| match a {
            CodeActionOrCommand::CodeAction(ca) if ca.kind == Some(CodeActionKind::QUICKFIX) => {
                Some(ca)
            }
            _ => None,
        })
        .expect("quickfix action expected");
    assert!(quickfix.title.contains("Create note: \"missing-note\""));
    assert!(quickfix.edit.is_some());
    assert_source_action_present(&response);
}

// ---- create-note: which files may be created, and where ----

fn vault_root() -> std::path::PathBuf {
    if cfg!(windows) {
        Path::new("C:\\vault").to_path_buf()
    } else {
        Path::new("/vault").to_path_buf()
    }
}

/// The "Create note" quickfix offered for a broken `[[target]]`, if any.
fn create_note_action(target: &str) -> Option<CodeAction> {
    let text = format!("Link to [[{target}]] here");
    let rel_a = Path::new("doc-a.md");
    let mut state = SatzState::default();
    state.index = Index::build(vec![parse_document(&text, rel_a)]);
    state.set_vault_root(Some(vault_root()));
    let (uri_str, abs) = if cfg!(windows) {
        ("file:///C:/vault/doc-a.md", "C:\\vault\\doc-a.md")
    } else {
        ("file:///vault/doc-a.md", "/vault/doc-a.md")
    };
    state.open_docs.insert(
        uri_str.to_string(),
        crate::state::OpenDocument::new(uri_str, Path::new(abs).to_path_buf(), &text, 1),
    );
    let params = CodeActionParams {
        text_document: TextDocumentIdentifier {
            uri: uri_str.parse().unwrap(),
        },
        range: Range::new(Position::new(0, 10), Position::new(0, 10)),
        context: CodeActionContext::default(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    code_action(params, &state)?
        .into_iter()
        .find_map(|a| match a {
            CodeActionOrCommand::CodeAction(ca)
                if ca.kind == Some(CodeActionKind::QUICKFIX)
                    && ca.title.starts_with("Create note") =>
            {
                Some(ca)
            }
            _ => None,
        })
}

/// `(created file path relative to the vault, CreateFile options, new file content)`.
fn created_file(action: &CodeAction) -> (String, Option<CreateFileOptions>, String) {
    let Some(DocumentChanges::Operations(ops)) = &action.edit.as_ref().unwrap().document_changes
    else {
        panic!("expected document change operations");
    };
    let mut create = None;
    let mut content = String::new();
    for op in ops {
        match op {
            DocumentChangeOperation::Op(ResourceOp::Create(c)) => create = Some(c),
            DocumentChangeOperation::Edit(e) => {
                if let Some(OneOf::Left(edit)) = e.edits.first() {
                    content = edit.new_text.clone();
                }
            }
            _ => {}
        }
    }
    let create = create.expect("a CreateFile operation");
    let path = crate::convert::uri_to_path(create.uri.as_str()).expect("file URI");
    let rel = path
        .strip_prefix(vault_root())
        .unwrap_or_else(|_| panic!("{path:?} is outside the vault {:?}", vault_root()))
        .to_string_lossy()
        .replace('\\', "/");
    (rel, create.options.clone(), content)
}

#[test]
fn create_note_puts_the_file_where_the_link_says_inside_the_vault() {
    for (target, expected) in [
        ("new", "new.md"),
        ("a/b/c", "a/b/c.md"),
        ("tlp/2.0121", "tlp/2.0121.md"),
        ("v1.2.3", "v1.2.3.md"),
        ("Türkçe Not", "Türkçe Not.md"),
        ("with.md", "with.md"),
        ("sub\\name", "sub/name.md"),
        ("a//b", "a/b.md"),
        ("/abs", "abs.md"),
        ("note with   spaces", "note with   spaces.md"),
    ] {
        let action = create_note_action(target)
            .unwrap_or_else(|| panic!("{target:?} should offer a create-note fix"));
        let (rel, _, _) = created_file(&action);
        assert_eq!(rel, expected, "target {target:?}");
    }
}

#[test]
fn create_note_is_not_offered_for_targets_that_are_not_safe_note_names() {
    for target in [
        // escaping the vault
        "../../x",
        "a/../../x",
        "..",
        ".",
        "./x",
        "..\\..\\x",
        // drive letters / URL schemes / stream names
        "C:x",
        "C:\\x",
        "a:b",
        "mailto:a@b.c",
        // not a note: has a file extension
        "image.png",
        "doc.pdf",
        "archive.tar.gz",
        "photo.JPG",
        // characters Windows forbids in file names
        "con*",
        "a?b",
        "a<b",
        "a>b",
        "quote\"",
        // trailing dot
        "trailing.",
        "dir./name",
    ] {
        assert!(
            create_note_action(target).is_none(),
            "{target:?} must not offer to create a file"
        );
    }
    // Over-long names.
    assert!(create_note_action(&"a".repeat(256)).is_none());
    assert!(create_note_action(&"a".repeat(255)).is_some());
}

#[test]
fn create_note_never_overwrites_and_tolerates_an_existing_unindexed_file() {
    let action = create_note_action("new").unwrap();
    let (_, options, _) = created_file(&action);
    let options = options.expect("explicit CreateFile options");
    assert_eq!(options.overwrite, Some(false));
    assert_eq!(options.ignore_if_exists, Some(true));
}

#[test]
fn the_created_note_has_valid_frontmatter_even_for_awkward_names() {
    for (target, title) in [
        ("new", "new"),
        ("Q- what", "Q- what"),
        ("2.01231", "2.01231"),
        ("yes", "yes"),
        ("Türkçe Not", "Türkçe Not"),
        ("a/b/c", "c"),
    ] {
        let action = create_note_action(target).unwrap();
        let (_, _, content) = created_file(&action);
        let parsed = parse_document(&content, Path::new("n.md"));
        assert!(parsed.frontmatter_range.is_some(), "{target:?}:\n{content}");
        assert_eq!(
            parsed.frontmatter.title.as_deref(),
            Some(title),
            "{target:?}:\n{content}"
        );
        assert!(parsed.frontmatter.aliases.is_empty(), "{target:?}");
    }
}

#[test]
fn test_code_action_insert_frontmatter_template() {
    let rel_a = Path::new("doc-a.md");
    let doc_a = parse_document("# Doc A Without Frontmatter", rel_a);

    let mut state = SatzState::default();
    state.index = Index::build(vec![doc_a]);
    state.set_vault_root(Some(if cfg!(windows) {
        Path::new("C:\\").to_path_buf()
    } else {
        Path::new("/").to_path_buf()
    }));

    let uri_a_str = if cfg!(windows) {
        "file:///C:/doc-a.md"
    } else {
        "file:///doc-a.md"
    };

    state.open_docs.insert(
        uri_a_str.to_string(),
        crate::state::OpenDocument::new(
            uri_a_str,
            Path::new(if cfg!(windows) {
                "C:\\doc-a.md"
            } else {
                "/doc-a.md"
            })
            .to_path_buf(),
            "# Doc A Without Frontmatter",
            1,
        ),
    );

    let params = CodeActionParams {
        text_document: TextDocumentIdentifier {
            uri: uri_a_str.parse().unwrap(),
        },
        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
        context: CodeActionContext::default(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let response = code_action(params, &state).expect("CodeAction response expected");
    assert!(
            response
                .iter()
                .any(|a| matches!(a, CodeActionOrCommand::CodeAction(ca) if ca.title == "Insert frontmatter template"))
        );
}

#[test]
fn test_code_action_add_missing_heading() {
    let rel_a = Path::new("doc-a.md");
    let rel_b = Path::new("doc-b.md");

    let doc_a = parse_document(
        "---\ntitle: Doc A\n---\n\nLink to [[doc-b#İstemciler]] here",
        rel_a,
    );
    let doc_b = parse_document("---\ntitle: Doc B\n---\n\nExisting text.", rel_b);

    let mut state = SatzState::default();
    state.index = Index::build(vec![doc_a, doc_b]);
    state.set_vault_root(Some(if cfg!(windows) {
        Path::new("C:\\").to_path_buf()
    } else {
        Path::new("/").to_path_buf()
    }));

    let uri_a_str = if cfg!(windows) {
        "file:///C:/doc-a.md"
    } else {
        "file:///doc-a.md"
    };

    state.open_docs.insert(
        uri_a_str.to_string(),
        crate::state::OpenDocument::new(
            uri_a_str,
            Path::new(if cfg!(windows) {
                "C:\\doc-a.md"
            } else {
                "/doc-a.md"
            })
            .to_path_buf(),
            "---\ntitle: Doc A\n---\n\nLink to [[doc-b#İstemciler]] here",
            1,
        ),
    );

    let params = CodeActionParams {
        text_document: TextDocumentIdentifier {
            uri: uri_a_str.parse().unwrap(),
        },
        range: Range::new(Position::new(4, 12), Position::new(4, 12)),
        context: CodeActionContext::default(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let response = code_action(params, &state).expect("CodeAction response expected");
    // One quickfix for the missing heading, plus the always-offered "Format entire vault"
    // source action.
    assert_eq!(response.len(), 2);

    let quickfix = response
        .iter()
        .find_map(|a| match a {
            CodeActionOrCommand::CodeAction(ca) if ca.kind == Some(CodeActionKind::QUICKFIX) => {
                Some(ca)
            }
            _ => None,
        })
        .expect("quickfix action expected");
    assert!(
        quickfix
            .title
            .contains("Add heading '## İstemciler' to \"Doc B\"")
    );
    assert!(quickfix.edit.is_some());
    assert_source_action_present(&response);
}

/// `a.md` (open, links to `b#Missing`) and `b.md`, optionally open at `b_version`.
fn heading_fix_edit(b_text: &str, b_open_version: Option<i32>) -> DocumentChanges {
    let text_a = "# A

[[b#Missing]]
";
    let mut state = SatzState::default();
    state.index = Index::build(vec![
        parse_document(text_a, Path::new("a.md")),
        parse_document(b_text, Path::new("b.md")),
    ]);
    let root = if cfg!(windows) { "C:\\" } else { "/" };
    state.set_vault_root(Some(Path::new(root).to_path_buf()));
    let uri_a = if cfg!(windows) {
        "file:///C:/a.md"
    } else {
        "file:///a.md"
    };
    let uri_b = if cfg!(windows) {
        "file:///C:/b.md"
    } else {
        "file:///b.md"
    };
    state.open_document(uri_a, text_a, &Path::new(root).join("a.md"), 1);
    if let Some(version) = b_open_version {
        state.open_document(uri_b, b_text, &Path::new(root).join("b.md"), version);
    }
    let response = code_action(
        CodeActionParams {
            text_document: TextDocumentIdentifier {
                uri: uri_a.parse().unwrap(),
            },
            range: Range::new(Position::new(2, 4), Position::new(2, 4)),
            context: CodeActionContext::default(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
        &state,
    )
    .expect("actions");
    response
        .into_iter()
        .find_map(|a| match a {
            CodeActionOrCommand::CodeAction(ca) if ca.title.starts_with("Add heading") => {
                ca.edit.and_then(|e| e.document_changes)
            }
            _ => None,
        })
        .expect("an add-heading fix with document edits")
}

fn only_edit(changes: DocumentChanges) -> TextDocumentEdit {
    let DocumentChanges::Edits(mut edits) = changes else {
        panic!("plain document edits expected");
    };
    assert_eq!(edits.len(), 1);
    edits.remove(0)
}

#[test]
fn the_added_heading_names_the_open_targets_version_and_lands_at_its_end() {
    let b = "# B

body
";
    let edit = only_edit(heading_fix_edit(b, Some(7)));
    assert_eq!(edit.text_document.version, Some(7));
    let edits: Vec<TextEdit> = edit
        .edits
        .into_iter()
        .map(|e| match e {
            OneOf::Left(t) => t,
            OneOf::Right(a) => a.text_edit,
        })
        .collect();
    assert_eq!(
        crate::convert::apply_text_edits(b, &edits),
        "# B

body

## Missing
"
    );
}

#[test]
fn the_added_heading_of_a_closed_target_carries_no_version() {
    let b = "# B

no trailing newline";
    let edit = only_edit(heading_fix_edit(b, None));
    assert_eq!(edit.text_document.version, None);
    let edits: Vec<TextEdit> = edit
        .edits
        .into_iter()
        .map(|e| match e {
            OneOf::Left(t) => t,
            OneOf::Right(a) => a.text_edit,
        })
        .collect();
    assert_eq!(
        crate::convert::apply_text_edits(b, &edits),
        "# B

no trailing newline

## Missing
"
    );
}

fn assert_source_action_present(response: &[CodeActionOrCommand]) {
    let source_action = response
        .iter()
        .find_map(|a| match a {
            CodeActionOrCommand::CodeAction(ca) if ca.kind == Some(CodeActionKind::SOURCE) => {
                Some(ca)
            }
            _ => None,
        })
        .expect("'Format entire vault' source action expected");
    assert_eq!(source_action.title, "Format entire vault");
    let command = source_action
        .command
        .as_ref()
        .expect("source action should carry a Command");
    assert_eq!(
        command.command,
        crate::handlers::execute_command::FORMAT_WORKSPACE_COMMAND
    );
}

#[test]
fn format_entire_vault_is_offered_only_while_the_config_is_valid() {
    let rel_a = Path::new("doc-a.md");
    let text = "---\ntitle: Doc A\n---\n\nPlain content, no links.";
    let doc_a = parse_document(text, rel_a);
    let uri_a_str = if cfg!(windows) {
        "file:///C:/doc-a.md"
    } else {
        "file:///doc-a.md"
    };
    let abs_a = Path::new(if cfg!(windows) {
        "C:\\doc-a.md"
    } else {
        "/doc-a.md"
    });

    let mut state = SatzState::default();
    state.index = Index::build(vec![doc_a]);
    state.set_vault_root(Some(if cfg!(windows) {
        Path::new("C:\\").to_path_buf()
    } else {
        Path::new("/").to_path_buf()
    }));
    state.open_docs.insert(
        uri_a_str.to_string(),
        crate::state::OpenDocument::new(uri_a_str, abs_a.to_path_buf(), text, 1),
    );
    let params = || CodeActionParams {
        text_document: TextDocumentIdentifier {
            uri: uri_a_str.parse().unwrap(),
        },
        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
        context: CodeActionContext::default(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    // Control: valid config -> the source action is offered.
    let response = code_action(params(), &state).expect("source action expected");
    assert_source_action_present(&response);

    // Invalid config -> nothing is offered (no quickfix applies to this document either).
    state.config_error = Some("invalid .satz.toml: line 1".to_string());
    assert!(code_action(params(), &state).is_none());

    // Fixed -> offered again.
    state.config_error = None;
    let response = code_action(params(), &state).expect("source action expected");
    assert_source_action_present(&response);
}

#[test]
fn test_source_action_absent_when_formatter_disabled() {
    let rel_a = Path::new("doc-a.md");
    // Has frontmatter already and no links, so no quickfix action applies — isolates whether
    // the source action alone appears.
    let doc_a = parse_document("---\ntitle: Doc A\n---\n\nPlain content, no links.", rel_a);

    let mut state = SatzState::default();
    state.config.formatter.enabled = false;
    state.index = Index::build(vec![doc_a]);
    state.set_vault_root(Some(if cfg!(windows) {
        Path::new("C:\\").to_path_buf()
    } else {
        Path::new("/").to_path_buf()
    }));

    let uri_a_str = if cfg!(windows) {
        "file:///C:/doc-a.md"
    } else {
        "file:///doc-a.md"
    };

    state.open_docs.insert(
        uri_a_str.to_string(),
        crate::state::OpenDocument::new(
            uri_a_str,
            Path::new(if cfg!(windows) {
                "C:\\doc-a.md"
            } else {
                "/doc-a.md"
            })
            .to_path_buf(),
            "---\ntitle: Doc A\n---\n\nPlain content, no links.",
            1,
        ),
    );

    let params = CodeActionParams {
        text_document: TextDocumentIdentifier {
            uri: uri_a_str.parse().unwrap(),
        },
        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
        context: CodeActionContext::default(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    // No quickfix applies here, so with the formatter disabled there should be nothing
    // offered at all.
    assert!(code_action(params, &state).is_none());
}

// ---- quickfixes name the diagnostic they fix; the frontmatter offer follows the document ----

fn diagnostic(code: &str, line: u32, start: u32, end: u32) -> Diagnostic {
    Diagnostic {
        range: Range::new(Position::new(line, start), Position::new(line, end)),
        code: Some(NumberOrString::String(code.to_string())),
        message: code.to_string(),
        ..Default::default()
    }
}

fn actions_for(text: &str, cursor: Position, diagnostics: Vec<Diagnostic>) -> Vec<CodeAction> {
    let mut state = SatzState::default();
    state.index = Index::build(vec![parse_document(text, Path::new("a.md"))]);
    let root = if cfg!(windows) { "C:\\" } else { "/" };
    state.set_vault_root(Some(Path::new(root).to_path_buf()));
    let uri = if cfg!(windows) {
        "file:///C:/a.md"
    } else {
        "file:///a.md"
    };
    state.open_document(uri, text, &Path::new(root).join("a.md"), 1);
    code_action(
        CodeActionParams {
            text_document: TextDocumentIdentifier {
                uri: uri.parse().unwrap(),
            },
            range: Range::new(cursor, cursor),
            context: CodeActionContext {
                diagnostics,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
        &state,
    )
    .unwrap_or_default()
    .into_iter()
    .filter_map(|a| match a {
        CodeActionOrCommand::CodeAction(ca) => Some(ca),
        _ => None,
    })
    .collect()
}

const WITH_FM: &str = "---
title: A
---

[[missing]] and [[b]]
";

#[test]
fn the_create_note_fix_names_the_broken_link_diagnostic_under_it() {
    let broken = diagnostic("broken-link", 4, 0, 11);
    let other_line = diagnostic("broken-link", 0, 0, 3);
    let orphan = diagnostic("orphan-note", 4, 0, 11);
    let actions = actions_for(
        WITH_FM,
        Position::new(4, 3),
        vec![broken.clone(), other_line, orphan],
    );
    let create = actions
        .iter()
        .find(|a| a.title.starts_with("Create note"))
        .unwrap();
    assert_eq!(create.diagnostics, Some(vec![broken]));
}

#[test]
fn a_fix_without_a_matching_diagnostic_carries_none() {
    let actions = actions_for(WITH_FM, Position::new(4, 3), vec![]);
    let create = actions
        .iter()
        .find(|a| a.title.starts_with("Create note"))
        .unwrap();
    assert_eq!(create.diagnostics, None);
}

#[test]
fn a_note_that_has_frontmatter_is_not_offered_a_template_at_any_cursor_position() {
    for line in 0..5 {
        let actions = actions_for(WITH_FM, Position::new(line, 0), vec![]);
        assert!(
            !actions
                .iter()
                .any(|a| a.title == "Insert frontmatter template"),
            "line {line}"
        );
    }
}

#[test]
fn a_note_without_frontmatter_is_offered_one_and_an_unclosed_block_is_left_alone() {
    let plain = actions_for(
        "# A

text
",
        Position::new(2, 0),
        vec![],
    );
    assert!(
        plain
            .iter()
            .any(|a| a.title == "Insert frontmatter template")
    );
    let unclosed = actions_for(
        "---
title: A

text
",
        Position::new(3, 0),
        vec![],
    );
    assert!(
        !unclosed
            .iter()
            .any(|a| a.title == "Insert frontmatter template")
    );
}
