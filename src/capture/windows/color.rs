use std::sync::OnceLock;

use anyhow::{Result, ensure};
use half::f16;
use tracing::{debug, warn};

use super::display::DisplayColorInfo;
use crate::game_settings::GameColorSettings;

const REC709_TO_REC2020: [[f32; 3]; 3] = [
    [0.627_403_9, 0.329_283_03, 0.043_313_067],
    [0.069_097_29, 0.919_540_4, 0.011_362_316],
    [0.016_391_44, 0.088_013_31, 0.895_595_25],
];
const GAME_COLOR_INVERSE: [[f32; 3]; 3] = [
    [1.635_04, -0.605_63, -0.076_350_0],
    [-0.123_25, 1.138_85, -0.010_29],
    [-0.017_16, -0.101_73, 1.210_73],
];
const SCRGB_NITS: f32 = 80.0;
const PQ_MAX_NITS: f32 = 10_000.0;
const HDR_TONE_LUT_SEGMENTS: usize = 8192;
const MATCHER_GAMMA: f32 = 0.4545;

pub(in crate::capture) struct ColorNormalizer {
    mode: NormalizationMode,
    #[cfg(feature = "diagnostics")]
    diagnostic_tag: String,
}

enum NormalizationMode {
    Sdr {
        lut: Box<[u8; 65536]>,
    },
    HdrIdentityTone {
        matrix: [[f32; 3]; 3],
    },
    HdrAdjustedTone {
        pre_matrix: [[f32; 3]; 3],
        tone: HdrToneLut,
        post_matrix: [[f32; 3]; 3],
    },
}

struct HdrToneLut {
    values: Box<[f32]>,
}

impl ColorNormalizer {
    pub(super) fn new(settings: GameColorSettings, display: DisplayColorInfo) -> Result<Self> {
        ensure!(
            settings.screen_brightness.is_finite(),
            "screen_brightness is not finite"
        );
        ensure!(
            settings.ui_brightness.is_finite() && settings.ui_brightness > 0.0,
            "ui_brightness must be finite and greater than zero"
        );

        let windows_hdr = display.hdr_active;
        let sdr_white_level = display.sdr_white_level;
        let effective_hdr = windows_hdr && settings.hdr_enabled;
        if settings.hdr_enabled && !display.hdr_active {
            warn!("game HDR is enabled while display HDR is inactive; using SDR normalization");
        }

        let mode = if !effective_hdr {
            NormalizationMode::Sdr {
                lut: build_sdr_lut(display.sdr_white_level, settings.screen_brightness),
            }
        } else if settings.screen_brightness == 1.0 {
            let matrix = scale_matrix(
                multiply_matrices(GAME_COLOR_INVERSE, REC709_TO_REC2020),
                SCRGB_NITS / settings.ui_brightness,
            );
            NormalizationMode::HdrIdentityTone { matrix }
        } else {
            NormalizationMode::HdrAdjustedTone {
                pre_matrix: scale_matrix(REC709_TO_REC2020, SCRGB_NITS),
                tone: HdrToneLut::new(settings.screen_brightness),
                post_matrix: scale_matrix(GAME_COLOR_INVERSE, 1.0 / settings.ui_brightness),
            }
        };

        debug!(
            windows_hdr,
            game_hdr = settings.hdr_enabled,
            effective_hdr,
            sdr_white_level,
            screen_brightness = settings.screen_brightness,
            ui_brightness = settings.ui_brightness,
            mode = mode.label(),
            "configured UI color normalization"
        );
        #[cfg(feature = "diagnostics")]
        let diagnostic_tag = format!(
            "winhdr{}-gamehdr{}-sdr{}-screen{:.6}-ui{:.6}",
            u8::from(display.hdr_active),
            u8::from(settings.hdr_enabled),
            display.sdr_white_level,
            settings.screen_brightness,
            settings.ui_brightness,
        );
        Ok(Self {
            mode,
            #[cfg(feature = "diagnostics")]
            diagnostic_tag,
        })
    }
}

impl ColorNormalizer {
    pub(super) fn convert_row(&self, source: &[u8], destination: &mut [u8]) {
        debug_assert_eq!(source.len() / 8, destination.len() / 4);
        for (pixel, output) in source.chunks_exact(8).zip(destination.chunks_exact_mut(4)) {
            let rgb = match &self.mode {
                NormalizationMode::Sdr { lut } => [
                    lut[u16::from_le_bytes([pixel[0], pixel[1]]) as usize],
                    lut[u16::from_le_bytes([pixel[2], pixel[3]]) as usize],
                    lut[u16::from_le_bytes([pixel[4], pixel[5]]) as usize],
                ],
                NormalizationMode::HdrIdentityTone { matrix } => {
                    let raw = decode_rgb(pixel);
                    mat_vec(*matrix, raw).map(linear_to_matcher_u8)
                }
                NormalizationMode::HdrAdjustedTone {
                    pre_matrix,
                    tone,
                    post_matrix,
                } => {
                    let raw = decode_rgb(pixel);
                    let adjusted = mat_vec(*pre_matrix, raw).map(|value| tone.sample(value));
                    mat_vec(*post_matrix, adjusted).map(linear_to_matcher_u8)
                }
            };
            output[..3].copy_from_slice(&rgb);
            output[3] = 255;
        }
    }

