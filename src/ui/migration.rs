//! Read-only migration comparisons; selection keys stay separate from display models.
use std::{rc::Rc, sync::mpsc::Sender};

use slint::{Model, VecModel};

use super::{
    AppUi, CandidateGroupRole, MigrationGroupRow, MigrationPairRow, MigrationPairState,
    MigrationPhase, PresetPanel, PreviewRole, UiRequest, equipment_role, image, preset_number,
    scan_role,
};
use crate::{
    item::{EquipmentKind, StratagemCategory},
    preset::{
        MigrationGroupKey, MigrationGroupSummary, MigrationPair, MigrationReport,
        MigrationSelection, MigrationStatus,
    },
};

#[derive(Default)]
pub(super) struct View {
    groups: Vec<MigrationGroupKey>,
    selection: Option<MigrationSelection>,
}

pub(super) fn connect(panel: &PresetPanel, send: Sender<UiRequest>) {
    let preview_send = send.clone();
    panel.on_load_migration_preview(move || {
        let _ = preview_send.send(UiRequest::LoadMigrationPreview);
    });
    panel.on_resolve_migration(move |choice| {
        let request = match choice {
            0 => UiRequest::ResolveMigration {
                migrate_presets: None,
            },
            1 => UiRequest::ResolveMigration {
                migrate_presets: Some(false),
            },
            2 => UiRequest::ResolveMigration {
                migrate_presets: Some(true),
            },
            3 => UiRequest::ContinueMigration,
            _ => return,
        };
        let _ = send.send(request);
    });
}

impl AppUi {
    pub fn show_migration(
        &mut self,
        report: std::result::Result<&MigrationReport, &str>,
        phase: MigrationPhase,
        groups: &[MigrationGroupSummary],
    ) {
        self.window.set_loading(false);
        self.window.set_migration_phase(phase);
        let empty = MigrationReport::default();
        let (report, error) = match report {
            Ok(report) => (report, ""),
            Err(error) => (&empty, error),
        };
        self.window.set_migration_error(error.into());
        self.window.set_migration_preview_error("".into());
        self.window
            .set_migration_preset_count(report.presets as i32);
        self.window.set_migration_item_count(report.items as i32);
        self.window
            .set_migration_unmatched_count(report.unmatched_items as i32);
        let current = self
            .window
            .get_presets()
            .row_data(self.window.get_current_index() as usize);
        let selected = groups.iter().position(|group| group.matched < group.total)
            .or_else(|| groups.iter().position(|group| group.status != MigrationStatus::Matched))
            .or_else(|| groups.iter().position(|group| matches!(
                &group.key, MigrationGroupKey::Preset(id) if current.as_ref().is_some_and(|preset| preset.name == id.as_str())
            )))
            .unwrap_or(0);
        let rows = groups
            .iter()
            .map(|group| {
                let (role, number, is_preset) = match &group.key {
                    MigrationGroupKey::Preset(id) => {
                        (PreviewRole::Stratagem, preset_number(id), true)
                    }
                    MigrationGroupKey::Catalog(kind) => (scan_role(*kind), "", false),
                };
                MigrationGroupRow {
                    label: group.label.as_str().into(),
                    number: number.into(),
                    role,
                    is_preset,
                    matched: group.matched as i32,
                    total: group.total as i32,
                    old_count: group.old_count as i32,
                    new_count: group.new_count as i32,
                    state: state(group.status),
                }
            })
            .collect::<Vec<_>>();
        self.migration_view = View {
            groups: groups.iter().map(|group| group.key.clone()).collect(),
            selection: None,
        };
        self.window
            .set_migration_groups(Rc::new(VecModel::from(rows)).into());
        self.window.set_migration_group_index(selected as i32);
        self.window.set_migration_all_items(false);
        self.window.set_migration_only_unmatched(false);
        self.window.set_migration_pairs(Default::default());
        self.window.set_migration_preview_loading(false);
        self.window.set_migration_open(true);
        self.window.invoke_focus_migration();
        if phase == MigrationPhase::Review {
            self.status.scan_review_ready();
        }
    }

