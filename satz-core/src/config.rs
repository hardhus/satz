#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct VaultConfig {
    pub id_scheme: IdSchemeConfig,
    pub daily_note: DailyNoteConfig,
    pub frontmatter: FrontmatterConfig,
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
"#;
        let cfg = VaultConfig::from_toml(toml_str).unwrap();
        assert_eq!(cfg.id_scheme, IdSchemeConfig::Hierarchical);
        assert_eq!(cfg.daily_note.folder, "journal");
        assert_eq!(cfg.daily_note.format, "%Y/%m/%d");
        assert_eq!(cfg.frontmatter.required_fields, vec!["title", "date"]);
    }
}