    #[cfg(feature = "diagnostics")]
    pub(super) fn diagnostic_tag(&self) -> &str {
        &self.diagnostic_tag
    }
}

impl NormalizationMode {
    fn label(&self) -> &'static str {
        match self {
            Self::Sdr { .. } => "sdr",
            Self::HdrIdentityTone { .. } => "hdr_identity_tone",
            Self::HdrAdjustedTone { .. } => "hdr_adjusted_tone",
        }
    }
}

impl HdrToneLut {
    fn new(screen_brightness: f32) -> Self {
        let exponent = 2.0f64.powf(screen_brightness as f64 - 1.0);
        let values = (0..=HDR_TONE_LUT_SEGMENTS)
            .map(|index| {
                let position = index as f64 / HDR_TONE_LUT_SEGMENTS as f64;
                let nits = PQ_MAX_NITS as f64 * position * position;
                pq_decode(pq_encode(nits).powf(exponent)) as f32
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self { values }
    }

    fn sample(&self, nits: f32) -> f32 {
        let position =
            (nits.clamp(0.0, PQ_MAX_NITS) / PQ_MAX_NITS).sqrt() * HDR_TONE_LUT_SEGMENTS as f32;
        let lower = (position.floor() as usize).min(HDR_TONE_LUT_SEGMENTS - 1);
        let fraction = position - lower as f32;
        self.values[lower] + (self.values[lower + 1] - self.values[lower]) * fraction
    }
}

fn build_sdr_lut(sdr_white_level: u32, screen_brightness: f32) -> Box<[u8; 65536]> {
    let scale = 1000.0 / sdr_white_level.max(1) as f32;
    let exponent = 2.0f32.powf(screen_brightness - 1.0);
    let mut lut = Box::new([0u8; 65536]);
    for bits in 0u32..=u16::MAX as u32 {
        let raw = f16::from_bits(bits as u16).to_f32();
        let linear = if raw.is_finite() { raw * scale } else { 0.0 };
        let encoded = linear_to_srgb(linear).powf(exponent);
        lut[bits as usize] = normalized_to_u8(encoded);
    }
    lut
}

fn decode_f16(low: u8, high: u8) -> f32 {
    let value = f16::from_bits(u16::from_le_bytes([low, high])).to_f32();
    if value.is_finite() { value } else { 0.0 }
}

fn decode_rgb(pixel: &[u8]) -> [f32; 3] {
    [
        decode_f16(pixel[0], pixel[1]),
        decode_f16(pixel[2], pixel[3]),
        decode_f16(pixel[4], pixel[5]),
    ]
}

fn linear_to_srgb(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

fn linear_to_matcher_u8(value: f32) -> u8 {
    if !value.is_finite() {
        return 0;
    }
    let index = (value.clamp(0.0, 1.0) * u16::MAX as f32 + 0.5) as usize;
    matcher_gamma_lut()[index]
}

fn matcher_gamma_lut() -> &'static [u8; 65536] {
    static LUT: OnceLock<Box<[u8; 65536]>> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut values = Box::new([0u8; 65536]);
        for (index, value) in values.iter_mut().enumerate() {
            *value = normalized_to_u8((index as f32 / u16::MAX as f32).powf(MATCHER_GAMMA));
        }
        values
    })
}

fn normalized_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

fn mat_vec(matrix: [[f32; 3]; 3], vector: [f32; 3]) -> [f32; 3] {
    matrix.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
}

fn multiply_matrices(left: [[f32; 3]; 3], right: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    std::array::from_fn(|row| {
        std::array::from_fn(|col| {
            left[row][0] * right[0][col]
                + left[row][1] * right[1][col]
                + left[row][2] * right[2][col]
        })
    })
}

fn scale_matrix(matrix: [[f32; 3]; 3], scale: f32) -> [[f32; 3]; 3] {
    matrix.map(|row| row.map(|value| value * scale))
}

fn pq_encode(nits: f64) -> f64 {
    const M1: f64 = 2610.0 / 16384.0;
    const M2: f64 = 2523.0 / 32.0;
    const C1: f64 = 3424.0 / 4096.0;
    const C2: f64 = 2413.0 / 128.0;
    const C3: f64 = 2392.0 / 128.0;

    let powered = (nits.clamp(0.0, PQ_MAX_NITS as f64) / PQ_MAX_NITS as f64).powf(M1);
    ((C1 + C2 * powered) / (1.0 + C3 * powered)).powf(M2)
}

fn pq_decode(code: f64) -> f64 {
    const INV_M1: f64 = 16384.0 / 2610.0;
    const INV_M2: f64 = 32.0 / 2523.0;
    const C1: f64 = 3424.0 / 4096.0;
    const C2: f64 = 2413.0 / 128.0;
    const C3: f64 = 2392.0 / 128.0;

    let powered = code.clamp(0.0, 1.0).powf(INV_M2);
    let linear = ((powered - C1).max(0.0) / (C2 - C3 * powered)).powf(INV_M1);
    linear * PQ_MAX_NITS as f64
}
