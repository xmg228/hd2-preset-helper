use std::time::Instant;

use anyhow::{Context, Result, ensure};
use image::RgbaImage;
use rayon::prelude::*;
use tracing::{debug, trace};

use crate::item::{ItemKind, StratagemCategory};

use super::booster;
use super::matcher::{
    MATCH_THRESHOLD, MatchDomain, PreparedTemplate, SemanticImage, compare, prepare_template,
    render_to_raster,
};
use super::semantic_extractor::{
    SemanticExtraction, SemanticSource, crop_slot_sample, stratagem_foreground_response,
};
use super::{
    Classification, ImageSample, ItemAvailability, LIST_ICON_SIZE_LOGICAL, RoiObservation, Slot,
    SlotLayout,
};
const ENV_PHASES: [f32; 5] = [-0.45, -0.225, 0.0, 0.225, 0.45];

struct PreparedTemplateEntry {
    item_id: String,
    category: Option<StratagemCategory>,
    prepared: PreparedTemplate,
    reference_yellow_luma: Option<f32>,
}

struct TemplateSource {
    item_id: String,
    category: Option<StratagemCategory>,
    semantic: SemanticImage,
    physical_size: f32,
    reference_yellow_luma: Option<f32>,
}

/// Classifies selectable list slots against templates captured with a preset.
pub struct TemplateClassifier {
    item_kind: ItemKind,
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
    pub availability: ItemAvailability,
}

struct SlotMatchOutcome {
    accepted: bool,
    candidates: Vec<TemplateMatchCandidate>,
    #[cfg(feature = "diagnostics")]
    diagnostics: super::diagnostics::SlotDiagnostics,
}

