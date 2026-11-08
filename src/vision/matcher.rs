use anyhow::{Result, ensure};

pub(super) const CANONICAL_SIDE: usize = 80;
const CANONICAL_PIXELS: usize = CANONICAL_SIDE * CANONICAL_SIDE;
const ALIGNMENT_CLASS_WEIGHT: f64 = 0.4;
const LOCAL_WEIGHT: f64 = 0.2;
const FINAL_CLASS_SCALE: f64 = 0.70;
const FINAL_SECONDARY_WEIGHT: f64 = 0.35;
const FINAL_EVIDENCE_WEIGHT: f64 = 1.92;
const SPATIAL_WEIGHT: f64 = 0.5;
const ALIGN_LIMIT: f32 = 1.1;
const PHASE_UNCERTAINTY_PX: f32 = 0.4;
const ENV_STATE_COUNT: usize = 25;
const TEMPLATE_SUPPORT_EPS: f32 = 0.01;
const OUTSIDE_ALPHA_GATE_LOW: f32 = 0.08;
const OUTSIDE_ALPHA_GATE_HIGH: f32 = 0.20;
const REGULAR_HEX_HALF_HEIGHT_RATIO: f32 = 0.866_025_4;
const COLOR_GAIN_MIN: f64 = 0.98;
const COLOR_GAIN_MAX: f64 = 1.02;
const COLOR_GAIN_EPS: f64 = 1e-8;

pub const MATCH_THRESHOLD: f64 = 0.218_684_125_871_763_5;

#[derive(Clone, Copy)]
pub enum MatchDomain {
    Full,
    InsetHex { half_width: f32 },
}

/// Extracted semantic coverage in `[CLASS, WHITE]` channel order.
#[derive(Clone)]
pub struct SemanticImage {
    width: usize,
    height: usize,
    center_x: f32,
    center_y: f32,
    pixels: Vec<[f32; 2]>,
}

impl SemanticImage {
    pub fn new(width: usize, height: usize, pixels: Vec<[f32; 2]>) -> Result<Self> {
        ensure!(
            width > 0 && height > 0 && pixels.len() == width * height,
            "semantic image dimensions do not match its pixel data"
        );
        Ok(Self {
            width,
            height,
            center_x: width as f32 * 0.5,
            center_y: height as f32 * 0.5,
            pixels,
        })
    }

    /// Retains the physical icon center when the semantic image is a core crop.
    pub fn with_center(mut self, center_x: f32, center_y: f32) -> Self {
        self.center_x = center_x;
        self.center_y = center_y;
        self
    }

    pub fn center(&self) -> (f32, f32) {
        (self.center_x, self.center_y)
    }

    pub(super) fn pixels(&self) -> &[[f32; 2]] {
        &self.pixels
    }

    pub(crate) fn secondary_response_u8(&self) -> Vec<u8> {
        self.pixels
            .iter()
            .map(|pixel| (pixel[1].clamp(0.0, 1.0) * 255.0).round() as u8)
            .collect()
    }
}

