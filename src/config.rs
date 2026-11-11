use std::fs;
use std::path::Path;

use crate::preset::{PresetMetadata, PresetScope};
use anyhow::{Context, Result};
use tracing::info;

mod migration;
mod settings;
mod shortcuts;
pub use settings::SettingsDraft;
pub use shortcuts::ShortcutBindings;

const DEFAULT_CONFIG_TOML: &str = include_str!("../data/config.toml");
pub const AUTO_MONITOR: &str = "auto";

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub presets: PresetsConfig,
    pub ui: UiSettings,
    pub updates: UpdatesConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdatesConfig {
    pub check_automatically: bool,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self {
            check_automatically: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PresetsConfig {
    pub apply_in_saved_order: bool,
    pub auto_ready_up: bool,
    pub save_fallback_when_taken: bool,
    pub scope: PresetScope,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
pub enum UiLanguage {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "en")]
    English,
    #[serde(rename = "zh-Hans")]
    SimplifiedChinese,
    #[serde(rename = "zh-Hant")]
    TraditionalChinese,
    #[serde(rename = "ja")]
    Japanese,
    #[serde(rename = "ko")]
    Korean,
    #[serde(rename = "de")]
    German,
    #[serde(rename = "fr")]
    French,
    #[serde(rename = "it")]
    Italian,
    #[serde(rename = "es-ES")]
    SpanishSpain,
    #[serde(rename = "es-419")]
    SpanishLatinAmerica,
    #[serde(rename = "pt-BR")]
    PortugueseBrazil,
    #[serde(rename = "pt-PT")]
    PortuguesePortugal,
    #[serde(rename = "pl")]
    Polish,
    #[serde(rename = "ru")]
    Russian,
}

impl UiLanguage {
    pub const ALL: [Self; 15] = [
        Self::Auto,
        Self::English,
        Self::SimplifiedChinese,
        Self::TraditionalChinese,
        Self::Japanese,
        Self::Korean,
        Self::German,
        Self::French,
        Self::Italian,
        Self::SpanishSpain,
        Self::SpanishLatinAmerica,
        Self::PortugueseBrazil,
        Self::PortuguesePortugal,
        Self::Polish,
        Self::Russian,
    ];

    pub fn locale(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::English => Some("en"),
            Self::SimplifiedChinese => Some("zh-Hans"),
            Self::TraditionalChinese => Some("zh-Hant"),
            Self::Japanese => Some("ja"),
            Self::Korean => Some("ko"),
            Self::German => Some("de"),
            Self::French => Some("fr"),
            Self::Italian => Some("it"),
            Self::SpanishSpain => Some("es-ES"),
            Self::SpanishLatinAmerica => Some("es-419"),
            Self::PortugueseBrazil => Some("pt-BR"),
            Self::PortuguesePortugal => Some("pt-PT"),
            Self::Polish => Some("pl"),
            Self::Russian => Some("ru"),
        }
    }

    pub fn from_locale(locale: &str) -> Option<Self> {
        let locale = locale.replace('_', "-").to_ascii_lowercase();
        let mut parts = locale.split('-');
        match parts.next()? {
            "zh" => Some(
                if locale.contains("-hant")
                    || !locale.contains("-hans")
                        && parts.any(|part| matches!(part, "tw" | "hk" | "mo"))
                {
                    Self::TraditionalChinese
                } else {
                    Self::SimplifiedChinese
                },
            ),
            "es" => Some(
                if parts.any(|part| {
                    matches!(
                        part,
                        "419"
                            | "ar"
                            | "bo"
                            | "br"
                            | "bz"
                            | "cl"
                            | "co"
                            | "cr"
                            | "cu"
                            | "do"
                            | "ec"
                            | "gt"
                            | "hn"
                            | "mx"
                            | "ni"
                            | "pa"
                            | "pe"
                            | "pr"
                            | "py"
                            | "sv"
                            | "us"
                            | "uy"
                            | "ve"
                    )
                }) {
                    Self::SpanishLatinAmerica
                } else {
                    Self::SpanishSpain
                },
            ),
            "pt" => Some(if parts.any(|part| part == "br") {
                Self::PortugueseBrazil
            } else {
                Self::PortuguesePortugal
            }),
            language => Self::ALL
                .into_iter()
                .find(|candidate| candidate.locale() == Some(language)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PanelPlacement {
    pub size: [u32; 2],
    pub position: Option<[i32; 2]>,
}

impl Default for PanelPlacement {
    fn default() -> Self {
        Self {
            size: [900, 620],
            position: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiSettings {
    pub panel_shortcut: String,
    pub panel: PanelPlacement,
    pub keep_panel_open_on_focus_loss: bool,
    pub monitor: String,
    pub language: UiLanguage,
    pub compact_preset_list: bool,
    pub notice_revision: u32,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            panel_shortcut: "Ctrl + Shift + Space".into(),
            panel: PanelPlacement::default(),
            keep_panel_open_on_focus_loss: false,
            monitor: AUTO_MONITOR.to_string(),
            language: UiLanguage::default(),
            compact_preset_list: false,
            notice_revision: 0,
        }
    }
}

fn write_setting(
    document: &mut toml_edit::DocumentMut,
    section: &str,
    key: &str,
    value: impl Into<toml_edit::Value>,
) -> Result<()> {
    let defaults = DEFAULT_CONFIG_TOML.parse::<toml_edit::DocumentMut>()?;
    let default_key = defaults[section]
        .as_table_like()
        .and_then(|table| table.key(key))
        .with_context(|| format!("unknown setting: {section}.{key}"))?;
    let table = section_table(document, section)?;
    let mut value = value.into();
    if let Some(previous) = table.get(key).and_then(toml_edit::Item::as_value) {
        *value.decor_mut() = previous.decor().clone();
    }
    if let Some(setting) = table.get_mut(key) {
        *setting = toml_edit::Item::Value(value);
    } else {
        table
            .entry_format(default_key)
            .or_insert(toml_edit::Item::Value(value));
    }
    Ok(())
}

pub fn save_scope(config_path: &Path, scope: PresetScope) -> Result<()> {
    let mut value: toml_edit::InlineTable = [
        ("stratagems", scope.stratagems),
        ("equipment", scope.equipment),
    ]
    .into_iter()
    .collect();
    let items: toml_edit::Array = scope
        .equipment_items
        .iter()
        .map(crate::item::EquipmentKind::label)
        .collect();
    value.insert("equipment_items", items.into());
    edit_config(config_path, |document| {
        write_setting(document, "presets", "scope", value)
    })
}

pub fn save_compact_preset_list(config_path: &Path, compact: bool) -> Result<()> {
    edit_config(config_path, |document| {
        write_setting(document, "ui", "compact_preset_list", compact)
    })
}

pub(crate) fn save_notice_revision(config_path: &Path, revision: u32) -> Result<()> {
    edit_config(config_path, |document| {
        write_setting(document, "ui", "notice_revision", i64::from(revision))
    })
}

pub fn save_panel_placement(config_path: &Path, placement: PanelPlacement) -> Result<()> {
    let mut value = toml_edit::InlineTable::new();
    let size: toml_edit::Array = placement.size.into_iter().map(i64::from).collect();
    value.insert("size", size.into());
    if let Some(position) = placement.position {
        let position: toml_edit::Array = position.into_iter().map(i64::from).collect();
        value.insert("position", position.into());
    }
    edit_config(config_path, |document| {
        write_setting(document, "ui", "panel", value)
    })
}

fn edit_config(
    config_path: &Path,
    edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<()>,
) -> Result<()> {
    let text = fs::read_to_string(config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("failed to parse {}", config_path.display()))?;
    migration::prepare(&mut document)?;
    edit(&mut document)?;
    crate::persistence::write(config_path, document.to_string().as_bytes())
}

fn section_table<'a>(
    document: &'a mut toml_edit::DocumentMut,
    section: &str,
) -> Result<&'a mut dyn toml_edit::TableLike> {
    if document.get(section).is_none() {
        let defaults = DEFAULT_CONFIG_TOML.parse::<toml_edit::DocumentMut>()?;
        document[section] = defaults[section].clone();
    }
    // Child tables and their comments need an ordinary section, not an inline one.
    if let Some(inline) = document[section].as_inline_table() {
        document[section] = toml_edit::Item::Table(inline.clone().into_table());
    }
    document[section]
        .as_table_like_mut()
        .with_context(|| format!("{section} configuration must be a table"))
}

pub(crate) fn cleanup_legacy(config_path: &Path) -> Result<()> {
    let text = fs::read_to_string(config_path)?;
    let mut document = text.parse::<toml_edit::DocumentMut>()?;
    if migration::prepare(&mut document)? != migration::Format::Current {
        crate::persistence::write(config_path, document.to_string().as_bytes())?;
    }
    Ok(())
}

pub(crate) struct LoadedConfig {
    pub config: AppConfig,
    legacy_document: toml::Value,
    pub legacy_hotkeys: bool,
}

impl LoadedConfig {
    pub fn legacy_preset_metadata(&self) -> Result<Option<Vec<PresetMetadata>>> {
        migration::metadata(&self.legacy_document, self.legacy_hotkeys)
    }
}

pub fn load_app_config(config_path: &Path) -> Result<LoadedConfig> {
    if !config_path.try_exists()? {
        if let Some(parent) = config_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory {}", parent.display())
            })?;
        }
        crate::persistence::write(config_path, DEFAULT_CONFIG_TOML.as_bytes())
            .with_context(|| format!("failed to create {}", config_path.display()))?;
        info!("default configuration created");
    }
    let text = fs::read_to_string(config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let mut document = text.parse::<toml_edit::DocumentMut>()?;
    let legacy_document = toml::from_str(&text)?;
    let legacy_hotkeys = migration::prepare(&mut document)? == migration::Format::Hotkeys;
    let config: AppConfig = toml::from_str(&document.to_string())
        .with_context(|| format!("failed to parse {}", config_path.display()))?;
    Ok(LoadedConfig {
        config,
        legacy_document,
        legacy_hotkeys,
    })
}
