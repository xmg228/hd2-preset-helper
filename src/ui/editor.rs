//! Cached item choices stay in a UI draft until the worker commits them to a preset.
use std::rc::Rc;
use std::sync::mpsc::Sender;

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use super::{
    AppUi, CandidateCatalogRow, CandidateGroupRole, CandidateGroupRow, CandidateRow, PresetPanel,
    PresetRow, PreviewRole, PreviewRow, PreviewState, UiRequest, image,
};
use crate::{
    item::EquipmentKind,
    item_cache::{
        CachedChoices, EquipmentChoice, IconChoice,
        preview::{IconCandidate, ItemCandidates},
    },
    scan::ScanKind,
};

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    Rc::new(VecModel::from(rows)).into()
}

fn editable(window: &PresetPanel) -> bool {
    window.get_editor_open() && !window.get_editor_loading() && !window.get_editor_busy()
}

fn update_choice(window: &PresetPanel, index: usize, choice: CandidateRow) {
    let slots = window.get_editor_draft();
    let Some(mut slot) = slots.row_data(index) else {
        return;
    };
    slot.picture = choice.picture.clone();
    slot.state = if choice.item >= 0 {
        PreviewState::Ready
    } else {
        PreviewState::Missing
    };
    slot.error = Default::default();
    slots.set_row_data(index, slot);
    window.get_editor_choices().set_row_data(index, choice);
    window.set_editor_dirty(true);
    window.set_editor_error("".into());
    window.set_editor_can_save(
        window
            .get_editor_choices()
            .iter()
            .take(6)
            .all(|choice| choice.item == -1)
            || slots
                .iter()
                .take(4)
                .all(|slot| slot.state == PreviewState::Ready),
    );
    update_randomize_available(window);
}

fn candidates(catalogs: &ModelRc<CandidateCatalogRow>, slot: usize) -> Vec<CandidateRow> {
    let mut items = Vec::new();
    if let Some(catalog) = catalogs.row_data(slot) {
        for group in catalog.groups.iter() {
            items.extend(group.items.iter());
        }
    }
    items
}

fn has_fallback(window: &PresetPanel) -> bool {
    window
        .get_editor_draft()
        .row_data(5)
        .is_some_and(|slot| slot.state != PreviewState::Missing)
}

fn can_randomize(equipment: bool, catalogs: &ModelRc<CandidateCatalogRow>, fallback: bool) -> bool {
    let count = |slot| {
        catalogs.row_data(slot).map_or(0, |catalog| {
            catalog
                .groups
                .iter()
                .map(|group| group.items.row_count())
                .sum::<usize>()
        })
    };
    if equipment {
        (6..12).all(|slot| count(slot) > 0)
    } else {
        count(0) >= 4 && count(4) > usize::from(fallback)
    }
}

fn update_randomize_available(window: &PresetPanel) {
    window.set_editor_can_randomize_stratagems(can_randomize(
        false,
        &window.get_editor_catalogs(),
        has_fallback(window),
    ));
    window.set_editor_can_randomize_equipment(can_randomize(
        true,
        &window.get_editor_catalogs(),
        false,
    ));
}

fn random_choices(
    equipment: bool,
    catalogs: &ModelRc<CandidateCatalogRow>,
    fallback: bool,
) -> Option<Vec<CandidateRow>> {
    if !can_randomize(equipment, catalogs, fallback) {
        return None;
    }
    if equipment {
        return (6..12)
            .map(|slot| fastrand::choice(candidates(catalogs, slot)))
            .collect();
    }
    let mut stratagems = candidates(catalogs, 0);
    let mut boosters = candidates(catalogs, 4);
    fastrand::shuffle(&mut stratagems);
    fastrand::shuffle(&mut boosters);
    stratagems.truncate(4);
    boosters.truncate(if fallback { 2 } else { 1 });
    stratagems.extend(boosters);
    if !fallback {
        stratagems.push(CandidateRow {
            category: -2,
            item: -2,
            ..Default::default()
        });
    }
    Some(stratagems)
}