pub struct PreparedTemplate {
    basis: TemplateBasis,
    envelope: NuisanceEnvelope,
    domain: CanonicalDomain,
    absorb_cw_difference: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct MatchResult {
    pub score: f64,
    pub evidence: f64,
    pub white_evidence: f64,
    pub class_evidence: f64,
    pub spatial: f64,
    pub qx: f32,
    pub qy: f32,
    pub evaluations: usize,
    pub global_white: f64,
    pub local_white: f64,
    pub global_class: f64,
    pub local_class: f64,
    pub overlap_white: f64,
    pub overlap_class: f64,
    pub white_gain: f64,
    pub class_gain: f64,
}

/// Builds the frozen Env5 nuisance model from the 25 self-rendered candidate states.
pub fn prepare_template(
    template: &SemanticImage,
    template_physical_size: f32,
    env_candidates: &[SemanticImage],
    candidate_physical_size: f32,
    domain: MatchDomain,
    absorb_cw_difference: bool,
) -> Result<PreparedTemplate> {
    ensure!(
        template_physical_size > 0.0 && candidate_physical_size > 0.0,
        "physical icon sizes must be positive"
    );
    ensure!(
        env_candidates.len() == ENV_STATE_COUNT,
        "Env5 requires exactly {ENV_STATE_COUNT} self states"
    );
    let domain = CanonicalDomain::new(domain);
    let basis = TemplateBasis::new(template, template_physical_size);
    let mut residuals = env_candidates.iter().map(|candidate| {
        register(
            &basis,
            &domain,
            candidate,
            candidate_physical_size,
            false,
            false,
        )
        .residual
    });
    let first = residuals
        .next()
        .expect("Env5 state count was checked above");
    let mut envelope = NuisanceEnvelope::from_residual(&first);
    for residual in residuals {
        envelope.include(&residual);
    }

    Ok(PreparedTemplate {
        basis,
        envelope,
        domain,
        absorb_cw_difference,
    })
}

pub fn compare(
    template: &PreparedTemplate,
    candidate: &SemanticImage,
    candidate_physical_size: f32,
) -> Result<MatchResult> {
    compare_profile(template, candidate, candidate_physical_size, false)
}

/// Associate selected list icons with their brighter home samples during scanning.
pub(super) fn compare_scanned(
    template: &PreparedTemplate,
    candidate: &SemanticImage,
    candidate_physical_size: f32,
) -> Result<MatchResult> {
    compare_profile(template, candidate, candidate_physical_size, true)
}

fn compare_profile(
    template: &PreparedTemplate,
    candidate: &SemanticImage,
    candidate_physical_size: f32,
    normalize_brightness: bool,
) -> Result<MatchResult> {
    ensure!(
        candidate_physical_size > 0.0,
        "candidate physical icon size must be positive"
    );
    let registration = register(
        &template.basis,
        &template.domain,
        candidate,
        candidate_physical_size,
        template.absorb_cw_difference,
        normalize_brightness,
    );
    let verification = verify(&registration.residual, &template.envelope, &template.domain);
    let white_evidence = registration.residual.white.evidence();
    let class_evidence = registration.residual.class.evidence();
    let scaled_class = FINAL_CLASS_SCALE * class_evidence;
    let evidence = white_evidence.max(scaled_class)
        + FINAL_SECONDARY_WEIGHT * white_evidence.min(scaled_class);

    Ok(MatchResult {
        score: verification.spatial + FINAL_EVIDENCE_WEIGHT * evidence,
        evidence,
        white_evidence,
        class_evidence,
        spatial: verification.spatial,
        qx: registration.qx,
        qy: registration.qy,
        evaluations: registration.evaluations,
        global_white: registration.residual.white.global,
        local_white: registration.residual.white.local,
        global_class: registration.residual.class.global,
        local_class: registration.residual.class.local,
        overlap_white: verification.overlap_white,
        overlap_class: verification.overlap_class,
        white_gain: registration.gains.white,
        class_gain: registration.gains.class,
    })
}

struct CanonicalImage {
    class: Vec<f32>,
    white: Vec<f32>,
}

struct CanonicalDomain {
    weights: Vec<f32>,
    indices: Vec<usize>,
}

impl CanonicalDomain {
    fn new(domain: MatchDomain) -> Self {
        let weights = (0..CANONICAL_PIXELS)
            .map(|index| {
                let x = index % CANONICAL_SIDE;
                let y = index / CANONICAL_SIDE;
                match domain {
                    MatchDomain::Full => 1.0,
                    MatchDomain::InsetHex { half_width } => {
                        let x = ((x as f32 + 0.5) / CANONICAL_SIDE as f32 - 0.5).abs();
                        let y = ((y as f32 + 0.5) / CANONICAL_SIDE as f32 - 0.5).abs();
                        let half_height = half_width * REGULAR_HEX_HALF_HEIGHT_RATIO;
                        let vertical = y / half_height;
                        let row_half_width = half_width * (1.0 - 0.5 * vertical);
                        (vertical <= 1.0 && x <= row_half_width) as u8 as f32
                    }
                }
            })
            .collect::<Vec<_>>();
        let indices = weights
            .iter()
            .enumerate()
            .filter_map(|(index, &weight)| (weight > 0.0).then_some(index))
            .collect();
        Self { weights, indices }
    }

