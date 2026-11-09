//! Verify cached routes; uncertain observations get a bounded refresh or row probe.
use anyhow::{Result, ensure};
use serde::Serialize;

use super::{
    EquipmentItem, InputAction, NAME_MATCH_THRESHOLD, Navigation,
    cache::{CachedList, Position, Target},
    row::{RowScan, RowStep},
};
use crate::{
    input::Key,
    item::EquipmentKind,
    item_cache::EquipmentCatalog,
    vision::{equipment::EquipmentObservation, text::TextTemplate},
};

const MAX_VISITS: usize = 1024;
const MAX_REOBSERVATIONS: usize = 2;
const MAX_PROBE_ROWS: usize = 3;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Cached,
    Locating,
}

#[derive(Clone, Serialize)]
pub(super) struct Visit {
    pub index: usize,
    phase: Phase,
    pub row: usize,
    pub col: usize,
    pub columns: usize,
    pub target_cosine: f32,
    pub category_cosine: Option<f32>,
}

pub(super) enum Next {
    Observe,
    Input(InputAction),
    Found,
    Rebuild,
}

#[derive(Default)]
struct RowProbe {
    row: Option<(usize, RowScan<TextTemplate>)>,
    candidates: Option<Vec<Position>>,
    completed: usize,
    resampled: bool,
}

pub(super) struct Search<'a> {
    target: &'a EquipmentItem,
    list: CachedList<'a>,
    selected: Option<Target>,
    position: Option<Position>,
    expected: Option<Position>,
    probe: Option<RowProbe>,
    reobservations: usize,
    visits: usize,
}

impl<'a> Search<'a> {
    pub fn new(
        kind: EquipmentKind,
        target: &'a EquipmentItem,
        catalog: &'a EquipmentCatalog,
        position: Option<Position>,
    ) -> Self {
        let list = CachedList::new(kind, catalog);
        let selected = list.target(target.name.signature());
        Self {
            target,
            list,
            selected,
            position: None,
            expected: position,
            probe: None,
            reobservations: 0,
            visits: 0,
        }
    }

    pub fn advance(
        &mut self,
        page: &EquipmentObservation,
        navigation: Option<Navigation>,
    ) -> Result<(Visit, Next)> {
        ensure!(
            self.visits < MAX_VISITS,
            "cached equipment navigation exceeded {MAX_VISITS} observations"
        );
        self.visits += 1;
        if let Some(input) = navigation {
            self.reobservations = 0;
            if input.retried {
                self.position = None;
                self.expected = None;
                self.probe = None;
            } else if self.probe.is_none() {
                self.expected = self
                    .position
                    .and_then(|position| self.list.predict(position, input.action));
                self.position = None;
            }
        }
        ensure!(
            page.ready() || self.reobservations < MAX_REOBSERVATIONS,
            "equipment page remained unreadable while locating the cached focus"
        );
        let score = page.name.cosine(&self.target.name);
        let next = self.next(page, score);
        let focus = page.focus.and_then(|index| page.slots.get(index));
        let visit = Visit {
            index: self.visits,
            phase: if self.probe.is_some() || self.position.is_none() {
                Phase::Locating
            } else {
                Phase::Cached
            },
            row: focus.map_or(0, |slot| slot.row),
            col: focus.map_or(0, |slot| slot.col),
            columns: focus.map_or(0, |focus| {
                page.slots
                    .iter()
                    .filter(|slot| slot.row == focus.row)
                    .count()
            }),
            target_cosine: score,
            category_cosine: self
                .target
                .category
                .as_ref()
                .map(|title| page.category.cosine(title)),
        };
        tracing::debug!(
            visit = self.visits,
            phase = ?visit.phase,
            position = ?self.position,
            target_cosine = score,
            next = match next { Next::Observe => "observe", Next::Input(action) => action.name(), Next::Found => "found", Next::Rebuild => "rebuild" },
            "cached equipment observation"
        );
        Ok((visit, next))
    }

