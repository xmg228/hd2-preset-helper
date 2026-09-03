use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use anyhow::{Result, anyhow, bail, ensure};
use image::RgbaImage;
use rayon::prelude::*;
use tracing::{debug, trace};

use crate::assets::{IconCatalog, icon_image, resize_rgba_box};
use crate::item::{ItemKind, StratagemCategory};

use super::color::{
    StratagemCategoryCandidates, StratagemColorEvidence, add_stratagem_color_evidence, luma601_u8,
};
use super::{Classification, Slot, SlotLayout};

const TEMPLATE_BACKGROUND: f32 = 30.0;
const ALPHA_MASK_THRESHOLD: u8 = 64;
const BOOSTER_HEX_CALIBRATION_RADIUS: f32 = 120.0 / 256.0;
const BOOSTER_SHELL_INSET_PX: f32 = 3.0;
const REGULAR_HEX_HALF_HEIGHT_RATIO: f32 = 0.866_025_4;
const LIST_ICON_SCALE: f32 = 68.0 / 104.0;
const HOME_STRATAGEM_SCALE: f32 = 93.0 / 104.0;
const HOME_BOOSTER_SCALE: f32 = 93.0 / 104.0;
const STRATAGEM_GRAY_WEIGHT: f32 = 0.40;
const STRATAGEM_GRADIENT_WEIGHT: f32 = 0.60;
const REFINE_SCALE_RADIUS: i32 = 1;
const MARGIN_QUALITY_SPAN: f32 = 0.10;

struct SourceTemplate {
    item_id: String,
    kind: ItemKind,
    category: Option<StratagemCategory>,
    path: Box<str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ProfileKey {
    kind: ItemKind,
    canvas_w: u32,
    canvas_h: u32,
    icon_size: u32,
}

#[derive(Clone, Copy)]
struct MaskPoint {
    x: u16,
    y: u16,
}

struct PreparedTemplate {
    source_index: usize,
    category: Option<StratagemCategory>,
    gray: Vec<f32>,
    gradient: Vec<f32>,
}

struct TemplateProfile {
    key: ProfileKey,
    mask: Vec<MaskPoint>,
    templates: Vec<PreparedTemplate>,
}

struct RenderedTemplate {
    source_index: usize,
    category: Option<StratagemCategory>,
    gray: Vec<f32>,
    gradient: Vec<f32>,
    alpha: Vec<u8>,
}

struct SlotFeatures {
    width: usize,
    height: usize,
    gray: Vec<f32>,
    gradient: Vec<f32>,
    categories: StratagemCategoryCandidates,
}

struct MatchOutcome {
    classification: Option<Classification>,
    deepest_layer: SearchLayer,
    all_category_fallback: bool,
}

struct MatchStats {
    accepted: bool,
    deepest_layer: SearchLayer,
    all_category_fallback: bool,
}

struct LayerSearchResult {
    classification: Option<Classification>,
    deepest_layer: SearchLayer,
}

#[derive(Default)]
struct ScoreScratch {
    gray: Vec<f32>,
    gradient: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SearchLayer {
    Initial,
    Translation,
    Scale,
}

pub struct TemplateClassifier {
    sources: Vec<SourceTemplate>,
    profiles: RwLock<HashMap<ProfileKey, Arc<TemplateProfile>>>,
    profile_build_lock: Mutex<()>,
    threshold: f32,
    min_margin: f32,
}

impl TemplateClassifier {
    pub fn load(catalog: &IconCatalog, threshold: f32, min_margin: f32) -> Result<Self> {
        let mut items = catalog.iter().collect::<Vec<_>>();
        items.sort_unstable_by_key(|(item_id, _)| *item_id);
        let sources = items
            .into_iter()
            .map(|(item_id, item)| SourceTemplate {
                item_id: item_id.to_owned(),
                kind: item.kind,
                category: item.category,
                path: item.path.clone(),
            })
            .collect::<Vec<_>>();

        ensure!(
            sources
                .iter()
                .any(|source| source.kind == ItemKind::Stratagem),
            "icon manifest contains no stratagem templates"
        );
        ensure!(
            sources
                .iter()
                .any(|source| source.kind == ItemKind::Booster),
            "icon manifest contains no booster templates"
        );

        debug!(
            stratagems = sources
                .iter()
                .filter(|source| source.kind == ItemKind::Stratagem)
                .count(),
            boosters = sources
                .iter()
                .filter(|source| source.kind == ItemKind::Booster)
                .count(),
            "loaded icon template metadata"
        );

        Ok(Self {
            sources,
            profiles: RwLock::new(HashMap::new()),
            profile_build_lock: Mutex::new(()),
            threshold,
            min_margin,
        })
    }

