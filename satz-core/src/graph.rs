use std::collections::HashMap;

use petgraph::graph::{DiGraph, NodeIndex};
use serde::{Deserialize, Serialize};

use crate::index::Index;
use crate::model::{DocId, LinkKind};

/// A node in the vault graph representing a document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphNode {
    pub id: String,
    pub title: String,
    pub path: String,
    pub tags: Vec<String>,
}

/// An edge in the vault graph representing a directional link between documents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub kind: String,
    pub label: Option<String>,
}

/// Structured export of vault graph nodes and edges for JSON serialization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphData {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// Vault graph model with petgraph integration.
#[derive(Debug, Clone)]
pub struct VaultGraph {
    pub inner: DiGraph<GraphNode, GraphEdge>,
    pub node_indices: HashMap<DocId, NodeIndex>,
}

impl VaultGraph {
    /// Builds a `VaultGraph` from the given `Index`.
    pub fn build(index: &Index) -> Self {
        let mut graph = DiGraph::new();
        let mut node_indices = HashMap::new();

        // 1. Add all document nodes
        for doc in index.documents() {
            let tags = doc.tags.iter().map(|t| t.name.clone()).collect();
            let node = GraphNode {
                id: doc.id.as_str().to_string(),
                title: doc.title.clone(),
                path: doc.path.to_string_lossy().replace('\\', "/"),
                tags,
            };
            let idx = graph.add_node(node);
            node_indices.insert(doc.id.clone(), idx);
        }

        // 2. Add edges for links between documents
        for doc in index.documents() {
            let Some(&src_idx) = node_indices.get(&doc.id) else {
                continue;
            };

            for link in &doc.links {
                if link.target_doc.starts_with("http://") || link.target_doc.starts_with("https://")
                {
                    continue;
                }

                let target_id = if link.target_doc.is_empty() {
                    Some(&doc.id)
                } else {
                    index.resolve_link(&link.target_doc)
                };

                if let Some(target_id) = target_id
                    && let Some(&tgt_idx) = node_indices.get(target_id)
                {
                    let kind_str = match link.kind {
                        LinkKind::WikiLink => "wikilink",
                        LinkKind::Embed => "embed",
                        LinkKind::Markdown => "markdown",
                        LinkKind::Footnote => "footnote",
                    };

                    let label = link
                        .target_heading
                        .clone()
                        .or_else(|| link.target_block.clone());

                    let edge = GraphEdge {
                        source: doc.id.as_str().to_string(),
                        target: target_id.as_str().to_string(),
                        kind: kind_str.to_string(),
                        label,
                    };

                    graph.add_edge(src_idx, tgt_idx, edge);
                }
            }
        }

        Self {
            inner: graph,
            node_indices,
        }
    }

    /// Number of nodes (documents) in the graph.
    pub fn node_count(&self) -> usize {
        self.inner.node_count()
    }

    /// Number of edges (links) in the graph.
    pub fn edge_count(&self) -> usize {
        self.inner.edge_count()
    }

    /// Converts the graph to a structured `GraphData` representation.
    pub fn to_data(&self) -> GraphData {
        let mut nodes = Vec::with_capacity(self.inner.node_count());
        for idx in self.inner.node_indices() {
            nodes.push(self.inner[idx].clone());
        }

        let mut edges = Vec::with_capacity(self.inner.edge_count());
        for edge in self.inner.edge_weights() {
            edges.push(edge.clone());
        }

        GraphData { nodes, edges }
    }

    /// Exports the graph to a JSON string.
    pub fn export_json(&self) -> Result<String, serde_json::Error> {
        let data = self.to_data();
        serde_json::to_string_pretty(&data)
    }

    /// Exports the graph to Graphviz DOT format.
    pub fn export_dot(&self) -> String {
        let mut dot = String::new();
        dot.push_str("digraph \"satz\" {\n");
        dot.push_str("    rankdir=LR;\n");
        dot.push_str("    node [shape=box, style=\"rounded,filled\", fillcolor=\"#f9f9f9\", fontname=\"Helvetica\"];\n");
        dot.push_str("    edge [fontname=\"Helvetica\", fontsize=10];\n\n");

        for idx in self.inner.node_indices() {
            let node = &self.inner[idx];
            let label = if node.title.is_empty() || node.title == node.id {
                escape_dot_string(&node.id)
            } else {
                format!(
                    "{}\\n({})",
                    escape_dot_string(&node.title),
                    escape_dot_string(&node.id)
                )
            };
            dot.push_str(&format!(
                "    \"{}\" [label=\"{}\"];\n",
                escape_dot_string(&node.id),
                label
            ));
        }

        dot.push('\n');

        for edge in self.inner.edge_weights() {
            let label_attr = if let Some(lbl) = &edge.label {
                format!(" [label=\"{}\"]", escape_dot_string(lbl))
            } else {
                String::new()
            };
            dot.push_str(&format!(
                "    \"{}\" -> \"{}\"{};\n",
                escape_dot_string(&edge.source),
                escape_dot_string(&edge.target),
                label_attr
            ));
        }

        dot.push_str("}\n");
        dot
    }
}

fn escape_dot_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::parser::parse_document;

    #[test]
    fn test_build_vault_graph() {
        let doc_a_src = "# Note A\nLinks to [[Note B]] and [[Note B#Sec]].";
        let doc_b_src = "# Note B\nLinks to [[Note C]].";
        let doc_c_src = "# Note C\nEnd.";

        let doc_a = parse_document(doc_a_src, Path::new("note-a.md"));
        let doc_b = parse_document(doc_b_src, Path::new("note-b.md"));
        let doc_c = parse_document(doc_c_src, Path::new("note-c.md"));

        let index = Index::build(vec![doc_a, doc_b, doc_c]);
        let graph = VaultGraph::build(&index);

        assert_eq!(graph.node_count(), 3);
        assert_eq!(graph.edge_count(), 3);

        // JSON export check
        let json = graph.export_json().expect("JSON export should succeed");
        assert!(json.contains("note-a.md"));
        assert!(json.contains("note-b.md"));
        assert!(json.contains("note-c.md"));

        // DOT export check
        let dot = graph.export_dot();
        assert!(dot.starts_with("digraph \"satz\" {"));
        assert!(dot.contains("\"note-a.md\" -> \"note-b.md\""));
        assert!(dot.contains("\"note-b.md\" -> \"note-c.md\""));
    }
}
