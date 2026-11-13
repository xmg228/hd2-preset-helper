//! Closed-library icon lookup, independent of live-slot confirmation.
use anyhow::Result;

use super::IconCatalog;
use crate::{
    item::ItemKind,
    vision::{IconTemplate, ImageSample, icon_cosine_errors},
};

const ICON_MATCH_MAX_ERROR: f32 = 0.30;

impl IconCatalog {
    pub(crate) fn match_icon(&self, kind: ItemKind, source: &ImageSample) -> Result<Option<usize>> {
        if self.items.is_empty() {
            return Ok(None);
        }
        let category = IconTemplate::extract(kind, source)?.category;
        let (indices, templates): (Vec<_>, Vec<_>) = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.icon.category == category)
            .map(|(index, item)| (index, &*item.icon))
            .unzip();
        let scores = icon_cosine_errors(kind, &templates, &[source])?;
        Ok(scores[0]
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| left.total_cmp(right))
            .filter(|(_, error)| **error <= ICON_MATCH_MAX_ERROR)
            .map(|(index, _)| indices[index]))
    }
}