impl TemplateClassifier {
    pub fn new(
        item_kind: ItemKind,
        sources: Vec<(String, ImageSample)>,
        current_ui_scale: f32,
        initial_page: &RoiObservation,
    ) -> Result<Self> {
        ensure!(!sources.is_empty(), "no local templates were provided");
        ensure!(
            current_ui_scale.is_finite() && current_ui_scale > 0.0,
            "current UI scale must be positive"
        );
        ensure!(
            initial_page.layout == SlotLayout::List(item_kind),
            "template classifier requires an open {} list",
            item_kind.label()
        );

        let load_start = Instant::now();
        let sources = sources
            .into_iter()
            .map(|(item_id, sample)| {
                let (category, semantic, reference_yellow_luma) = match item_kind {
                    ItemKind::Stratagem => {
                        let source = SemanticSource::prepare(&sample).with_context(|| {
                            format!("failed to prepare local template {item_id}")
                        })?;
                        let category = source
                            .infer_category()
                            .with_context(|| format!("failed to infer category for {item_id}"))?;
                        let semantic = source
                            .extract(category)
                            .with_context(|| format!("failed to extract local template {item_id}"))?
                            .image;
                        (Some(category), semantic, None)
                    }
                    ItemKind::Booster => {
                        let extraction = booster::extract(&sample).with_context(|| {
                            format!("failed to extract local template {item_id}")
                        })?;
                        let reference_yellow_luma = booster::yellow_luma(&extraction);
                        (None, extraction.image, Some(reference_yellow_luma))
                    }
                };
                Ok(TemplateSource {
                    item_id,
                    category,
                    semantic,
                    physical_size: sample.geometry.physical_size,
                    reference_yellow_luma,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let categories = sources.iter().fold(Vec::new(), |mut categories, source| {
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
            .find(|slot| slot.kind.is_selectable_item_for(item_kind))
            .with_context(|| format!("{} list contains no candidate slot", item_kind.label()))?;
        let candidate_physical_size = LIST_ICON_SIZE_LOGICAL * current_ui_scale;
        let candidate_geometry = crop_sample(
            item_kind,
            &initial_page.image,
            sample_slot,
            candidate_physical_size,
        )?;
        let domain = match item_kind {
            ItemKind::Stratagem => MatchDomain::Full,
            ItemKind::Booster => MatchDomain::InsetHex {
                half_width: booster::INTERIOR_HALF_WIDTH,
            },
        };
        let match_threshold = match item_kind {
            ItemKind::Stratagem => MATCH_THRESHOLD,
            ItemKind::Booster => booster::MATCH_THRESHOLD,
        };

        let prepared = sources
            .into_par_iter()
            .map(|source| {
                let mut env_candidates = Vec::with_capacity(25);
                for phase_y in ENV_PHASES {
                    for phase_x in ENV_PHASES {
                        env_candidates.push(render_to_raster(
                            &source.semantic,
                            source.physical_size,
                            (
                                candidate_geometry.image.width() as usize,
                                candidate_geometry.image.height() as usize,
                            ),
                            (
                                candidate_geometry.geometry.center_x,
                                candidate_geometry.geometry.center_y,
                            ),
                            candidate_physical_size,
                            (phase_x, phase_y),
                        ));
                    }
                }
                let prepared = prepare_template(
                    &source.semantic,
                    source.physical_size,
                    &env_candidates,
                    candidate_physical_size,
                    domain,
                    item_kind == ItemKind::Stratagem,
                )
                .with_context(|| format!("failed to prepare local template {}", source.item_id))?;
                Ok(PreparedTemplateEntry {
                    item_id: source.item_id,
                    category: source.category,
                    prepared,
                    reference_yellow_luma: source.reference_yellow_luma,
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
            elapsed = ?load_start.elapsed(),
            "template matching profile built"
        );

        Ok(Self {
            item_kind,
            templates: prepared,
            categories,
            candidate_physical_size,
            match_threshold,
        })
    }

    pub(crate) fn alignment_response(
        &self,
        screenshot: &RgbaImage,
        slot: &Slot,
    ) -> Result<Vec<u8>> {
        let sample = crop_sample(
            self.item_kind,
            screenshot,
            slot,
            self.candidate_physical_size,
        )?;
        match self.item_kind {
            ItemKind::Stratagem => Ok(sample
                .image
                .pixels()
                .map(|pixel| stratagem_foreground_response(pixel[0], pixel[1], pixel[2]))
                .collect()),
            ItemKind::Booster => booster::glyph_response(&sample),
        }
    }

    pub fn classify_batch(&self, page: &mut RoiObservation) -> Result<Vec<TemplateMatchCandidate>> {
        ensure!(
            page.layout == SlotLayout::List(self.item_kind),
            "local {} matcher cannot classify another layout",
            self.item_kind.label()
        );
        let started = Instant::now();
        let screenshot = &page.image;
        let results = page
            .slots
            .par_iter_mut()
            .filter(|slot| slot.kind.is_selectable_item_for(self.item_kind))
            .map(|slot| self.classify_slot(screenshot, slot))
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

        #[cfg(feature = "diagnostics")]
        super::diagnostics::record_frame(results.iter().map(|result| &result.diagnostics))?;

        debug!(
            target: "hd2_preset_helper::perf",
            algorithm = "template_matcher",
            slots = results.len(),
            templates = self.templates.len(),
            comparisons = results.len() * self.templates.len(),
            accepted,
            failed = results.len() - accepted,
            elapsed = ?started.elapsed(),
            "template classifier timing"
        );
        Ok(page_candidates)
    }

    fn classify_slot(&self, screenshot: &RgbaImage, slot: &mut Slot) -> Result<SlotMatchOutcome> {
        let sample = crop_sample(
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
            .map(|(template, extraction, candidate_result)| {
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
                    availability: item_availability(template, extraction),
                }
            })
            .collect();

        #[cfg(feature = "diagnostics")]
        let diagnostics = super::diagnostics::collect_slot(
            slot,
            ranked.iter().map(|(template, extraction, result)| {
                (
                    template.item_id.as_str(),
                    template.category,
                    *extraction,
                    *result,
                )
            }),
            accepted,
            self.match_threshold,
        );
        trace_match(
            best.item_id.as_str(),
            best.category,
            result,
            margin,
            accepted,
            self.match_threshold,
            best_extraction,
        );
        slot.classification = accepted.then(|| Classification {
            item_id: best.item_id.clone(),
            match_error: result.score as f32,
            match_margin: margin as f32,
            gate_quality: ((self.match_threshold - result.score) / self.match_threshold)
                .clamp(0.0, 1.0) as f32,
            availability: item_availability(best, best_extraction),
        });
        Ok(SlotMatchOutcome {
            accepted,
            candidates,
            #[cfg(feature = "diagnostics")]
            diagnostics,
        })
    }
}

fn item_availability(
    template: &PreparedTemplateEntry,
    extraction: &SemanticExtraction,
) -> ItemAvailability {
    let Some(reference) = template.reference_yellow_luma else {
        return ItemAvailability::Available;
    };
    let brightness_ratio = booster::yellow_luma(extraction) / reference;
    if brightness_ratio >= booster::AVAILABLE_BRIGHTNESS_RATIO {
        ItemAvailability::Available
    } else {
        ItemAvailability::Unavailable { brightness_ratio }
    }
}

fn trace_match(
    item_id: &str,
    category: Option<StratagemCategory>,
    result: &super::matcher::MatchResult,
    margin: f64,
    accepted: bool,
    threshold: f64,
    extraction: &SemanticExtraction,
) {
    trace!(
        target: "hd2_preset_helper::template",
        algorithm = "template_matcher",
        item_id,
        category = category.map(StratagemCategory::label),
        accepted,
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
        primary_endpoint = ?extraction.primary_endpoint,
        secondary_endpoint = ?extraction.secondary_endpoint,
        primary_mass = extraction.primary_mass,
        secondary_mass = extraction.secondary_mass,
        "template match evaluated"
    );
}

fn crop_sample(
    item_kind: ItemKind,
    image: &RgbaImage,
    slot: &Slot,
    physical_size: f32,
) -> Result<ImageSample> {
    match item_kind {
        ItemKind::Stratagem => crop_slot_sample(image, slot, physical_size),
        ItemKind::Booster => booster::crop_sample(image, slot, physical_size),
    }
}