    fn prewarm_nominal_profiles(&self, slots: &[Slot], layout: SlotLayout) -> Result<()> {
        let mut keys = Vec::new();
        for slot in slots {
            let Some(kind) = classification_kind_for_slot(slot, layout) else {
                continue;
            };
            let key = ProfileKey {
                kind,
                canvas_w: slot.w,
                canvas_h: slot.h,
                icon_size: nominal_icon_size(layout, slot, kind),
            };
            if !keys.contains(&key) {
                keys.push(key);
            }
        }

        // The first two search layers use only the nominal profile. Neighboring
        // sizes are built lazily if the scale layer is reached, then cached for
        // later slots with the same item kind and canvas size.
        for key in keys {
            self.profile(key)?;
        }
        Ok(())
    }

    fn profile(&self, key: ProfileKey) -> Result<Arc<TemplateProfile>> {
        if let Some(profile) = self
            .profiles
            .read()
            .map_err(|_| anyhow!("template profile cache lock was poisoned"))?
            .get(&key)
            .cloned()
        {
            return Ok(profile);
        }

        let _build_guard = self
            .profile_build_lock
            .lock()
            .map_err(|_| anyhow!("template profile build lock was poisoned"))?;
        if let Some(profile) = self
            .profiles
            .read()
            .map_err(|_| anyhow!("template profile cache lock was poisoned"))?
            .get(&key)
            .cloned()
        {
            return Ok(profile);
        }

        let built = Arc::new(self.build_profile(key)?);
        self.profiles
            .write()
            .map_err(|_| anyhow!("template profile cache lock was poisoned"))?
            .insert(key, built.clone());
        Ok(built)
    }

    fn build_profile(&self, key: ProfileKey) -> Result<TemplateProfile> {
        let total_start = Instant::now();
        ensure!(
            key.canvas_w > 0 && key.canvas_h > 0,
            "zero-size template canvas"
        );
        ensure!(key.icon_size > 0, "zero-size icon template");
        ensure!(
            key.icon_size <= key.canvas_w.min(key.canvas_h),
            "icon size {} exceeds template canvas {}x{}",
            key.icon_size,
            key.canvas_w,
            key.canvas_h
        );

        let render_start = Instant::now();
        let rendered_results: Vec<Result<RenderedTemplate>> = self
            .sources
            .par_iter()
            .enumerate()
            .filter(|(_, source)| source.kind == key.kind)
            .map(|(source_index, source)| render_source(source_index, source, key))
            .collect();
        let rendered = rendered_results.into_iter().collect::<Result<Vec<_>>>()?;
        ensure!(
            !rendered.is_empty(),
            "template profile has no source images"
        );
        let render_time = render_start.elapsed();
        let prepare_start = Instant::now();

        let pixel_count = key.canvas_w as usize * key.canvas_h as usize;
        let mut union_alpha = vec![0u8; pixel_count];
        for template in &rendered {
            for (union, alpha) in union_alpha.iter_mut().zip(&template.alpha) {
                *union = (*union).max(*alpha);
            }
        }

        let mask: Vec<MaskPoint> = union_alpha
            .iter()
            .enumerate()
            .filter_map(|(index, alpha)| {
                let x = index % key.canvas_w as usize;
                let y = index / key.canvas_w as usize;
                (*alpha > ALPHA_MASK_THRESHOLD
                    && (key.kind != ItemKind::Booster
                        || booster_interior_contains(key, x as u32, y as u32)))
                .then_some(MaskPoint {
                    x: x as u16,
                    y: y as u16,
                })
            })
            .collect();
        ensure!(mask.len() >= 64, "template mask is unexpectedly small");

        let prepared_results: Vec<Result<PreparedTemplate>> = rendered
            .into_par_iter()
            .map(|template| {
                let gray =
                    normalized_mask_values(&template.gray, key.canvas_w as usize, &mask, 0, 0)?;
                let gradient = if key.kind == ItemKind::Stratagem {
                    normalized_mask_values(&template.gradient, key.canvas_w as usize, &mask, 0, 0)?
                } else {
                    Vec::new()
                };
                Ok(PreparedTemplate {
                    source_index: template.source_index,
                    category: template.category,
                    gray,
                    gradient,
                })
            })
            .collect();
        let templates = prepared_results.into_iter().collect::<Result<Vec<_>>>()?;

        let prepare_time = prepare_start.elapsed();
        debug!(
            target: "hd2_preset_helper::perf",
            kind = ?key.kind,
            canvas_w = key.canvas_w,
            canvas_h = key.canvas_h,
            icon_size = key.icon_size,
            mask_pixels = mask.len(),
            booster_shell_ignored = key.kind == ItemKind::Booster,
            templates = templates.len(),
            decode_render = ?render_time,
            prepare = ?prepare_time,
            total = ?total_start.elapsed(),
            "template profile built"
        );

        Ok(TemplateProfile {
            key,
            mask,
            templates,
        })
    }

