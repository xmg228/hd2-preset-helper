use crate::item::StratagemCategory;

#[derive(Clone, Copy)]
struct ColorProfile {
    chroma: [f32; 3],
    luma_low: f32,
    luma_full: f32,
    distance_full: f32,
    distance_zero: f32,
}

#[derive(Clone, Copy)]
struct ColorSample {
    chroma: [f32; 3],
    luma: f32,
}

impl ColorSample {
    #[inline]
    fn from_rgb(r: u8, g: u8, b: u8) -> Option<Self> {
        let sum = r as f32 + g as f32 + b as f32;
        if sum <= 1.0 {
            return None;
        }
        Some(Self {
            chroma: [r as f32 / sum, g as f32 / sum, b as f32 / sum],
            luma: luma601_u8(r, g, b) as f32,
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StratagemColorEvidence {
    offensive: f32,
    supply: f32,
    defensive: f32,
    sampled_pixels: usize,
}

impl StratagemColorEvidence {
    fn ordered(self) -> [(StratagemCategory, f32); 3] {
        let mut values = [
            (StratagemCategory::Offensive, self.offensive),
            (StratagemCategory::Supply, self.supply),
            (StratagemCategory::Defensive, self.defensive),
        ];
        values.sort_by(|left, right| right.1.total_cmp(&left.1));
        values
    }

    pub(crate) fn candidate_categories(self) -> StratagemCategoryCandidates {
        let ordered = self.ordered();
        let total = self.offensive + self.supply + self.defensive;
        let best_share = ordered[0].1 / total.max(f32::EPSILON);
        let best_density = ordered[0].1 / self.sampled_pixels.max(1) as f32;

        if best_share >= 0.85 && best_density >= 0.006 {
            StratagemCategoryCandidates::One(ordered[0].0)
        } else if best_share >= 0.60 && best_density >= 0.003 {
            StratagemCategoryCandidates::Two(ordered[0].0, ordered[1].0)
        } else {
            StratagemCategoryCandidates::All
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StratagemCategoryCandidates {
    One(StratagemCategory),
    Two(StratagemCategory, StratagemCategory),
    All,
}

impl StratagemCategoryCandidates {
    #[inline]
    pub(crate) fn contains(self, category: StratagemCategory) -> bool {
        match self {
            Self::One(first) => category == first,
            Self::Two(first, second) => category == first || category == second,
            Self::All => true,
        }
    }
}

// Reference colors and soft ranges measured from representative non-empty slot crops.
const WHITE: ColorProfile = ColorProfile {
    chroma: [255.0 / 747.0, 255.0 / 747.0, 237.0 / 747.0],
    luma_low: 100.0,
    luma_full: 180.0,
    distance_full: 0.006,
    distance_zero: 0.035,
};
const OFFENSIVE_RED: ColorProfile = ColorProfile {
    chroma: [201.0 / 367.0, 90.0 / 367.0, 76.0 / 367.0],
    luma_low: 55.0,
    luma_full: 115.0,
    distance_full: 0.030,
    distance_zero: 0.130,
};
const DEFENSIVE_GREEN: ColorProfile = ColorProfile {
    chroma: [103.0 / 333.0, 148.0 / 333.0, 82.0 / 333.0],
    luma_low: 67.0,
    luma_full: 119.0,
    distance_full: 0.023,
    distance_zero: 0.075,
};
const SUPPLY_BLUE: ColorProfile = ColorProfile {
    chroma: [77.0 / 460.0, 177.0 / 460.0, 206.0 / 460.0],
    luma_low: 60.0,
    luma_full: 140.0,
    distance_full: 0.020,
    distance_zero: 0.090,
};
const BOOSTER_YELLOW: ColorProfile = ColorProfile {
    chroma: [255.0 / 515.0, 222.0 / 515.0, 38.0 / 515.0],
    luma_low: 75.0,
    luma_full: 160.0,
    distance_full: 0.025,
    distance_zero: 0.100,
};
const REAL_ICON_COLORS: [ColorProfile; 5] = [
    WHITE,
    OFFENSIVE_RED,
    DEFENSIVE_GREEN,
    SUPPLY_BLUE,
    BOOSTER_YELLOW,
];

#[inline]
pub fn luma601_u8(r: u8, g: u8, b: u8) -> u8 {
    ((77u16 * r as u16 + 150u16 * g as u16 + 29u16 * b as u16 + 128) >> 8) as u8
}

pub fn icon_likeness(r: u8, g: u8, b: u8) -> f32 {
    let Some(sample) = ColorSample::from_rgb(r, g, b) else {
        return 0.0;
    };
    REAL_ICON_COLORS
        .iter()
        .map(|profile| color_likeness(sample, *profile))
        .fold(0.0, f32::max)
}

pub fn booster_yellow_likeness(r: u8, g: u8, b: u8) -> f32 {
    ColorSample::from_rgb(r, g, b)
        .map(|sample| color_likeness(sample, BOOSTER_YELLOW))
        .unwrap_or(0.0)
}

#[inline]
pub(crate) fn add_stratagem_color_evidence(
    evidence: &mut StratagemColorEvidence,
    r: u8,
    g: u8,
    b: u8,
) {
    evidence.sampled_pixels += 1;
    let Some(sample) = ColorSample::from_rgb(r, g, b) else {
        return;
    };
    let offensive = color_likeness(sample, OFFENSIVE_RED);
    let supply = color_likeness(sample, SUPPLY_BLUE);
    let defensive = color_likeness(sample, DEFENSIVE_GREEN);

    evidence.offensive += offensive * offensive;
    evidence.supply += supply * supply;
    evidence.defensive += defensive * defensive;
}

fn color_likeness(sample: ColorSample, profile: ColorProfile) -> f32 {
    let distance = ((sample.chroma[0] - profile.chroma[0]).powi(2)
        + (sample.chroma[1] - profile.chroma[1]).powi(2)
        + (sample.chroma[2] - profile.chroma[2]).powi(2))
    .sqrt();
    let brightness = 0.35 + 0.65 * smoothstep(profile.luma_low, profile.luma_full, sample.luma);
    let chromaticity = 1.0 - smoothstep(profile.distance_full, profile.distance_zero, distance);

    brightness * chromaticity
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