pub(super) fn connect(window: &PresetPanel, send: Sender<UiRequest>) {
    let edit = send.clone();
    window.on_edit_items(move || {
        let _ = edit.send(UiRequest::EditItems);
    });
    let scan = send.clone();
    window.on_scan_missing_items(move || {
        let _ = scan.send(UiRequest::ScanMissingItems);
    });
    let refresh = send.clone();
    let weak = window.as_weak();
    window.on_refresh_items(move || {
        let Some(window) = weak.upgrade() else { return };
        if !editable(&window) {
            return;
        }
        let kind = match window.get_editor_slot() {
            0..=3 => ScanKind::Stratagems,
            4..=5 => ScanKind::Booster,
            slot @ 6..=11 => ScanKind::Equipment(EquipmentKind::ALL[slot as usize - 6]),
            _ => return,
        };
        let _ = refresh.send(UiRequest::RefreshItems(kind));
    });
    let weak = window.as_weak();
    window.on_choose_item(move |category, item| {
        let Some(window) = weak.upgrade() else { return };
        if !editable(&window) {
            return;
        }
        let Some(candidate) = window
            .get_editor_catalogs()
            .row_data(window.get_editor_slot() as usize)
            .and_then(|catalog| catalog.groups.row_data(category as usize))
            .and_then(|group| group.items.iter().find(|candidate| candidate.item == item))
        else {
            return;
        };
        update_choice(&window, window.get_editor_slot() as usize, candidate);
    });
    let weak = window.as_weak();
    window.on_clear_item(move || {
        let Some(window) = weak.upgrade() else { return };
        if editable(&window) && (4..6).contains(&window.get_editor_slot()) {
            update_choice(
                &window,
                window.get_editor_slot() as usize,
                CandidateRow {
                    category: -2,
                    item: -2,
                    ..Default::default()
                },
            );
        }
    });
    let weak = window.as_weak();
    window.on_randomize_items(move |equipment| {
        let Some(window) = weak.upgrade() else { return };
        if !editable(&window) {
            return;
        }
        if let Some(choices) = random_choices(
            equipment,
            &window.get_editor_catalogs(),
            has_fallback(&window),
        ) {
            for (index, choice) in choices.into_iter().enumerate() {
                update_choice(&window, index + if equipment { 6 } else { 0 }, choice);
            }
        }
    });
    let weak = window.as_weak();
    window.on_save_items(move || {
        let Some(window) = weak.upgrade() else { return };
        if !editable(&window) || !window.get_editor_dirty() || !window.get_editor_can_save() {
            return;
        }
        let selected = window.get_editor_choices();
        let choices = CachedChoices {
            stratagems: std::array::from_fn(|index| {
                match selected.row_data(index).map(|choice| choice.item) {
                    Some(-2) => IconChoice::Clear,
                    Some(item) if item >= 0 => IconChoice::Item(item as usize),
                    _ => IconChoice::Keep,
                }
            }),
            equipment: std::array::from_fn(|index| {
                selected.row_data(index + 6).and_then(|choice| {
                    (choice.category >= 0 && choice.item >= 0).then_some(EquipmentChoice {
                        category: choice.category as usize,
                        name: choice.item as usize,
                    })
                })
            }),
        };
        window.set_editor_busy(true);
        let _ = send.send(UiRequest::SaveItemChoices {
            preset: window.get_editor_preset().to_string(),
            choices,
        });
    });
}

impl AppUi {
    pub fn is_editing_items(&self) -> bool {
        self.window.get_editor_open() || self.window.get_editor_busy()
    }

    pub fn open_item_editor(&self) -> bool {
        let Some(preset) = self
            .window
            .get_presets()
            .row_data(self.window.get_current_index() as usize)
        else {
            return false;
        };
        self.begin_item_editor(preset);
        true
    }

    pub(super) fn reset_item_editor_for_scan(&self) -> bool {
        let id = self.window.get_editor_preset();
        let Some(preset) = self.window.get_presets().iter().find(|row| row.name == id) else {
            return false;
        };
        let slot = self.window.get_editor_slot();
        self.begin_item_editor(preset);
        self.window.set_editor_slot(slot);
        true
    }

