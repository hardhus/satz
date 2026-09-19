use crate::model::range::ByteRange;

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct FootnoteDef {
    pub label: String,
    pub range: ByteRange,
}

impl FootnoteDef {
    pub fn new(label: String, range: ByteRange) -> Self {
        Self { label, range }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FootnoteTable {
    pub definitions: Vec<FootnoteDef>,
}

impl FootnoteTable {
    /// Finds the definition a `[^label]` reference points at.
    ///
    /// pulldown-cmark matches footnote labels case-insensitively (`[^A]` refers to `[^a]:`), so
    /// this does too; every consumer should go through here instead of comparing labels with
    /// `==`. Lowercasing approximates pulldown's Unicode case folding, which is identical for
    /// letters in practice (verified for ASCII and `Ü`/`ü`).
    pub fn find_def(&self, label: &str) -> Option<&FootnoteDef> {
        let wanted = label.to_lowercase();
        self.definitions
            .iter()
            .find(|d| d.label.to_lowercase() == wanted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(labels: &[&str]) -> FootnoteTable {
        FootnoteTable {
            definitions: labels
                .iter()
                .map(|l| FootnoteDef::new((*l).to_string(), ByteRange::new(0, 0)))
                .collect(),
        }
    }

    #[test]
    fn find_def_matches_labels_case_insensitively() {
        let t = table(&["razi", "Ü1"]);
        assert_eq!(t.find_def("razi").map(|d| d.label.as_str()), Some("razi"));
        assert_eq!(t.find_def("RAZI").map(|d| d.label.as_str()), Some("razi"));
        assert_eq!(t.find_def("ü1").map(|d| d.label.as_str()), Some("Ü1"));
        assert!(t.find_def("other").is_none());
    }
}