    fn next(&mut self, page: &EquipmentObservation, score: f32) -> Next {
        if !page.ready() {
            return self.reobserve().unwrap_or(Next::Rebuild);
        }
        // A unique, already focused target needs no position lookup or navigation.
        if self.visits == 1
            && score >= NAME_MATCH_THRESHOLD
            && self.selected.is_some()
            && self.list.target(page.name.signature()) == self.selected
        {
            return Next::Found;
        }
        if self.selected.is_none() {
            return Next::Rebuild;
        }
        if self.probe.is_some() {
            return self.probe_row(page, score);
        }
        let located = match self.expected.or(self.position) {
            Some(position) => self.list.matches(position, page).then_some(position),
            None => self.list.locate(page),
        };
        if let Some(position) = located {
            self.position = Some(position);
            self.expected = None;
            return self.route(score);
        }
        if let Some(next) = self.reobserve() {
            return next;
        }
        self.probe = Some(RowProbe::default());
        self.probe_row(page, score)
    }

    fn reobserve(&mut self) -> Option<Next> {
        if self.reobservations >= MAX_REOBSERVATIONS {
            return None;
        }
        self.reobservations += 1;
        Some(Next::Observe)
    }

    fn probe_row(&mut self, page: &EquipmentObservation, score: f32) -> Next {
        let focus = &page.slots[page.focus.unwrap()];
        let col = focus.col;
        let width = page
            .slots
            .iter()
            .filter(|slot| slot.row == focus.row)
            .count();
        let probe = self.probe.as_mut().unwrap();
        let (landing_col, sampling) = probe
            .row
            .get_or_insert_with(|| (col, RowScan::new(width, col)));
        let names = match sampling.advance(width, col, page.name.to_template()) {
            Ok(RowStep::Column(col)) => return Next::Input(InputAction::ClickColumn(col)),
            Ok(RowStep::Complete(names)) => names,
            Err(()) => return Next::Rebuild,
        };
        let mut positions = self.list.locate_row(&page.category, &names, col);
        if let Some(candidates) = &probe.candidates {
            positions.retain(|position| {
                candidates.iter().any(|candidate| {
                    candidate.category == position.category && candidate.row == position.row
                })
            });
        }
        if positions.len() == 1 {
            let position = positions[0];
            if self.expected.is_some_and(|expected| {
                expected.category != position.category || expected.row != position.row
            }) {
                return Next::Rebuild;
            }
            self.position = Some(position);
            self.expected = None;
            self.probe = None;
            self.reobservations = 0;
            return self.route(score);
        }
        // The landing observation triggered this probe and may be its only stale
        // sample. Re-read it once after the other columns, before rebuilding.
        if positions.is_empty() && !probe.resampled {
            probe.resampled = true;
            return if col == *landing_col {
                Next::Observe
            } else {
                Next::Input(InputAction::ClickColumn(*landing_col))
            };
        }
        probe.completed += 1;
        if positions.is_empty() || probe.completed >= MAX_PROBE_ROWS {
            return Next::Rebuild;
        }
        let action = InputAction::Key(Key::Down);
        probe.candidates = Some(
            positions
                .into_iter()
                .filter_map(|position| self.list.predict(position, action))
                .collect(),
        );
        probe.row = None;
        probe.resampled = false;
        self.expected = self.expected.and_then(|mut position| {
            position.col = col;
            self.list.predict(position, action)
        });
        Next::Input(action)
    }

    fn route(&mut self, score: f32) -> Next {
        let position = self.position.unwrap();
        let target = self.selected.unwrap();
        if self.list.is_target(position, target) {
            if score >= NAME_MATCH_THRESHOLD {
                Next::Found
            } else {
                self.reobserve().unwrap_or(Next::Rebuild)
            }
        } else {
            self.list
                .next(position, target)
                .map(Next::Input)
                .unwrap_or(Next::Rebuild)
        }
    }
}