    fn tail_pixels(&self) -> usize {
        self.indices.len().div_ceil(100).max(1)
    }
}

struct TemplateBasis {
    source: SemanticImage,
    source_physical_size: f32,
    image: CanonicalImage,
    class_gradient: Vec<f32>,
    white_gradient: Vec<f32>,
}

impl TemplateBasis {
    fn new(source: &SemanticImage, source_physical_size: f32) -> Self {
        let image = remap(source, source_physical_size, 0.0, 0.0);
        let class_gradient = gradient(&image.class);
        let white_gradient = gradient(&image.white);
        Self {
            source: source.clone(),
            source_physical_size,
            image,
            class_gradient,
            white_gradient,
        }
    }
}

struct ChannelResidual {
    global: f64,
    local: f64,
    map: Vec<f32>,
}

impl ChannelResidual {
    fn evidence(&self) -> f64 {
        self.global + LOCAL_WEIGHT * self.local
    }
}

struct Residual {
    white: ChannelResidual,
    class: ChannelResidual,
}

struct Registration {
    residual: Residual,
    gains: SemanticGains,
    qx: f32,
    qy: f32,
    evaluations: usize,
}

#[derive(Clone, Copy)]
struct SemanticGains {
    white: f64,
    class: f64,
}

impl SemanticGains {
    const IDENTITY: Self = Self {
        white: 1.0,
        class: 1.0,
    };
}

struct NuisanceEnvelope {
    global_white: f64,
    local_white: f64,
    global_class: f64,
    local_class: f64,
    white_map: Vec<f32>,
    class_map: Vec<f32>,
}

impl NuisanceEnvelope {
    fn from_residual(residual: &Residual) -> Self {
        Self {
            global_white: residual.white.global,
            local_white: residual.white.local,
            global_class: residual.class.global,
            local_class: residual.class.local,
            white_map: residual.white.map.clone(),
            class_map: residual.class.map.clone(),
        }
    }

    fn include(&mut self, residual: &Residual) {
        self.global_white = self.global_white.max(residual.white.global);
        self.local_white = self.local_white.max(residual.white.local);
        self.global_class = self.global_class.max(residual.class.global);
        self.local_class = self.local_class.max(residual.class.local);
        for (envelope, value) in self.white_map.iter_mut().zip(&residual.white.map) {
            *envelope = envelope.max(*value);
        }
        for (envelope, value) in self.class_map.iter_mut().zip(&residual.class.map) {
            *envelope = envelope.max(*value);
        }
    }
}

struct Verification {
    spatial: f64,
    overlap_white: f64,
    overlap_class: f64,
}

fn verify(
    residual: &Residual,
    envelope: &NuisanceEnvelope,
    domain: &CanonicalDomain,
) -> Verification {
    let excess_global_white = (residual.white.global - envelope.global_white).max(0.0);
    let excess_local_white = (residual.white.local - envelope.local_white).max(0.0);
    let excess_global_class = (residual.class.global - envelope.global_class).max(0.0);
    let excess_local_class = (residual.class.local - envelope.local_class).max(0.0);
    let overlap_white = spatial_overlap(&residual.white.map, &envelope.white_map, domain);
    let overlap_class = spatial_overlap(&residual.class.map, &envelope.class_map, domain);
    let local_white =
        LOCAL_WEIGHT * excess_local_white * (1.0 + SPATIAL_WEIGHT * (1.0 - overlap_white));
    let local_class =
        LOCAL_WEIGHT * excess_local_class * (1.0 + SPATIAL_WEIGHT * (1.0 - overlap_class));

    Verification {
        spatial: excess_global_white
            .max(local_white)
            .max(excess_global_class)
            .max(local_class),
        overlap_white,
        overlap_class,
    }
}

fn spatial_overlap(observed: &[f32], nuisance: &[f32], domain: &CanonicalDomain) -> f64 {
    let mut indices = domain.indices.clone();
    let tail_pixels = domain.tail_pixels();
    indices.select_nth_unstable_by(tail_pixels, |left, right| {
        observed[*right].total_cmp(&observed[*left])
    });

    let mut observed_sum = 0.0f64;
    let mut overlap_sum = 0.0f64;
    for &index in &indices[..tail_pixels] {
        observed_sum += observed[index] as f64;
        overlap_sum += observed[index].min(nuisance[index]) as f64;
    }
    let observed_sum = observed_sum as f32;
    let overlap_sum = overlap_sum as f32;
    if observed_sum <= 1e-12 {
        1.0
    } else {
        (overlap_sum / observed_sum).clamp(0.0, 1.0) as f64
    }
}

fn register(
    template: &TemplateBasis,
    domain: &CanonicalDomain,
    candidate: &SemanticImage,
    candidate_physical_size: f32,
    absorb_cw_difference: bool,
    normalize_brightness: bool,
) -> Registration {
    let mut search = AlignmentSearch::new(
        template,
        domain,
        candidate,
        candidate_physical_size,
        normalize_brightness,
    );
    let stage1 = search_grid(&mut search, 0.0, 0.0, 0.9, None);
    let stage2 = search_grid(&mut search, stage1.qx, stage1.qy, 0.45, Some(stage1));
    let (best3, samples) = search_third_stage(&mut search, stage2.qx, stage2.qy);
    let selected = quadratic_proposal(&samples)
        .map(|(dx, dy)| {
            let qx = (stage2.qx + dx).clamp(-ALIGN_LIMIT, ALIGN_LIMIT);
            let qy = (stage2.qy + dy).clamp(-ALIGN_LIMIT, ALIGN_LIMIT);
            SearchPoint {
                objective: search.evaluate(qx, qy),
                qx,
                qy,
            }
        })
        .filter(|proposal| proposal.objective < best3.objective)
        .unwrap_or(best3);

    let actual = search.actual_coordinate(selected.qx, selected.qy);
    let mut remapped = remap(candidate, candidate_physical_size, actual.0, actual.1);
    let gains = if absorb_cw_difference || normalize_brightness {
        let expected_native = render_to_raster(
            &template.source,
            template.source_physical_size,
            (candidate.width, candidate.height),
            (candidate.center_x, candidate.center_y),
            candidate_physical_size,
            (0.0, 0.0),
        );
        let expected = remap(
            &expected_native,
            candidate_physical_size,
            actual.0,
            actual.1,
        );
        if normalize_brightness {
            fit_common_brightness(&expected, &mut remapped)
        } else {
            absorb_cw_difference_gain(&expected, &mut remapped)
        }
    } else {
        SemanticGains::IDENTITY
    };
    suppress_outside_template_support(&template.image, &mut remapped);
    Registration {
        residual: full_residual(template, &remapped, candidate_physical_size, domain),
        gains,
        qx: selected.qx,
        qy: selected.qy,
        evaluations: search.cache.len(),
    }
}

#[derive(Clone, Copy)]
struct SearchPoint {
    objective: f64,
    qx: f32,
    qy: f32,
}

struct CacheEntry {
    key: (i32, i32),
    actual_qx: f32,
    actual_qy: f32,
    objective: f64,
}

struct AlignmentSearch<'a> {
    template: &'a TemplateBasis,
    domain: &'a CanonicalDomain,
    candidate: &'a SemanticImage,
    candidate_physical_size: f32,
    normalize_brightness: bool,
    cache: Vec<CacheEntry>,
}

