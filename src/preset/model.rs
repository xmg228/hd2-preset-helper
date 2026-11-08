//! An ordered preset tree; identity is independent of placement.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::vision::SampleGeometry;

pub(super) const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PresetLibrary {
    schema_version: u32,
    pub(super) entries: Vec<Entry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Preset {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub shortcut: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stratagems: Option<StratagemPreset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equipment: Option<SavedEquipment>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Entry {
    Preset(Box<Preset>),
    /// Logical grouping only; template directories follow preset IDs.
    Folder {
        id: String,
        label: String,
        #[serde(default, skip_serializing_if = "is_false")]
        collapsed: bool,
        entries: Vec<Entry>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct LocalTemplate {
    /// Local template path relative to the directory containing presets.json.
    pub path: String,
    #[serde(flatten)]
    pub geometry: SampleGeometry,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct StratagemPreset {
    pub stratagems: Vec<LocalTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub booster: Option<LocalTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_booster: Option<LocalTemplate>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct SavedEquipment {
    pub(super) client_size: (u32, u32),
    // Same order as the six entry cards: helmet, armor, cape, primary, secondary, throwable.
    pub(super) items: [Option<SavedItem>; 6],
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct SavedItem {
    pub(super) name: String,
    /// Partial saves may replace a name at a different resolution from the other items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) name_client_size: Option<(u32, u32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) category: Option<String>,
    /// A repaired category can be captured at a different resolution from the names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) category_client_size: Option<(u32, u32)>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PresetMetadata {
    pub id: String,
    pub label: String,
    pub shortcut: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LibraryMetadata {
    pub presets: Vec<PresetMetadata>,
    pub entries: Vec<EntryMetadata>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct EntryMetadata {
    pub id: String,
    pub label: String,
    /// Empty for root entries; otherwise the containing folder ID.
    pub parent: String,
    pub is_folder: bool,
    pub collapsed: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl Entry {
    fn id(&self) -> &str {
        match self {
            Self::Preset(preset) => &preset.id,
            Self::Folder { id, .. } => id,
        }
    }
}

impl Preset {
    pub(super) fn image_paths(&self) -> impl Iterator<Item = &str> {
        let stratagems = self
            .stratagems
            .iter()
            .flat_map(|preset| {
                preset
                    .stratagems
                    .iter()
                    .chain(preset.booster.iter())
                    .chain(preset.fallback_booster.iter())
            })
            .map(|template| template.path.as_str());
        let equipment = self
            .equipment
            .iter()
            .flat_map(|saved| &saved.items)
            .flatten()
            .flat_map(|item| std::iter::once(item.name.as_str()).chain(item.category.as_deref()));
        stratagems.chain(equipment)
    }
}

impl Default for PresetLibrary {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            entries: Vec::new(),
        }
    }
}

impl PresetLibrary {
    fn all_entries(&self) -> impl Iterator<Item = &Entry> {
        let mut stack = vec![self.entries.iter()];
        std::iter::from_fn(move || {
            loop {
                let Some(entry) = stack.last_mut()?.next() else {
                    stack.pop();
                    continue;
                };
                if let Entry::Folder { entries, .. } = entry {
                    stack.push(entries.iter());
                }
                return Some(entry);
            }
        })
    }

    pub(super) fn presets(&self) -> impl Iterator<Item = &Preset> {
        self.all_entries().filter_map(|entry| match entry {
            Entry::Preset(preset) => Some(preset.as_ref()),
            Entry::Folder { .. } => None,
        })
    }

    fn presets_mut(&mut self) -> impl Iterator<Item = &mut Preset> {
        let mut stack = vec![self.entries.iter_mut()];
        std::iter::from_fn(move || {
            loop {
                let Some(entry) = stack.last_mut()?.next() else {
                    stack.pop();
                    continue;
                };
                match entry {
                    Entry::Preset(preset) => return Some(preset.as_mut()),
                    Entry::Folder { entries, .. } => stack.push(entries.iter_mut()),
                }
            }
        })
    }

    pub(super) fn into_presets(self) -> impl Iterator<Item = Preset> {
        let mut stack = vec![self.entries.into_iter()];
        std::iter::from_fn(move || {
            loop {
                let Some(entry) = stack.last_mut()?.next() else {
                    stack.pop();
                    continue;
                };
                match entry {
                    Entry::Preset(preset) => return Some(*preset),
                    Entry::Folder { entries, .. } => stack.push(entries.into_iter()),
                }
            }
        })
    }

    pub(super) fn get(&self, id: &str) -> Result<&Preset> {
        self.presets()
            .find(|preset| preset.id == id)
            .ok_or_else(|| anyhow::anyhow!("preset not found: {id}"))
    }

    pub(super) fn get_mut(&mut self, id: &str) -> Result<&mut Preset> {
        self.presets_mut()
            .find(|preset| preset.id == id)
            .ok_or_else(|| anyhow::anyhow!("preset not found: {id}"))
    }

    pub(super) fn library_metadata(&self) -> LibraryMetadata {
        fn visit(entries: &[Entry], parent: &str, metadata: &mut LibraryMetadata) {
            for entry in entries {
                let (label, children, collapsed) = match entry {
                    Entry::Preset(preset) => {
                        metadata.presets.push(PresetMetadata {
                            id: preset.id.clone(),
                            label: preset.label.trim().into(),
                            shortcut: preset.shortcut.clone(),
                        });
                        (preset.label.as_str(), None, false)
                    }
                    Entry::Folder {
                        label,
                        entries,
                        collapsed,
                        ..
                    } => (label.as_str(), Some(entries), *collapsed),
                };
                metadata.entries.push(EntryMetadata {
                    id: entry.id().into(),
                    label: label.trim().into(),
                    parent: parent.into(),
                    is_folder: children.is_some(),
                    collapsed,
                });
                if let Some(children) = children {
                    visit(children, entry.id(), metadata);
                }
            }
        }
        let mut metadata = LibraryMetadata::default();
        visit(&self.entries, "", &mut metadata);
        metadata
    }

    pub(super) fn add_folder(&mut self) -> Result<String> {
        let number = self
            .all_entries()
            .filter_map(|entry| ordinal(entry.id(), "folder_"))
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .context("folder ID space exhausted")?;
        let id = format!("folder_{number}");
        self.entries.push(Entry::Folder {
            id: id.clone(),
            label: String::new(),
            collapsed: false,
            entries: Vec::new(),
        });
        Ok(id)
    }

    pub(super) fn toggle_folder(&mut self, id: &str) -> Result<bool> {
        let (entries, index) = Self::locate_entry(&mut self.entries, id)
            .with_context(|| format!("folder not found: {id}"))?;
        let Entry::Folder { collapsed, .. } = &mut entries[index] else {
            anyhow::bail!("entry is not a folder: {id}");
        };
        *collapsed = !*collapsed;
        Ok(*collapsed)
    }

    pub(super) fn rename_folder(&mut self, id: &str, label: &str) -> Result<()> {
        validate_label(label)?;
        let (entries, index) = Self::locate_entry(&mut self.entries, id)
            .with_context(|| format!("folder not found: {id}"))?;
        let Entry::Folder { label: saved, .. } = &mut entries[index] else {
            anyhow::bail!("entry is not a folder: {id}");
        };
        *saved = label.trim().into();
        Ok(())
    }

    pub(super) fn ungroup_folder(&mut self, id: &str) -> Result<()> {
        let (entries, index) = Self::locate_entry(&mut self.entries, id)
            .with_context(|| format!("folder not found: {id}"))?;
        ensure!(
            matches!(entries[index], Entry::Folder { .. }),
            "entry is not a folder: {id}"
        );
        let Entry::Folder {
            entries: children, ..
        } = entries.remove(index)
        else {
            unreachable!("folder kind was validated before removal");
        };
        entries.splice(index..index, children);
        Ok(())
    }

    pub(super) fn move_entry(
        &mut self,
        id: &str,
        parent: Option<&str>,
        before: Option<&str>,
    ) -> Result<()> {
        let source = self
            .all_entries()
            .find(|entry| entry.id() == id)
            .with_context(|| format!("entry not found: {id}"))?;
        ensure!(
            parent.is_none() || !matches!(source, Entry::Folder { .. }),
            "folders cannot be nested"
        );
        let destination = if let Some(parent) = parent {
            match self.all_entries().find(|entry| entry.id() == parent) {
                Some(Entry::Folder { entries, .. }) => entries.as_slice(),
                _ => anyhow::bail!("destination folder not found: {parent}"),
            }
        } else {
            self.entries.as_slice()
        };
        if let Some(before) = before {
            ensure!(
                destination.iter().any(|entry| entry.id() == before),
                "destination entry {before} is not in the requested folder"
            );
            if id == before {
                return Ok(());
            }
        }

        // All fallible destination checks precede removal. Moving a preset cannot
        // remove its parent, and moving a folder is restricted to the root.
        let (entries, index) = Self::locate_entry(&mut self.entries, id)
            .context("validated source entry was not found")?;
        let entry = entries.remove(index);
        let entries = if let Some(parent) = parent {
            let (entries, index) = Self::locate_entry(&mut self.entries, parent)
                .expect("validated destination folder remains after removal");
            let Entry::Folder { entries, .. } = &mut entries[index] else {
                unreachable!("destination folder kind was validated");
            };
            entries
        } else {
            &mut self.entries
        };
        let index = before.map_or(entries.len(), |before| {
            entries
                .iter()
                .position(|entry| entry.id() == before)
                .expect("validated destination entry remains after removal")
        });
        entries.insert(index, entry);
        Ok(())
    }

    pub(super) fn set_shortcuts(&mut self, bindings: &BTreeMap<String, String>) -> Result<()> {
        ensure!(
            bindings.len() == self.presets().count()
                && self
                    .presets()
                    .all(|preset| bindings.contains_key(&preset.id)),
            "preset list changed while editing shortcuts; reopen Settings"
        );
        for preset in self.presets_mut() {
            preset.shortcut.clone_from(&bindings[&preset.id]);
        }
        Ok(())
    }

    pub(super) fn remove(&mut self, id: &str) -> Result<()> {
        self.take_preset(id).map(|_| ())
    }

    pub(super) fn insert_after(&mut self, id: &str, preset: Preset) -> Result<()> {
        fn expand_ancestors(entries: &mut [Entry], id: &str) -> bool {
            if entries.iter().any(|entry| entry.id() == id) {
                return true;
            }
            entries.iter_mut().any(|entry| {
                if let Entry::Folder {
                    entries, collapsed, ..
                } = entry
                    && expand_ancestors(entries, id)
                {
                    *collapsed = false;
                    true
                } else {
                    false
                }
            })
        }

        self.get(id)?;
        let (entries, index) = Self::locate_entry(&mut self.entries, id)
            .with_context(|| format!("preset not found: {id}"))?;
        entries.insert(index + 1, Entry::Preset(Box::new(preset)));
        expand_ancestors(&mut self.entries, id);
        Ok(())
    }

    fn take_preset(&mut self, id: &str) -> Result<Entry> {
        self.get(id)?;
        let (entries, index) = Self::locate_entry(&mut self.entries, id)
            .with_context(|| format!("preset not found: {id}"))?;
        Ok(entries.remove(index))
    }

    fn locate_entry<'a>(
        entries: &'a mut Vec<Entry>,
        id: &str,
    ) -> Option<(&'a mut Vec<Entry>, usize)> {
        if let Some(index) = entries.iter().position(|entry| entry.id() == id) {
            return Some((entries, index));
        }
        entries.iter_mut().find_map(|entry| match entry {
            Entry::Folder { entries, .. } => Self::locate_entry(entries, id),
            _ => None,
        })
    }

    pub(super) fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == SCHEMA_VERSION,
            "unsupported preset schema version {}; expected {SCHEMA_VERSION}",
            self.schema_version
        );
        fn visit<'a>(entries: &'a [Entry], seen: &mut BTreeSet<&'a str>) -> Result<()> {
            for entry in entries {
                let (id, label, prefix) = match entry {
                    Entry::Preset(preset) => (&preset.id, &preset.label, "preset_"),
                    Entry::Folder { id, label, .. } => (id, label, "folder_"),
                };
                validate_id(id, prefix)?;
                validate_label(label)?;
                ensure!(seen.insert(id), "duplicate preset or folder ID: {id}");
                if let Entry::Folder { entries, .. } = entry {
                    visit(entries, seen)?;
                }
            }
            Ok(())
        }
        visit(&self.entries, &mut BTreeSet::new())
    }

    pub(super) fn image_paths(&self) -> BTreeSet<&str> {
        self.presets().flat_map(Preset::image_paths).collect()
    }
}

pub(super) fn ordinal(id: &str, prefix: &str) -> Option<usize> {
    id.strip_prefix(prefix)?
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0 && id == format!("{prefix}{n}"))
}

pub(super) fn validate_id(id: &str, prefix: &str) -> Result<()> {
    ensure!(ordinal(id, prefix).is_some(), "invalid {prefix} ID: {id}");
    Ok(())
}

pub(super) fn validate_label(label: &str) -> Result<()> {
    ensure!(
        !label.chars().any(char::is_control),
        "preset or folder name must not contain control characters"
    );
    Ok(())
}
