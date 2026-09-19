use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tracing::info;

use crate::input;

const DEFAULT_CONFIG_TOML: &str = include_str!("../data/config.toml");
// Historical config value, independent of the current platform's data directory.
const LEGACY_PRESETS_PATH: &str = "data/presets.json";
const MAX_PRESET_HOTKEYS: usize = 12;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub presets: PresetsConfig,
    pub hotkey: HotkeyConfig,
    pub overlay: OverlaySettings,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PresetsConfig {
    #[serde(rename = "path")]
    legacy_path: Option<PathBuf>,
    pub apply_in_saved_order: bool,
    pub auto_ready_up: bool,
    pub save_fallback_when_taken: bool,
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HotkeyConfig {
    pub modifiers: Vec<input::HotkeyModifier>,
    pub keys: Vec<input::Key>,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            modifiers: vec![input::HotkeyModifier::Ctrl, input::HotkeyModifier::Shift],
            keys: vec![
                input::Key::F7,
                input::Key::F8,
                input::Key::F9,
                input::Key::F10,
                input::Key::F11,
                input::Key::F12,
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverlaySettings {
    pub enabled: bool,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

pub fn save_preset_setting(config_path: &Path, key: &str, value: bool) -> Result<()> {
    let text = fs::read_to_string(config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("failed to parse {}", config_path.display()))?;
    let defaults = DEFAULT_CONFIG_TOML
        .parse::<toml_edit::DocumentMut>()
        .context("failed to parse the embedded default configuration for editing")?;

    if document.get("presets").is_none() {
        document["presets"] = defaults["presets"].clone();
    }
    let default_presets = defaults["presets"]
        .as_table_like()
        .context("default presets configuration is not a table")?;
    let default_key = default_presets
        .key(key)
        .with_context(|| format!("unknown preset setting: {key}"))?;
    let presets = document["presets"]
        .as_table_like_mut()
        .context("presets configuration is not a table")?;
    if let Some(setting) = presets.get_mut(key) {
        *setting = toml_edit::value(value);
    } else {
        presets
            .entry_format(default_key)
            .or_insert(toml_edit::value(value));
    }

    fs::write(config_path, document.to_string())
        .with_context(|| format!("failed to update {}", config_path.display()))
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
    let mut config: AppConfig = toml::from_str(&text)
        .with_context(|| format!("failed to parse {}", config_path.display()))?;
    let reset_action = config_reset_action(&config);
    if reset_action != ConfigResetAction::None {
        fs::write(config_path, DEFAULT_CONFIG_TOML)
            .with_context(|| format!("failed to reset {}", config_path.display()))?;
        config = toml::from_str(DEFAULT_CONFIG_TOML)
            .context("failed to parse the embedded default configuration")?;
        info!(
            path = %config_path.display(),
            notify = reset_action == ConfigResetAction::Notify,
            "legacy configuration reset"
        );
    }
    validate_hotkey_keys(&config.hotkey.keys)?;
    validate_preset_labels(&config.presets.labels)?;
    Ok((config, reset_action == ConfigResetAction::Notify))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfigResetAction {
    None,
    Silent,
    Notify,
}

fn config_reset_action(config: &AppConfig) -> ConfigResetAction {
    let Some(legacy_path) = config.presets.legacy_path.as_deref() else {
        return ConfigResetAction::None;
    };

    let mut current_without_legacy_path = config.clone();
    current_without_legacy_path.presets.legacy_path = None;
    if legacy_path == Path::new(LEGACY_PRESETS_PATH)
        && current_without_legacy_path == AppConfig::default()
    {
        ConfigResetAction::Silent
    } else {
        ConfigResetAction::Notify
    }
}

fn validate_hotkey_keys(keys: &[input::Key]) -> Result<()> {
    if keys.is_empty() || keys.len() > MAX_PRESET_HOTKEYS {
        bail!(
            "hotkey.keys must contain 1 to {} keys, got {}",
            MAX_PRESET_HOTKEYS,
            keys.len(),
        );
    }
    if let Some(key) = keys.iter().find(|key| !key.is_function_key()) {
        bail!("hotkey.keys only supports f1 through f12, got {key:?}");
    }

    Ok(())
}

fn validate_preset_labels(labels: &BTreeMap<String, String>) -> Result<()> {
    for (preset, label) in labels {
        let valid_key = preset
            .strip_prefix("preset_")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|index| (1..=MAX_PRESET_HOTKEYS).contains(index))
            .is_some_and(|index| preset == &format!("preset_{index}"));
        if !valid_key {
            bail!(
                "presets.labels key must be preset_1 through preset_{MAX_PRESET_HOTKEYS}, got {preset}"
            );
        }
        if label.chars().any(char::is_control) {
            bail!("presets.labels.{preset} must not contain control characters");
        }
    }
    Ok(())
}
