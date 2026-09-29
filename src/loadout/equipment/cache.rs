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

pub(super) type Name = usize;
const NAME_MATCH_MARGIN: f32 = 0.04;

#[derive(Clone, Copy)]
pub(super) struct Target {
    pub category: usize,
    pub name: Name,
    pub score: f32,
}

#[derive(Default)]
pub(crate) struct EquipmentCache {
    kinds: [Vec<Category>; 6],
    dirty: bool,
}

struct Category {
    title: Option<TextSignature>,
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
    pub columns: usize,
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
    pub(super) fn search(&mut self, kind: EquipmentKind) -> SearchCache<'_> {
        SearchCache {
            categories: &mut self.kinds[kind.index()],
            dirty: &mut self.dirty,
            columns: kind.list_columns(),
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
        let title = page.category.as_ref().map(TextSample::signature);
        let category = self
            .categories
            .iter()
            .position(|entry| match (&entry.title, &title) {
                (None, None) => true,
                (Some(a), Some(b)) => a.cosine(b) >= NAME_MATCH_THRESHOLD,
                _ => false,
            })
            .unwrap_or_else(|| {
                self.categories.push(Category {
                    title: title.cloned(),
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
                        Key::Down => c.width < self.columns,
                        Key::Up => width < self.columns,
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
                .map(|last| last / self.columns * self.columns + col)
        } else if let Some(before) = self.current.filter(|_| !changed && continuous) {
            before.position.and_then(|p| {
                let row = p / self.columns;
                match key {
                    Some(Key::Down) => Some((row + 1) * self.columns + col),
                    Some(Key::Up) => row.checked_sub(1).map(|r| r * self.columns + col),
                    _ => Some(row * self.columns + col),
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
                        boundary.is_some()
                            == (p / self.columns + 1 == order.len().div_ceil(self.columns))
                    }
                    Some(Key::Up) => boundary.is_some() == (p / self.columns == 0),
                    _ => true,
                })
            });
        let position = expected.filter(|&p| {
            boundary_agrees
                && p % self.columns == col
                && order.get(p) == Some(&name)
                && order.len().saturating_sub(p - col).min(self.columns) == width
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
        if self.categories[from].next == Some(to) {
            return;
        }
        if let Some(old) = self.categories[from].next {
            self.categories[old].previous = None;
        }
        if let Some(old) = self.categories[to].previous {
            self.categories[old].next = None;
        }
        self.categories[from].next = Some(to);
        self.categories[to].previous = Some(from);
        self.categories[from].circular = false;
        self.categories[to].circular = false;
        *self.dirty = true;
    }

    pub fn category_index(&self, target: &TextSample) -> Option<usize> {
        let target = target.signature();
        self.categories.iter().position(|c| {
            c.title
                .as_ref()
                .is_some_and(|t| t.cosine(target) >= NAME_MATCH_THRESHOLD)
        })
    }

    pub fn category_key(&self, target: usize) -> Option<Key> {
        let from = self.current?.category;
        if from == target {
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
            (Some(back), Some(ahead)) if back < ahead => Some(Key::Z),
            (_, Some(_)) => Some(Key::C),
            (Some(_), None) => Some(Key::Z),
            _ => None,
        }
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
                    && p % self.columns == current.col
                    && order
                        .len()
                        .saturating_sub(p - current.col)
                        .min(self.columns)
                        == current.width
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

    pub fn next(&self, target: Name) -> Option<InputAction> {
        let current = self.current?;
        let position = current.position?;
        let category = &self.categories[current.category];
        let row_count = category.order.len().div_ceil(self.columns) as i32;
        let (rows, col) = category
            .order
            .iter()
            .enumerate()
            .filter(|&(_, &name)| name == target)
            .map(|(p, _)| {
                let mut rows = (p / self.columns) as i32 - (position / self.columns) as i32;
                if category.circular {
                    rows = rows.rem_euclid(row_count);
                    if rows > row_count - rows {
                        rows -= row_count;
                    }
                }
                (rows, p % self.columns)
            })
            .min_by_key(|&(r, c)| (r.abs(), c != current.col))?;
        let action = if rows != 0 {
            InputAction::Key(if rows < 0 { Key::Up } else { Key::Down })
        } else if col != current.col {
            InputAction::ClickColumn(col)
        } else {
            return None;
        };
        tracing::debug!(
            action = action.name(),
            rows_remaining = rows,
            target_col = col,
            "using complete equipment cache"
        );
        Some(action)
    }
}