    fn classify_slot(
        &self,
        rgba: &RgbaImage,
        slot: &Slot,
        layout: SlotLayout,
        kind: ItemKind,
    ) -> Result<MatchOutcome> {
        let features = extract_slot_features(rgba, slot, kind)?;
        let nominal = nominal_icon_size(layout, slot, kind);
        let categories = if kind == ItemKind::Stratagem {
            features.categories
        } else {
            StratagemCategoryCandidates::All
        };
        let initial_profile = self.profile(ProfileKey {
            kind,
            canvas_w: slot.w,
            canvas_h: slot.h,
            icon_size: nominal,
        })?;
        let mut scores = vec![f32::NEG_INFINITY; self.sources.len()];
        let mut scratch = ScoreScratch::default();

        let first = self.search_layers(
            &features,
            categories,
            false,
            &initial_profile,
            &mut scores,
            &mut scratch,
        )?;
        let use_all_categories = kind == ItemKind::Stratagem
            && categories != StratagemCategoryCandidates::All
            && first.classification.is_none();
        if !use_all_categories {
            return Ok(MatchOutcome {
                classification: first.classification,
                deepest_layer: first.deepest_layer,
                all_category_fallback: false,
            });
        }

        scores.fill(f32::NEG_INFINITY);
        let fallback = self.search_layers(
            &features,
            StratagemCategoryCandidates::All,
            true,
            &initial_profile,
            &mut scores,
            &mut scratch,
        )?;
        Ok(MatchOutcome {
            classification: fallback.classification,
            deepest_layer: first.deepest_layer.max(fallback.deepest_layer),
            all_category_fallback: true,
        })
    }

    fn search_layers(
        &self,
        features: &SlotFeatures,
        categories: StratagemCategoryCandidates,
        all_category_fallback: bool,
        initial_profile: &TemplateProfile,
        scores: &mut [f32],
        scratch: &mut ScoreScratch,
    ) -> Result<LayerSearchResult> {
        let key = initial_profile.key;
        let kind = key.kind;
        let initial_offsets = initial_offsets(key.canvas_w, key.canvas_h, key.icon_size);
        let translation_offsets = offsets_excluding(SEARCH_OFFSETS, initial_offsets);
        score_profile(
            features,
            initial_profile,
            categories,
            initial_offsets,
            scores,
            scratch,
        )?;
        if let Some(classification) =
            self.evaluate_scores(scores, kind, SearchLayer::Initial, all_category_fallback)?
        {
            return Ok(LayerSearchResult {
                classification: Some(classification),
                deepest_layer: SearchLayer::Initial,
            });
        }

        if !translation_offsets.is_empty() {
            score_profile(
                features,
                initial_profile,
                categories,
                &translation_offsets,
                scores,
                scratch,
            )?;
        }
        if let Some(classification) = self.evaluate_scores(
            scores,
            kind,
            SearchLayer::Translation,
            all_category_fallback,
        )? {
            return Ok(LayerSearchResult {
                classification: Some(classification),
                deepest_layer: SearchLayer::Translation,
            });
        }

        for icon_size in neighboring_icon_sizes(key.icon_size, key.canvas_w.min(key.canvas_h)) {
            let profile = self.profile(ProfileKey { icon_size, ..key })?;
            score_profile(
                features,
                &profile,
                categories,
                SEARCH_OFFSETS,
                scores,
                scratch,
            )?;
        }
        Ok(LayerSearchResult {
            classification: self.evaluate_scores(
                scores,
                kind,
                SearchLayer::Scale,
                all_category_fallback,
            )?,
            deepest_layer: SearchLayer::Scale,
        })
    }

