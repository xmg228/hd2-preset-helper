//! Dense, operation-local evidence for building or repairing one category.
use std::collections::BTreeMap;

use super::{
    InputAction, Navigation,
    cache::{Location, Name},
};
use crate::input::Key;

pub(super) enum Step {
    Input(InputAction),
    Complete {
        order: Vec<Name>,
        position: Option<usize>,
    },
    Rebuild,
}

enum Purpose {
    Build {
        head: Option<i32>,
        end: Option<i32>,
    },
    Repair {
        old: Vec<Name>,
        left_limit: usize,
        right_limit: usize,
        // A verified unchanged row or the real top bounds the splice on the left.
        left: Option<(i32, usize)>,
        real_head: bool,
    },
}

pub(super) struct Survey {
    pub category: usize,
    columns: usize,
    purpose: Purpose,
    rows: BTreeMap<i32, Vec<Name>>,
    at: i32,
    row: Vec<Option<Name>>,
    reverse: bool,
    returning: bool,
}

impl Survey {
    pub fn build(location: Location, columns: usize) -> Self {
        tracing::info!(
            category = location.category,
            "scanning complete equipment category"
        );
        Self {
            category: location.category,
            columns,
            purpose: Purpose::Build {
                head: location.head.then_some(0),
                end: None,
            },
            rows: BTreeMap::new(),
            at: 0,
            row: Vec::new(),
            reverse: false,
            returning: false,
        }
    }

    pub fn repair(location: Location, columns: usize, old: Vec<Name>) -> Self {
        let left_limit = location
            .previous
            .into_iter()
            .chain(location.known)
            .min()
            .unwrap_or(old.len());
        let right_limit = location
            .previous
            .into_iter()
            .chain(location.known)
            .max()
            .unwrap_or(0);
        tracing::info!(
            category = location.category,
            left_limit,
            right_limit,
            "repairing equipment order from live rows"
        );
        Self {
            purpose: Purpose::Repair {
                old,
                left_limit,
                right_limit,
                left: location.head.then_some((0, 0)),
                real_head: location.head,
            },
            ..Self::build(location, columns)
        }
    }

    pub fn advance(&mut self, location: Location, navigation: Option<Navigation>) -> Step {
        if navigation.is_some_and(|n| n.retried) {
            return Step::Rebuild;
        }
        let key = navigation.and_then(|n| match n.action {
            InputAction::Key(k) => Some(k),
            _ => None,
        });
        if self.returning {
            self.returning = false;
            if location.category != self.category {
                return Step::Rebuild;
            }
            self.row.clear();
        } else if let Some(direction) = location.boundary.filter(|_| navigation.is_some()) {
            match &mut self.purpose {
                Purpose::Build { head, end } if direction == Key::Down => {
                    *end = Some(self.at + 1);
                    if let Some(head) = *head {
                        return Step::Complete {
                            order: self.flatten(head, self.at + 1),
                            position: (location.category == self.category).then_some(location.col),
                        };
                    }
                    self.at += 1;
                    *head = Some(self.at);
                    self.row.clear();
                    if location.category != self.category {
                        // Z returns from the next category to this category's first row.
                        self.returning = true;
                        return Step::Input(InputAction::Key(Key::Z));
                    }
                }
                Purpose::Repair {
                    left, real_head, ..
                } if direction == Key::Up => {
                    // The row BEFORE crossing was the actual top. Return with Down;
                    // its real column is observed, not assumed to survive a short row.
                    *left = Some((self.at, 0));
                    *real_head = true;
                    self.returning = true;
                    return Step::Input(InputAction::Key(Key::Down));
                }
                Purpose::Repair { .. } if direction == Key::Down => {
                    return self.finish_tail(location);
                }
                _ => return Step::Rebuild,
            }
        } else if matches!(key, Some(Key::Up | Key::Down)) {
            self.at += if key == Some(Key::Down) { 1 } else { -1 };
            self.row.clear();
        }
        if location.category != self.category {
            return Step::Rebuild;
        }
        if self.row.is_empty() {
            // Rows just inspected during backtracking need only landing verification.
            self.row = self
                .rows
                .get(&self.at)
                .filter(|row| row.len() == location.width && row[location.col] == location.name)
                .map(|row| row.iter().copied().map(Some).collect())
                .unwrap_or_else(|| vec![None; location.width]);
            self.reverse = location.col * 2 > location.width - 1;
        }
        if self.row.len() != location.width {
            return Step::Rebuild;
        }
        self.row[location.col] = Some(location.name);
        let next = if self.reverse {
            self.row.iter().rposition(Option::is_none)
        } else {
            self.row.iter().position(Option::is_none)
        };
        if let Some(col) = next {
            return Step::Input(InputAction::ClickColumn(col));
        }
        let row = self.row.iter().map(|n| n.unwrap()).collect::<Vec<_>>();
        self.rows.insert(self.at, row.clone());
        match &mut self.purpose {
            Purpose::Build {
                head: Some(head),
                end: Some(end),
            } if self.at >= *head && self.rows.get(&0) == Some(&row) && distinct(&row) => {
                // The starting row was in the middle. Rotate only after its full
                // reappearance, not merely after a name in a contiguous duplicate run.
                let (head, end) = (*head, *end);
                let mut order = self.flatten(head, self.at);
                let position = order.len() + location.col;
                order.extend(self.flatten(0, end));
                Step::Complete {
                    order,
                    position: Some(position),
                }
            }
            Purpose::Build { .. } => Step::Input(InputAction::Key(Key::Down)),
            Purpose::Repair {
                old,
                left_limit,
                left,
                ..
            } => {
                if left.is_none() {
                    *left = unique_match(old, &row)
                        .filter(|&p| {
                            p <= *left_limit
                                && p % self.columns == 0
                                && old.len().saturating_sub(p).min(self.columns) == row.len()
                        })
                        .map(|p| (self.at, p));
                }
                if left.is_none() {
                    return Step::Input(InputAction::Key(Key::Up));
                }
                self.finish_interval(location)
                    .unwrap_or(Step::Input(InputAction::Key(Key::Down)))
            }
        }
    }

