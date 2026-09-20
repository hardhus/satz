use tower_lsp_server::ls_types::{
    Location, Position, Range, SymbolInformation, SymbolKind, WorkspaceSymbolParams,
    WorkspaceSymbolResponse,
};

use crate::convert::{byte_range_to_lsp, path_to_uri};
use crate::rank::Ranker;
use crate::state::SatzState;

pub fn workspace_symbol(
    params: WorkspaceSymbolParams,
    state: &SatzState,
) -> Option<WorkspaceSymbolResponse> {
    let raw_query = params.query.trim();
    tracing::debug!(query = raw_query, "workspace_symbol");

    // Check for `tag:tagname query` prefix
    let (tag_filter, search_query) = if let Some(rest) = raw_query.strip_prefix("tag:") {
        if let Some((tag, rest_q)) = rest.split_once(' ') {
            (Some(tag.trim()), rest_q.trim())
        } else {
            (Some(rest.trim()), "")
        }
    } else {
        (None, raw_query)
    };

    let mut candidate_docs: Vec<&satz_core::Document> = if let Some(tag) = tag_filter {
        state.index.docs_with_tag(tag).collect()
    } else {
        state.index.documents().collect()
    };

    // Sort documents by path for stable ordering
    candidate_docs.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));

    let mut ranker = Ranker::new(search_query);
    let mut scored_symbols: Vec<(u32, SymbolInformation)> = Vec::new();

    for doc in candidate_docs {
        let doc_path = match state.vault_root() {
            Some(root) if !doc.path.is_absolute() => root.join(&doc.path),
            _ => doc.path.clone(),
        };
        let doc_uri = match path_to_uri(&doc_path) {
            Some(u) => u,
            None => continue,
        };

        let default_location = Location::new(
            doc_uri.clone(),
            Range::new(Position::new(0, 0), Position::new(0, 0)),
        );

        // 1. Match Doc Title
        let title_name = if !doc.title.is_empty() {
            doc.title.clone()
        } else {
            doc.id.as_str().to_string()
        };

        if let Some(score) = ranker.score(&title_name) {
            // The LSP type marks this field `#[deprecated]` but the protocol still requires it.
            #[allow(deprecated)]
            scored_symbols.push((
                score,
                SymbolInformation {
                    name: title_name,
                    kind: SymbolKind::FILE,
                    tags: None,
                    deprecated: None,
                    location: default_location.clone(),
                    container_name: None,
                },
            ));
        }

        // 2. Match Doc Aliases
        for alias in &doc.frontmatter.aliases {
            if let Some(score) = ranker.score(alias) {
                // The LSP type marks this field `#[deprecated]` but the protocol still requires it.
                #[allow(deprecated)]
                scored_symbols.push((
                    score,
                    SymbolInformation {
                        name: format!("{} (alias)", alias),
                        kind: SymbolKind::KEY,
                        tags: None,
                        deprecated: None,
                        location: default_location.clone(),
                        container_name: Some(doc.title.clone()),
                    },
                ));
            }
        }

        // 3. Match Headings
        for heading in &doc.headings {
            let heading_text = heading.text.trim();
            // The note's own top heading is already listed as the note (its title).
            if heading.level == 1 && heading_text == doc.title {
                continue;
            }
            if let Some(score) = ranker.score(heading_text) {
                let range = byte_range_to_lsp(heading.range, &doc.line_index);
                // The LSP type marks this field `#[deprecated]` but the protocol still requires it.
                #[allow(deprecated)]
                scored_symbols.push((
                    score,
                    SymbolInformation {
                        name: heading_text.to_string(),
                        kind: SymbolKind::STRING,
                        tags: None,
                        deprecated: None,
                        location: Location::new(doc_uri.clone(), range),
                        container_name: Some(doc.title.clone()),
                    },
                ));
            }
        }
    }

    // Sort by match score descending
    scored_symbols.sort_by_key(|a| std::cmp::Reverse(a.0));

    let results: Vec<SymbolInformation> = scored_symbols
        .into_iter()
        .take(100)
        .map(|(_, sym)| sym)
        .collect();

    Some(WorkspaceSymbolResponse::Flat(results))
}

