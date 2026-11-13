//! Read-only picture pairs for one migration group at a time.
use std::{collections::BTreeSet, path::Path};

use anyhow::{Context, Result};
use image::{RgbaImage, imageops::FilterType};

use super::{
    MigrationPreset, MigrationStatus, PresetMigration, equipment, icon_catalog, replacement_names,
    stratagems,
};
use crate::{
    item::{EquipmentKind, ItemKind, StratagemCategory},
    item_cache::{EquipmentChoice, ItemCache},
    scan::ScanKind,
    vision::{
        IconSample,
        equipment::EquipmentText,
        render_icon,
        text::{render_category, render_text},
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MigrationGroupKey {
    Preset(String),
    Catalog(ScanKind),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MigrationSelection {
    pub group: MigrationGroupKey,
    pub all_items: bool,
    pub only_unmatched: bool,
}

#[derive(Clone)]
pub(crate) struct MigrationGroupSummary {
    pub key: MigrationGroupKey,
    pub label: String,
    pub matched: usize,
    pub total: usize,
    pub old_count: usize,
    pub new_count: usize,
    pub status: MigrationStatus,
}

pub(crate) struct MigrationPair {
    /// Same twelve-slot order used by the preset editor.
    pub slot: usize,
    pub left: Option<RgbaImage>,
    pub right: Option<RgbaImage>,
    pub status: MigrationStatus,
    pub references: usize,
    pub is_category: bool,
    pub category: Option<StratagemCategory>,
    /// Unlocated saved items remain visible without pretending to be old-library entries.
    pub source: Option<String>,
}

impl MigrationPair {
    fn item(slot: usize, status: MigrationStatus) -> Self {
        Self {
            slot,
            left: None,
            right: None,
            status,
            references: 0,
            is_category: false,
            category: None,
            source: None,
        }
    }
}

impl PresetMigration {
    pub(crate) fn groups(
        &self,
        previous: &ItemCache,
        scanned: Option<&ItemCache>,
    ) -> Vec<MigrationGroupSummary> {
        let Some(scanned) = scanned else {
            return self
                .presets
                .iter()
                .map(|preset| MigrationGroupSummary {
                    key: MigrationGroupKey::Preset(preset.id.clone()),
                    label: preset.label.clone(),
                    matched: preset.matched_count(),
                    total: preset.item_count(),
                    old_count: 0,
                    new_count: 0,
                    status: if preset.matched_count() == preset.item_count() {
                        MigrationStatus::Matched
                    } else {
                        MigrationStatus::Unmatched
                    },
                })
                .collect();
        };
        self.kinds
            .iter()
            .map(|&kind| {
                let counts = self
                    .presets
                    .iter()
                    .map(|preset| kind_counts(preset, kind))
                    .fold((0, 0), |(a, b), (matched, total)| (a + matched, b + total));
                MigrationGroupSummary {
                    key: MigrationGroupKey::Catalog(kind),
                    label: String::new(),
                    matched: counts.0,
                    total: counts.1,
                    old_count: catalog_count(previous, kind),
                    new_count: catalog_count(scanned, kind),
                    status: self.catalog_status(kind),
                }
            })
            .collect()
    }

    pub(crate) fn preview(
        &self,
        path: &Path,
        previous: &ItemCache,
        scanned: Option<&ItemCache>,
        selection: &MigrationSelection,
    ) -> Result<Vec<MigrationPair>> {
        match &selection.group {
            MigrationGroupKey::Preset(id) => {
                let preset = self
                    .presets
                    .iter()
                    .find(|preset| &preset.id == id)
                    .context("migration preset is not available")?;
                self.saved_pairs(
                    path,
                    previous,
                    preset,
                    None,
                    selection.only_unmatched,
                    false,
                )
            }
            MigrationGroupKey::Catalog(kind) => {
                let scanned = scanned.context("new item lists have not been scanned")?;
                let mut pairs = match *kind {
                    ScanKind::Equipment(kind) => {
                        self.equipment_pairs(previous, scanned, kind, selection)?
                    }
                    ScanKind::Stratagems => {
                        self.icon_pairs(previous, scanned, ItemKind::Stratagem, selection)?
                    }
                    ScanKind::Booster => {
                        self.icon_pairs(previous, scanned, ItemKind::Booster, selection)?
                    }
                };
                for preset in &self.presets {
                    pairs.extend(self.saved_pairs(
                        path,
                        previous,
                        preset,
                        Some(*kind),
                        true,
                        true,
                    )?);
                }
                Ok(pairs)
            }
        }
    }

    fn catalog_status(&self, kind: ScanKind) -> MigrationStatus {
        if self.report.missing.contains(&kind) {
            MigrationStatus::Missing
        } else if self.report.incompatible.contains(&kind) {
            MigrationStatus::Incompatible
        } else {
            MigrationStatus::Matched
        }
    }

    fn saved_pairs(
        &self,
        path: &Path,
        previous: &ItemCache,
        preset: &MigrationPreset,
        kind: Option<ScanKind>,
        only_unmatched: bool,
        unlocated_only: bool,
    ) -> Result<Vec<MigrationPair>> {
        let mut pairs = Vec::new();
        for (slot, item) in preset.icons.iter().enumerate() {
            let Some(item) = item else { continue };
            let (item_kind, scan_kind) = icon_kind(slot);
            if kind.is_some_and(|kind| kind != scan_kind)
                || only_unmatched && item.status == MigrationStatus::Matched
                || unlocated_only && item.old.is_some()
            {
                continue;
            }
            let mut pair = MigrationPair::item(slot, item.status);
            let sample = IconSample::extract(
                item_kind,
                stratagems::load_template_sample(path, &item.source)?,
            )?;
            pair.left = Some(thumbnail(render_icon(&sample)?, 64));
            if let Some(old) = item.old {
                let icon = &icon_catalog(previous, item_kind)
                    .and_then(|catalog| catalog.items.get(old))
                    .context("old migration icon is not available")?
                    .icon;
                pair.right = Some(thumbnail(render_icon(icon)?, 64));
            }
            pair.source = unlocated_only.then(|| preset.id.clone());
            pairs.push(pair);
        }
        for equipment_kind in EquipmentKind::ALL {
            let Some(item) = &preset.equipment[equipment_kind.index()] else {
                continue;
            };
            if kind.is_some_and(|kind| kind != ScanKind::Equipment(equipment_kind))
                || only_unmatched && item.status == MigrationStatus::Matched
                || unlocated_only && item.old.is_some()
            {
                continue;
            }
            let mut pair = MigrationPair::item(equipment_kind.index() + 6, item.status);
            let sample =
                equipment::load_text(path, &item.source.name, item.sizes.0, EquipmentText::Name)?;
            pair.left = Some(thumbnail(render_text(&sample), 32));
            if let Some(old) = item.old {
                pair.right = Some(equipment_picture(previous, equipment_kind, old)?);
            }
            pair.source = unlocated_only.then(|| preset.id.clone());
            pairs.push(pair);
        }
        Ok(pairs)
    }

    fn equipment_pairs(
        &self,
        previous: &ItemCache,
        scanned: &ItemCache,
        kind: EquipmentKind,
        selection: &MigrationSelection,
    ) -> Result<Vec<MigrationPair>> {
        let status = self.catalog_status(ScanKind::Equipment(kind));
        let old = previous.equipment[kind.index()].as_ref();
        let new = scanned.equipment[kind.index()].as_ref();
        let mapping = if status == MigrationStatus::Matched {
            old.zip(new)
                .and_then(|(old, new)| replacement_names(old, new))
        } else {
            None
        };
        let mut pairs = Vec::new();
        if !selection.only_unmatched || status != MigrationStatus::Matched {
            if let Some(old) = old {
                for (category_index, category) in old.categories.iter().enumerate() {
                    let mut items = Vec::new();
                    let mut seen = BTreeSet::new();
                    for &name in &category.order {
                        let choice = EquipmentChoice {
                            category: category_index,
                            name,
                        };
                        let references = self.equipment_references(kind, choice);
                        if !selection.all_items && (references == 0 || !seen.insert(name)) {
                            continue;
                        }
                        let mut pair = MigrationPair::item(kind.index() + 6, status);
                        pair.left = Some(equipment_picture(previous, kind, choice)?);
                        pair.references = references;
                        if let Some(name) = mapping
                            .as_ref()
                            .and_then(|names| names[category_index][name])
                        {
                            pair.right = Some(equipment_picture(
                                scanned,
                                kind,
                                EquipmentChoice {
                                    category: category_index,
                                    name,
                                },
                            )?);
                        }
                        items.push(pair);
                    }
                    if !items.is_empty() {
                        let mut heading = MigrationPair::item(kind.index() + 6, status);
                        heading.is_category = true;
                        heading.left = Some(thumbnail(render_category(&category.title), 24));
                        if mapping.is_some() {
                            heading.right = new
                                .and_then(|catalog| catalog.categories.get(category_index))
                                .map(|category| thumbnail(render_category(&category.title), 24));
                        }
                        pairs.push(heading);
                        pairs.extend(items);
                    }
                }
            }
            if selection.all_items
                && mapping.is_none()
                && let Some(new) = new
            {
                for (category_index, category) in new.categories.iter().enumerate() {
                    let mut heading = MigrationPair::item(kind.index() + 6, status);
                    heading.is_category = true;
                    heading.right = Some(thumbnail(render_category(&category.title), 24));
                    pairs.push(heading);
                    for &name in &category.order {
                        let mut pair = MigrationPair::item(kind.index() + 6, status);
                        pair.right = Some(equipment_picture(
                            scanned,
                            kind,
                            EquipmentChoice {
                                category: category_index,
                                name,
                            },
                        )?);
                        pairs.push(pair);
                    }
                }
            }
        }
        Ok(pairs)
    }

    fn icon_pairs(
        &self,
        previous: &ItemCache,
        scanned: &ItemCache,
        kind: ItemKind,
        selection: &MigrationSelection,
    ) -> Result<Vec<MigrationPair>> {
        let scan_kind = if kind == ItemKind::Stratagem {
            ScanKind::Stratagems
        } else {
            ScanKind::Booster
        };
        let status = self.catalog_status(scan_kind);
        let mut pairs = Vec::new();
        if selection.only_unmatched && status == MigrationStatus::Matched {
            return Ok(pairs);
        }
        if let Some(old) = icon_catalog(previous, kind) {
            let mut category = None;
            for (index, item) in old.items.iter().enumerate() {
                let references = self.icon_references(kind, index);
                if !selection.all_items && references == 0 {
                    continue;
                }
                if item.icon.category != category {
                    category = item.icon.category;
                    let mut heading = MigrationPair::item(icon_slot(kind), status);
                    heading.is_category = true;
                    heading.category = category;
                    pairs.push(heading);
                }
                let mut pair = MigrationPair::item(icon_slot(kind), status);
                pair.left = Some(thumbnail(render_icon(&item.icon)?, 64));
                pair.references = references;
                if status == MigrationStatus::Matched {
                    pair.right = icon_catalog(scanned, kind)
                        .and_then(|catalog| catalog.items.get(index))
                        .map(|item| render_icon(&item.icon).map(|image| thumbnail(image, 64)))
                        .transpose()?;
                }
                pairs.push(pair);
            }
        }
        if selection.all_items
            && status != MigrationStatus::Matched
            && let Some(new) = icon_catalog(scanned, kind)
        {
            let mut category = None;
            for item in &new.items {
                if item.icon.category != category {
                    category = item.icon.category;
                    let mut heading = MigrationPair::item(icon_slot(kind), status);
                    heading.is_category = true;
                    heading.category = category;
                    pairs.push(heading);
                }
                let mut pair = MigrationPair::item(icon_slot(kind), status);
                pair.right = Some(thumbnail(render_icon(&item.icon)?, 64));
                pairs.push(pair);
            }
        }
        Ok(pairs)
    }

    fn equipment_references(&self, kind: EquipmentKind, choice: EquipmentChoice) -> usize {
        self.presets
            .iter()
            .filter(|preset| {
                preset.equipment[kind.index()]
                    .as_ref()
                    .is_some_and(|item| item.old == Some(choice))
            })
            .count()
    }

    fn icon_references(&self, kind: ItemKind, index: usize) -> usize {
        let slots = if kind == ItemKind::Stratagem {
            0..4
        } else {
            4..6
        };
        self.presets
            .iter()
            .filter(|preset| {
                slots.clone().any(|slot| {
                    preset.icons[slot]
                        .as_ref()
                        .is_some_and(|item| item.old == Some(index))
                })
            })
            .count()
    }
}

fn kind_counts(preset: &MigrationPreset, kind: ScanKind) -> (usize, usize) {
    match kind {
        ScanKind::Equipment(kind) => preset.equipment[kind.index()]
            .as_ref()
            .map_or((0, 0), |item| {
                (usize::from(item.status == MigrationStatus::Matched), 1)
            }),
        ScanKind::Stratagems | ScanKind::Booster => {
            let range = if kind == ScanKind::Stratagems {
                0..4
            } else {
                4..6
            };
            preset.icons[range]
                .iter()
                .flatten()
                .fold((0, 0), |(matched, total), item| {
                    (
                        matched + usize::from(item.status == MigrationStatus::Matched),
                        total + 1,
                    )
                })
        }
    }
}

fn catalog_count(cache: &ItemCache, kind: ScanKind) -> usize {
    match kind {
        ScanKind::Equipment(kind) => cache.equipment[kind.index()].as_ref().map_or(0, |catalog| {
            catalog
                .categories
                .iter()
                .map(|category| category.order.len())
                .sum()
        }),
        ScanKind::Stratagems => cache
            .stratagems
            .as_ref()
            .map_or(0, |catalog| catalog.items.len()),
        ScanKind::Booster => cache
            .boosters
            .as_ref()
            .map_or(0, |catalog| catalog.items.len()),
    }
}

fn equipment_picture(
    cache: &ItemCache,
    kind: EquipmentKind,
    choice: EquipmentChoice,
) -> Result<RgbaImage> {
    let sample = cache.equipment[kind.index()]
        .as_ref()
        .and_then(|catalog| catalog.categories.get(choice.category))
        .and_then(|category| category.names.get(choice.name))
        .context("migration equipment picture is not available")?;
    Ok(thumbnail(render_text(sample), 32))
}

fn icon_kind(slot: usize) -> (ItemKind, ScanKind) {
    if slot < 4 {
        (ItemKind::Stratagem, ScanKind::Stratagems)
    } else {
        (ItemKind::Booster, ScanKind::Booster)
    }
}

fn icon_slot(kind: ItemKind) -> usize {
    if kind == ItemKind::Stratagem { 0 } else { 4 }
}

fn thumbnail(image: RgbaImage, height: u32) -> RgbaImage {
    if image.height() <= height {
        return image;
    }
    let width =
        (u64::from(image.width()) * u64::from(height) / u64::from(image.height())).max(1) as u32;
    image::imageops::resize(&image, width, height, FilterType::Triangle)
}
