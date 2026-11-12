use std::time::Instant;

use anyhow::{Context, Result, ensure};
use image::RgbaImage;
use rayon::prelude::*;
use tracing::{debug, trace};

use crate::item::{ItemKind, StratagemCategory};

use super::booster;
use super::icon::{IconTemplate, crop_icon_sample};
use super::matcher::{
    MATCH_THRESHOLD, MatchDomain, PreparedTemplate, SemanticImage, compare, compare_scanned,
    prepare_template, render_to_raster,
};
use super::semantic_extractor::{SemanticExtraction, SemanticSource};
use super::{
    Classification, HOME_ICON_SIZE_LOGICAL, ImageSample, ItemAvailability, LIST_ICON_SIZE_LOGICAL,
    RoiObservation, Slot, SlotKind, SlotLayout,
};
const ENV_PHASES: [f32; 5] = [-0.45, -0.225, 0.0, 0.225, 0.45];
const SCAN_STRATAGEM_MATCH_THRESHOLD: f64 = 0.25;
const SCAN_MATCH_MARGIN: f64 = 0.04;

struct PreparedTemplateEntry {
    item_id: String,
    category: Option<StratagemCategory>,
    prepared: PreparedTemplate,
}

/// Classifies home or list slots against templates captured with a preset.
pub struct TemplateClassifier {
    item_kind: ItemKind,
    layout: SlotLayout,
    templates: Vec<PreparedTemplateEntry>,
    categories: Vec<StratagemCategory>,
    candidate_physical_size: f32,
    match_threshold: f64,
}

#[derive(Clone)]
pub struct TemplateMatchCandidate {
    pub item_id: String,
    pub slot: Slot,
    /// Raw matching error; lower is better.
    pub score: f64,
    pub match_margin: f32,
    pub gate_quality: f32,
}

struct SlotMatchOutcome {
    accepted: bool,
    candidates: Vec<TemplateMatchCandidate>,
}

