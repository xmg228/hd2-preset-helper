//! Check saved items against the old library before scanning their replacements.
mod preview;

pub(crate) use preview::{
    MigrationGroupKey, MigrationGroupSummary, MigrationPair, MigrationSelection,
};

use std::path::Path;

use anyhow::{Context, Result, ensure};

use super::{
    equipment,
    model::{LocalTemplate, Preset, SavedEquipment, SavedItem},
    storage, stratagems,
};
use crate::{
    item::{EquipmentKind, ItemKind},
    item_cache::{
        CachedIcon, EquipmentCatalog, EquipmentChoice, IconCatalog, IconChoice, ItemCache,
    },
    scan::ScanKind,
    vision::equipment::EquipmentText,
};

#[derive(Clone, Debug, Default)]
pub(crate) struct MigrationReport {
    pub presets: usize,
    pub items: usize,
    pub unmatched_items: usize,
    pub incompatible: Vec<ScanKind>,
    pub missing: Vec<ScanKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MigrationStatus {
    Matched,
    Unmatched,
    Missing,
    Incompatible,
    Duplicate,
}

#[derive(Clone)]
pub(crate) struct PresetMigration {
    kinds: Vec<ScanKind>,
    report: MigrationReport,
    presets: Vec<MigrationPreset>,
}

#[derive(Clone)]
struct MigrationPreset {
    id: String,
    label: String,
    equipment: [Option<EquipmentRecord>; 6],
    icons: [Option<IconRecord>; 6],
}

#[derive(Clone)]
struct EquipmentRecord {
    source: SavedItem,
    sizes: ((u32, u32), (u32, u32)),
    old: Option<EquipmentChoice>,
    new: Option<EquipmentChoice>,
    status: MigrationStatus,
}

#[derive(Clone)]
struct IconRecord {
    source: LocalTemplate,
    old: Option<usize>,
    new: Option<usize>,
    status: MigrationStatus,
}

impl MigrationPreset {
    fn item_count(&self) -> usize {
        self.equipment.iter().flatten().count() + self.icons.iter().flatten().count()
    }

    fn matched_count(&self) -> usize {
        self.equipment
            .iter()
            .flatten()
            .filter(|item| item.status == MigrationStatus::Matched)
            .count()
            + self
                .icons
                .iter()
                .flatten()
                .filter(|item| item.status == MigrationStatus::Matched)
                .count()
    }
}

impl PresetMigration {
    /// Locate saved items in the old library without changing any files.
    pub(crate) fn inspect(path: &Path, cache: &ItemCache, kinds: &[ScanKind]) -> Result<Self> {
        let mut report = MigrationReport::default();
        for &kind in kinds {
            let available = match kind {
                ScanKind::Equipment(kind) => cache.equipment[kind.index()].is_some(),
                ScanKind::Stratagems => cache.stratagems.is_some(),
                ScanKind::Booster => cache.boosters.is_some(),
            };
            if !available {
                report.missing.push(kind);
            }
        }

        let library = storage::load(path)?;
        let mut presets = Vec::new();
        for preset in library.presets() {
            let mut record = MigrationPreset {
                id: preset.id.clone(),
                label: preset.label.clone(),
                equipment: std::array::from_fn(|_| None),
                icons: std::array::from_fn(|_| None),
            };
            if let Some(saved) = &preset.equipment {
                for &kind in kinds {
                    let ScanKind::Equipment(kind) = kind else {
                        continue;
                    };
                    let Some(source) = &saved.items[kind.index()] else {
                        continue;
                    };
                    let sizes = effective_sizes(saved, source);
                    let catalog = &cache.equipment[kind.index()];
                    let old = if let Some(catalog) = catalog {
                        let sample =
                            equipment::load_text(path, &source.name, sizes.0, EquipmentText::Name)
                                .with_context(|| {
                                    format!(
                                        "failed to read {} in preset {}",
                                        kind.label(),
                                        preset.id
                                    )
                                })?;
                        catalog.match_name(sample.signature())
                    } else {
                        None
                    };
                    record.equipment[kind.index()] = Some(EquipmentRecord {
                        source: source.clone(),
                        sizes,
                        old,
                        new: None,
                        status: if catalog.is_none() {
                            MigrationStatus::Missing
                        } else if old.is_some() {
                            MigrationStatus::Matched
                        } else {
                            MigrationStatus::Unmatched
                        },
                    });
                }
            }
            if let Some(saved) = &preset.stratagems
                && kinds
                    .iter()
                    .any(|kind| !matches!(kind, ScanKind::Equipment(_)))
            {
                stratagems::validate(&preset.id, saved)?;
                let sources = saved
                    .stratagems
                    .iter()
                    .enumerate()
                    .chain(saved.booster.iter().map(|source| (4, source)))
                    .chain(saved.fallback_booster.iter().map(|source| (5, source)));
                for (slot, source) in sources {
                    let (kind, scan_kind) = if slot < 4 {
                        (ItemKind::Stratagem, ScanKind::Stratagems)
                    } else {
                        (ItemKind::Booster, ScanKind::Booster)
                    };
                    if !kinds.contains(&scan_kind) {
                        continue;
                    }
                    let catalog = icon_catalog(cache, kind);
                    let old = if let Some(catalog) = catalog {
                        let sample =
                            stratagems::load_template_sample(path, source).with_context(|| {
                                format!("failed to read {} in preset {}", kind.label(), preset.id)
                            })?;
                        catalog.match_icon(kind, &sample)?
                    } else {
                        None
                    };
                    record.icons[slot] = Some(IconRecord {
                        source: source.clone(),
                        old,
                        new: None,
                        status: if catalog.is_none() {
                            MigrationStatus::Missing
                        } else if old.is_some() {
                            MigrationStatus::Matched
                        } else {
                            MigrationStatus::Unmatched
                        },
                    });
                }
                // Distinct saved slots must not collapse onto the same library item.
                let duplicate: [bool; 6] = std::array::from_fn(|index| {
                    record.icons[index]
                        .as_ref()
                        .and_then(|item| item.old)
                        .is_some_and(|position| {
                            let group = if index < 4 { 0..4 } else { 4..6 };
                            group.filter(|&other| other != index).any(|other| {
                                record.icons[other]
                                    .as_ref()
                                    .is_some_and(|other| other.old == Some(position))
                            })
                        })
                });
                for (item, duplicate) in record.icons.iter_mut().zip(duplicate) {
                    if duplicate {
                        let item = item.as_mut().unwrap();
                        item.old = None;
                        item.status = MigrationStatus::Duplicate;
                    }
                }
            }
            if record.item_count() != 0 {
                presets.push(record);
            }
        }
        let mut migration = Self {
            kinds: kinds.to_vec(),
            report,
            presets,
        };
        migration.update_report();
        Ok(migration)
    }

