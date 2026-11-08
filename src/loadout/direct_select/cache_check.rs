//! Check a saved list layout against observations in the current map coordinates.
use anyhow::Result;
use serde::Serialize;

use crate::{
    item::ItemKind,
    item_cache::{IconCatalog, IconPosition},
    vision::{IconTemplate, icon_cosine_errors},
};

use super::{list_map::ListMap, page_navigation::PageSnapshot};

const ANCHOR_ERROR: f32 = 0.16;
const ANCHOR_MARGIN: f32 = 0.04;
const CHANGED_ERROR: f32 = 0.30;
// In list-icon widths; comfortably below the distance between rows.
const POSITION_TOLERANCE: f32 = 0.12;

#[derive(Default)]
pub(super) struct CacheCheck {
    offset_y: Option<f32>,
}

#[derive(Serialize)]
pub(super) struct CacheAssessment {
    pub(super) changed: bool,
    pub(super) reason: &'static str,
    anchors: usize,
    conflicts: usize,
    offset_y: Option<f32>,
}

struct ObservedIcon {
    position: IconPosition,
    clear: bool,
}

#[derive(Default)]
struct Boundaries {
    top: Option<f32>,
    bottom: Option<IconPosition>,
    total: Option<usize>,
}

impl CacheCheck {
    pub(super) fn inspect(
        &mut self,
        kind: ItemKind,
        catalog: &IconCatalog,
        map: &ListMap,
        page: &PageSnapshot,
    ) -> Result<CacheAssessment> {
        let top = page.roi.scrollbar.is_some_and(|bar| bar.at_top);
        let bottom = page.roi.scrollbar.is_some_and(|bar| bar.at_bottom);
        let first_row = page.slot_samples.iter().map(|slot| slot.row).min();
        let last_row = page.slot_samples.iter().map(|slot| slot.row).max();
        let mut observed = Vec::new();
        let mut samples = Vec::new();
        for slot in &page.slot_samples {
            let Some(sample) = slot.native.as_deref() else {
                continue;
            };
            observed.push(ObservedIcon {
                position: IconPosition {
                    col: slot.col,
                    y: map.content_y(slot.page_y) / sample.geometry.physical_size,
                },
                clear: (top || Some(slot.row) != first_row)
                    && (bottom || Some(slot.row) != last_row)
                    || page.roi.scrollbar.is_none()
                        && last_row
                            .zip(first_row)
                            .is_some_and(|(last, first)| last - first < 2),
            });
            samples.push(sample);
        }
        let size = samples.first().map(|sample| sample.geometry.physical_size);
        let extent = |last: bool| {
            let y = page
                .slot_samples
                .iter()
                .map(|slot| map.content_y(slot.page_y));
            if last {
                y.max_by(f32::total_cmp)
            } else {
                y.min_by(f32::total_cmp)
            }
        };
        let boundaries = Boundaries {
            top: top.then(|| Some(extent(false)? / size?)).flatten(),
            bottom: bottom
                .then(|| {
                    Some(IconPosition {
                        y: extent(true)? / size?,
                        col: page
                            .slot_samples
                            .iter()
                            .filter(|slot| Some(slot.row) == last_row)
                            .map(|slot| slot.col)
                            .max()?,
                    })
                })
                .flatten(),
            total: (map.boundaries() == (true, true)).then(|| map.scan_count()),
        };
        let templates = catalog
            .items
            .iter()
            .map(|item| &*item.icon as &IconTemplate)
            .collect::<Vec<_>>();
        let scores = icon_cosine_errors(kind, &templates, &samples)?;
        let positions = catalog
            .items
            .iter()
            .map(|item| item.position)
            .collect::<Vec<_>>();
        Ok(self.assess(&positions, &observed, &scores, boundaries))
    }

