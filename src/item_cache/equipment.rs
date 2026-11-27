//! Name identities shared by equipment navigation and preset refreshes.
use std::sync::Arc;

use image::RgbaImage;

use super::{EquipmentCatalog, EquipmentCategory, EquipmentChoice, EquipmentName};
use crate::vision::text::{TextSample, TextSignature};

// One threshold for smoothed name identities and saved-target acceptance.
pub(crate) const NAME_MATCH_THRESHOLD: f32 = 0.96;

struct NameRanking<T> {
    best: T,
    score: f32,
    runner_up: Option<f32>,
}

impl<T> NameRanking<T> {
    fn accepts(&self) -> bool {
        self.score >= NAME_MATCH_THRESHOLD
            && self
                .runner_up
                .is_none_or(|score| score < NAME_MATCH_THRESHOLD)
    }
}

fn rank_names<'a, T>(
    target: &TextSignature,
    candidates: impl Iterator<Item = (T, &'a TextSignature)>,
) -> Option<NameRanking<T>> {
    let mut scores = candidates
        .map(|(id, signature)| (id, signature.cosine(target)))
        .collect::<Vec<_>>();
    scores.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
    let mut scores = scores.into_iter();
    let (best, score) = scores.next()?;
    Some(NameRanking {
        best,
        score,
        runner_up: scores.next().map(|(_, score)| score),
    })
}

impl EquipmentCatalog {
    /// Match one name identity, not one physical occurrence of that name.
    pub fn match_name(&self, target: &TextSignature) -> Option<EquipmentChoice> {
        rank_names(
            target,
            self.categories
                .iter()
                .enumerate()
                .flat_map(|(category, entry)| {
                    entry
                        .names
                        .iter()
                        .enumerate()
                        .filter(|(name, _)| entry.order.contains(name))
                        .map(move |(name, entry)| {
                            (EquipmentChoice { category, name }, entry.sample.signature())
                        })
                }),
        )
        .filter(|ranking| ranking.accepts())
        .map(|ranking| ranking.best)
    }

    pub fn thumbnail(&self, choice: EquipmentChoice) -> Option<&Arc<RgbaImage>> {
        self.categories
            .get(choice.category)?
            .names
            .get(choice.name)?
            .thumbnail
            .as_ref()
    }
}

impl EquipmentCategory {
    pub fn match_name(&self, target: &TextSignature) -> Option<usize> {
        rank_names(
            target,
            self.names
                .iter()
                .enumerate()
                .map(|(index, entry)| (index, entry.sample.signature())),
        )
        .filter(|ranking| ranking.accepts())
        .map(|ranking| ranking.best)
    }

    /// Intern names only while materializing a completed scan.
    pub fn identify(
        &mut self,
        sample: TextSample,
        thumbnail: Option<Arc<RgbaImage>>,
    ) -> Option<usize> {
        if let Some(ranking) = rank_names(
            sample.signature(),
            self.names
                .iter()
                .enumerate()
                .map(|(index, entry)| (index, entry.sample.signature())),
        )
        .filter(|ranking| ranking.score >= NAME_MATCH_THRESHOLD)
        {
            if !ranking.accepts() {
                return None;
            }
            let entry = &mut self.names[ranking.best];
            if entry.thumbnail.is_none() {
                entry.thumbnail = thumbnail;
            }
            return Some(ranking.best);
        }
        self.names.push(EquipmentName { sample, thumbnail });
        Some(self.names.len() - 1)
    }
}