impl<'a> AlignmentSearch<'a> {
    fn new(
        template: &'a TemplateBasis,
        domain: &'a CanonicalDomain,
        candidate: &'a SemanticImage,
        candidate_physical_size: f32,
        normalize_brightness: bool,
    ) -> Self {
        Self {
            template,
            domain,
            candidate,
            candidate_physical_size,
            normalize_brightness,
            cache: Vec::with_capacity(26),
        }
    }

    fn evaluate(&mut self, qx: f32, qy: f32) -> f64 {
        let qx = qx.clamp(-ALIGN_LIMIT, ALIGN_LIMIT);
        let qy = qy.clamp(-ALIGN_LIMIT, ALIGN_LIMIT);
        let key = cache_key(qx, qy);
        if let Some(entry) = self.cache.iter().find(|entry| entry.key == key) {
            return entry.objective;
        }

        let mut remapped = remap(self.candidate, self.candidate_physical_size, qx, qy);
        if self.normalize_brightness {
            fit_common_brightness(&self.template.image, &mut remapped);
        }
        let objective = global_residual(
            &self.template.image.white,
            &remapped.white,
            &self.template.white_gradient,
            self.candidate_physical_size,
            self.domain,
        ) + ALIGNMENT_CLASS_WEIGHT
            * global_residual(
                &self.template.image.class,
                &remapped.class,
                &self.template.class_gradient,
                self.candidate_physical_size,
                self.domain,
            );
        self.cache.push(CacheEntry {
            key,
            actual_qx: qx,
            actual_qy: qy,
            objective,
        });
        objective
    }

    fn actual_coordinate(&self, qx: f32, qy: f32) -> (f32, f32) {
        let key = cache_key(qx, qy);
        self.cache
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| (entry.actual_qx, entry.actual_qy))
            .expect("selected alignment point must have been evaluated")
    }
}

