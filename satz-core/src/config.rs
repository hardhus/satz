#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct VaultConfig {
    pub id_scheme: IdSchemeConfig,
    pub daily_note: DailyNoteConfig,
    pub frontmatter: FrontmatterConfig,
    pub lsp: LspConfig,
    pub hover: HoverConfig,
    pub diagnostics: DiagnosticsConfig,
    pub formatter: FormatterConfig,
    pub turkish_i_folding: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DiagnosticsConfig {
    pub moc_tags: Vec<String>,
    pub workspace: bool,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            moc_tags: vec!["moc".to_string(), "index".to_string()],
            workspace: true,
        }
    }
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
    pub enabled: bool,
    pub line_width: usize,
    pub blank_lines_around_headings: u8,
    pub final_newline: bool,
    pub normalize_links: bool,
    pub tables: TablesConfig,
    pub lists: ListsConfig,
    pub emphasis: EmphasisConfig,
    pub misc: MiscConfig,
}

impl Default for FormatterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            line_width: 80,
            blank_lines_around_headings: 1,
            final_newline: true,
            normalize_links: true,
            tables: TablesConfig::default(),
            lists: ListsConfig::default(),
            emphasis: EmphasisConfig::default(),
            misc: MiscConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TablesConfig {
    /// Enable GFM pipe-table detection and column alignment. Default: true.
    pub enable: bool,
    /// Minimum whitespace padding on each side of a cell's content. Default: 1.
    pub cell_padding: usize,
    /// Minimum width (in display columns) reserved for a column's dashes. Default: 3.
    pub min_column_width: usize,
}

