use crate::state::SatzState;
use tower_lsp_server::ls_types::{CodeLens, CodeLensParams, Command, Position, Range};

/// Computes CodeLens entries for a document, displaying incoming backlink count.
pub fn code_lens(params: CodeLensParams, state: &SatzState) -> Option<Vec<CodeLens>> {
    if !state.config.lsp.codelens.enable {
        return None;
    }

    let uri = params.text_document.uri.as_str();
    tracing::debug!(uri, "code_lens");
    let open_doc = state.open_docs.get(uri)?;
    let rel_path =
        crate::state::SatzState::get_rel_path(&open_doc.path, state.vault_root.as_deref());
    let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");
    let doc_id = satz_core::DocId::new(&rel_path_str);

    let count = state.index.incoming_from_others(&doc_id).count();
    let title = match count {
        0 => "0 backlinks".to_string(),
        1 => "1 backlink".to_string(),
        n => format!("{} backlinks", n),
    };

    Some(vec![CodeLens {
        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
        command: Some(Command {
            title,
            command: crate::handlers::execute_command::SHOW_BACKLINKS_COMMAND.to_string(),
            arguments: Some(vec![serde_json::json!(uri)]),
        }),
        data: None,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use satz_core::{Index, VaultConfig, parse_document};
    use std::path::Path;
    use tower_lsp_server::ls_types::TextDocumentIdentifier;

    #[test]
    fn test_codelens_disabled_by_default() {
        let doc_a = parse_document("# Doc A\n\nContent", Path::new("doc-a.md"));
        let state = SatzState {
            index: Index::build(vec![doc_a]),
            vault_root: Some(Path::new("").to_path_buf()),
            ..Default::default()
        };

        let params = CodeLensParams {
            text_document: TextDocumentIdentifier {
                uri: "file:///doc-a.md".parse().unwrap(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        assert!(code_lens(params, &state).is_none());
    }

    #[test]
    fn test_codelens_enabled_with_backlinks() {
        let doc_a = parse_document("# Doc A", Path::new("doc-a.md"));
        let doc_b = parse_document("# Doc B\n\n[[doc-a]]", Path::new("doc-b.md"));
        let doc_c = parse_document("# Doc C\n\n[[doc-a]]", Path::new("doc-c.md"));

        let mut config = VaultConfig::default();
        config.lsp.codelens.enable = true;

        let uri_str = "file:///doc-a.md";
        let rel_a = Path::new("doc-a.md");

        let mut state = SatzState {
            index: Index::build(vec![doc_a, doc_b, doc_c]),
            vault_root: Some(Path::new("").to_path_buf()),
            config,
            ..Default::default()
        };
        state.open_docs.insert(
            uri_str.to_string(),
            crate::state::OpenDocument::new(uri_str, rel_a.to_path_buf(), "# Doc A", 1),
        );

        let params = CodeLensParams {
            text_document: TextDocumentIdentifier {
                uri: uri_str.parse().unwrap(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result = code_lens(params, &state).expect("CodeLens expected");
        assert_eq!(result.len(), 1);
        let lens = &result[0];
        assert_eq!(lens.range.start.line, 0);
        let cmd = lens.command.as_ref().unwrap();
        assert_eq!(cmd.title, "2 backlinks");
    }

    fn lens_title_for_a(files: &[(&str, &str)]) -> String {
        let mut config = VaultConfig::default();
        config.lsp.codelens.enable = true;
        let uri_str = "file:///doc-a.md";
        let rel_a = Path::new("doc-a.md");
        let text_a = files
            .iter()
            .find(|(p, _)| *p == "doc-a.md")
            .map(|(_, t)| *t)
            .unwrap();
        let mut state = SatzState {
            index: Index::build(
                files
                    .iter()
                    .map(|(p, t)| parse_document(t, Path::new(p)))
                    .collect(),
            ),
            vault_root: Some(Path::new("").to_path_buf()),
            config,
            ..Default::default()
        };
        state.open_docs.insert(
            uri_str.to_string(),
            crate::state::OpenDocument::new(uri_str, rel_a.to_path_buf(), text_a, 1),
        );
        let params = CodeLensParams {
            text_document: TextDocumentIdentifier {
                uri: uri_str.parse().unwrap(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let result = code_lens(params, &state).expect("CodeLens expected");
        result[0].command.as_ref().unwrap().title.clone()
    }

    #[test]
    fn the_lens_command_is_the_advertised_one_and_carries_the_note_uri() {
        let mut config = VaultConfig::default();
        config.lsp.codelens.enable = true;
        let mut state = SatzState {
            index: Index::build(vec![parse_document(
                "# A
",
                Path::new("doc-a.md"),
            )]),
            vault_root: Some(Path::new("").to_path_buf()),
            config,
            ..Default::default()
        };
        state.open_docs.insert(
            "file:///doc-a.md".to_string(),
            crate::state::OpenDocument::new(
                "file:///doc-a.md",
                Path::new("doc-a.md").to_path_buf(),
                "# A
",
                1,
            ),
        );
        let params = CodeLensParams {
            text_document: TextDocumentIdentifier {
                uri: "file:///doc-a.md".parse().unwrap(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let lens = code_lens(params, &state).unwrap();
        let command = lens[0].command.as_ref().unwrap();
        assert!(
            crate::handlers::execute_command::SUPPORTED_COMMANDS
                .contains(&command.command.as_str())
        );
        assert_eq!(
            command.arguments,
            Some(vec![serde_json::json!("file:///doc-a.md")])
        );
    }

    #[test]
    fn a_self_link_does_not_count_as_a_backlink() {
        assert_eq!(
            lens_title_for_a(&[("doc-a.md", "# A\n\n[[doc-a]] and [[#A]]\n")]),
            "0 backlinks"
        );
        assert_eq!(
            lens_title_for_a(&[
                ("doc-a.md", "# A\n\n[[doc-a]]\n"),
                ("doc-b.md", "[[doc-a]]\n"),
            ]),
            "1 backlink"
        );
    }

    #[test]
    fn a_note_that_links_later_is_counted_by_the_next_request() {
        let mut config = VaultConfig::default();
        config.lsp.codelens.enable = true;
        let uri = "file:///doc-a.md";
        let mut state = SatzState {
            index: Index::build(vec![
                parse_document("# A\n", Path::new("doc-a.md")),
                parse_document("# B\n", Path::new("doc-b.md")),
            ]),
            vault_root: Some(Path::new("").to_path_buf()),
            config,
            ..Default::default()
        };
        state.open_docs.insert(
            uri.to_string(),
            crate::state::OpenDocument::new(uri, Path::new("doc-a.md").to_path_buf(), "# A\n", 1),
        );
        let title = |state: &SatzState| {
            let params = CodeLensParams {
                text_document: TextDocumentIdentifier {
                    uri: uri.parse().unwrap(),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            };
            code_lens(params, state).unwrap()[0]
                .command
                .as_ref()
                .unwrap()
                .title
                .clone()
        };
        assert_eq!(title(&state), "0 backlinks");
        state
            .index
            .replace_doc(parse_document("# B\n\n[[doc-a]]\n", Path::new("doc-b.md")));
        assert_eq!(title(&state), "1 backlink");
        state
            .index
            .replace_doc(parse_document("# B\n", Path::new("doc-b.md")));
        assert_eq!(title(&state), "0 backlinks");
    }
}
