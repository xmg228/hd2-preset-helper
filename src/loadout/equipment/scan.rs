//! Complete row cycles from an arbitrary entry, with boundary-based completion as fallback.
use anyhow::{Context, Result, ensure};

use super::{
    EquipmentItem, InputAction, NAME_IDENTITY_THRESHOLD, NAME_MATCH_THRESHOLD, NavigationWait,
    Session,
    cache::Position,
    row::{RowScan, RowStep},
};
use crate::{
    input::Key,
    item::EquipmentKind,
    item_cache::{EquipmentCatalog, EquipmentCategory},
    scan::{ScanCancellation, ScanKind, ScanProgress},
    vision::{
        ScrollbarObservation,
        equipment::{EquipmentObservation, EquipmentObserver},
        text::TextSample,
    },
};

const MAX_SCAN_VISITS: usize = 4096;

/// One immutable reference. Content position is approximate; row order is discrete.
struct PositionReference {
    upper_y: f64,
    focus_y: f64,
    k: f64,
    pitch: f64,
}

impl PositionReference {
    fn new(observer: &EquipmentObserver, page: &EquipmentObservation) -> Option<Self> {
        let bar = page.scrollbar?;
        let height = f64::from(bar.lower_y - bar.upper_y);
        let k = observer.scrollbar_span() / height;
        (height > 0.0 && k.is_finite()).then_some(Self {
            upper_y: f64::from(bar.upper_y),
            focus_y: focus_y(page)?,
            k,
            pitch: observer.row_pitch() * page.scale,
        })
    }

    fn at_origin(&self, page: &EquipmentObservation) -> bool {
        let Some(bar) = page.scrollbar else {
            return false;
        };
        let Some(y) = focus_y(page) else {
            return false;
        };
        let distance = y - self.focus_y + self.k * (f64::from(bar.upper_y) - self.upper_y);
        distance.abs() < self.pitch / 3.0
    }
}

fn focus_y(page: &EquipmentObservation) -> Option<f64> {
    let rect = page.slots.get(page.focus?)?.rect;
    Some((rect[1] + rect[3]) * 0.5)
}

fn wrapped_down(
    before: Option<ScrollbarObservation>,
    page: &EquipmentObservation,
    focus_wrapped: bool,
) -> bool {
    match (before, page.scrollbar) {
        (Some(old), Some(new)) => {
            let reset = f64::from(old.upper_y - new.upper_y) > 3.0 * page.scale;
            reset && (old.at_bottom && new.at_top || focus_wrapped)
                || focus_wrapped && old.at_top && old.at_bottom && new.at_top && new.at_bottom
        }
        _ => false,
    }
}

struct ScannedRow {
    category: TextSample,
    names: Vec<TextSample>,
    boundary_before: bool,
}

impl ScannedRow {
    fn matches(&self, category: &TextSample, names: &[TextSample]) -> bool {
        self.category.cosine(category) >= NAME_MATCH_THRESHOLD
            && self.names.len() == names.len()
            && self
                .names
                .iter()
                .zip(names)
                .all(|(a, b)| a.cosine(b) >= NAME_IDENTITY_THRESHOLD)
    }
}

