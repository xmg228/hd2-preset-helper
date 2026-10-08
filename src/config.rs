use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::preset::PresetScope;
use anyhow::{Context, Result, bail, ensure};
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
}

impl AppConfig {
    pub fn shortcut_bindings(&self) -> ShortcutBindings {
        ShortcutBindings {
            panel: self.ui.panel_shortcut.clone(),
            presets: self
                .presets
                .slots
                .iter()
                .map(|(name, slot)| (name.clone(), slot.shortcut.clone()))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PresetsConfig {
    pub slots: BTreeMap<String, PresetSlot>,
    pub apply_in_saved_order: bool,
    pub auto_ready_up: bool,
    pub save_fallback_when_taken: bool,
    pub scope: PresetScope,
}

impl Default for PresetsConfig {
    fn default() -> Self {
        Self {
            slots: (1..=6)
                .map(|n| {
                    (
                        format!("preset_{n}"),
                        PresetSlot {
                            label: String::new(),
                            shortcut: format!("Ctrl + Shift + F{}", n + 6),
                        },
                    )
                })
                .collect(),
            apply_in_saved_order: false,
            auto_ready_up: false,
            save_fallback_when_taken: false,
            scope: PresetScope::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PresetSlot {
    pub label: String,
    pub shortcut: String,
}

impl PresetSlot {
    fn to_item(&self) -> toml_edit::Item {
        let mut table = toml_edit::Table::new();
        table.insert("label", toml_edit::value(self.label.as_str()));
        table.insert("shortcut", toml_edit::value(self.shortcut.as_str()));
        toml_edit::Item::Table(table)
    }
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiSettings {
    pub panel_shortcut: String,
    pub monitor: String,
    pub language: UiLanguage,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            panel_shortcut: "Ctrl + Shift + Space".into(),
            monitor: AUTO_MONITOR.to_string(),
            language: UiLanguage::default(),
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

pub fn save_preset_label(config_path: &Path, preset: &str, label: &str) -> Result<()> {
    validate_preset_label(preset, label)?;
    edit_config(config_path, |document| {
        write_slot_field(document, preset, "label", label)
    })
}

pub fn save_scope(config_path: &Path, scope: PresetScope) -> Result<()> {
    let value: toml_edit::InlineTable = [
        ("stratagems", scope.stratagems),
        ("equipment", scope.equipment),
    ]
    .into_iter()
    .collect();
    edit_config(config_path, |document| {
        write_setting(document, "presets", "scope", value)
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
    edit(&mut document)?;
    fs::write(config_path, document.to_string())
        .with_context(|| format!("failed to update {}", config_path.display()))
}

fn slots_table(document: &mut toml_edit::DocumentMut) -> Result<&mut dyn toml_edit::TableLike> {
    // Omitted slots mean the defaults; an explicit empty table means no presets.
    let presets = section_table(document, "presets")?;
    if !presets.contains_key("slots") {
        let defaults = DEFAULT_CONFIG_TOML.parse::<toml_edit::DocumentMut>()?;
        presets.insert("slots", defaults["presets"]["slots"].clone());
    }
    presets
        .get_mut("slots")
        .expect("slots inserted above")
        .as_table_like_mut()
        .context("presets.slots must be a table")
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

fn slot_table<'a>(
    document: &'a mut toml_edit::DocumentMut,
    preset: &str,
) -> Result<&'a mut dyn toml_edit::TableLike> {
    slots_table(document)?
        .entry(preset)
        .or_insert_with(|| PresetSlot::default().to_item())
        .as_table_like_mut()
        .with_context(|| format!("presets.slots.{preset} must be a table"))
}

fn write_slot_field(
    document: &mut toml_edit::DocumentMut,
    preset: &str,
    key: &str,
    text: &str,
) -> Result<()> {
    let table = slot_table(document, preset)?;
    let mut value = toml_edit::Value::from(text);
    if let Some(previous) = table.get(key).and_then(toml_edit::Item::as_value) {
        *value.decor_mut() = previous.decor().clone();
    }
    table.insert(key, toml_edit::Item::Value(value));
    Ok(())
}

pub fn add_preset(config_path: &Path, preset: &str) -> Result<()> {
    validate_preset_label(preset, "")?;
    edit_config(config_path, |document| {
        let slots = slots_table(document)?;
        ensure!(
            !slots.contains_key(preset),
            "preset already exists: {preset}"
        );
        slots.insert(preset, PresetSlot::default().to_item());
        Ok(())
    })
}

pub fn remove_preset(config_path: &Path, preset: &str) -> Result<()> {
    edit_config(config_path, |document| {
        slots_table(document)?.remove(preset);
        // Keep the empty parent explicit so deserialization cannot restore defaults.
        if let Some(table) = document["presets"]["slots"].as_table_mut()
            && table.is_empty()
        {
            table.set_implicit(false);
        }
        Ok(())
    })
}

pub fn load_app_config(config_path: &Path) -> Result<(AppConfig, bool)> {
    if !config_path.exists() {
        if let Some(parent) = config_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory {}", parent.display())
            })?;
        }
        fs::write(config_path, DEFAULT_CONFIG_TOML)
            .with_context(|| format!("failed to create {}", config_path.display()))?;
        info!(path = %config_path.display(), "default configuration created");
    }
    let text = fs::read_to_string(config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let mut document = text.parse::<toml_edit::DocumentMut>()?;
    let migrated = migration::migrate(&mut document)?;
    let config: AppConfig = toml::from_str(&document.to_string())
        .with_context(|| format!("failed to parse {}", config_path.display()))?;
    for (preset, slot) in &config.presets.slots {
        validate_preset_label(preset, &slot.label)?;
    }
    config.shortcut_bindings().validate()?;
    if migrated {
        fs::write(config_path, document.to_string()).context("failed to migrate configuration")?;
    }
    Ok((config, migrated))
}

fn validate_preset_label(preset: &str, label: &str) -> Result<()> {
    let valid_key = preset
        .strip_prefix("preset_")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|index| *index > 0)
        .is_some_and(|index| preset == format!("preset_{index}"));
    if !valid_key {
        bail!("preset ID must be preset_N with a positive integer N, got {preset}");
    }
    if label.chars().any(char::is_control) {
        bail!("presets.slots.{preset}.label must not contain control characters");
    }
    Ok(())
}