impl Default for TablesConfig {
    fn default() -> Self {
        Self {
            enable: true,
            cell_padding: 1,
            min_column_width: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ListsConfig {
    /// Enable list marker and task-checkbox normalization. Default: true.
    pub enable: bool,
    /// Unordered list marker character: "-", "*", or "+". Default: "-".
    pub marker: String,
    /// Renumber ordered lists sequentially (1. 2. 3. ...) starting from the list's own first
    /// number, regardless of what the user typed for later items. Default: true.
    pub renumber_ordered: bool,
}

impl Default for ListsConfig {
    fn default() -> Self {
        Self {
            enable: true,
            marker: "-".to_string(),
            renumber_ordered: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct EmphasisConfig {
    /// Enable emphasis/strong delimiter normalization. Default: true.
    pub enable: bool,
    /// Italic delimiter: "*" or "_". Default: "*".
    pub italic_marker: String,
    /// Bold delimiter: "**" or "__". Default: "**".
    pub bold_marker: String,
}

impl Default for EmphasisConfig {
    fn default() -> Self {
        Self {
            enable: true,
            italic_marker: "*".to_string(),
            bold_marker: "**".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct MiscConfig {
    /// Enable thematic-break and code-fence style normalization. Default: true.
    pub enable: bool,
    /// Thematic break (horizontal rule) style: "---", "***", or "___". Default: "---".
    pub hr_style: String,
    /// Code fence style: "```" or "~~~". Default: "```".
    pub code_fence_style: String,
    /// Guarantee exactly one space after every blockquote `>` marker (at every nesting level).
    /// Default: true.
    pub blockquote_single_space: bool,
}

impl Default for MiscConfig {
    fn default() -> Self {
        Self {
            enable: true,
            hr_style: "---".to_string(),
            code_fence_style: "```".to_string(),
            blockquote_single_space: true,
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
    /// Maximum number of (content hash -> formatted text) entries kept in the
    /// `satz.formatWorkspace` result cache. Not an LRU: once at capacity, new distinct hashes
    /// are simply not cached (existing entries keep serving hits) rather than evicting anything.
    /// Default: 2000.
    pub format_cache_capacity: usize,
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            codelens: CodelensConfig::default(),
            inlay_hints: InlayHintConfig::default(),
            reparse_debounce_ms: 200,
            reparse_max_wait_ms: 500,
            format_cache_capacity: 2000,
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
    pub aliases: DailyAliasesConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DailyAliasesConfig {
    pub today: Vec<String>,
    pub yesterday: Vec<String>,
    pub tomorrow: Vec<String>,
}

impl Default for DailyAliasesConfig {
    fn default() -> Self {
        Self {
            today: vec![
                "bugün".to_string(),
                "bugun".to_string(),
                "today".to_string(),
            ],
            yesterday: vec![
                "dün".to_string(),
                "dun".to_string(),
                "yesterday".to_string(),
            ],
            tomorrow: vec![
                "yarın".to_string(),
                "yarin".to_string(),
                "tomorrow".to_string(),
            ],
        }
    }
}

impl Default for DailyNoteConfig {
    fn default() -> Self {
        Self {
            folder: "daily".to_string(),
            format: "%Y-%m-%d".to_string(),
            aliases: DailyAliasesConfig::default(),
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
        assert_eq!(cfg.lsp.reparse_debounce_ms, 200);
        assert_eq!(cfg.lsp.reparse_max_wait_ms, 500);
        assert_eq!(cfg.lsp.format_cache_capacity, 2000);
        assert_eq!(cfg.hover.preview_lines, 8);
        assert!(cfg.formatter.enabled);
        assert_eq!(cfg.formatter.line_width, 80);
        assert_eq!(cfg.formatter.blank_lines_around_headings, 1);
        assert!(cfg.formatter.final_newline);
        assert!(cfg.formatter.normalize_links);
        assert!(cfg.formatter.tables.enable);
        assert_eq!(cfg.formatter.tables.cell_padding, 1);
        assert_eq!(cfg.formatter.tables.min_column_width, 3);
        assert!(cfg.formatter.lists.enable);
        assert_eq!(cfg.formatter.lists.marker, "-");
        assert!(cfg.formatter.lists.renumber_ordered);
        assert!(cfg.formatter.emphasis.enable);
        assert_eq!(cfg.formatter.emphasis.italic_marker, "*");
        assert_eq!(cfg.formatter.emphasis.bold_marker, "**");
        assert!(cfg.formatter.misc.enable);
        assert_eq!(cfg.formatter.misc.hr_style, "---");
        assert_eq!(cfg.formatter.misc.code_fence_style, "```");
        assert!(cfg.formatter.misc.blockquote_single_space);
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

[lsp]
reparse_debounce_ms = 300
reparse_max_wait_ms = 900
format_cache_capacity = 500

[lsp.codelens]
enable = true

[lsp.inlay_hints]
enable = false

[hover]
preview_lines = 12

[formatter]
enabled = false
line_width = 100
blank_lines_around_headings = 2
final_newline = false
normalize_links = false

[formatter.tables]
enable = false
cell_padding = 2
min_column_width = 5

[formatter.lists]
enable = false
marker = "*"
renumber_ordered = false

[formatter.emphasis]
enable = false
italic_marker = "_"
bold_marker = "__"

[formatter.misc]
enable = false
hr_style = "***"
code_fence_style = "~~~"
blockquote_single_space = false
"#;
        let cfg = VaultConfig::from_toml(toml_str).unwrap();
        assert_eq!(cfg.id_scheme, IdSchemeConfig::Hierarchical);
        assert_eq!(cfg.daily_note.folder, "journal");
        assert_eq!(cfg.daily_note.format, "%Y/%m/%d");
        assert_eq!(cfg.frontmatter.required_fields, vec!["title", "date"]);
        assert!(cfg.lsp.codelens.enable);
        assert!(!cfg.lsp.inlay_hints.enable);
        assert_eq!(cfg.lsp.reparse_debounce_ms, 300);
        assert_eq!(cfg.lsp.reparse_max_wait_ms, 900);
        assert_eq!(cfg.lsp.format_cache_capacity, 500);
        assert_eq!(cfg.hover.preview_lines, 12);
        assert!(!cfg.formatter.enabled);
        assert_eq!(cfg.formatter.line_width, 100);
        assert_eq!(cfg.formatter.blank_lines_around_headings, 2);
        assert!(!cfg.formatter.final_newline);
        assert!(!cfg.formatter.normalize_links);
        assert!(!cfg.formatter.tables.enable);
        assert_eq!(cfg.formatter.tables.cell_padding, 2);
        assert_eq!(cfg.formatter.tables.min_column_width, 5);
        assert!(!cfg.formatter.lists.enable);
        assert_eq!(cfg.formatter.lists.marker, "*");
        assert!(!cfg.formatter.lists.renumber_ordered);
        assert!(!cfg.formatter.emphasis.enable);
        assert_eq!(cfg.formatter.emphasis.italic_marker, "_");
        assert_eq!(cfg.formatter.emphasis.bold_marker, "__");
        assert!(!cfg.formatter.misc.enable);
        assert_eq!(cfg.formatter.misc.hr_style, "***");
        assert_eq!(cfg.formatter.misc.code_fence_style, "~~~");
        assert!(!cfg.formatter.misc.blockquote_single_space);
    }
}