fn cache_key(qx: f32, qy: f32) -> (i32, i32) {
    (
        (qx as f64 * 1_000_000.0).round_ties_even() as i32,
        (qy as f64 * 1_000_000.0).round_ties_even() as i32,
    )
}

fn search_grid(
    search: &mut AlignmentSearch<'_>,
    center_x: f32,
    center_y: f32,
    step: f32,
    initial: Option<SearchPoint>,
) -> SearchPoint {
    let mut best = initial.unwrap_or(SearchPoint {
        objective: f64::INFINITY,
        qx: 0.0,
        qy: 0.0,
    });
    for offset_y in [-step, 0.0, step] {
        for offset_x in [-step, 0.0, step] {
            let qx = (center_x + offset_x).clamp(-ALIGN_LIMIT, ALIGN_LIMIT);
            let qy = (center_y + offset_y).clamp(-ALIGN_LIMIT, ALIGN_LIMIT);
            let objective = search.evaluate(qx, qy);
            if objective < best.objective {
                best = SearchPoint { objective, qx, qy };
            }
        }
    }
    best
}

fn search_third_stage(
    search: &mut AlignmentSearch<'_>,
    center_x: f32,
    center_y: f32,
) -> (SearchPoint, [QuadraticSample; 9]) {
    const STEP: f32 = 0.225;

    let mut best = SearchPoint {
        objective: f64::INFINITY,
        qx: center_x,
        qy: center_y,
    };
    let mut samples = [QuadraticSample::default(); 9];
    let mut index = 0;
    for offset_y in [-STEP, 0.0, STEP] {
        for offset_x in [-STEP, 0.0, STEP] {
            let qx = (center_x + offset_x).clamp(-ALIGN_LIMIT, ALIGN_LIMIT);
            let qy = (center_y + offset_y).clamp(-ALIGN_LIMIT, ALIGN_LIMIT);
            let objective = search.evaluate(qx, qy);
            samples[index] = QuadraticSample {
                x: offset_x as f64,
                y: offset_y as f64,
                value: objective,
            };
            index += 1;
            if objective < best.objective {
                best = SearchPoint { objective, qx, qy };
            }
        }
    }
    (best, samples)
}

#[derive(Clone, Copy, Default)]
struct QuadraticSample {
    x: f64,
    y: f64,
    value: f64,
}

fn quadratic_proposal(samples: &[QuadraticSample; 9]) -> Option<(f32, f32)> {
    const STEP: f64 = 0.225;
    const MAX_OFFSET: f64 = STEP * 1.25;

    let mut sum = 0.0;
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut sum_x2 = 0.0;
    let mut sum_xy = 0.0;
    let mut sum_y2 = 0.0;
    for sample in samples {
        sum += sample.value;
        sum_x += sample.x * sample.value;
        sum_y += sample.y * sample.value;
        sum_x2 += sample.x * sample.x * sample.value;
        sum_xy += sample.x * sample.y * sample.value;
        sum_y2 += sample.y * sample.y * sample.value;
    }

    let step2 = STEP * STEP;
    let step4 = step2 * step2;
    let c1 = sum_x / (6.0 * step2);
    let c2 = sum_y / (6.0 * step2);
    let curvature_sum = (sum_x2 + sum_y2) / (2.0 * step4) - 2.0 * sum / (3.0 * step2);
    let curvature_difference = (sum_x2 - sum_y2) / (2.0 * step4);
    let c3 = 0.5 * (curvature_sum + curvature_difference);
    let c4 = sum_xy / (4.0 * step4);
    let c5 = 0.5 * (curvature_sum - curvature_difference);

    let h00 = 2.0 * c3;
    let h01 = c4;
    let h11 = 2.0 * c5;
    let min_eigenvalue = 0.5 * (h00 + h11 - ((h00 - h11) * (h00 - h11) + 4.0 * h01 * h01).sqrt());
    let determinant = h00 * h11 - h01 * h01;
    if min_eigenvalue <= 1e-8 || determinant == 0.0 {
        return None;
    }

    let dx = (h01 * c2 - h11 * c1) / determinant;
    let dy = (h01 * c1 - h00 * c2) / determinant;
    if !dx.is_finite() || !dy.is_finite() || dx.abs() > MAX_OFFSET || dy.abs() > MAX_OFFSET {
        return None;
    }
    Some((dx as f32, dy as f32))
}