/// Classify only after the physical row cycle is complete.
fn complete_categories(
    mut rows: Vec<ScannedRow>,
    start: usize,
    cut: Option<usize>,
    columns: usize,
    current_col: usize,
    original_col: usize,
) -> Result<(Vec<EquipmentCategory>, Position, Position)> {
    rows.drain(..start);
    ensure!(
        !rows.is_empty(),
        "equipment scan collected no complete rows"
    );
    let row_count = rows.len();
    let rotation = cut
        .filter(|&cut| cut >= start)
        .map_or(0, |cut| (cut - start) % row_count);
    rows.rotate_left(rotation);
    let current_row = (row_count - rotation) % row_count;
    let original_row = (row_count - (start + rotation) % row_count) % row_count;
    let mut current = None;
    let mut original = None;
    let mut categories: Vec<EquipmentCategory> = Vec::new();
    let mut previous_width = columns;
    for (index, row) in rows.into_iter().enumerate() {
        if categories.is_empty() || row.boundary_before {
            categories.push(EquipmentCategory {
                title: row.category,
                names: Vec::new(),
                order: Vec::new(),
            });
        } else {
            ensure!(
                previous_width == columns,
                "equipment scan found a short row before a category's end"
            );
        }
        previous_width = row.names.len();
        let category_index = categories.len() - 1;
        let category = categories.last_mut().unwrap();
        for (wanted, col, position) in [
            (current_row, current_col, &mut current),
            (original_row, original_col, &mut original),
        ] {
            if index == wanted {
                ensure!(
                    col < row.names.len(),
                    "equipment scan position has an invalid column"
                );
                *position = Some(Position {
                    category: category_index,
                    row: category.order.len() / columns,
                    col,
                });
            }
        }
        for sample in row.names {
            let name = category
                .identify(&sample)
                .context("equipment scan found ambiguous item names")?;
            category.order.push(name);
        }
    }
    Ok((categories, current.unwrap(), original.unwrap()))
}

pub(super) struct Collection {
    pub catalog: EquipmentCatalog,
    pub position: Position,
    original: Position,
}

fn row_index(categories: &[EquipmentCategory], columns: usize, position: Position) -> usize {
    categories[..position.category]
        .iter()
        .map(|c| c.order.len().div_ceil(columns))
        .sum::<usize>()
        + position.row
}

