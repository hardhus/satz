#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VaultConfig {
    /// RESERVED: accepted so existing files keep loading, but currently has no effect (every
    /// document is identified by its vault-relative path).
    pub id_scheme: IdSchemeConfig,
    pub daily_note: DailyNoteConfig,
    pub frontmatter: FrontmatterConfig,
    pub lsp: LspConfig,
    pub hover: HoverConfig,
    pub diagnostics: DiagnosticsConfig,
    pub formatter: FormatterConfig,
    /// RESERVED: accepted so existing files keep loading, but currently has no effect (key
    /// folding always treats Turkish `İ`/`I` specially).
    pub turkish_i_folding: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiagnosticsConfig {
    pub moc_tags: Vec<String>,
    /// RESERVED: accepted so existing files keep loading, but currently has no effect.
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
#[serde(default, deny_unknown_fields)]
pub struct HoverConfig {
    pub preview_lines: usize,
}

impl Default for HoverConfig {
    fn default() -> Self {
        Self { preview_lines: 8 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
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
    pub wrap: WrapConfig,
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
            wrap: WrapConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WrapConfig {
    /// Reflow top-level paragraphs to `line_width`. Default: false (opt-in — existing vaults
    /// are never silently rewrapped; add `[formatter.wrap] enable = true` to turn it on).
    pub enable: bool,
    /// How a wikilink's or markdown link's width counts against `line_width`:
    /// - `"raw"` (default): the full source text (`[[path#heading|display]]`) counts, so the
    ///   editor's own line never exceeds the limit.
    /// - `"display"`: only the alias/display text (or the bare target if there's no alias)
    ///   counts, matching what a rendered viewer would actually show — the raw .md line can
    ///   still run longer than `line_width` in this mode.
    ///
    /// A link is never split either way; see `FormatterConfig` doc comment.
    pub link_width_mode: String,
}

impl Default for WrapConfig {
    fn default() -> Self {
        Self {
            enable: false,
            link_width_mode: "raw".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
pub struct LspConfig {
    pub codelens: CodelensConfig,
    pub inlay_hints: InlayHintConfig,
    pub semantic_tokens: SemanticTokensConfig,
    pub reparse_debounce_ms: u64,
    pub reparse_max_wait_ms: u64,
    /// Maximum number of (content hash -> formatted text) entries kept in the
    /// `satz.formatWorkspace` result cache. Not an LRU: once at capacity, new distinct hashes
    /// are simply not cached (existing entries keep serving hits) rather than evicting anything.
    /// Default: 2000.
    pub format_cache_capacity: usize,
    /// Most items one completion answer carries. A bigger vault (thousands of notes, headings
    /// and tags) would otherwise send every one of them on every keystroke; when the answer is cut
    /// it is marked incomplete and the client asks again as the user types. `0` = no limit.
    /// Default: 200.
    pub completion_limit: usize,
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            codelens: CodelensConfig::default(),
            inlay_hints: InlayHintConfig::default(),
            semantic_tokens: SemanticTokensConfig::default(),
            reparse_debounce_ms: 200,
            reparse_max_wait_ms: 500,
            format_cache_capacity: 2000,
            completion_limit: 200,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SemanticTokensConfig {
    /// Splits a `[[target|display]]` / `![[target|display]]` link's semantic token in two at the
    /// `|`, so the displayed alias text can be themed differently from the target/heading part.
    /// Links with no alias are unaffected either way. Default: true.
    pub split_link_display: bool,
}

impl Default for SemanticTokensConfig {
    fn default() -> Self {
        Self {
            split_link_display: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CodelensConfig {
    /// Enable CodeLens backlink count. Default: false (terminal-first).
    pub enable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
pub struct DailyNoteConfig {
    pub folder: String,
    pub format: String,
    pub aliases: DailyAliasesConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
pub struct FrontmatterConfig {
    pub required_fields: Vec<String>,
}

/// The vault-root file holding a vault's configuration. Only this exact file in the vault ROOT is
/// read; there is no other config file name and no per-folder config.
pub const CONFIG_FILE_NAME: &str = ".satz.toml";

impl VaultConfig {
    /// Parses and validates a configuration. Unknown keys are errors (every config struct is
    /// `deny_unknown_fields`), so a typo like `enabled` for `enable` is reported instead of
    /// silently ignored.
    pub fn from_toml(toml_str: &str) -> Result<Self, toml::de::Error> {
        use serde::de::Error as _;
        let config: Self = toml::from_str(toml_str)?;
        config.validate().map_err(toml::de::Error::custom)?;
        Ok(config)
    }

    /// Checks values serde alone can't: `daily_note.format` must be a valid `strftime` string,
    /// because formatting a date with an invalid one panics (`satz daily`, and relative daily
    /// links like `[[bugün]]` in the language server).
    fn validate(&self) -> Result<(), String> {
        use chrono::format::{Item, StrftimeItems};
        if StrftimeItems::new(&self.daily_note.format).any(|item| matches!(item, Item::Error)) {
            return Err(format!(
                "invalid daily_note.format {:?}: not a valid strftime format string",
                self.daily_note.format
            ));
        }

        let f = &self.formatter;
        one_of(
            "formatter.misc.hr_style",
            &f.misc.hr_style,
            &["---", "***", "___"],
        )?;
        one_of(
            "formatter.misc.code_fence_style",
            &f.misc.code_fence_style,
            &["```", "~~~"],
        )?;
        one_of(
            "formatter.emphasis.italic_marker",
            &f.emphasis.italic_marker,
            &["*", "_"],
        )?;
        one_of(
            "formatter.emphasis.bold_marker",
            &f.emphasis.bold_marker,
            &["**", "__"],
        )?;
        one_of("formatter.lists.marker", &f.lists.marker, &["-", "*", "+"])?;
        one_of(
            "formatter.wrap.link_width_mode",
            &f.wrap.link_width_mode,
            &["raw", "display"],
        )?;
        Ok(())
    }

    /// Loads `<vault_root>/.satz.toml`.
    ///
    /// - missing file -> the default configuration
    /// - present but unreadable (a directory, permissions, not UTF-8) or invalid (bad TOML,
    ///   unknown key, wrong type, invalid `daily_note.format`) -> an error naming the file and,
    ///   for TOML errors, the line and column. It never silently falls back to defaults: callers
    ///   decide how to surface the error, but they always get it.
    ///
    /// A leading UTF-8 byte-order mark (which Windows editors add) is accepted: the `toml` parser
    /// skips it itself (covered by `load_accepts_a_utf8_bom`, which fails if that ever changes).
    pub fn load(vault_root: &std::path::Path) -> anyhow::Result<Self> {
        let path = vault_root.join(CONFIG_FILE_NAME);
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => anyhow::bail!("failed to read {}: {}", path.display(), e),
        };
        Self::from_toml(&content).map_err(|e| anyhow::anyhow!("invalid {}: {}", path.display(), e))
    }
}

/// A string setting whose value must be one of a fixed set. (The formatter would otherwise
/// silently fall back to its default for a typo, so the user never learns the setting was ignored.)
fn one_of(key: &str, value: &str, allowed: &[&str]) -> Result<(), String> {
    if allowed.contains(&value) {
        return Ok(());
    }
    let choices: Vec<String> = allowed.iter().map(|a| format!("{a:?}")).collect();
    Err(format!(
        "invalid {key} {value:?}: expected one of {}",
        choices.join(", ")
    ))
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
        assert!(cfg.lsp.semantic_tokens.split_link_display);
        assert_eq!(cfg.lsp.reparse_debounce_ms, 200);
        assert_eq!(cfg.lsp.reparse_max_wait_ms, 500);
        assert_eq!(cfg.lsp.format_cache_capacity, 2000);
        assert_eq!(cfg.lsp.completion_limit, 200);
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
        assert!(!cfg.formatter.wrap.enable);
        assert_eq!(cfg.formatter.wrap.link_width_mode, "raw");
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
completion_limit = 50

[lsp.codelens]
enable = true

[lsp.inlay_hints]
enable = false

[lsp.semantic_tokens]
split_link_display = false

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

[formatter.wrap]
enable = true
link_width_mode = "display"
"#;
        let cfg = VaultConfig::from_toml(toml_str).unwrap();
        assert_eq!(cfg.id_scheme, IdSchemeConfig::Hierarchical);
        assert_eq!(cfg.daily_note.folder, "journal");
        assert_eq!(cfg.daily_note.format, "%Y/%m/%d");
        assert_eq!(cfg.frontmatter.required_fields, vec!["title", "date"]);
        assert!(cfg.lsp.codelens.enable);
        assert!(!cfg.lsp.inlay_hints.enable);
        assert!(!cfg.lsp.semantic_tokens.split_link_display);
        assert_eq!(cfg.lsp.reparse_debounce_ms, 300);
        assert_eq!(cfg.lsp.reparse_max_wait_ms, 900);
        assert_eq!(cfg.lsp.format_cache_capacity, 500);
        assert_eq!(cfg.lsp.completion_limit, 50);
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
        assert!(cfg.formatter.wrap.enable);
        assert_eq!(cfg.formatter.wrap.link_width_mode, "display");
    }

    // ---- strict parsing / loading -------------------------------------------------------

    fn err_of(toml_str: &str) -> String {
        match VaultConfig::from_toml(toml_str) {
            Ok(_) => panic!("expected a config error for:\n{toml_str}"),
            Err(e) => e.to_string(),
        }
    }

    /// Every table a user can write in `.satz.toml` (`""` = the top level).
    const ALL_SECTIONS: &[&str] = &[
        "",
        "formatter",
        "formatter.tables",
        "formatter.lists",
        "formatter.emphasis",
        "formatter.misc",
        "formatter.wrap",
        "lsp",
        "lsp.codelens",
        "lsp.inlay_hints",
        "lsp.semantic_tokens",
        "hover",
        "diagnostics",
        "daily_note",
        "daily_note.aliases",
        "frontmatter",
    ];

    #[test]
    fn unknown_key_rejected_in_every_section() {
        // One assertion per config struct: forgetting `deny_unknown_fields` on any of them
        // would let a typo in that section pass silently.
        for section in ALL_SECTIONS {
            let toml_str = if section.is_empty() {
                "bogus_key = 1\n".to_string()
            } else {
                format!("[{section}]\nbogus_key = 1\n")
            };
            let msg = err_of(&toml_str);
            assert!(
                msg.contains("bogus_key"),
                "section {section:?}: error should name the unknown key, got: {msg}"
            );
        }
    }

    #[test]
    fn unknown_key_is_rejected_even_next_to_valid_ones() {
        let msg = err_of("[formatter]\nline_width = 100\nlinewidth = 90\n");
        assert!(msg.contains("linewidth"), "{msg}");
    }

    #[test]
    fn typo_of_a_real_key_names_the_expected_keys() {
        let msg = err_of("[formatter.wrap]\nenabled = true\n");
        assert!(msg.contains("enabled"), "{msg}");
        assert!(msg.contains("enable"), "should list the real key: {msg}");
    }

    #[test]
    fn partial_and_empty_configs_still_load() {
        assert_eq!(VaultConfig::from_toml("").unwrap(), VaultConfig::default());
        assert_eq!(
            VaultConfig::from_toml("# just a comment\n").unwrap(),
            VaultConfig::default()
        );

        let cfg = VaultConfig::from_toml("[formatter]\nline_width = 100\n").unwrap();
        assert_eq!(cfg.formatter.line_width, 100);
        assert!(
            cfg.formatter.enabled,
            "untouched fields keep their defaults"
        );
        assert_eq!(cfg.daily_note, VaultConfig::default().daily_note);

        let cfg = VaultConfig::from_toml("[daily_note]\nfolder = \"x\"\n").unwrap();
        assert_eq!(cfg.daily_note.folder, "x");
        assert_eq!(cfg.daily_note.format, "%Y-%m-%d");
        assert_eq!(cfg.formatter, VaultConfig::default().formatter);
    }

    #[test]
    fn wrong_types_and_ranges_are_rejected() {
        for (label, toml_str) in [
            ("string for integer", "[formatter]\nline_width = \"wide\"\n"),
            (
                "u8 overflow",
                "[formatter]\nblank_lines_around_headings = 300\n",
            ),
            ("negative usize", "[formatter]\nline_width = -1\n"),
            ("string for bool", "[formatter.wrap]\nenable = \"yes\"\n"),
            ("float for integer", "[hover]\npreview_lines = 1.5\n"),
            ("unknown enum value", "id_scheme = \"bogus\"\n"),
            (
                "string for list",
                "[daily_note.aliases]\ntoday = \"bugun\"\n",
            ),
            ("table for scalar", "[formatter]\nline_width = { a = 1 }\n"),
            ("broken toml", "[formatter\nline_width = 1\n"),
        ] {
            assert!(
                VaultConfig::from_toml(toml_str).is_err(),
                "{label}: should be rejected: {toml_str:?}"
            );
        }
    }

    /// The fenced ```toml block under `## Full example` in docs/configuration.md.
    fn docs_full_example() -> &'static str {
        const DOC: &str = include_str!("../../docs/configuration.md");
        let start = DOC
            .find("## Full example")
            .expect("docs must have a Full example section");
        let rest = &DOC[start..];
        let fence = rest
            .find("```toml")
            .expect("docs example must be a toml block");
        let body_start = fence + rest[fence..].find('\n').expect("fence line ends") + 1;
        // The closing fence is a ``` at the start of a line; a mid-line "```" (as in
        // `code_fence_style = "```"`) must not end the block.
        let body = &rest[body_start..];
        let end = body
            .match_indices("\n```")
            .map(|(i, _)| i)
            .find(|&i| {
                let after = &body[i + 4..];
                after.is_empty() || after.starts_with('\n') || after.starts_with('\r')
            })
            .expect("docs example must be closed");
        &body[..end + 1]
    }

    #[test]
    fn docs_full_example_matches_defaults_exactly() {
        // Comparing whole TOML trees (not just parsed structs) means the docs must list EVERY
        // key, each with its actual default -- a key added to the code but not to the docs, or a
        // default changed without updating the docs, fails here.
        let documented: toml::Value = toml::from_str(docs_full_example()).unwrap();
        let actual = toml::Value::try_from(VaultConfig::default()).unwrap();
        assert_eq!(documented, actual);
        assert_eq!(
            VaultConfig::from_toml(docs_full_example()).unwrap(),
            VaultConfig::default()
        );
    }

    #[test]
    fn daily_note_format_is_validated_without_panicking() {
        for ok in [
            "%Y-%m-%d",
            "%Y/%m/%d",
            "daily-%d.%m",
            "%B",
            "100%%",
            "plain",
        ] {
            let cfg = VaultConfig::from_toml(&format!("[daily_note]\nformat = \"{ok}\"\n"))
                .unwrap_or_else(|e| panic!("{ok:?} should be valid: {e}"));
            // Actually formatting with an accepted string must never panic.
            let _ = chrono::NaiveDate::from_ymd_opt(2026, 9, 19)
                .unwrap()
                .format(&cfg.daily_note.format)
                .to_string();
        }
        for bad in ["%Q", "%", "%Y%", "%-", "%!"] {
            let msg = err_of(&format!("[daily_note]\nformat = \"{bad}\"\n"));
            assert!(msg.contains("daily_note.format"), "{bad:?}: {msg}");
        }
    }

    /// A unique, self-cleaning temp directory (no extra dev-dependency needed).
    struct TempVault(std::path::PathBuf);
    impl TempVault {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "satz_cfg_{tag}_{}_{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn write(&self, bytes: &[u8]) {
            std::fs::write(self.0.join(CONFIG_FILE_NAME), bytes).unwrap();
        }
    }
    impl Drop for TempVault {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn config_file_name_is_dot_satz_toml() {
        assert_eq!(CONFIG_FILE_NAME, ".satz.toml");
    }

    #[test]
    fn load_missing_and_empty_files_give_defaults() {
        let v = TempVault::new("missing");
        assert_eq!(VaultConfig::load(&v.0).unwrap(), VaultConfig::default());
        v.write(b"");
        assert_eq!(VaultConfig::load(&v.0).unwrap(), VaultConfig::default());
        v.write(b"   \n\n# only comments\n");
        assert_eq!(VaultConfig::load(&v.0).unwrap(), VaultConfig::default());
    }

    #[test]
    fn load_applies_a_valid_file() {
        let v = TempVault::new("valid");
        v.write(b"[hover]\npreview_lines = 3\n[formatter.lists]\nmarker = \"*\"\n");
        let cfg = VaultConfig::load(&v.0).unwrap();
        assert_eq!(cfg.hover.preview_lines, 3);
        assert_eq!(cfg.formatter.lists.marker, "*");
        assert_eq!(
            cfg.formatter.tables,
            VaultConfig::default().formatter.tables
        );
    }

    #[test]
    fn load_reports_file_path_and_location_for_invalid_toml() {
        let v = TempVault::new("badtoml");
        v.write(b"[formatter]\nline_width = 100\nthis is not toml\n");
        let msg = VaultConfig::load(&v.0).unwrap_err().to_string();
        assert!(
            msg.contains(CONFIG_FILE_NAME),
            "should name the file: {msg}"
        );
        assert!(msg.contains("line 3"), "should give the line: {msg}");
    }

    #[test]
    fn load_rejects_unknown_keys_and_names_them() {
        let v = TempVault::new("unknown");
        v.write(b"[formatter.wrap]\nenabled = true\n");
        let msg = VaultConfig::load(&v.0).unwrap_err().to_string();
        assert!(
            msg.contains(CONFIG_FILE_NAME) && msg.contains("enabled"),
            "{msg}"
        );
    }

    #[test]
    fn load_accepts_a_utf8_bom() {
        // Windows editors such as Notepad prepend a BOM; the file is still a valid config.
        let v = TempVault::new("bom");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"[hover]\npreview_lines = 5\n");
        v.write(&bytes);
        assert_eq!(VaultConfig::load(&v.0).unwrap().hover.preview_lines, 5);
    }

    #[test]
    fn load_fails_when_config_path_is_a_directory() {
        let v = TempVault::new("isdir");
        std::fs::create_dir_all(v.0.join(CONFIG_FILE_NAME)).unwrap();
        let msg = VaultConfig::load(&v.0).unwrap_err().to_string();
        assert!(msg.contains(CONFIG_FILE_NAME), "{msg}");
    }

    #[test]
    fn load_fails_on_non_utf8_bytes() {
        let v = TempVault::new("nonutf8");
        v.write(&[0xFF, 0xFE, 0x00, b'[', 0x80]);
        let msg = VaultConfig::load(&v.0).unwrap_err().to_string();
        assert!(msg.contains(CONFIG_FILE_NAME), "{msg}");
    }

    #[test]
    fn load_does_not_read_config_files_from_subdirectories() {
        // Only the vault root's `.satz.toml` counts; a stray one in a subfolder must not.
        let v = TempVault::new("subdir");
        std::fs::create_dir_all(v.0.join("sub")).unwrap();
        std::fs::write(
            v.0.join("sub").join(CONFIG_FILE_NAME),
            "[hover]\npreview_lines = 1\n",
        )
        .unwrap();
        assert_eq!(VaultConfig::load(&v.0).unwrap(), VaultConfig::default());
    }

    // ---- string-valued formatter settings must be one of the values the formatter knows ----

    /// The error `from_toml` gives for `[section]\nkey = value`, or `None` if it is accepted.
    fn rejection(section: &str, key: &str, value: &str) -> Option<String> {
        let toml = format!("[{section}]\n{key} = \"{value}\"\n");
        VaultConfig::from_toml(&toml).err().map(|e| e.to_string())
    }

    #[test]
    fn every_known_value_of_a_string_setting_is_accepted() {
        for (section, key, values) in [
            ("formatter.misc", "hr_style", &["---", "***", "___"][..]),
            ("formatter.misc", "code_fence_style", &["```", "~~~"][..]),
            ("formatter.emphasis", "italic_marker", &["*", "_"][..]),
            ("formatter.emphasis", "bold_marker", &["**", "__"][..]),
            ("formatter.lists", "marker", &["-", "*", "+"][..]),
            ("formatter.wrap", "link_width_mode", &["raw", "display"][..]),
        ] {
            for value in values {
                assert_eq!(
                    rejection(section, key, value),
                    None,
                    "{section}.{key} = {value:?}"
                );
            }
        }
    }

    #[test]
    fn an_unknown_value_is_an_error_naming_the_key_the_value_and_the_choices() {
        for (section, key, bad, a_choice) in [
            ("formatter.misc", "hr_style", "====", "\"---\""),
            ("formatter.misc", "hr_style", "* * *", "\"***\""),
            ("formatter.misc", "code_fence_style", "```` ", "\"~~~\""),
            ("formatter.emphasis", "italic_marker", "**", "\"_\""),
            ("formatter.emphasis", "bold_marker", "*", "\"__\""),
            ("formatter.lists", "marker", "=", "\"+\""),
            ("formatter.wrap", "link_width_mode", "RAW", "\"display\""),
            ("formatter.wrap", "link_width_mode", "", "\"raw\""),
        ] {
            let message = rejection(section, key, bad)
                .unwrap_or_else(|| panic!("{section}.{key} = {bad:?} should be rejected"));
            let short_key = key;
            assert!(message.contains(short_key), "{message}");
            assert!(message.contains(&format!("{bad:?}")), "{message}");
            assert!(message.contains(a_choice), "{message}");
            assert!(message.contains("expected one of"), "{message}");
        }
    }

    #[test]
    fn a_bad_value_is_reported_by_load_with_the_file_name() {
        let v = TempVault::new("bad-value");
        v.write(b"[formatter.misc]\nhr_style = \"====\"\n");
        let error = VaultConfig::load(&v.0).unwrap_err().to_string();
        assert!(error.contains(".satz.toml"), "{error}");
        assert!(error.contains("hr_style"), "{error}");
    }

    #[test]
    fn defaults_and_the_documented_example_still_validate() {
        assert!(VaultConfig::default().validate().is_ok());
    }
}