    fn evaluate_scores(
        &self,
        scores: &[f32],
        kind: ItemKind,
        search_layer: SearchLayer,
        all_category_fallback: bool,
    ) -> Result<Option<Classification>> {
        let mut best: Option<(usize, f32)> = None;
        let mut second_score = f32::NEG_INFINITY;
        for (index, score) in scores.iter().copied().enumerate() {
            if !score.is_finite() || self.sources[index].kind != kind {
                continue;
            }
            match best {
                Some((_, current)) if score <= current => {
                    second_score = second_score.max(score);
                }
                Some((_, current)) => {
                    second_score = current;
                    best = Some((index, score));
                }
                None => best = Some((index, score)),
            }
        }

        let Some((best_index, best)) = best else {
            bail!("template scoring produced no finite candidates for {kind:?}");
        };
        let margin = if second_score.is_finite() {
            best - second_score
        } else {
            f32::INFINITY
        };
        if best < self.threshold || margin < self.min_margin {
            trace!(
                target: "hd2_preset_helper::template",
                item_id = %self.sources[best_index].item_id,
                raw_score = best,
                raw_margin = margin,
                score_threshold = self.threshold,
                margin_threshold = self.min_margin,
                search_layer = ?search_layer,
                all_category_fallback,
                "template match below acceptance gates"
            );
            return Ok(None);
        }

        let gate_quality = self.gate_quality(best, margin);
        trace!(
            target: "hd2_preset_helper::template",
            item_id = %self.sources[best_index].item_id,
            match_score = best,
            match_margin = margin,
            gate_quality,
            search_layer = ?search_layer,
            all_category_fallback,
            "template match accepted"
        );
        Ok(Some(Classification {
            item_id: self.sources[best_index].item_id.clone(),
            match_score: best,
            match_margin: margin,
            gate_quality,
        }))
    }

    fn gate_quality(&self, score: f32, margin: f32) -> f32 {
        let score_span = (1.0 - self.threshold).max(0.05);
        let score_quality = ((score - self.threshold) / score_span).clamp(0.0, 1.0);
        let margin_quality = if margin.is_finite() {
            ((margin - self.min_margin) / MARGIN_QUALITY_SPAN).clamp(0.0, 1.0)
        } else {
            1.0
        };
        score_quality.min(margin_quality)
    }

