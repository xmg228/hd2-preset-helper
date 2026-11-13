//! Equipment catalog identities, position checks, and navigation routes.
use super::{CATEGORY_MATCH_THRESHOLD, InputAction, NAME_MATCH_THRESHOLD};
use crate::{
    input::Key,
    item::EquipmentKind,
    item_cache::{EquipmentCatalog, EquipmentChoice},
    vision::{
        equipment::EquipmentObservation,
        text::{TextSignature, TextTemplate},
    },
};
use serde::Serialize;

pub(super) type Name = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Position {
    pub category: usize,
    pub row: usize,
    pub col: usize,
}

pub(super) type Target = EquipmentChoice;

pub(super) struct CachedList<'a> {
    kind: EquipmentKind,
    catalog: &'a EquipmentCatalog,
}

impl<'a> CachedList<'a> {
    pub fn new(kind: EquipmentKind, catalog: &'a EquipmentCatalog) -> Self {
        Self { kind, catalog }
    }

    pub fn target(&self, name: &TextSignature) -> Option<Target> {
        self.catalog.match_name(name)
    }

    fn rows(&self, category: usize) -> usize {
        self.catalog.categories[category]
            .order
            .len()
            .div_ceil(self.kind.list_columns())
    }

    fn neighbor(&self, category: usize, forward: bool) -> usize {
        let count = self.catalog.categories.len();
        if forward {
            (category + 1) % count
        } else {
            (category + count - 1) % count
        }
    }

    fn row_names(&self, category: usize, row: usize) -> Option<&[Name]> {
        let columns = self.kind.list_columns();
        let order = &self.catalog.categories.get(category)?.order;
        let start = row.checked_mul(columns)?;
        order
            .get(start..(start + columns).min(order.len()))
            .filter(|row| !row.is_empty())
    }

    fn category_matches(&self, category: usize, title: &TextTemplate) -> bool {
        !self.kind.has_categories()
            || self.catalog.categories[category].title.cosine(title) >= CATEGORY_MATCH_THRESHOLD
    }

    pub fn matches(&self, position: Position, page: &EquipmentObservation) -> bool {
        let Some(focus) = page.focus.and_then(|index| page.slots.get(index)) else {
            return false;
        };
        let Some(row) = self.row_names(position.category, position.row) else {
            return false;
        };
        let width = page
            .slots
            .iter()
            .filter(|slot| slot.row == focus.row)
            .count();
        if position.col != focus.col
            || row.len() != width
            || !self.category_matches(position.category, &page.category)
        {
            return false;
        }
        let category = &self.catalog.categories[position.category];
        category
            .match_name(page.name.signature())
            .is_some_and(|name| row.get(position.col) == Some(&name))
    }

    pub fn locate(&self, page: &EquipmentObservation) -> Option<Position> {
        let focus = page.slots.get(page.focus?)?;
        let target = self.target(page.name.signature())?;
        if !self.category_matches(target.category, &page.category) {
            return None;
        }
        let columns = self.kind.list_columns();
        let category = &self.catalog.categories[target.category];
        let width = page
            .slots
            .iter()
            .filter(|slot| slot.row == focus.row)
            .count();
        let mut positions = category
            .order
            .iter()
            .enumerate()
            .filter(|(index, name)| {
                **name == target.name
                    && index % columns == focus.col
                    && category
                        .order
                        .len()
                        .saturating_sub(index - focus.col)
                        .min(columns)
                        == width
            })
            .map(|(index, _)| Position {
                category: target.category,
                row: index / columns,
                col: focus.col,
            });
        let first = positions.next()?;
        positions.next().is_none().then_some(first)
    }

    pub fn locate_row(
        &self,
        title: &TextTemplate,
        names: &[TextTemplate],
        col: usize,
    ) -> Vec<Position> {
        let mut positions = Vec::new();
        for (category, entry) in self.catalog.categories.iter().enumerate() {
            if !self.category_matches(category, title) {
                continue;
            }
            for (row, old) in entry.order.chunks(self.kind.list_columns()).enumerate() {
                if old.len() == names.len()
                    && old
                        .iter()
                        .zip(names)
                        .all(|(&id, name)| entry.names[id].cosine(name) >= NAME_MATCH_THRESHOLD)
                {
                    positions.push(Position { category, row, col });
                }
            }
        }
        positions
    }