    fn assess(
        &mut self,
        cached: &[IconPosition],
        observed: &[ObservedIcon],
        scores: &[Vec<f32>],
        boundaries: Boundaries,
    ) -> CacheAssessment {
        let mut result = CacheAssessment {
            changed: false,
            reason: "insufficient_anchors",
            anchors: 0,
            conflicts: 0,
            offset_y: self.offset_y,
        };
        if boundaries.total.is_some_and(|total| total != cached.len())
            || cached.is_empty() && !observed.is_empty()
        {
            result.changed = true;
            result.reason = "item_count_changed";
            return result;
        }

        // Find identities without a column restriction, so a changed column is
        // evidence rather than a reason to silently choose a different item.
        let mut anchors = observed
            .iter()
            .zip(scores)
            .enumerate()
            .filter_map(|(query, (slot, row))| {
                let (index, &best) = row.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1))?;
                let second = row
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != index)
                    .map(|(_, error)| *error)
                    .fold(f32::INFINITY, f32::min);
                (slot.clear && best <= ANCHOR_ERROR && second - best >= ANCHOR_MARGIN)
                    .then_some((query, index))
            })
            .collect::<Vec<_>>();
        let mut counts = vec![0; cached.len()];
        for &(_, index) in &anchors {
            counts[index] += 1;
        }
        anchors.retain(|&(_, index)| counts[index] == 1);
        result.anchors = anchors.len();
        let offsets = anchors
            .iter()
            .filter_map(|&(query, index)| {
                let position = observed[query].position;
                (position.col == cached[index].col).then_some(position.y - cached[index].y)
            })
            .collect::<Vec<_>>();

        if self.offset_y.is_none() {
            let support = |offset: f32| {
                offsets
                    .iter()
                    .filter(|&&other| (other - offset).abs() <= POSITION_TOLERANCE)
                    .count()
            };
            self.offset_y = offsets
                .iter()
                .copied()
                .map(|offset| (offset, support(offset)))
                .max_by_key(|&(_, count)| count)
                .filter(|&(_, count)| count >= 2)
                .map(|(offset, _)| offset);
        }
        result.offset_y = self.offset_y;
        result.conflicts = anchors
            .iter()
            .filter(|&&(query, index)| {
                let position = observed[query].position;
                position.col != cached[index].col
                    || self.offset_y.is_some_and(|offset| {
                        (position.y - cached[index].y - offset).abs() > POSITION_TOLERANCE
                    })
            })
            .count();
        if result.conflicts >= 2 {
            result.changed = true;
            result.reason = "anchors_disagree_with_layout";
            return result;
        }
        let Some(offset) = self.offset_y else {
            return result;
        };

        let expected = |position: IconPosition| {
            cached.iter().position(|old| {
                old.col == position.col && (old.y + offset - position.y).abs() <= POSITION_TOLERANCE
            })
        };
        let extra = observed
            .iter()
            .filter(|slot| slot.clear && expected(slot.position).is_none())
            .count();
        let different = observed
            .iter()
            .zip(scores)
            .filter(|(slot, row)| {
                slot.clear
                    && expected(slot.position).is_some_and(|index| row[index] >= CHANGED_ERROR)
            })
            .count();
        let last_y = cached
            .iter()
            .map(|position| position.y)
            .max_by(f32::total_cmp);
        let last_col = last_y.and_then(|y| {
            cached
                .iter()
                .filter(|position| (position.y - y).abs() <= POSITION_TOLERANCE)
                .map(|position| position.col)
                .max()
        });
        let boundary_changed = boundaries
            .top
            .is_some_and(|y| (y - offset).abs() > POSITION_TOLERANCE)
            || boundaries.bottom.is_some_and(|position| {
                last_y.is_some_and(|y| (position.y - offset - y).abs() > POSITION_TOLERANCE)
                    || last_col != Some(position.col)
            });
        result.reason = if boundary_changed {
            "list_boundary_changed"
        } else if extra > 0 {
            "unexpected_slot_position"
        } else if different >= 2 {
            "expected_items_changed"
        } else {
            "layout_consistent"
        };
        result.changed = boundary_changed || extra > 0 || different >= 2;
        result.conflicts += extra + different;
        result
    }
}
