#![allow(clippy::collapsible_if)]

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
        let doc_path = match &state.vault_root {
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
        let title_name = if !doc.title.is_empty() && doc.title != "Untitled" {
            doc.title.clone()
        } else {
            doc.id.as_str().to_string()
        };

        if let Some(score) = ranker.score(&title_name) {
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
                #[allow(deprecated)]
                scored_symbols.push((
                    score,
                    SymbolInformation {
                        name: format!("{} (alias)", alias),
                        kind: SymbolKind::NULL,
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
            if let Some(score) = ranker.score(heading_text) {
                let range = byte_range_to_lsp(heading.range, &doc.line_index);
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
#[allow(unused_variables)]
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
        state.vault_root = Some(if cfg!(windows) {
            Path::new("C:\\").to_path_buf()
        } else {
            Path::new("/").to_path_buf()
        });

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
            assert_eq!(symbols.len(), 2);
            assert!(symbols.iter().all(|s| s.name.contains("Wittgenstein")));
        } else {
            panic!("Expected flat symbols response");
        }
    }
}