    pub fn migration_preview_request(&mut self) -> Option<MigrationSelection> {
        if !self.window.get_migration_open() {
            return None;
        }
        let group = self
            .migration_view
            .groups
            .get(self.window.get_migration_group_index() as usize)?
            .clone();
        let selection = MigrationSelection {
            group,
            all_items: self.window.get_migration_all_items(),
            only_unmatched: self.window.get_migration_only_unmatched(),
        };
        if self.migration_view.selection.as_ref() == Some(&selection) {
            return None;
        }
        self.migration_view.selection = Some(selection.clone());
        self.window.set_migration_pairs(Default::default());
        self.window.set_migration_preview_error("".into());
        self.window.set_migration_preview_loading(true);
        Some(selection)
    }

    pub fn migration_preview_loaded(
        &self,
        selection: MigrationSelection,
        result: std::result::Result<Vec<MigrationPair>, String>,
    ) {
        if !self.window.get_migration_open()
            || self.migration_view.selection.as_ref() != Some(&selection)
        {
            return;
        }
        self.window.set_migration_preview_loading(false);
        match result {
            Ok(pairs) => {
                let rows = pairs
                    .into_iter()
                    .map(|pair| {
                        let (left, has_left) = picture(pair.left);
                        let (right, has_right) = picture(pair.right);
                        let preset_slot = self.window.get_migration_phase()
                            == MigrationPhase::Preflight
                            || pair.source.is_some();
                        let (role, number) = match pair.slot {
                            0..=3 => (
                                PreviewRole::Stratagem,
                                if preset_slot { pair.slot as i32 + 1 } else { 0 },
                            ),
                            4 => (PreviewRole::Booster, 0),
                            5 => (PreviewRole::FallbackBooster, 0),
                            slot => (equipment_role(EquipmentKind::ALL[slot - 6]), 0),
                        };
                        let label = pair
                            .source
                            .as_ref()
                            .and_then(|id| self.preset_info.iter().find(|preset| preset.id == *id))
                            .map_or("", |preset| preset.label.as_str());
                        MigrationPairRow {
                            role,
                            number,
                            left,
                            right,
                            has_left,
                            has_right,
                            state: state(pair.status),
                            references: pair.references as i32,
                            is_category: pair.is_category,
                            category: match pair.category {
                                Some(StratagemCategory::Offensive) => CandidateGroupRole::Offensive,
                                Some(StratagemCategory::Supply) => CandidateGroupRole::Supply,
                                Some(StratagemCategory::Defensive) => CandidateGroupRole::Defensive,
                                None if pair.is_category => CandidateGroupRole::Title,
                                None => CandidateGroupRole::All,
                            },
                            source: pair.source.is_some(),
                            source_label: label.into(),
                            source_number: pair.source.as_deref().map_or("", preset_number).into(),
                        }
                    })
                    .collect::<Vec<_>>();
                self.window
                    .set_migration_pairs(Rc::new(VecModel::from(rows)).into());
            }
            Err(error) => self.window.set_migration_preview_error(error.into()),
        }
    }

    pub fn dismiss_migration(&mut self) {
        self.migration_view = View::default();
        self.window.set_migration_open(false);
        self.window.set_migration_pairs(Default::default());
        self.window.set_migration_groups(Default::default());
        self.window.invoke_focus_current_view();
    }
}

fn state(status: MigrationStatus) -> MigrationPairState {
    match status {
        MigrationStatus::Matched => MigrationPairState::Matched,
        MigrationStatus::Unmatched => MigrationPairState::Unmatched,
        MigrationStatus::Missing => MigrationPairState::Missing,
        MigrationStatus::Incompatible => MigrationPairState::Incompatible,
        MigrationStatus::Duplicate => MigrationPairState::Duplicate,
    }
}

fn picture(picture: Option<image::RgbaImage>) -> (slint::Image, bool) {
    match picture.filter(|picture| picture.width() != 0 && picture.height() != 0) {
        Some(picture) => (image(&picture), true),
        None => (Default::default(), false),
    }
}
