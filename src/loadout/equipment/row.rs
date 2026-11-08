//! Collect one physical row in the order selected by its landing column.

pub(super) enum RowStep<T> {
    Column(usize),
    Complete(Vec<T>),
}

pub(super) struct RowScan<T> {
    items: Vec<Option<T>>,
    reverse: bool,
}

impl<T: Clone> RowScan<T> {
    pub(super) fn new(width: usize, col: usize) -> Self {
        Self {
            items: vec![None; width],
            reverse: col > width.saturating_sub(1) / 2,
        }
    }

    pub(super) fn observed_slots(&self) -> usize {
        self.items.iter().flatten().count()
    }

    pub(super) fn advance(&mut self, width: usize, col: usize, item: T) -> Result<RowStep<T>, ()> {
        if self.items.len() != width || col >= width {
            return Err(());
        }
        self.items[col] = Some(item);
        let next = if self.reverse {
            self.items.iter().rposition(Option::is_none)
        } else {
            self.items.iter().position(Option::is_none)
        };
        Ok(match next {
            Some(col) => RowStep::Column(col),
            None => RowStep::Complete(self.items.iter().flatten().cloned().collect()),
        })
    }
}