#[cfg(test)]
// Test states are built field by field so each test shows exactly what it sets up.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use satz_core::{Index, parse_document};
    use std::path::Path;

    #[test]
    fn test_workspace_symbol_search() {
        let rel_a = Path::new("doc-a.md");
        let rel_b = Path::new("doc-b.md");
        let doc_a = parse_document(
            "---\ntags: [philosophy]\n---\n# Wittgenstein Tractatus",
            rel_a,
        );
        let doc_b = parse_document("# Rust Programming\n## Ownership and Lifetimes", rel_b);

        let mut state = SatzState::default();
        state.index = Index::build(vec![doc_a, doc_b]);
        state.set_vault_root(Some(if cfg!(windows) {
            Path::new("C:\\").to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        }));

        // 1. General search for "Wittgen"
        let params = WorkspaceSymbolParams {
            query: "Wittgen".to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let response = workspace_symbol(params, &state).expect("Response expected");
        if let WorkspaceSymbolResponse::Flat(symbols) = response {
            assert!(symbols.iter().any(|s| s.name.contains("Wittgenstein")));
        } else {
            panic!("Expected flat symbols response");
        }

        // 2. Tag filtered search for "tag:philosophy Tract"
        let params_tag = WorkspaceSymbolParams {
            query: "tag:philosophy Tract".to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let response_tag = workspace_symbol(params_tag, &state).expect("Response expected");
        if let WorkspaceSymbolResponse::Flat(symbols) = response_tag {
            // The note itself; its H1 has the same text and is not listed a second time.
            assert_eq!(symbols.len(), 1);
            assert!(symbols.iter().all(|s| s.name.contains("Wittgenstein")));
        } else {
            panic!("Expected flat symbols response");
        }
    }

    fn symbols_for(files: &[(&str, &str)], query: &str) -> Vec<(String, SymbolKind)> {
        let mut state = SatzState::default();
        state.index = Index::build(
            files
                .iter()
                .map(|(path, text)| parse_document(text, Path::new(path)))
                .collect(),
        );
        state.set_vault_root(Some(if cfg!(windows) {
            Path::new("C:\\").to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        }));
        let params = WorkspaceSymbolParams {
            query: query.to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let Some(WorkspaceSymbolResponse::Flat(symbols)) = workspace_symbol(params, &state) else {
            return Vec::new();
        };
        let mut found: Vec<(String, SymbolKind)> =
            symbols.into_iter().map(|s| (s.name, s.kind)).collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found
    }

    #[test]
    fn a_notes_own_h1_is_not_listed_twice() {
        let found = symbols_for(&[("a.md", "# Alpha Doc\n\n## Sub One\n")], "");
        assert_eq!(
            found,
            vec![
                ("Alpha Doc".to_string(), SymbolKind::FILE),
                ("Sub One".to_string(), SymbolKind::STRING),
            ]
        );
    }

    #[test]
    fn a_heading_that_differs_from_the_title_is_still_listed() {
        let found = symbols_for(
            &[("a.md", "---\ntitle: Front Title\n---\n# Heading One\n")],
            "",
        );
        assert_eq!(
            found,
            vec![
                ("Front Title".to_string(), SymbolKind::FILE),
                ("Heading One".to_string(), SymbolKind::STRING),
            ]
        );
    }

    #[test]
    fn aliases_have_their_own_symbol_kind() {
        let found = symbols_for(
            &[("a.md", "---\naliases: [short]\n---\n# Alpha\n")],
            "short",
        );
        assert_eq!(found, vec![("short (alias)".to_string(), SymbolKind::KEY)]);
    }

    #[test]
    fn a_note_without_any_title_is_listed_by_its_file_name() {
        let found = symbols_for(&[("plain-note.md", "just some text\n")], "plain");
        assert_eq!(found, vec![("plain-note".to_string(), SymbolKind::FILE)]);
    }

    #[test]
    fn turkish_capital_i_headings_are_found_with_a_lowercase_query() {
        let found = symbols_for(&[("a.md", "# İş Notları\n\n## İstanbul planı\n")], "iş");
        assert!(found.iter().any(|(n, _)| n == "İş Notları"), "{found:?}");
        let found = symbols_for(&[("a.md", "# X\n\n## İstanbul planı\n")], "istanbul");
        assert!(
            found.iter().any(|(n, _)| n == "İstanbul planı"),
            "{found:?}"
        );
    }
}
