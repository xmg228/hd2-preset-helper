//! Complete category orders. Live scans/repairs belong to `survey`, not this cache.
mod storage;

use super::{InputAction, NAME_IDENTITY_THRESHOLD, NAME_MATCH_THRESHOLD, Navigation};
use crate::{
    input::Key,
    item::EquipmentKind,
    vision::{
        equipment::EquipmentObservation,
        text::{TextSample, TextSignature},
    },
};
use anyhow::{Context, Result};
use std::collections::BTreeMap;

pub(super) type Name = usize;
const NAME_MATCH_MARGIN: f32 = 0.04;
const CATEGORY_IDENTITY_THRESHOLD: f32 = 0.97;

#[derive(Clone, Copy)]
pub(super) struct Target {
    pub category: usize,
    pub name: Name,
    pub score: f32,
}

#[derive(Default)]
pub(crate) struct EquipmentCache {
    scales: BTreeMap<String, [Vec<Category>; 6]>,
    dirty: bool,
}

struct Category {
    title: TextSignature,
    names: Vec<TextSignature>,
    order: Vec<Name>,
    previous: Option<usize>,
    next: Option<usize>,
    circular: bool,
}

impl Category {
    /// Name IDs are local to a search; retain only identities in the completed order.
    fn compact(&mut self) {
        let mut indices = vec![None; self.names.len()];
        for &name in &self.order {
            indices[name] = Some(0);
        }
        if indices.iter().all(Option::is_some) {
            return;
        }
        let mut index = 0;
        let mut retained = 0;
        self.names.retain(|_| {
            let keep = indices[index].is_some();
            if keep {
                indices[index] = Some(retained);
                retained += 1;
            }
            index += 1;
            keep
        });
        for name in &mut self.order {
            *name = indices[*name].unwrap();
        }
    }

    fn ranked_names(
        &self,
        name: &TextSignature,
        candidates: impl Iterator<Item = Name>,
    ) -> Vec<(Name, f32)> {
        let mut scores = candidates
            .map(|i| (i, self.names[i].cosine(name)))
            .collect::<Vec<_>>();
        scores.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
        scores
    }

    fn identify(&mut self, name: &TextSignature) -> Option<Name> {
        let scores = self.ranked_names(name, 0..self.names.len());
        if let Some(&(index, score)) = scores
            .first()
            .filter(|(_, s)| *s >= NAME_IDENTITY_THRESHOLD)
        {
            return (score - scores.get(1).map_or(0.0, |entry| entry.1) >= NAME_MATCH_MARGIN)
                .then_some(index);
        }
        // A moderately similar name may be a different item. Keep its own evidence.
        self.names.push(name.clone());
        Some(self.names.len() - 1)
    }
}

#[derive(Clone, Copy)]
pub(super) struct Location {
    pub category: usize,
    pub name: Name,
    pub col: usize,
    pub width: usize,
    /// Verified occurrence, including the distance of the last input.
    pub position: Option<usize>,
    /// Unique old occurrence, even when its column/distance no longer agrees.
    pub known: Option<usize>,
    pub previous: Option<usize>,
    pub head: bool,
    pub boundary: Option<Key>,
}

pub(super) struct SearchCache<'a> {
    categories: &'a mut Vec<Category>,
    dirty: &'a mut bool,
    pub kind: EquipmentKind,
    current: Option<Location>,
}

impl Drop for SearchCache<'_> {
    fn drop(&mut self) {
        // No Survey/Target can use these IDs once this search releases the cache.
        // Pruning unsaved observations does not change the persistent map.
        for category in self.categories.iter_mut() {
            category.compact();
        }
    }
}

impl EquipmentCache {
    pub(super) fn search(&mut self, kind: EquipmentKind, scale: f64) -> SearchCache<'_> {
        SearchCache {
            // Decimal keys retain the exact UI scale, including in the JSON cache.
            categories: &mut self.scales.entry(scale.to_string()).or_default()[kind.index()],
            dirty: &mut self.dirty,
            kind,
            current: None,
        }
    }
}