    pub fn is_target(&self, position: Position, target: Target) -> bool {
        position.category == target.category
            && self
                .row_names(position.category, position.row)
                .and_then(|row| row.get(position.col))
                == Some(&target.name)
    }

    pub fn predict(&self, mut position: Position, action: InputAction) -> Option<Position> {
        self.row_names(position.category, position.row)?;
        match action {
            InputAction::ClickColumn(col) => position.col = col,
            InputAction::Key(Key::Down) => {
                if position.row + 1 < self.rows(position.category) {
                    position.row += 1;
                } else {
                    position.category = self.neighbor(position.category, true);
                    position.row = 0;
                }
            }
            InputAction::Key(Key::Up) => {
                if position.row > 0 {
                    position.row -= 1;
                } else {
                    position.category = self.neighbor(position.category, false);
                    position.row = self.rows(position.category).checked_sub(1)?;
                }
            }
            InputAction::Key(key @ (Key::Z | Key::C)) => {
                if !self.kind.has_categories() {
                    return None;
                }
                position.category = self.neighbor(position.category, key == Key::C);
                position.row = 0;
            }
            _ => return None,
        }
        let width = self.row_names(position.category, position.row)?.len();
        if matches!(action, InputAction::ClickColumn(_)) {
            (position.col < width).then_some(position)
        } else {
            position.col = position.col.min(width - 1);
            Some(position)
        }
    }

    fn category_route(&self, from: Position, target: usize) -> Option<(usize, Key)> {
        let count = self.catalog.categories.len();
        if !self.kind.has_categories() || count < 2 {
            return None;
        }
        if from.category == target {
            return Some((2, Key::C));
        }
        let forward = (target + count - from.category) % count;
        let backward = (from.category + count - target) % count;
        Some(if backward < forward {
            (backward, Key::Z)
        } else {
            (forward, Key::C)
        })
    }

    fn row_distance(
        &self,
        current: Position,
        target: usize,
        target_row: usize,
        forward: bool,
    ) -> Option<usize> {
        let (mut category, mut row, mut steps) = (current.category, current.row, 0);
        for _ in 0..=self.catalog.categories.len() {
            let rows = self.rows(category);
            if rows == 0 {
                return None;
            }
            if category == target
                && if forward {
                    target_row >= row
                } else {
                    target_row <= row
                }
            {
                return Some(steps + target_row.abs_diff(row));
            }
            steps += if forward { rows - row } else { row + 1 };
            category = self.neighbor(category, forward);
            row = if forward {
                0
            } else {
                self.rows(category).checked_sub(1)?
            };
        }
        None
    }

    pub fn next(&self, current: Position, target: Target) -> Option<InputAction> {
        let columns = self.kind.list_columns();
        let category = self.catalog.categories.get(target.category)?;
        let last_row = self.rows(target.category).checked_sub(1)?;
        let head = self.category_route(current, target.category);
        let after = self.neighbor(target.category, true);
        let tail = if current.category == after && current.row == 0 {
            Some((1, Key::Up))
        } else {
            self.category_route(current, after)
                .map(|(steps, key)| (steps + 1, key))
        };
        let mut best: Option<(usize, InputAction)> = None;
        let mut consider = |cost, action| {
            if best.is_none_or(|(old, _)| cost < old) {
                best = Some((cost, action));
            }
        };
        for (index, &name) in category.order.iter().enumerate() {
            if name != target.name {
                continue;
            }
            let (row, col) = (index / columns, index % columns);
            // Prefer direct travel on ties and delay the final column click.
            for (forward, key) in [(true, Key::Down), (false, Key::Up)] {
                if let Some(steps) = self.row_distance(current, target.category, row, forward) {
                    if steps == 0 {
                        if col != current.col {
                            consider(1, InputAction::ClickColumn(col));
                        }
                    } else {
                        let direct = current.category == target.category
                            && if forward {
                                row > current.row
                            } else {
                                row < current.row
                            };
                        let click = usize::from(!direct || col != current.col);
                        consider(steps + click, InputAction::Key(key));
                    }
                }
            }
            if let Some((steps, key)) = head {
                consider(steps + row + 1, InputAction::Key(key));
            }
            if let Some((steps, key)) = tail {
                consider(steps + last_row - row + 1, InputAction::Key(key));
            }
        }
        best.map(|(_, action)| action)
    }
}