    fn begin_item_editor(&self, preset: PresetRow) {
        let mut slots = (0..6)
            .map(|index| PreviewRow {
                role: match index {
                    4 => PreviewRole::Booster,
                    5 => PreviewRole::FallbackBooster,
                    _ => PreviewRole::Stratagem,
                },
                number: if index < 4 { index + 1 } else { 0 },
                ..Default::default()
            })
            .collect::<Vec<_>>();
        for item in preset.stratagems.items.iter() {
            let index = match item.role {
                PreviewRole::Stratagem if (1..=4).contains(&item.number) => {
                    item.number as usize - 1
                }
                PreviewRole::Booster => 4,
                PreviewRole::FallbackBooster => 5,
                _ => continue,
            };
            slots[index] = item;
        }
        slots.extend(preset.equipment.items.iter());
        self.window.set_editor_can_save(true);
        self.window.set_editor_preset(preset.name);
        self.window.set_editor_draft(model(slots));
        self.window.set_editor_choices(model(vec![
            CandidateRow {
                category: -1,
                item: -1,
                ..Default::default()
            };
            12
        ]));
        self.window.set_editor_catalogs(Default::default());
        self.window.set_editor_can_randomize_stratagems(false);
        self.window.set_editor_can_randomize_equipment(false);
        self.window.set_editor_missing(false);
        self.window.set_editor_dirty(false);
        self.window.set_editor_slot(0);
        self.window.set_editor_error("".into());
        self.window.set_editor_loading(true);
        self.window.set_editor_open(true);
        self.window.invoke_focus_item_editor();
    }

    pub fn item_candidates_loaded(&self, result: Result<ItemCandidates, String>) {
        if !self.window.get_editor_open() {
            return;
        }
        self.window.set_editor_loading(false);
        let candidates = match result {
            Ok(candidates) => candidates,
            Err(error) => {
                self.window.set_editor_error(error.into());
                return;
            }
        };
        self.window.set_editor_missing(candidates.missing);
        let equipment = candidates
            .equipment
            .into_iter()
            .map(|categories| {
                let groups = categories.into_iter().map(|category| CandidateGroupRow {
                    title: image(&category.title),
                    role: CandidateGroupRole::Title,
                    items: model(
                        category
                            .items
                            .into_iter()
                            .map(|item| CandidateRow {
                                category: item.choice.category as i32,
                                item: item.choice.name as i32,
                                picture: image(&item.picture),
                            })
                            .collect(),
                    ),
                });
                CandidateCatalogRow {
                    groups: model(groups.collect()),
                }
            })
            .collect::<Vec<_>>();
        let icons = candidates.icons;
        let mut groups = Vec::new();
        for (role, items) in [
            CandidateGroupRole::Offensive,
            CandidateGroupRole::Supply,
            CandidateGroupRole::Defensive,
        ]
        .into_iter()
        .zip(icons.stratagems)
        {
            if !items.is_empty() {
                groups.push(icon_group(role, groups.len(), items));
            }
        }
        let stratagems = CandidateCatalogRow {
            groups: model(groups),
        };
        let boosters = CandidateCatalogRow {
            groups: model(if icons.boosters.is_empty() {
                Vec::new()
            } else {
                vec![icon_group(CandidateGroupRole::All, 0, icons.boosters)]
            }),
        };
        let mut catalogs = vec![
            stratagems.clone(),
            stratagems.clone(),
            stratagems.clone(),
            stratagems,
            boosters.clone(),
            boosters,
        ];
        catalogs.extend(equipment);
        self.window.set_editor_catalogs(model(catalogs));
        update_randomize_available(&self.window);
    }

    pub fn item_choices_saved(&self, preset: &str, result: &Result<(), String>) {
        self.window.set_editor_busy(false);
        if self.window.get_editor_preset() != preset {
            return;
        }
        match result {
            Ok(()) => {
                self.set_status("");
                self.window.invoke_close_dialogs();
            }
            Err(error) if self.window.get_editor_open() => {
                self.window.set_editor_error(error.as_str().into())
            }
            Err(error) => self.set_status(error),
        }
    }
}

fn icon_group(
    role: CandidateGroupRole,
    category: usize,
    items: Vec<IconCandidate>,
) -> CandidateGroupRow {
    CandidateGroupRow {
        role,
        items: model(
            items
                .into_iter()
                .map(|item| CandidateRow {
                    category: category as i32,
                    item: item.index as i32,
                    picture: image(&item.picture),
                })
                .collect(),
        ),
        ..Default::default()
    }
}
