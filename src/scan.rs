//! Complete item-list collection, independent of preset selection.
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::{
    automation::{AutomationSession, Cancellation},
    capture::CaptureSessionManager,
    evidence::OperationEvidence,
    item::{EquipmentKind, ItemKind},
    item_cache::{EquipmentCatalog, IconCatalog, ItemCache},
    loadout::{
        bind_loadout_region, equipment,
        pages::{Page, show_page},
        scan_icon_list, scan_loadout_home,
    },
    vision::RecognizerRuntime,
    window::WindowIdentity,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ScanKind {
    Stratagems,
    Booster,
    Equipment(EquipmentKind),
}

impl ScanKind {
    pub const ALL: [Self; 8] = [
        Self::Stratagems,
        Self::Booster,
        Self::Equipment(EquipmentKind::Helmet),
        Self::Equipment(EquipmentKind::Armor),
        Self::Equipment(EquipmentKind::Cape),
        Self::Equipment(EquipmentKind::Primary),
        Self::Equipment(EquipmentKind::Secondary),
        Self::Equipment(EquipmentKind::Throwable),
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Stratagems => "stratagems",
            Self::Booster => "booster",
            Self::Equipment(kind) => kind.label(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) enum ScanScope {
    All,
    One(ScanKind),
    Missing,
}

impl ScanScope {
    pub fn from_index(index: i32) -> Option<Self> {
        if index == 0 {
            Some(Self::All)
        } else {
            usize::try_from(index.checked_sub(1)?)
                .ok()
                .and_then(|i| ScanKind::ALL.get(i))
                .copied()
                .map(Self::One)
        }
    }

    pub(crate) fn kinds(self, cache: &ItemCache) -> Vec<ScanKind> {
        match self {
            Self::All => ScanKind::ALL.to_vec(),
            Self::One(kind) => vec![kind],
            Self::Missing => ScanKind::ALL
                .into_iter()
                .filter(|kind| match kind {
                    ScanKind::Stratagems => cache.stratagems.is_none(),
                    ScanKind::Booster => cache.boosters.is_none(),
                    ScanKind::Equipment(kind) => cache.equipment[kind.index()].is_none(),
                })
                .collect(),
        }
    }
}

pub(crate) struct ScanProgress {
    pub kind: ScanKind,
    pub category: Option<image::RgbaImage>,
    pub items: usize,
}

pub(crate) enum ScannedList {
    Equipment(EquipmentKind, EquipmentCatalog),
    Icons(ItemKind, IconCatalog),
}

impl ScannedList {
    pub(crate) fn store(self, cache: &mut ItemCache) {
        match self {
            Self::Equipment(kind, catalog) => cache.set_equipment(kind, catalog),
            Self::Icons(kind, catalog) => cache.set_icons(kind, catalog),
        }
    }

    pub(crate) fn commit(self, cache: &mut ItemCache, path: &Path) -> Result<()> {
        match self {
            Self::Equipment(kind, catalog) => cache.commit_equipment(path, kind, catalog),
            Self::Icons(kind, catalog) => cache.commit_icons(path, kind, catalog),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Equipment(_, catalog) => catalog.categories.iter().map(|c| c.order.len()).sum(),
            Self::Icons(_, catalog) => catalog.items.len(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ScanSummary {
    pub completed: Vec<ScanKind>,
    pub items: usize,
    pub migrated_items: Option<usize>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute(
    runtime: &RecognizerRuntime,
    capture_session: &mut CaptureSessionManager,
    expected_window: WindowIdentity,
    kinds: &[ScanKind],
    cancel: &Cancellation,
    mut progress: impl FnMut(ScanProgress),
    evidence: &OperationEvidence,
    mut accept: impl FnMut(ScannedList) -> Result<()>,
) -> Result<ScanSummary> {
    cancel.check()?;
    let Some(&first) = kinds.first() else {
        return Ok(ScanSummary::default());
    };
    let prepared = crate::automation::prepare_game(capture_session, expected_window)?;
    let target = prepared.target;
    let capture = prepared.source;
    let colors = prepared.colors;
    evidence.record("scan-context", None, serde_json::json!({"kinds": kinds, "client_size": target.client_size(), "game_colors": colors}));
    let first_page = page_for(first);
    let origin = show_page(
        capture, runtime, &target, colors, first_page, evidence, cancel,
    )?;
    let mut home = first_page;
    let mut summary = ScanSummary::default();
    let result = (|| -> Result<()> {
        for &kind in kinds {
            cancel.check()?;
            let page = page_for(kind);
            if home != page {
                show_page(capture, runtime, &target, colors, page, evidence, cancel)?;
                home = page;
            }
            progress(ScanProgress {
                kind,
                category: None,
                items: 0,
            });
            let list = match kind {
                ScanKind::Equipment(equipment_kind) => {
                    let categories = equipment::with_session(
                        capture,
                        target.clone(),
                        colors,
                        evidence,
                        cancel.clone(),
                        |session| session.scan(equipment_kind, &mut progress),
                    )?;
                    ScannedList::Equipment(equipment_kind, categories)
                }
                ScanKind::Stratagems | ScanKind::Booster => {
                    let bound = bind_loadout_region(capture, runtime.calibration(), colors)?;
                    let recognizer = runtime.bind(bound.geometry);
                    let mut automation = AutomationSession::new(
                        bound.region,
                        target.clone(),
                        evidence,
                        cancel.clone(),
                    )?;
                    let observation = scan_loadout_home(&mut automation, recognizer)?;
                    let item_kind = if kind == ScanKind::Stratagems {
                        ItemKind::Stratagem
                    } else {
                        ItemKind::Booster
                    };
                    let items = scan_icon_list(
                        recognizer,
                        &mut automation,
                        item_kind,
                        observation,
                        &mut progress,
                    )?;
                    ScannedList::Icons(item_kind, items)
                }
            };
            let count = list.len();
            cancel.check()?;
            accept(list).with_context(|| format!("failed to accept scanned {}", kind.label()))?;
            summary.completed.push(kind);
            summary.items += count;
            tracing::info!(
                kind = kind.label(),
                items = count,
                completed = summary.completed.len(),
                "item list scanned"
            );
        }
        cancel.check()?;
        if home != origin {
            show_page(capture, runtime, &target, colors, origin, evidence, cancel)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        return Err(if summary.completed.is_empty() {
            error
        } else {
            error.context(format!(
                "{} item types were scanned before scanning stopped ({} items)",
                summary.completed.len(),
                summary.items
            ))
        });
    }
    Ok(summary)
}

fn page_for(kind: ScanKind) -> Page {
    match kind {
        ScanKind::Equipment(_) => Page::Equipment,
        _ => Page::Stratagems,
    }
}