    fn flatten(&self, start: i32, end: i32) -> Vec<Name> {
        (start..end)
            .flat_map(|row| self.rows[&row].iter().copied())
            .collect()
    }

    fn finish_interval(&self, location: Location) -> Option<Step> {
        let Purpose::Repair {
            old,
            right_limit,
            left: Some((start, old_start)),
            ..
        } = &self.purpose
        else {
            return None;
        };
        if self.at < 0 {
            return None;
        } // Still before the row that triggered repair.
        let row = &self.rows[&self.at];
        let right = unique_match(old, row)?;
        let old_end = right + row.len();
        if old_end <= *right_limit || right < *old_start {
            return None;
        }
        let observed = self.flatten(*start, self.at + 1);
        // Only insertion-like edits with preserved old order are patched locally.
        // Reordering/unrecognizable identities require a real-boundary full scan.
        if !subsequence(&old[*old_start..old_end], &observed) {
            return None;
        }
        let position = old_start + observed.len() - row.len() + location.col;
        let mut order = old[..*old_start].to_vec();
        order.extend(observed);
        order.extend_from_slice(&old[old_end..]);
        if position % self.columns != location.col
            || order
                .len()
                .saturating_sub(position - location.col)
                .min(self.columns)
                != location.width
        {
            return None;
        }
        tracing::info!(
            old_start,
            old_end,
            old_slots = old.len(),
            slots = order.len(),
            "equipment cache interval repaired"
        );
        Some(Step::Complete {
            order,
            position: Some(position),
        })
    }

    fn finish_tail(&self, location: Location) -> Step {
        let Purpose::Repair {
            old,
            left: Some((start, old_start)),
            real_head,
            ..
        } = &self.purpose
        else {
            return Step::Rebuild;
        };
        let observed = self.flatten(*start, self.at + 1);
        if !real_head && !subsequence(&old[*old_start..], &observed) {
            return Step::Rebuild;
        }
        let mut order = old[..*old_start].to_vec();
        order.extend(observed);
        tracing::info!(
            old_start,
            old_slots = old.len(),
            slots = order.len(),
            "equipment cache suffix rebuilt"
        );
        Step::Complete {
            order,
            position: (location.category == self.category).then_some(location.col),
        }
    }
}

fn distinct(names: &[Name]) -> bool {
    names
        .first()
        .is_some_and(|first| names.iter().any(|n| n != first))
}

/// Equal names may repeat. A unique multi-name sequence identifies the occurrence.
fn unique_match(order: &[Name], row: &[Name]) -> Option<usize> {
    if !distinct(row) || row.len() > order.len() {
        return None;
    }
    let mut matches = order
        .windows(row.len())
        .enumerate()
        .filter_map(|(p, r)| (r == row).then_some(p));
    matches.next().filter(|_| matches.next().is_none())
}

fn subsequence(old: &[Name], observed: &[Name]) -> bool {
    let mut at = 0;
    for &name in observed {
        if old.get(at) == Some(&name) {
            at += 1;
        }
    }
    at == old.len()
}