impl SearchCache<'_> {
    pub fn category_indices(&self) -> std::ops::Range<usize> {
        0..self.categories.len()
    }

    pub fn observe(
        &mut self,
        page: &EquipmentObservation,
        navigation: Option<Navigation>,
    ) -> Result<Location> {
        let focus = &page.slots[page.focus.context("equipment focus missing")?];
        let title = page.category.signature();
        let columns = self.kind.list_columns();
        // Single-category lists also need a visual language check on their first visit.
        if !self.kind.has_categories()
            && self.current.is_none()
            && self
                .categories
                .first()
                .is_some_and(|entry| entry.title.cosine(title) < CATEGORY_IDENTITY_THRESHOLD)
        {
            self.categories.clear();
            *self.dirty = true;
            tracing::info!(kind = ?self.kind, "equipment heading changed; rebuilding this cache");
        }
        // Only a strong, same-scale heading match can reuse a map identity.
        let category = if self.kind.has_categories() {
            self.match_category(title, CATEGORY_IDENTITY_THRESHOLD)
        } else {
            (!self.categories.is_empty()).then_some(0)
        }
        .unwrap_or_else(|| {
            self.categories.push(Category {
                title: title.clone(),
                names: Vec::new(),
                order: Vec::new(),
                previous: None,
                next: None,
                circular: false,
            });
            self.categories.len() - 1
        });
        let col = focus.col;
        let width = page.slots.iter().filter(|s| s.row == focus.row).count();
        let name = self.categories[category]
            .identify(page.name.signature())
            .context("equipment name matches multiple cached identities; search stopped")?;
        let key = navigation.and_then(|n| match n.action {
            InputAction::Key(k) => Some(k),
            _ => None,
        });
        let continuous = navigation.is_some_and(|n| !n.retried);
        let changed = self.current.is_some_and(|c| c.category != category);
        let boundary = key.filter(|k| {
            continuous
                && matches!(k, Key::Up | Key::Down)
                && (changed
                    || navigation.is_some_and(|n| n.wrapped)
                    || self.current.is_some_and(|c| match k {
                        Key::Down => c.width < columns,
                        Key::Up => width < columns,
                        _ => false,
                    }))
        });
        if continuous
            && changed
            && let Some(current) = self.current
            && let Some(key) = key
        {
            self.connect_categories(current.category, category, key);
        } else if boundary.is_some() && !changed && !self.categories[category].circular {
            self.categories[category].circular = true;
            *self.dirty = true;
        }
        let head = matches!(key, Some(Key::Z | Key::C)) || boundary == Some(Key::Down);
        let order = &self.categories[category].order;
        let mut occurrences = order
            .iter()
            .enumerate()
            .filter_map(|(p, &n)| (n == name).then_some(p));
        let known = occurrences.next().filter(|_| occurrences.next().is_none());
        let previous = self.current.filter(|_| !changed).and_then(|c| c.position);
        let expected = if head {
            Some(col)
        } else if boundary == Some(Key::Up) {
            order
                .len()
                .checked_sub(1)
                .map(|last| last / columns * columns + col)
        } else if let Some(before) = self.current.filter(|_| !changed && continuous) {
            before.position.and_then(|p| {
                let row = p / columns;
                match key {
                    Some(Key::Down) => Some((row + 1) * columns + col),
                    Some(Key::Up) => row.checked_sub(1).map(|r| r * columns + col),
                    _ => Some(row * columns + col),
                }
            })
        } else {
            known
        };
        // Whole-row additions preserve columns, but change old travel distances/endpoints.
        let boundary_agrees = !continuous
            || changed
            || self.current.is_none_or(|c| {
                c.position.is_none_or(|p| match key {
                    Some(Key::Down) => {
                        boundary.is_some() == (p / columns + 1 == order.len().div_ceil(columns))
                    }
                    Some(Key::Up) => boundary.is_some() == (p / columns == 0),
                    _ => true,
                })
            });
        let position = expected.filter(|&p| {
            boundary_agrees
                && p % columns == col
                && order.get(p) == Some(&name)
                && order.len().saturating_sub(p - col).min(columns) == width
        });
        let location = Location {
            category,
            name,
            col,
            width,
            position,
            known,
            previous,
            head,
            boundary,
        };
        self.current = Some(location);
        Ok(location)
    }

    fn connect_categories(&mut self, mut from: usize, mut to: usize, key: Key) {
        match key {
            Key::C | Key::Down => {}
            Key::Z | Key::Up => std::mem::swap(&mut from, &mut to),
            _ => return,
        }
        if self.categories[from].next != Some(to) {
            if let Some(old) = self.categories[from].next {
                self.categories[old].previous = None;
            }
            if let Some(old) = self.categories[to].previous {
                self.categories[old].next = None;
            }
            self.categories[from].next = Some(to);
            self.categories[to].previous = Some(from);
            *self.dirty = true;
        }
        *self.dirty |= self.categories[from].circular || self.categories[to].circular;
        self.categories[from].circular = false;
        self.categories[to].circular = false;
    }

    /// A saved heading may come from another resolution; it only guides navigation.
    pub fn category_hint(&self, target: &TextSample) -> Option<usize> {
        self.match_category(target.signature(), NAME_MATCH_THRESHOLD)
    }

    fn match_category(&self, target: &TextSignature, threshold: f32) -> Option<usize> {
        self.categories
            .iter()
            .enumerate()
            .map(|(category, entry)| (category, entry.title.cosine(target)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .filter(|(_, score)| *score >= threshold)
            .map(|(category, _)| category)
    }

    pub fn category_key(&self, target: usize) -> Option<Key> {
        let from = self.current?.category;
        if from == target {
            return None;
        }
        self.category_route(target).map(|(_, key)| key)
    }

    /// Z/C reaches a category's first row, regardless of the starting row.
    fn category_route(&self, target: usize) -> Option<(usize, Key)> {
        if !self.kind.has_categories() {
            return None;
        }
        let from = self.current?.category;
        if from == target {
            // Leave and return to reset this category to its first row.
            for (key, neighbor) in [
                (Key::C, self.categories[from].next),
                (Key::Z, self.categories[from].previous),
            ] {
                if let Some(at) = neighbor.filter(|&at| at != from)
                    && match key {
                        Key::C => self.categories[at].previous == Some(from),
                        _ => self.categories[at].next == Some(from),
                    }
                {
                    return Some((2, key));
                }
            }
            return None;
        }
        let distance = |forward: bool| {
            let mut at = from;
            for steps in 1..=self.categories.len() {
                at = if forward {
                    self.categories[at].next?
                } else {
                    self.categories[at].previous?
                };
                if at == target {
                    return Some(steps);
                }
            }
            None
        };
        match (distance(false), distance(true)) {
            (Some(back), Some(ahead)) if back < ahead => Some((back, Key::Z)),
            (_, Some(ahead)) => Some((ahead, Key::C)),
            (Some(back), None) => Some((back, Key::Z)),
            _ => None,
        }
    }

    /// Vertical travel requires complete row counts along the connected path.
    fn row_distance(&self, target: usize, target_row: usize, forward: bool) -> Option<usize> {
        let columns = self.kind.list_columns();
        let current = self.current?;
        let (mut at, mut row, mut steps) = (current.category, current.position? / columns, 0);
        for _ in 0..=self.categories.len() {
            let category = &self.categories[at];
            let rows = category.order.len().div_ceil(columns);
            if rows == 0 {
                return None;
            }
            if at == target
                && if forward {
                    target_row >= row
                } else {
                    target_row <= row
                }
            {
                return Some(steps + target_row.abs_diff(row));
            }
            steps += if forward { rows - row } else { row + 1 };
            at = if category.circular {
                at
            } else if forward {
                category.next?
            } else {
                category.previous?
            };
            row = if forward {
                0
            } else {
                self.categories[at]
                    .order
                    .len()
                    .div_ceil(columns)
                    .checked_sub(1)?
            };
        }
        None
    }

    pub fn is_complete(&self, category: usize) -> bool {
        !self.categories[category].order.is_empty()
    }

    pub fn is_circular(&self, category: usize) -> bool {
        self.categories[category].circular
    }

    pub fn target(
        &self,
        categories: impl IntoIterator<Item = usize>,
        target: &TextSignature,
    ) -> Option<Target> {
        // Rank distinct names in the complete order, not occurrences or abandoned scans.
        let mut scores = categories
            .into_iter()
            .flat_map(|category| {
                let entry = &self.categories[category];
                entry
                    .names
                    .iter()
                    .enumerate()
                    .filter(|(name, _)| entry.order.contains(name))
                    .map(move |(name, signature)| Target {
                        category,
                        name,
                        score: signature.cosine(target),
                    })
            })
            .collect::<Vec<_>>();
        scores.sort_unstable_by(|a, b| b.score.total_cmp(&a.score));
        let best = *scores.first()?;
        (best.score >= NAME_MATCH_THRESHOLD
            && best.score - scores.get(1).map_or(0.0, |entry| entry.score) >= NAME_MATCH_MARGIN)
            .then_some(best)
    }

    pub fn take_order(&mut self, category: usize) -> Vec<Name> {
        *self.dirty |= !self.categories[category].order.is_empty();
        std::mem::take(&mut self.categories[category].order)
    }

    pub fn replace(
        &mut self,
        category: usize,
        order: Vec<Name>,
        position: Option<usize>,
    ) -> Location {
        let columns = self.kind.list_columns();
        tracing::info!(
            category,
            slots = order.len(),
            "complete equipment category cached"
        );
        self.categories[category].order = order;
        *self.dirty = true;
        if let Some(current) = &mut self.current
            && current.category == category
        {
            let order = &self.categories[category].order;
            current.position = position.filter(|&p| {
                order.get(p) == Some(&current.name)
                    && p % columns == current.col
                    && order.len().saturating_sub(p - current.col).min(columns) == current.width
            });
            let mut occurrences = order
                .iter()
                .enumerate()
                .filter_map(|(p, &n)| (n == current.name).then_some(p));
            current.known = occurrences.next().filter(|_| occurrences.next().is_none());
            current.previous = None;
        }
        self.current.unwrap()
    }

    pub fn next(&self, target: Target) -> Option<InputAction> {
        let columns = self.kind.list_columns();
        let current = self.current?;
        // An unverified target-category position must be repaired, not routed.
        // Other categories can still use Z/C or their observed real head.
        if current.category == target.category && current.position.is_none() {
            return None;
        }
        let category = &self.categories[target.category];
        let last_row = category.order.len().div_ceil(columns).checked_sub(1)?;
        let head = self.category_route(target.category);
        // The real next-category head is enough to enter the target tail;
        // its own old map may be absent or stale. Include that Up in the cost.
        let tail = category.next.and_then(|next| {
            if current.category == next && current.head {
                Some((1, Key::Up))
            } else {
                self.category_route(next)
                    .map(|(steps, key)| (steps + 1, key))
            }
        });
        let mut best: Option<(usize, InputAction)> = None;
        let mut consider = |cost, action| {
            if best.is_none_or(|(old_cost, _)| cost < old_cost) {
                best = Some((cost, action));
            }
        };
        for (p, &name) in category.order.iter().enumerate() {
            if name != target.name {
                continue;
            }
            let (row, col) = (p / columns, p % columns);
            // Prefer direct travel on ties. Only click after reaching the target row.
            for (forward, key) in [(true, Key::Down), (false, Key::Up)] {
                if let Some(steps) = self.row_distance(target.category, row, forward) {
                    if steps == 0 {
                        if col != current.col {
                            consider(1, InputAction::ClickColumn(col));
                        }
                    } else {
                        let direct = current.category == target.category
                            && current.position.is_some_and(|p| {
                                if forward {
                                    row > p / columns
                                } else {
                                    row < p / columns
                                }
                            });
                        // Crossing a short tail may reset the column. Budget one
                        // final click; the real landing column is observed later.
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
        let (cost, action) = best?;
        tracing::debug!(
            action = action.name(),
            estimated_inputs = cost,
            "using cached equipment route"
        );
        Some(action)
    }
}
