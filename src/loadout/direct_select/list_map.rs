use std::collections::HashMap;

use tracing::debug;

use crate::item::ItemKind;
use crate::vision::RoiObservation;

use super::ScrollDirection;

#[derive(Clone, Copy, Debug)]
struct GridPosition {
    row: i32,
    col: u32,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum NavigationHint {
    Scroll(ScrollDirection),
    Recenter(ScrollDirection),
    ExpectedVisible,
}

pub(super) struct ListMap {
    row_base: i32,
    items: HashMap<String, GridPosition>,
}

impl ListMap {
    pub(super) fn new(page: &RoiObservation, item_kind: ItemKind) -> Self {
        let mut map = Self {
            row_base: 0,
            items: HashMap::new(),
        };
        map.record_current(page, item_kind);
        map
    }

    pub(super) fn relocalize(&mut self, page: &RoiObservation, item_kind: ItemKind) -> bool {
        let candidates = page
            .slots
            .iter()
            .filter(|slot| slot.kind.is_selectable_item_for(item_kind))
            .filter_map(|slot| {
                let item_id = &slot.classification.as_ref()?.item_id;
                let mapped = self.items.get(item_id)?;
                (mapped.col == slot.col).then_some(mapped.row - slot.row as i32)
            })
            .collect::<Vec<_>>();
        let Some(row_base) = dominant_value(&candidates) else {
            return false;
        };

        self.row_base = row_base;
        self.record_current(page, item_kind);
        debug!(
            row_base,
            alignment_support = candidates
                .iter()
                .filter(|candidate| **candidate == row_base)
                .count(),
            landmarks = self.items.len(),
            "temporary list map relocalized"
        );
        true
    }

    pub(super) fn advance(
        &mut self,
        previous: &RoiObservation,
        current: &RoiObservation,
        item_kind: ItemKind,
        direction: ScrollDirection,
    ) {
        if self.relocalize(current, item_kind) {
            return;
        }

        let Some((previous_min, previous_max)) = local_row_range(previous, item_kind) else {
            return;
        };
        let Some((current_min, current_max)) = local_row_range(current, item_kind) else {
            return;
        };
        self.row_base = match direction {
            ScrollDirection::Down => self.row_base + previous_max + 1 - current_min,
            ScrollDirection::Up => self.row_base + previous_min - 1 - current_max,
        };
        self.record_current(current, item_kind);
        debug!(
            row_base = self.row_base,
            ?direction,
            landmarks = self.items.len(),
            "temporary list map extended without a shared landmark"
        );
    }

    pub(super) fn record_current(&mut self, page: &RoiObservation, item_kind: ItemKind) {
        for slot in page
            .slots
            .iter()
            .filter(|slot| slot.kind.is_selectable_item_for(item_kind))
        {
            let Some(classification) = &slot.classification else {
                continue;
            };
            self.items.insert(
                classification.item_id.clone(),
                GridPosition {
                    row: self.row_base + slot.row as i32,
                    col: slot.col,
                },
            );
        }
    }

    pub(super) fn navigation_hint(
        &self,
        item_id: &str,
        page: &RoiObservation,
        item_kind: ItemKind,
    ) -> NavigationHint {
        let Some(target) = self.items.get(item_id) else {
            return NavigationHint::Scroll(ScrollDirection::Down);
        };
        let Some((local_min, local_max)) = local_row_range(page, item_kind) else {
            return NavigationHint::ExpectedVisible;
        };
        let visible_min = self.row_base + local_min;
        let visible_max = self.row_base + local_max;

        if target.row < visible_min {
            NavigationHint::Scroll(ScrollDirection::Up)
        } else if target.row > visible_max {
            NavigationHint::Scroll(ScrollDirection::Down)
        } else if local_min != local_max && target.row == visible_min {
            NavigationHint::Recenter(ScrollDirection::Up)
        } else if local_min != local_max && target.row == visible_max {
            NavigationHint::Recenter(ScrollDirection::Down)
        } else {
            NavigationHint::ExpectedVisible
        }
    }
}

fn local_row_range(page: &RoiObservation, item_kind: ItemKind) -> Option<(i32, i32)> {
    let mut rows = page
        .slots
        .iter()
        .filter(|slot| slot.kind.is_selectable_item_for(item_kind))
        .map(|slot| slot.row as i32);
    let first = rows.next()?;
    Some(rows.fold((first, first), |(min, max), row| {
        (min.min(row), max.max(row))
    }))
}

fn dominant_value(values: &[i32]) -> Option<i32> {
    values
        .iter()
        .copied()
        .max_by_key(|candidate| values.iter().filter(|value| **value == *candidate).count())
}