impl Session<'_> {
    pub(crate) fn scan(
        &mut self,
        kind: EquipmentKind,
        progress: &mut impl FnMut(ScanProgress),
        cancel: &ScanCancellation,
    ) -> Result<EquipmentCatalog> {
        self.automation.set_cancellation(cancel.clone());
        self.open(kind)?;
        let original = EquipmentItem {
            name: self.page.name.to_template(),
            category: Some(self.page.category.to_template()),
            client_size: self.client_size,
        };
        let collection = self.collect(&mut |category, items| {
            progress(ScanProgress {
                kind: ScanKind::Equipment(kind),
                category: Some(crate::vision::text::render_category(category)),
                items,
            })
        })?;
        cancel.check()?;
        let columns = kind.list_columns();
        let categories = &collection.catalog.categories;
        let rows = categories
            .iter()
            .map(|c| c.order.len().div_ceil(columns))
            .sum::<usize>();
        let from = row_index(categories, columns, collection.position);
        let to = row_index(categories, columns, collection.original);
        for _ in 0..(from + rows - to) % rows {
            let input = self.navigate(InputAction::Key(Key::Up), NavigationWait::Settled)?;
            ensure!(
                !input.retried,
                "equipment scan lost continuity while restoring the original position"
            );
        }
        let focus = self.page.focus.context("equipment scan focus is missing")?;
        if self.page.slots[focus].col != collection.original.col {
            let input = self.navigate(
                InputAction::ClickColumn(collection.original.col),
                NavigationWait::Settled,
            )?;
            ensure!(
                !input.retried,
                "equipment scan lost continuity while restoring the original column"
            );
        }
        self.equip_and_return(&original)
            .context("failed to restore the equipment selected before scanning")?;
        Ok(collection.catalog)
    }

    /// Collect the open type, leaving focus at the returned occurrence for navigation.
    pub(super) fn collect(
        &mut self,
        progress: &mut impl FnMut(&TextSample, usize),
    ) -> Result<Collection> {
        self.check_active()?;
        let kind = self.observer.kind();
        let original_col =
            self.page.slots[self.page.focus.context("equipment scan focus is missing")?].col;
        let reference = PositionReference::new(&self.observer, &self.page);
        let mut down_steps = 0;
        let mut rows: Vec<ScannedRow> = Vec::new();
        let mut cut = None;
        let mut first_head: Option<usize> = None;
        let mut boundary_before = false;
        let mut row: Option<RowScan<TextSample>> = None;
        let mut items = 0;
        let mut visits = 1;

        let start = loop {
            self.check_active()?;
            ensure!(
                visits < MAX_SCAN_VISITS,
                "equipment scan exceeded {MAX_SCAN_VISITS} observations"
            );
            let focus =
                &self.page.slots[self.page.focus.context("equipment scan focus is missing")?];
            let col = focus.col;
            let width = self
                .page
                .slots
                .iter()
                .filter(|s| s.row == focus.row)
                .count();
            let sampling = row.get_or_insert_with(|| RowScan::new(width, col));
            let step = sampling
                .advance(width, col, self.page.name.clone())
                .map_err(|()| anyhow::anyhow!("equipment row width changed while scanning"))?;
            let returning =
                cut.is_some() && reference.as_ref().is_some_and(|r| r.at_origin(&self.page));
            if let RowStep::Complete(names) = &step {
                // Compare completed rows, so a heading refreshed during column clicks
                // still belongs to this incoming edge.
                boundary_before |= kind.has_categories()
                    && rows.last().is_some_and(|previous| {
                        previous.category.native_cosine(&self.page.category) < 0.97
                    });
                if returning
                    && rows
                        .first()
                        .is_some_and(|first| first.matches(&self.page.category, names))
                {
                    // This last Down supplies the initially unknown edge into the entry row.
                    rows[0].boundary_before = boundary_before;
                    break 0;
                }
                if boundary_before {
                    if let Some(head) = first_head {
                        // Without entry geometry, a full head row must reappear at a boundary.
                        if rows[head].matches(&self.page.category, names) {
                            break head;
                        }
                    } else {
                        first_head = Some(rows.len());
                    }
                }
            }
            progress(
                &self.page.category,
                items
                    + if returning {
                        0
                    } else {
                        sampling.observed_slots()
                    },
            );
            let action = match step {
                RowStep::Column(col) => InputAction::ClickColumn(col),
                RowStep::Complete(names) => {
                    items += names.len();
                    rows.push(ScannedRow {
                        category: self.page.category.clone(),
                        names,
                        boundary_before,
                    });
                    row = None;
                    InputAction::Key(Key::Down)
                }
            };
            let before = self.page.scrollbar;
            let input = self.navigate(action, NavigationWait::Settled)?;
            ensure!(
                !input.retried,
                "equipment scan lost input continuity; the current type was not saved"
            );
            self.sync_category()?;
            if matches!(action, InputAction::Key(Key::Down)) {
                down_steps += 1;
                let global_wrap = wrapped_down(before, &self.page, input.wrapped)
                    || reference.is_none()
                        && before.is_none()
                        && self.page.scrollbar.is_none()
                        && input.wrapped;
                if global_wrap && cut.is_none() {
                    cut = Some(rows.len());
                }
                // Focus wrap still supplies a fallback head when a cap is missing;
                // only the independently confirmed global wrap sets the rotation cut.
                boundary_before = global_wrap || input.wrapped || width < kind.list_columns();
            }
            visits += 1;
        };
        let current_col =
            self.page.slots[self.page.focus.context("equipment scan focus is missing")?].col;
        let (categories, position, original) = complete_categories(
            rows,
            start,
            cut,
            kind.list_columns(),
            current_col,
            original_col,
        )?;
        let row_count = categories
            .iter()
            .map(|c| c.order.len().div_ceil(kind.list_columns()))
            .sum::<usize>();
        progress(
            &self.page.category,
            categories.iter().map(|category| category.order.len()).sum(),
        );
        tracing::debug!(
            categories = categories.len(),
            rows = row_count,
            down_steps,
            global_wrap = cut.is_some(),
            closed_at_entry = start == 0 && reference.is_some(),
            "equipment scan cycle completed"
        );

        Ok(Collection {
            catalog: EquipmentCatalog {
                client_size: self.client_size,
                categories,
            },
            position,
            original,
        })
    }
}
