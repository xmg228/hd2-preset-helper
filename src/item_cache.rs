//! Captured items and complete list orders, shared by learning and manual scans.
mod equipment;
mod icons;
pub(crate) mod preview;
mod storage;

pub(crate) use equipment::NAME_MATCH_THRESHOLD;

use serde::{Deserialize, Serialize};

use crate::{
    item::{EquipmentKind, ItemKind},
    vision::{
        IconSample,
        text::{TextSample, TextTemplate},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EquipmentChoice {
    pub category: usize,
    pub name: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IconChoice {
    Keep,
    Clear,
    Item(usize),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CachedChoices {
    pub stratagems: [IconChoice; 6],
    pub equipment: [Option<EquipmentChoice>; 6],
}

#[derive(Clone)]
pub(crate) struct EquipmentItem {
    pub name: TextTemplate,
    pub category: Option<TextTemplate>,
    pub client_size: (u32, u32),
}

pub(crate) type EquipmentSet = [Option<EquipmentItem>; 6];

#[derive(Clone)]
pub(crate) struct EquipmentCategory {
    pub title: TextSample,
    pub names: Vec<TextSample>,
    pub order: Vec<usize>,
}

#[derive(Clone)]
pub(crate) struct EquipmentCatalog {
    pub client_size: (u32, u32),
    pub categories: Vec<EquipmentCategory>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) struct IconPosition {
    pub col: u32,
    /// List `physical_size` units from the first row, including the no-Booster option.
    pub y: f32,
}

#[derive(Clone)]
pub(crate) struct CachedIcon {
    pub icon: IconSample,
    pub position: IconPosition,
}

#[derive(Clone)]
pub(crate) struct IconCatalog {
    pub items: Vec<CachedIcon>,
}

#[derive(Default)]
pub(crate) struct ItemCache {
    pub equipment: [Option<EquipmentCatalog>; 6],
    pub stratagems: Option<IconCatalog>,
    pub boosters: Option<IconCatalog>,
    dirty: bool,
}

impl ItemCache {
    pub fn set_equipment(&mut self, kind: EquipmentKind, catalog: EquipmentCatalog) {
        self.equipment[kind.index()] = Some(catalog);
        self.dirty = true;
    }

    pub fn set_icons(&mut self, kind: ItemKind, catalog: IconCatalog) {
        match kind {
            ItemKind::Stratagem => self.stratagems = Some(catalog),
            ItemKind::Booster => self.boosters = Some(catalog),
        }
        self.dirty = true;
    }

    pub fn commit_equipment(
        &mut self,
        path: &std::path::Path,
        kind: EquipmentKind,
        catalog: EquipmentCatalog,
    ) -> anyhow::Result<()> {
        let previous = self.equipment[kind.index()].replace(catalog);
        let dirty = self.dirty;
        self.dirty = true;
        if let Err(error) = self.save_if_changed(path) {
            self.equipment[kind.index()] = previous;
            self.dirty = dirty;
            return Err(error);
        }
        Ok(())
    }

    pub fn commit_icons(
        &mut self,
        path: &std::path::Path,
        kind: ItemKind,
        catalog: IconCatalog,
    ) -> anyhow::Result<()> {
        let field = match kind {
            ItemKind::Stratagem => &mut self.stratagems,
            ItemKind::Booster => &mut self.boosters,
        };
        let previous = field.replace(catalog);
        let dirty = self.dirty;
        self.dirty = true;
        if let Err(error) = self.save_if_changed(path) {
            match kind {
                ItemKind::Stratagem => self.stratagems = previous,
                ItemKind::Booster => self.boosters = previous,
            }
            self.dirty = dirty;
            return Err(error);
        }
        Ok(())
    }

    /// Publish scanned lists together, restoring the active cache if saving fails.
    pub(crate) fn commit_scanned(
        &mut self,
        path: &std::path::Path,
        mut scanned: Self,
    ) -> anyhow::Result<()> {
        let equipment = scanned.equipment.each_ref().map(Option::is_some);
        let stratagems = scanned.stratagems.is_some();
        let boosters = scanned.boosters.is_some();
        let swap = |cache: &mut Self, scanned: &mut Self| {
            for ((current, next), selected) in cache
                .equipment
                .iter_mut()
                .zip(&mut scanned.equipment)
                .zip(equipment)
            {
                if selected {
                    std::mem::swap(current, next);
                }
            }
            if stratagems {
                std::mem::swap(&mut cache.stratagems, &mut scanned.stratagems);
            }
            if boosters {
                std::mem::swap(&mut cache.boosters, &mut scanned.boosters);
            }
        };
        let dirty = self.dirty;
        swap(self, &mut scanned);
        self.dirty = true;
        if let Err(error) = self.save_if_changed(path) {
            swap(self, &mut scanned);
            self.dirty = dirty;
            return Err(error);
        }
        Ok(())
    }
}