fn remap(image: &SemanticImage, physical_size: f32, qx: f32, qy: f32) -> CanonicalImage {
    let mut class = vec![0.0; CANONICAL_PIXELS];
    let mut white = vec![0.0; CANONICAL_PIXELS];
    let center_x = image.center_x + qx;
    let center_y = image.center_y + qy;
    for y in 0..CANONICAL_SIDE {
        let source_y = center_y - physical_size * 0.5
            + ((y as f32 + 0.5) / CANONICAL_SIDE as f32) * physical_size
            - 0.5;
        for x in 0..CANONICAL_SIDE {
            let source_x = center_x - physical_size * 0.5
                + ((x as f32 + 0.5) / CANONICAL_SIDE as f32) * physical_size
                - 0.5;
            let pixel = sample_opencv_linear(image, source_x, source_y);
            let index = y * CANONICAL_SIDE + x;
            class[index] = pixel[0];
            white[index] = pixel[1];
        }
    }
    CanonicalImage { class, white }
}

pub(super) fn render_to_raster(
    source: &SemanticImage,
    source_physical_size: f32,
    target_size: (usize, usize),
    target_center: (f32, f32),
    target_physical_size: f32,
    phase: (f32, f32),
) -> SemanticImage {
    let (width, height) = target_size;
    let (target_center_x, target_center_y) = target_center;
    let (source_center_x, source_center_y) = source.center();
    let mut pixels = Vec::with_capacity(width * height);

    for y in 0..height {
        let normalized_y = (y as f32 + 0.5 - target_center_y - phase.1) / target_physical_size;
        let source_y = source_center_y + normalized_y * source_physical_size - 0.5;
        for x in 0..width {
            let normalized_x = (x as f32 + 0.5 - target_center_x - phase.0) / target_physical_size;
            let source_x = source_center_x + normalized_x * source_physical_size - 0.5;
            pixels.push(sample_opencv_linear(source, source_x, source_y));
        }
    }

    SemanticImage {
        width,
        height,
        center_x: target_center_x,
        center_y: target_center_y,
        pixels,
    }
}

fn fit_common_brightness(
    expected: &CanonicalImage,
    observed: &mut CanonicalImage,
) -> SemanticGains {
    let (cross, energy) = expected
        .class
        .iter()
        .chain(&expected.white)
        .zip(observed.class.iter().chain(&observed.white))
        .fold((0.0, 0.0), |(cross, energy), (&expected, &observed)| {
            let expected = expected as f64;
            (
                cross + expected * observed as f64,
                energy + expected * expected,
            )
        });
    let gain = cross / energy.max(COLOR_GAIN_EPS);
    let gain = if gain > COLOR_GAIN_EPS { gain } else { 1.0 };
    for value in observed.class.iter_mut().chain(&mut observed.white) {
        *value = (*value as f64 / gain).clamp(0.0, 1.0) as f32;
    }
    SemanticGains {
        white: gain,
        class: gain,
    }
}

fn absorb_cw_difference_gain(
    expected: &CanonicalImage,
    observed: &mut CanonicalImage,
) -> SemanticGains {
    SemanticGains {
        white: fit_and_apply_gain(&expected.white, &mut observed.white),
        class: fit_and_apply_gain(&expected.class, &mut observed.class),
    }
}

fn fit_and_apply_gain(expected: &[f32], observed: &mut [f32]) -> f64 {
    let (numerator, expected_energy) = expected.iter().zip(observed.iter()).fold(
        (0.0, 0.0),
        |(numerator, energy), (&expected, &observed)| {
            let expected = expected as f64;
            let observed = observed as f64;
            let weight = 0.2 + 0.8 * expected.max(observed);
            (
                numerator + weight * expected * observed,
                energy + weight * expected * expected,
            )
        },
    );
    let gain = if expected_energy <= COLOR_GAIN_EPS {
        1.0
    } else {
        (numerator / (expected_energy + COLOR_GAIN_EPS)).clamp(COLOR_GAIN_MIN, COLOR_GAIN_MAX)
    };
    for value in observed {
        *value = ((*value as f64 / gain).clamp(0.0, 1.0)) as f32;
    }
    gain
}