    pub fn classify_batch(
        &self,
        screenshot: &RgbaImage,
        slots: &mut [Slot],
        layout: SlotLayout,
    ) -> Result<()> {
        let classifiable = slots
            .iter()
            .filter(|slot| classification_kind_for_slot(slot, layout).is_some())
            .count();
        if classifiable == 0 {
            return Ok(());
        }

        let total_start = Instant::now();
        self.prewarm_nominal_profiles(slots, layout)?;
        let prewarm_time = total_start.elapsed();

        let match_start = Instant::now();
        let outcomes: Vec<Result<Option<MatchStats>>> = slots
            .par_iter_mut()
            .map(|slot| {
                let Some(kind) = classification_kind_for_slot(slot, layout) else {
                    return Ok(None);
                };
                let outcome = self.classify_slot(screenshot, slot, layout, kind)?;
                let stats = MatchStats {
                    accepted: outcome.classification.is_some(),
                    deepest_layer: outcome.deepest_layer,
                    all_category_fallback: outcome.all_category_fallback,
                };
                slot.classification = outcome.classification;
                Ok(Some(stats))
            })
            .collect();
        let outcomes = outcomes
            .into_iter()
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let match_time = match_start.elapsed();
        let translation_attempts = outcomes
            .iter()
            .filter(|outcome| outcome.deepest_layer >= SearchLayer::Translation)
            .count();
        let scale_attempts = outcomes
            .iter()
            .filter(|outcome| outcome.deepest_layer >= SearchLayer::Scale)
            .count();
        let all_category_attempts = outcomes
            .iter()
            .filter(|outcome| outcome.all_category_fallback)
            .count();
        let accepted = outcomes.iter().filter(|outcome| outcome.accepted).count();
        let failed = outcomes.len() - accepted;

        debug!(
            target: "hd2_preset_helper::perf",
            layout = ?layout,
            slots = outcomes.len(),
            accepted,
            failed,
            translation_attempts,
            scale_attempts,
            all_category_attempts,
            prewarm = ?prewarm_time,
            match_time = ?match_time,
            total = ?total_start.elapsed(),
            "template classifier timing"
        );

        Ok(())
    }
}

fn booster_interior_contains(key: ProfileKey, x: u32, y: u32) -> bool {
    let center_x = key.canvas_w as f32 * 0.5;
    let center_y = key.canvas_h as f32 * 0.5;
    let half_width =
        (key.icon_size as f32 * BOOSTER_HEX_CALIBRATION_RADIUS - BOOSTER_SHELL_INSET_PX).max(1.0);
    let half_height = half_width * REGULAR_HEX_HALF_HEIGHT_RATIO;
    let dx = ((x as f32 + 0.5) - center_x).abs();
    let dy = ((y as f32 + 0.5) - center_y).abs();
    let vertical = dy / half_height;
    let row_half_width = half_width * (1.0 - 0.5 * vertical);

    vertical <= 1.0 && dx <= row_half_width
}

fn classification_kind_for_slot(slot: &Slot, layout: SlotLayout) -> Option<ItemKind> {
    if layout == SlotLayout::Home && slot.kind.is_home_booster() {
        Some(ItemKind::Booster)
    } else {
        slot.kind.classification_kind()
    }
}

fn render_source(
    source_index: usize,
    source: &SourceTemplate,
    key: ProfileKey,
) -> Result<RenderedTemplate> {
    let image = icon_image(&source.path)?;
    let resized = resize_rgba_box(&image, key.icon_size, key.icon_size)?;
    drop(image);
    let width = key.canvas_w as usize;
    let height = key.canvas_h as usize;
    let mut gray = vec![TEMPLATE_BACKGROUND; width * height];
    let mut alpha = vec![0u8; width * height];
    let offset_x = (key.canvas_w - key.icon_size) / 2;
    let offset_y = (key.canvas_h - key.icon_size) / 2;

    for y in 0..key.icon_size {
        for x in 0..key.icon_size {
            let pixel = resized.get_pixel(x, y).0;
            let canvas_x = offset_x + x;
            let canvas_y = offset_y + y;
            let index = canvas_y as usize * width + canvas_x as usize;
            let weight = pixel[3] as f32 / 255.0;
            let foreground = luma601_u8(pixel[0], pixel[1], pixel[2]) as f32;
            gray[index] = TEMPLATE_BACKGROUND + (foreground - TEMPLATE_BACKGROUND) * weight;
            alpha[index] = pixel[3];
        }
    }
    let gradient = if key.kind == ItemKind::Stratagem {
        sobel_magnitude(&gray, width, height)
    } else {
        Vec::new()
    };

    Ok(RenderedTemplate {
        source_index,
        category: source.category,
        gray,
        gradient,
        alpha,
    })
}

fn extract_slot_features(
    screenshot: &RgbaImage,
    slot: &Slot,
    kind: ItemKind,
) -> Result<SlotFeatures> {
    ensure!(slot.w > 0 && slot.h > 0, "cannot classify a zero-size slot");
    ensure!(
        slot.x.saturating_add(slot.w) <= screenshot.width()
            && slot.y.saturating_add(slot.h) <= screenshot.height(),
        "slot crop lies outside screenshot"
    );
    let width = slot.w as usize;
    let height = slot.h as usize;
    let mut gray = vec![0.0f32; width * height];
    let mut evidence = StratagemColorEvidence::default();
    let inset_x = ((slot.w as f32 * 0.18).round() as u32).min(slot.w / 2);
    let inset_y = ((slot.h as f32 * 0.18).round() as u32).min(slot.h / 2);

    let screenshot_width = screenshot.width() as usize;
    let raw = screenshot.as_raw();
    for y in 0..slot.h {
        for x in 0..slot.w {
            let source_index =
                ((slot.y + y) as usize * screenshot_width + (slot.x + x) as usize) * 4;
            let r = raw[source_index];
            let g = raw[source_index + 1];
            let b = raw[source_index + 2];
            gray[y as usize * width + x as usize] = luma601_u8(r, g, b) as f32;
            if kind == ItemKind::Stratagem
                && x >= inset_x
                && y >= inset_y
                && x < slot.w - inset_x
                && y < slot.h - inset_y
            {
                add_stratagem_color_evidence(&mut evidence, r, g, b);
            }
        }
    }

    let gradient = if kind == ItemKind::Stratagem {
        sobel_magnitude(&gray, width, height)
    } else {
        Vec::new()
    };
    Ok(SlotFeatures {
        width,
        height,
        gray,
        gradient,
        categories: evidence.candidate_categories(),
    })
}

fn score_profile(
    features: &SlotFeatures,
    profile: &TemplateProfile,
    categories: StratagemCategoryCandidates,
    offsets: &[(i32, i32)],
    scores: &mut [f32],
    scratch: &mut ScoreScratch,
) -> Result<()> {
    debug_assert_eq!(features.width, profile.key.canvas_w as usize);
    debug_assert_eq!(features.height, profile.key.canvas_h as usize);
    let mask_len = profile.mask.len();
    scratch.gray.resize(mask_len, 0.0);
    if profile.key.kind == ItemKind::Stratagem {
        scratch.gradient.resize(mask_len, 0.0);
    } else {
        scratch.gradient.clear();
    }
    for &(dx, dy) in offsets {
        if !normalize_mask_values_into(
            &features.gray,
            features.width,
            features.height,
            &profile.mask,
            dx,
            dy,
            &mut scratch.gray,
        )? {
            continue;
        }
        if profile.key.kind == ItemKind::Stratagem
            && !normalize_mask_values_into(
                &features.gradient,
                features.width,
                features.height,
                &profile.mask,
                dx,
                dy,
                &mut scratch.gradient,
            )?
        {
            continue;
        }

        for template in &profile.templates {
            if let Some(category) = template.category
                && !categories.contains(category)
            {
                continue;
            }
            let gray_score = dot_unrolled(&scratch.gray, &template.gray);
            let gradient_score = if profile.key.kind == ItemKind::Stratagem {
                Some(dot_unrolled(&scratch.gradient, &template.gradient))
            } else {
                None
            };
            let score = gradient_score
                .map(|gradient| {
                    STRATAGEM_GRAY_WEIGHT * gray_score + STRATAGEM_GRADIENT_WEIGHT * gradient
                })
                .unwrap_or(gray_score);
            let target = &mut scores[template.source_index];
            if score > *target {
                *target = score;
            }
        }
    }
    Ok(())
}

fn normalized_mask_values(
    image: &[f32],
    width: usize,
    mask: &[MaskPoint],
    dx: i32,
    dy: i32,
) -> Result<Vec<f32>> {
    let height = image.len() / width;
    let mut output = vec![0.0; mask.len()];
    ensure!(
        normalize_mask_values_into(image, width, height, mask, dx, dy, &mut output)?,
        "masked template region has zero or invalid variance"
    );
    Ok(output)
}

fn normalize_mask_values_into(
    image: &[f32],
    width: usize,
    height: usize,
    mask: &[MaskPoint],
    dx: i32,
    dy: i32,
    output: &mut [f32],
) -> Result<bool> {
    debug_assert_eq!(mask.len(), output.len());
    let mut sum = 0.0f32;
    let mut sum_sq = 0.0f32;
    for point in mask {
        let x = point.x as i32 + dx;
        let y = point.y as i32 + dy;
        if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
            bail!("template search offset leaves slot bounds");
        }
        let value = image[y as usize * width + x as usize];
        sum += value;
        sum_sq += value * value;
    }
    let count = mask.len() as f32;
    let mean = sum / count;
    let variance_sum = (sum_sq - sum * mean).max(0.0);
    if !variance_sum.is_finite() || variance_sum <= 1e-8 {
        return Ok(false);
    }
    let inverse_norm = variance_sum.sqrt().recip();
    for (target, point) in output.iter_mut().zip(mask) {
        let x = (point.x as i32 + dx) as usize;
        let y = (point.y as i32 + dy) as usize;
        *target = (image[y * width + x] - mean) * inverse_norm;
    }
    Ok(true)
}

fn sobel_magnitude(gray: &[f32], width: usize, height: usize) -> Vec<f32> {
    let mut gradient = vec![0.0f32; gray.len()];
    if width < 3 || height < 3 {
        return gradient;
    }
    for y in 1..height - 1 {
        let above = (y - 1) * width;
        let current = y * width;
        let below = (y + 1) * width;
        for x in 1..width - 1 {
            let gx = -gray[above + x - 1] + gray[above + x + 1] - 2.0 * gray[current + x - 1]
                + 2.0 * gray[current + x + 1]
                - gray[below + x - 1]
                + gray[below + x + 1];
            let gy = -gray[above + x - 1] - 2.0 * gray[above + x] - gray[above + x + 1]
                + gray[below + x - 1]
                + 2.0 * gray[below + x]
                + gray[below + x + 1];
            gradient[current + x] = (gx * gx + gy * gy).sqrt();
        }
    }
    gradient
}

#[inline]
fn dot_unrolled(left: &[f32], right: &[f32]) -> f32 {
    debug_assert_eq!(left.len(), right.len());
    let mut sums = [0.0f32; 4];
    let chunks = left.len() / 4;
    for index in 0..chunks {
        let base = index * 4;
        sums[0] += left[base] * right[base];
        sums[1] += left[base + 1] * right[base + 1];
        sums[2] += left[base + 2] * right[base + 2];
        sums[3] += left[base + 3] * right[base + 3];
    }
    let mut sum = sums.into_iter().sum::<f32>();
    for index in chunks * 4..left.len() {
        sum += left[index] * right[index];
    }
    sum
}

fn scaled_icon_size(max_size: u32, scale: f32) -> u32 {
    ((max_size as f32 * scale).round() as u32).clamp(1, max_size.max(1))
}

fn nominal_icon_size(layout: SlotLayout, slot: &Slot, kind: ItemKind) -> u32 {
    debug_assert!(!slot.kind.is_home_booster() || layout == SlotLayout::Home);

    let scale = match (layout, kind) {
        (SlotLayout::Home, ItemKind::Stratagem) => HOME_STRATAGEM_SCALE,
        (SlotLayout::Home, ItemKind::Booster) => HOME_BOOSTER_SCALE,
        _ => LIST_ICON_SCALE,
    };
    scaled_icon_size(slot.w.min(slot.h), scale)
}

// Templates are rendered with floor-centered integer placement. When the remaining
// canvas extent is odd, the true geometric center lies on a half pixel, so both
// adjacent integer phases are plausible. Check all parity-compatible phases in the
// initial layer; the nominal-size translation neighborhood and neighboring scales are
// reserved for the second and third layers respectively.
fn initial_offsets(canvas_w: u32, canvas_h: u32, icon_size: u32) -> &'static [(i32, i32)] {
    const CENTERED: &[(i32, i32)] = &[(0, 0)];
    const X_HALF_PHASES: &[(i32, i32)] = &[(0, 0), (1, 0)];
    const Y_HALF_PHASES: &[(i32, i32)] = &[(0, 0), (0, 1)];
    const XY_HALF_PHASES: &[(i32, i32)] = &[(0, 0), (1, 0), (0, 1), (1, 1)];

