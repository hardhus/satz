#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct VaultConfig {
    pub id_scheme: IdSchemeConfig,
    pub daily_note: DailyNoteConfig,
    pub frontmatter: FrontmatterConfig,
    pub lsp: LspConfig,
    pub hover: HoverConfig,
    pub formatter: FormatterConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct HoverConfig {
    pub preview_lines: usize,
}

impl Default for HoverConfig {
    fn default() -> Self {
        Self { preview_lines: 8 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct FormatterConfig {
    pub line_width: usize,
    pub blank_lines_around_headings: u8,
    pub final_newline: bool,
    pub normalize_links: bool,
}

impl Default for FormatterConfig {
    fn default() -> Self {
        Self {
            line_width: 80,
            blank_lines_around_headings: 1,
            final_newline: true,
            normalize_links: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LspConfig {
    pub codelens: CodelensConfig,
    pub inlay_hints: InlayHintConfig,
    pub reparse_debounce_ms: u64,
    pub reparse_max_wait_ms: u64,
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            codelens: CodelensConfig::default(),
            inlay_hints: InlayHintConfig::default(),
            reparse_debounce_ms: 200,
            reparse_max_wait_ms: 500,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CodelensConfig {
    /// Enable CodeLens backlink count. Default: false (terminal-first).
    pub enable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct InlayHintConfig {
    /// Enable Inlay hints for links. Default: true.
    pub enable: bool,
}

impl Default for InlayHintConfig {
    fn default() -> Self {
        Self { enable: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdSchemeConfig {
    #[default]
    Path,
    Hierarchical,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DailyNoteConfig {
    pub folder: String,
    pub format: String,
}

impl Default for DailyNoteConfig {
    fn default() -> Self {
        Self {
            folder: "daily".to_string(),
            format: "%Y-%m-%d".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct FrontmatterConfig {
    pub required_fields: Vec<String>,
}

impl VaultConfig {
    pub fn from_toml(toml_str: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(toml_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = VaultConfig::default();
        assert_eq!(cfg.id_scheme, IdSchemeConfig::Path);
        assert_eq!(cfg.daily_note.folder, "daily");
        assert_eq!(cfg.daily_note.format, "%Y-%m-%d");
        assert!(!cfg.lsp.codelens.enable);
        assert!(cfg.lsp.inlay_hints.enable);
        assert_eq!(cfg.hover.preview_lines, 8);
        assert_eq!(cfg.formatter.line_width, 80);
        assert_eq!(cfg.formatter.blank_lines_around_headings, 1);
        assert!(cfg.formatter.final_newline);
        assert!(cfg.formatter.normalize_links);
    }

    #[test]
    fn test_parse_toml_config() {
        let toml_str = r#"
id_scheme = "hierarchical"

[daily_note]
folder = "journal"
format = "%Y/%m/%d"

[frontmatter]
required_fields = ["title", "date"]

[lsp.codelens]
enable = true

[lsp.inlay_hints]
enable = false

[hover]
preview_lines = 12

[formatter]
line_width = 100
blank_lines_around_headings = 2
final_newline = false
normalize_links = false
"#;
        let cfg = VaultConfig::from_toml(toml_str).unwrap();
        assert_eq!(cfg.id_scheme, IdSchemeConfig::Hierarchical);
        assert_eq!(cfg.daily_note.folder, "journal");
        assert_eq!(cfg.daily_note.format, "%Y/%m/%d");
        assert_eq!(cfg.frontmatter.required_fields, vec!["title", "date"]);
        assert!(cfg.lsp.codelens.enable);
        assert!(!cfg.lsp.inlay_hints.enable);
        assert_eq!(cfg.hover.preview_lines, 12);
        assert_eq!(cfg.formatter.line_width, 100);
        assert_eq!(cfg.formatter.blank_lines_around_headings, 2);
        assert!(!cfg.formatter.final_newline);
        assert!(!cfg.formatter.normalize_links);
    }
}