pub(super) fn sample_opencv_linear(image: &SemanticImage, x: f32, y: f32) -> [f32; 2] {
    const INTER_TAB_SIZE: i32 = 32;

    let fixed_x = (x * INTER_TAB_SIZE as f32).round_ties_even() as i32;
    let fixed_y = (y * INTER_TAB_SIZE as f32).round_ties_even() as i32;
    let x0 = fixed_x.div_euclid(INTER_TAB_SIZE);
    let y0 = fixed_y.div_euclid(INTER_TAB_SIZE);
    let fx = fixed_x.rem_euclid(INTER_TAB_SIZE) as f32 / INTER_TAB_SIZE as f32;
    let fy = fixed_y.rem_euclid(INTER_TAB_SIZE) as f32 / INTER_TAB_SIZE as f32;
    let mut output = [0.0; 2];

    for (source_y, weight_y) in [(y0, 1.0 - fy), (y0 + 1, fy)] {
        if source_y < 0 || source_y >= image.height as i32 {
            continue;
        }
        for (source_x, weight_x) in [(x0, 1.0 - fx), (x0 + 1, fx)] {
            if source_x < 0 || source_x >= image.width as i32 {
                continue;
            }
            let pixel = image.pixels[source_y as usize * image.width + source_x as usize];
            let weight = weight_x * weight_y;
            output[0] += pixel[0] * weight;
            output[1] += pixel[1] * weight;
        }
    }
    output
}

fn full_residual(
    template: &TemplateBasis,
    candidate: &CanonicalImage,
    candidate_physical_size: f32,
    domain: &CanonicalDomain,
) -> Residual {
    Residual {
        white: channel_residual(
            &template.image.white,
            &candidate.white,
            &template.white_gradient,
            candidate_physical_size,
            domain,
        ),
        class: channel_residual(
            &template.image.class,
            &candidate.class,
            &template.class_gradient,
            candidate_physical_size,
            domain,
        ),
    }
}

fn suppress_outside_template_support(template: &CanonicalImage, candidate: &mut CanonicalImage) {
    for index in 0..CANONICAL_PIXELS {
        if template.class[index].max(template.white[index]) > TEMPLATE_SUPPORT_EPS {
            continue;
        }
        candidate.class[index] = suppress_weak_alpha(candidate.class[index]);
        candidate.white[index] = suppress_weak_alpha(candidate.white[index]);
    }
}

fn suppress_weak_alpha(alpha: f32) -> f32 {
    let t = ((alpha - OUTSIDE_ALPHA_GATE_LOW) / (OUTSIDE_ALPHA_GATE_HIGH - OUTSIDE_ALPHA_GATE_LOW))
        .clamp(0.0, 1.0);
    alpha * t * t * (3.0 - 2.0 * t)
}

fn global_residual(
    template: &[f32],
    candidate: &[f32],
    template_gradient: &[f32],
    candidate_physical_size: f32,
    domain: &CanonicalDomain,
) -> f64 {
    let candidate_gradient = gradient(candidate);
    let uncertainty_scale =
        PHASE_UNCERTAINTY_PX * CANONICAL_SIDE as f32 / candidate_physical_size.max(1e-8);
    let mut weighted_sum = 0.0f64;
    let mut weight_sum = 0.0f64;
    for index in 0..CANONICAL_PIXELS {
        let raw = (template[index] - candidate[index]).abs();
        let uncertainty =
            uncertainty_scale * template_gradient[index].max(candidate_gradient[index]);
        let effective = 0.3 * raw + 0.7 * (raw - uncertainty).max(0.0);
        let weight = domain.weights[index] * (0.2 + 0.8 * template[index].max(candidate[index]));
        weighted_sum += (weight * effective) as f64;
        weight_sum += weight as f64;
    }
    let weighted_sum = weighted_sum as f32;
    let weight_sum = weight_sum as f32;
    (weighted_sum / (weight_sum + 1e-8)) as f64
}