    let x_half_pixel = !canvas_w.saturating_sub(icon_size).is_multiple_of(2);
    let y_half_pixel = !canvas_h.saturating_sub(icon_size).is_multiple_of(2);
    match (x_half_pixel, y_half_pixel) {
        (false, false) => CENTERED,
        (true, false) => X_HALF_PHASES,
        (false, true) => Y_HALF_PHASES,
        (true, true) => XY_HALF_PHASES,
    }
}

const SEARCH_OFFSETS: &[(i32, i32)] = &[
    (0, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

fn offsets_excluding(offsets: &[(i32, i32)], excluded: &[(i32, i32)]) -> Vec<(i32, i32)> {
    offsets
        .iter()
        .copied()
        .filter(|offset| !excluded.contains(offset))
        .collect()
}

fn neighboring_icon_sizes(nominal: u32, max_size: u32) -> Vec<u32> {
    let mut sizes = Vec::with_capacity((REFINE_SCALE_RADIUS * 2) as usize);
    for delta in -REFINE_SCALE_RADIUS..=REFINE_SCALE_RADIUS {
        if delta == 0 {
            continue;
        }
        let icon_size = nominal.saturating_add_signed(delta).max(1);
        if icon_size <= max_size && icon_size != nominal && !sizes.contains(&icon_size) {
            sizes.push(icon_size);
        }
    }
    sizes
}