impl TemplateClassifier {
    pub fn new(
        item_kind: ItemKind,
        sources: &[(String, ImageSample)],
        current_ui_scale: f32,
        initial_page: &RoiObservation,
    ) -> Result<Self> {
        let sources = sources
            .iter()
            .map(|(item_id, sample)| {
                Ok((
                    item_id.clone(),
                    IconTemplate::extract(item_kind, sample)
                        .with_context(|| format!("failed to extract local template {item_id}"))?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        Self::from_templates(item_kind, &sources, current_ui_scale, initial_page)
    }

    pub(crate) fn from_templates(
        item_kind: ItemKind,
        sources: &[(String, IconTemplate)],
        current_ui_scale: f32,
        initial_page: &RoiObservation,
    ) -> Result<Self> {
        ensure!(!sources.is_empty(), "no local templates were provided");
        ensure!(
            sources.iter().all(|(_, source)| match item_kind {
                ItemKind::Stratagem => source.category.is_some(),
                ItemKind::Booster => source.category.is_none(),
            }),
            "local template semantics do not match the requested item kind"
        );
        ensure!(
            current_ui_scale.is_finite() && current_ui_scale > 0.0,
            "current UI scale must be positive"
        );
        ensure!(
            initial_page.layout == SlotLayout::List(item_kind)
                || initial_page.layout == SlotLayout::Home,
            "template classifier cannot identify {} slots in this layout",
            item_kind.label()
        );

        let load_start = Instant::now();
        let categories = sources
            .iter()
            .fold(Vec::new(), |mut categories, (_, source)| {
                if let Some(category) = source.category
                    && !categories.contains(&category)
                {
                    categories.push(category);
                }
                categories
            });

        let sample_slot = initial_page
            .slots
            .iter()
            .find(|slot| is_candidate_slot(slot, item_kind))
            .with_context(|| format!("{} page contains no candidate slot", item_kind.label()))?;
        let candidate_physical_size = match initial_page.layout {
            SlotLayout::Home => HOME_ICON_SIZE_LOGICAL,
            SlotLayout::List(_) => LIST_ICON_SIZE_LOGICAL,
        } * current_ui_scale;
        let candidate_geometry = crop_icon_sample(
            item_kind,
            &initial_page.image,
            sample_slot,
            candidate_physical_size,
        )?;
        let match_threshold = match_threshold(item_kind);

        let prepared = sources
            .par_iter()
            .map(|(item_id, source)| {
                let prepared = prepare_icon_template(
                    item_kind,
                    &source.semantic,
                    source.geometry.physical_size,
                    &candidate_geometry,
                )
                .with_context(|| format!("failed to prepare local template {item_id}"))?;
                Ok(PreparedTemplateEntry {
                    item_id: item_id.clone(),
                    category: source.category,
                    prepared,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        debug!(
            target: "hd2_preset_helper::perf",
            item_kind = %item_kind.label(),
            templates = prepared.len(),
            current_ui_scale,
            candidate_physical_size,
            categories = ?categories
                .iter()
                .map(|category| category.label())
                .collect::<Vec<_>>(),
            template_categories = ?prepared
                .iter()
                .map(|template| (
                    template.item_id.as_str(),
                    template.category.map(StratagemCategory::label)
                ))
                .collect::<Vec<_>>(),
            elapsed_s = load_start.elapsed().as_secs_f64(),
            "template matching profile built"
        );

        Ok(Self {
            item_kind,
            layout: initial_page.layout,
            templates: prepared,
            categories,
            candidate_physical_size,
            match_threshold,
        })
    }

    pub(crate) fn same_item(
        kind: ItemKind,
        left: &ImageSample,
        right: &ImageSample,
    ) -> Result<bool> {
        let template = IconTemplate::extract(kind, left)?;
        let candidate = match template.category {
            Some(category) => SemanticSource::prepare(right)?.extract(category)?,
            None => booster::extract(right)?,
        };
        let prepared = prepare_icon_template(
            kind,
            &template.semantic,
            template.geometry.physical_size,
            right,
        )?;
        let result = compare(&prepared, &candidate.image, right.geometry.physical_size)?;
        Ok(result.score < match_threshold(kind))
    }

    pub(crate) fn item_availability(
        &self,
        screenshot: &RgbaImage,
        slot: &Slot,
    ) -> Result<ItemAvailability> {
        if self.item_kind != ItemKind::Booster {
            return Ok(ItemAvailability::Available);
        }
        let sample = crop_icon_sample(
            self.item_kind,
            screenshot,
            slot,
            self.candidate_physical_size,
        )?;
        let yellow_luma = booster::yellow_luma(&booster::extract(&sample)?);
        Ok(if yellow_luma >= booster::MIN_AVAILABLE_YELLOW_LUMA {
            ItemAvailability::Available
        } else {
            ItemAvailability::Unavailable { yellow_luma }
        })
    }

    pub(crate) fn match_scanned(
        kind: ItemKind,
        selected: &[ImageSample],
        scanned: &[ImageSample],
    ) -> Result<Vec<usize>> {
        if selected.is_empty() {
            return Ok(Vec::new());
        }
        let threshold = match kind {
            ItemKind::Stratagem => SCAN_STRATAGEM_MATCH_THRESHOLD,
            ItemKind::Booster => booster::MATCH_THRESHOLD,
        };
        let first = scanned.first().context("the scanned icon list is empty")?;
        let scores = selected
            .par_iter()
            .map(|source| {
                let template = IconTemplate::extract(kind, source)?;
                let prepared = prepare_icon_template(
                    kind,
                    &template.semantic,
                    template.geometry.physical_size,
                    first,
                )?;
                scanned
                    .iter()
                    .map(|sample| {
                        let candidate = match template.category {
                            Some(category) => SemanticSource::prepare(sample)?.extract(category)?,
                            None => booster::extract(sample)?,
                        };
                        let result = match kind {
                            ItemKind::Stratagem => compare_scanned(
                                &prepared,
                                &candidate.image,
                                sample.geometry.physical_size,
                            ),
                            ItemKind::Booster => {
                                compare(&prepared, &candidate.image, sample.geometry.physical_size)
                            }
                        }?;
                        Ok(result.score)
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;
        let mut positions = Vec::with_capacity(selected.len());
        for (index, row) in scores.iter().enumerate() {
            let mut ranked = row.iter().copied().enumerate().collect::<Vec<_>>();
            ranked.sort_unstable_by(|left, right| left.1.total_cmp(&right.1));
            let (position, score) = ranked[0];
            let margin = ranked
                .get(1)
                .map_or(f64::INFINITY, |(_, next)| next - score);
            let reverse_margin = scores
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, scores)| scores[position] - score)
                .min_by(f64::total_cmp)
                .unwrap_or(f64::INFINITY);
            debug!(
                item_kind = %kind.label(), home_sample = index, position,
                score, threshold, margin, reverse_margin, "scanned selection match evaluated"
            );
            ensure!(
                score < threshold
                    && margin >= SCAN_MATCH_MARGIN
                    && reverse_margin >= SCAN_MATCH_MARGIN
                    && !positions.contains(&position),
                "an equipped {} could not be uniquely located in the scanned list",
                kind.label(),
            );
            positions.push(position);
        }
        Ok(positions)
    }

    pub fn classify_batch(&self, page: &mut RoiObservation) -> Result<Vec<TemplateMatchCandidate>> {
        ensure!(
            page.layout == self.layout,
            "local {} matcher cannot classify another layout",
            self.item_kind.label()
        );
        let started = Instant::now();
        let screenshot = &page.image;
        let results = page
            .slots
            .par_iter_mut()
            .filter(|slot| is_candidate_slot(slot, self.item_kind))
            .map(|slot| {
                self.classify_slot(screenshot, slot).with_context(|| {
                    format!(
                        "failed to classify {} slot row {} col {}",
                        self.item_kind.label(),
                        slot.row,
                        slot.col
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let accepted = results.iter().filter(|result| result.accepted).count();
        let page_candidates = self
            .templates
            .iter()
            .filter_map(|template| {
                results
                    .iter()
                    .flat_map(|result| &result.candidates)
                    .filter(|candidate| candidate.item_id == template.item_id)
                    .min_by(|left, right| left.score.total_cmp(&right.score))
                    .cloned()
            })
            .collect();

        debug!(
            target: "hd2_preset_helper::perf",
            algorithm = "template_matcher",
            slots = results.len(),
            templates = self.templates.len(),
            comparisons = results.len() * self.templates.len(),
            accepted,
            rejected = results.len() - accepted,
            elapsed_s = started.elapsed().as_secs_f64(),
            "template classifier timing"
        );
        Ok(page_candidates)
    }

    fn classify_slot(&self, screenshot: &RgbaImage, slot: &mut Slot) -> Result<SlotMatchOutcome> {
        let sample = crop_icon_sample(
            self.item_kind,
            screenshot,
            slot,
            self.candidate_physical_size,
        )?;
        let extractions = match self.item_kind {
            ItemKind::Stratagem => {
                let source = SemanticSource::prepare(&sample)?;
                self.categories
                    .iter()
                    .map(|&category| {
                        source
                            .extract(category)
                            .map(|extraction| (Some(category), extraction))
                    })
                    .collect::<Result<Vec<_>>>()?
            }
            ItemKind::Booster => vec![(None, booster::extract(&sample)?)],
        };
        let mut ranked = self
            .templates
            .iter()
            .map(|template| {
                let extraction = extractions
                    .iter()
                    .find(|(category, _)| *category == template.category)
                    .map(|(_, extraction)| extraction)
                    .expect("every template category has a candidate extraction");
                compare(
                    &template.prepared,
                    &extraction.image,
                    self.candidate_physical_size,
                )
                .map(|result| (template, extraction, result))
            })
            .collect::<Result<Vec<_>>>()?;
        ranked.sort_by(|left, right| left.2.score.total_cmp(&right.2.score));

        let (best, best_extraction, result) = ranked
            .first()
            .context("template scoring produced no result")?;
        let second_score = ranked.get(1).map(|(_, _, result)| result.score);
        let margin = second_score.map_or(f64::INFINITY, |second| second - result.score);
        let accepted = result.score < self.match_threshold;
        let candidates = ranked
            .iter()
            .map(|(template, _, candidate_result)| {
                let competing_score = ranked
                    .iter()
                    .filter(|(other, _, _)| other.item_id != template.item_id)
                    .map(|(_, _, result)| result.score)
                    .min_by(f64::total_cmp);
                let candidate_margin =
                    competing_score.map_or(f64::INFINITY, |other| other - candidate_result.score);
                TemplateMatchCandidate {
                    item_id: template.item_id.clone(),
                    slot: slot.clone(),
                    score: candidate_result.score,
                    match_margin: candidate_margin as f32,
                    gate_quality: ((self.match_threshold - candidate_result.score)
                        / self.match_threshold)
                        .clamp(0.0, 1.0) as f32,
                }
            })
            .collect();

        trace_match(
            slot,
            best.item_id.as_str(),
            best.category,
            result,
            margin,
            self.match_threshold,
            best_extraction,
        );
        slot.classification = accepted.then(|| Classification {
            item_id: best.item_id.clone(),
            match_error: result.score as f32,
            match_margin: margin as f32,
            gate_quality: ((self.match_threshold - result.score) / self.match_threshold)
                .clamp(0.0, 1.0) as f32,
        });
        Ok(SlotMatchOutcome {
            accepted,
            candidates,
        })
    }
}

fn match_threshold(kind: ItemKind) -> f64 {
    match kind {
        ItemKind::Stratagem => MATCH_THRESHOLD,
        ItemKind::Booster => booster::MATCH_THRESHOLD,
    }
}

fn prepare_icon_template(
    kind: ItemKind,
    semantic: &SemanticImage,
    physical_size: f32,
    candidate: &ImageSample,
) -> Result<PreparedTemplate> {
    let env_candidates = (0..ENV_PHASES.len().pow(2)).map(|index| {
        let phase_y = ENV_PHASES[index / ENV_PHASES.len()];
        let phase_x = ENV_PHASES[index % ENV_PHASES.len()];
        render_to_raster(
            semantic,
            physical_size,
            (
                candidate.image.width() as usize,
                candidate.image.height() as usize,
            ),
            (candidate.geometry.center_x, candidate.geometry.center_y),
            candidate.geometry.physical_size,
            (phase_x, phase_y),
        )
    });
    let domain = match kind {
        ItemKind::Stratagem => MatchDomain::Full,
        ItemKind::Booster => MatchDomain::InsetHex {
            half_width: booster::INTERIOR_HALF_WIDTH,
        },
    };
    prepare_template(
        semantic,
        physical_size,
        env_candidates,
        candidate.geometry.physical_size,
        domain,
        kind == ItemKind::Stratagem,
    )
}

fn is_candidate_slot(slot: &Slot, item_kind: ItemKind) -> bool {
    slot.kind.is_selectable_item_for(item_kind)
        || (item_kind == ItemKind::Booster && slot.kind == SlotKind::HomeBooster)
}

fn trace_match(
    slot: &Slot,
    item_id: &str,
    category: Option<StratagemCategory>,
    result: &super::matcher::MatchResult,
    margin: f64,
    threshold: f64,
    extraction: &SemanticExtraction,
) {
    trace!(
        target: "hd2_preset_helper::template",
        algorithm = "template_matcher",
        item_id,
        row = slot.row,
        col = slot.col,
        category = category.map(StratagemCategory::label),
        accepted = result.score < threshold,
        score = result.score,
        threshold,
        margin,
        evidence = result.evidence,
        white_evidence = result.white_evidence,
        class_evidence = result.class_evidence,
        spatial = result.spatial,
        qx = result.qx,
        qy = result.qy,
        evaluations = result.evaluations,
        global_white = result.global_white,
        local_white = result.local_white,
        global_class = result.global_class,
        local_class = result.local_class,
        overlap_white = result.overlap_white,
        overlap_class = result.overlap_class,
        white_gain = result.white_gain,
        class_gain = result.class_gain,
        semantic_mode = extraction.mode,
        primary_endpoint = %format_args!("{:.3?}", extraction.primary_endpoint),
        secondary_endpoint = %format_args!("{:.3?}", extraction.secondary_endpoint),
        primary_mass = extraction.primary_mass,
        secondary_mass = extraction.secondary_mass,
        "template match evaluated"
    );
}