fn channel_residual(
    template: &[f32],
    candidate: &[f32],
    template_gradient: &[f32],
    candidate_physical_size: f32,
    domain: &CanonicalDomain,
) -> ChannelResidual {
    let candidate_gradient = gradient(candidate);
    let uncertainty_scale =
        PHASE_UNCERTAINTY_PX * CANONICAL_SIDE as f32 / candidate_physical_size.max(1e-8);
    let mut effective = vec![0.0; CANONICAL_PIXELS];
    let mut weighted_sum = 0.0f64;
    let mut weight_sum = 0.0f64;
    for index in 0..CANONICAL_PIXELS {
        let raw = (template[index] - candidate[index]).abs();
        let uncertainty =
            uncertainty_scale * template_gradient[index].max(candidate_gradient[index]);
        let value = 0.3 * raw + 0.7 * (raw - uncertainty).max(0.0);
        let weight = domain.weights[index] * (0.2 + 0.8 * template[index].max(candidate[index]));
        effective[index] = value * domain.weights[index];
        weighted_sum += (weight * value) as f64;
        weight_sum += weight as f64;
    }

    let mut map = local_filter(&effective);
    for (value, &weight) in map.iter_mut().zip(&domain.weights) {
        *value *= weight;
    }
    ChannelResidual {
        global: {
            let weighted_sum = weighted_sum as f32;
            let weight_sum = weight_sum as f32;
            (weighted_sum / (weight_sum + 1e-8)) as f64
        },
        local: upper_tail_mean(&map, domain) as f64,
        map,
    }
}

fn gradient(field: &[f32]) -> Vec<f32> {
    let mut output = vec![0.0; CANONICAL_PIXELS];
    for y in 0..CANONICAL_SIDE {
        let above = reflect101(y as i32 - 1, CANONICAL_SIDE);
        let below = reflect101(y as i32 + 1, CANONICAL_SIDE);
        for x in 0..CANONICAL_SIDE {
            let left = reflect101(x as i32 - 1, CANONICAL_SIDE);
            let right = reflect101(x as i32 + 1, CANONICAL_SIDE);
            let top_left = field[above * CANONICAL_SIDE + left];
            let top = field[above * CANONICAL_SIDE + x];
            let top_right = field[above * CANONICAL_SIDE + right];
            let middle_left = field[y * CANONICAL_SIDE + left];
            let middle_right = field[y * CANONICAL_SIDE + right];
            let bottom_left = field[below * CANONICAL_SIDE + left];
            let bottom = field[below * CANONICAL_SIDE + x];
            let bottom_right = field[below * CANONICAL_SIDE + right];
            let gx = (top_right + 2.0 * middle_right + bottom_right
                - top_left
                - 2.0 * middle_left
                - bottom_left)
                * 0.125;
            let gy = (bottom_left + 2.0 * bottom + bottom_right - top_left - 2.0 * top - top_right)
                * 0.125;
            output[y * CANONICAL_SIDE + x] = (gx * gx + gy * gy).sqrt();
        }
    }
    output
}

fn local_filter(field: &[f32]) -> Vec<f32> {
    const KERNEL: [f32; 5] = [0.14, 0.24, 0.24, 0.24, 0.14];

    let mut horizontal = vec![0.0; CANONICAL_PIXELS];
    for y in 0..CANONICAL_SIDE {
        for x in 0..CANONICAL_SIDE {
            let mut sum = 0.0;
            for (kernel_index, weight) in KERNEL.into_iter().enumerate() {
                let source_x = reflect101(x as i32 + kernel_index as i32 - 2, CANONICAL_SIDE);
                sum += weight * field[y * CANONICAL_SIDE + source_x];
            }
            horizontal[y * CANONICAL_SIDE + x] = sum;
        }
    }

    let mut output = vec![0.0; CANONICAL_PIXELS];
    for y in 0..CANONICAL_SIDE {
        for x in 0..CANONICAL_SIDE {
            let mut sum = 0.0;
            for (kernel_index, weight) in KERNEL.into_iter().enumerate() {
                let source_y = reflect101(y as i32 + kernel_index as i32 - 2, CANONICAL_SIDE);
                sum += weight * horizontal[source_y * CANONICAL_SIDE + x];
            }
            output[y * CANONICAL_SIDE + x] = sum;
        }
    }
    output
}

fn reflect101(position: i32, length: usize) -> usize {
    let length = length as i32;
    if position < 0 {
        (-position) as usize
    } else if position >= length {
        (2 * length - position - 2) as usize
    } else {
        position as usize
    }
}

fn upper_tail_mean(values: &[f32], domain: &CanonicalDomain) -> f32 {
    let mut sorted = domain
        .indices
        .iter()
        .map(|&index| values[index])
        .collect::<Vec<_>>();
    let tail_pixels = domain.tail_pixels();
    let split = sorted.len() - tail_pixels;
    sorted.select_nth_unstable_by(split, f32::total_cmp);
    sorted[split..].iter().sum::<f32>() / tail_pixels as f32
}