    pub(crate) fn report(&self) -> &MigrationReport {
        &self.report
    }

    fn update_report(&mut self) {
        self.report.presets = self
            .presets
            .iter()
            .filter(|preset| preset.matched_count() != 0)
            .count();
        self.report.items = self
            .presets
            .iter()
            .map(MigrationPreset::matched_count)
            .sum();
        let total: usize = self.presets.iter().map(MigrationPreset::item_count).sum();
        self.report.unmatched_items = total - self.report.items;
    }

    /// Fill new positions only where the complete list structure survived the scan.
    pub(crate) fn check(mut self, previous: &ItemCache, scanned: &ItemCache) -> Self {
        let mut names: [Option<Vec<Vec<Option<usize>>>>; 6] = std::array::from_fn(|_| None);
        let mut compatible = Vec::new();
        for &kind in &self.kinds {
            if self.report.missing.contains(&kind) {
                continue;
            }
            let matches = match kind {
                ScanKind::Equipment(kind) => {
                    names[kind.index()] = previous.equipment[kind.index()]
                        .as_ref()
                        .zip(scanned.equipment[kind.index()].as_ref())
                        .and_then(|(previous, current)| replacement_names(previous, current));
                    names[kind.index()].is_some()
                }
                ScanKind::Stratagems => previous
                    .stratagems
                    .as_ref()
                    .zip(scanned.stratagems.as_ref())
                    .is_some_and(|(old, new)| same_icon_layout(old, new)),
                ScanKind::Booster => previous
                    .boosters
                    .as_ref()
                    .zip(scanned.boosters.as_ref())
                    .is_some_and(|(old, new)| same_icon_layout(old, new)),
            };
            if matches {
                compatible.push(kind);
            } else {
                self.report.incompatible.push(kind);
            }
        }
        for preset in &mut self.presets {
            for kind in EquipmentKind::ALL {
                let Some(item) = &mut preset.equipment[kind.index()] else {
                    continue;
                };
                let Some(old) = item.old else {
                    continue;
                };
                item.new = names[kind.index()].as_ref().and_then(|names| {
                    names[old.category][old.name].map(|name| EquipmentChoice {
                        category: old.category,
                        name,
                    })
                });
                item.status = if item.new.is_some() {
                    MigrationStatus::Matched
                } else {
                    MigrationStatus::Incompatible
                };
            }
            for (slot, item) in preset.icons.iter_mut().enumerate() {
                let Some(item) = item else {
                    continue;
                };
                let Some(old) = item.old else {
                    continue;
                };
                let kind = if slot < 4 {
                    ScanKind::Stratagems
                } else {
                    ScanKind::Booster
                };
                item.new = compatible.contains(&kind).then_some(old);
                item.status = if item.new.is_some() {
                    MigrationStatus::Matched
                } else {
                    MigrationStatus::Incompatible
                };
            }
        }
        self.update_report();
        self
    }

    /// Apply accepted new positions after the caller publishes the scanned library.
    pub(crate) fn apply(self, path: &Path, cache: &ItemCache) -> Result<usize> {
        let mut updated = 0;
        for preset in self.presets {
            let choices = preset
                .equipment
                .each_ref()
                .map(|item| item.as_ref().and_then(|item| item.new));
            let icon_choices = preset.icons.each_ref().map(|item| {
                item.as_ref()
                    .and_then(|item| item.new)
                    .map_or(IconChoice::Keep, IconChoice::Item)
            });
            let count = choices.iter().flatten().count()
                + icon_choices
                    .iter()
                    .filter(|choice| matches!(choice, IconChoice::Item(_)))
                    .count();
            if count == 0 {
                continue;
            }
            let id = &preset.id;
            storage::edit_images(path, id, "", |record, images| {
                for (index, update) in preset.equipment.iter().enumerate() {
                    let Some(update) = update.as_ref().filter(|item| item.new.is_some()) else {
                        continue;
                    };
                    let unchanged = record.equipment.as_ref().is_some_and(|saved| {
                        saved.items[index].as_ref().is_some_and(|item| {
                            item == &update.source && effective_sizes(saved, item) == update.sizes
                        })
                    });
                    ensure!(unchanged, "preset {id} changed after the migration check");
                }
                for (index, update) in preset.icons.iter().enumerate() {
                    if let Some(update) = update.as_ref().filter(|item| item.new.is_some()) {
                        ensure!(
                            saved_icon(record, index) == Some(&update.source),
                            "preset {id} changed after the migration check"
                        );
                    }
                }
                equipment::write_cached(record, images, cache, &choices)?;
                if icon_choices
                    .iter()
                    .any(|choice| *choice != IconChoice::Keep)
                {
                    stratagems::refresh_cached(id, cache, &icon_choices, record, images)?;
                }
                Ok(())
            })
            .with_context(|| {
                format!("migrated {updated} items before migration stopped at preset {id}")
            })?;
            updated += count;
        }
        Ok(updated)
    }
}

fn saved_icon(record: &Preset, index: usize) -> Option<&LocalTemplate> {
    let saved = record.stratagems.as_ref()?;
    match index {
        0..4 => saved.stratagems.get(index),
        4 => saved.booster.as_ref(),
        _ => saved.fallback_booster.as_ref(),
    }
}

fn icon_catalog(cache: &ItemCache, kind: ItemKind) -> Option<&IconCatalog> {
    match kind {
        ItemKind::Stratagem => cache.stratagems.as_ref(),
        ItemKind::Booster => cache.boosters.as_ref(),
    }
}

fn same_icon_layout(previous: &IconCatalog, current: &IconCatalog) -> bool {
    let same_row = |pair: &[CachedIcon]| (pair[0].position.y - pair[1].position.y).abs() <= 0.1;
    previous.items.len() == current.items.len()
        && previous.items.iter().zip(&current.items).all(|(old, new)| {
            old.position.col == new.position.col && old.icon.category == new.icon.category
        })
        && previous
            .items
            .windows(2)
            .zip(current.items.windows(2))
            .all(|(old, new)| same_row(old) == same_row(new))
}

fn effective_sizes(saved: &SavedEquipment, item: &SavedItem) -> ((u32, u32), (u32, u32)) {
    let name = item.name_client_size.unwrap_or(saved.client_size);
    (name, item.category_client_size.unwrap_or(name))
}

/// Preserve each name's complete group of physical occurrences, without splits or merges.
fn replacement_names(
    previous: &EquipmentCatalog,
    current: &EquipmentCatalog,
) -> Option<Vec<Vec<Option<usize>>>> {
    if previous.categories.len() != current.categories.len() {
        return None;
    }
    previous
        .categories
        .iter()
        .zip(&current.categories)
        .map(|(previous, current)| {
            if previous.order.len() != current.order.len() {
                return None;
            }
            let mut forward = vec![None; previous.names.len()];
            let mut backward = vec![None; current.names.len()];
            for (&old, &new) in previous.order.iter().zip(&current.order) {
                let from = forward.get_mut(old)?;
                let to = backward.get_mut(new)?;
                if from.is_some_and(|name| name != new) || to.is_some_and(|name| name != old) {
                    return None;
                }
                *from = Some(new);
                *to = Some(old);
            }
            Some(forward)
        })
        .collect()
}
