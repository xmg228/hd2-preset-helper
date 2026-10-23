use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use image::RgbaImage;
use serde::Serialize;

use crate::item::{ItemKind, StratagemCategory};

use super::Slot;
use super::matcher::MatchResult;
use super::semantic_extractor::SemanticExtraction;

static RECORDER: OnceLock<Mutex<Recorder>> = OnceLock::new();

struct Recorder {
    writer: Option<BufWriter<File>>,
    next_frame: u64,
    anomaly_dir: PathBuf,
    next_anomaly: u64,
    map_dir: PathBuf,
    next_map: u64,
}

pub(super) struct SlotDiagnostics {
    row: u32,
    col: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    threshold: f64,
    best_margin: f64,
    comparisons: Vec<ComparisonDiagnostics>,
}

struct ComparisonDiagnostics {
    template: String,
    category: Option<StratagemCategory>,
    rank: usize,
    accepted: bool,
    result: MatchResult,
    semantic_mode: &'static str,
    primary_endpoint: [f32; 3],
    secondary_endpoint: [f32; 3],
    primary_mass: f32,
    secondary_mass: f32,
}

#[derive(Serialize)]
struct ScoreRecord<'a> {
    frame: u64,
    row: u32,
    col: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    template: &'a str,
    category: Option<StratagemCategory>,
    rank: usize,
    accepted: bool,
    score: f64,
    threshold: f64,
    best_margin: f64,
    evidence: f64,
    white_evidence: f64,
    class_evidence: f64,
    spatial: f64,
    qx: f32,
    qy: f32,
    evaluations: usize,
    global_white: f64,
    local_white: f64,
    global_class: f64,
    local_class: f64,
    overlap_white: f64,
    overlap_class: f64,
    white_gain: f64,
    class_gain: f64,
    semantic_mode: &'a str,
    primary_endpoint: [f32; 3],
    secondary_endpoint: [f32; 3],
    primary_mass: f32,
    secondary_mass: f32,
}

pub fn init(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("diagnostic score path has no parent")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;
    let anomaly_dir = parent.join("matcher-anomalies");
    std::fs::create_dir_all(&anomaly_dir)
        .with_context(|| format!("failed to create {}", anomaly_dir.display()))?;
    let map_dir = parent.join("list-maps");
    std::fs::create_dir_all(&map_dir)
        .with_context(|| format!("failed to create {}", map_dir.display()))?;
    let file = File::create(path)
        .with_context(|| format!("failed to create diagnostic scores {}", path.display()))?;
    RECORDER
        .set(Mutex::new(Recorder {
            writer: Some(BufWriter::new(file)),
            next_frame: 1,
            anomaly_dir,
            next_anomaly: 1,
            map_dir,
            next_map: 1,
        }))
        .map_err(|_| anyhow!("diagnostics were already initialized"))
}

pub fn create_list_map_case_directory(item_kind: ItemKind, reason: &str) -> Result<PathBuf> {
    let recorder = RECORDER.get().context("diagnostics were not initialized")?;
    let mut recorder = recorder
        .lock()
        .map_err(|_| anyhow!("diagnostics lock was poisoned"))?;
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let path = recorder.map_dir.join(format!(
        "{timestamp}-{:03}-{}-{reason}",
        recorder.next_map,
        item_kind.label()
    ));
    recorder.next_map += 1;
    std::fs::create_dir_all(&path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    Ok(path)
}

pub fn save_list_map_image(item_kind: ItemKind, image: &RgbaImage) -> Result<PathBuf> {
    let recorder = RECORDER.get().context("diagnostics were not initialized")?;
    let mut recorder = recorder
        .lock()
        .map_err(|_| anyhow!("diagnostics lock was poisoned"))?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis();
    let path = recorder.map_dir.join(format!(
        "{timestamp}-{:03}-{}.png",
        recorder.next_map,
        item_kind.label()
    ));
    recorder.next_map += 1;
    image
        .save(&path)
        .with_context(|| format!("failed to save list map image {}", path.display()))?;
    Ok(path)
}

pub fn save_fallback_slot(
    screenshot: &RgbaImage,
    slot: &Slot,
    item_id: &str,
    match_error: f64,
) -> Result<()> {
    let recorder = RECORDER.get().context("diagnostics were not initialized")?;
    let mut recorder = recorder
        .lock()
        .map_err(|_| anyhow!("diagnostics lock was poisoned"))?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis();
    let item_name = item_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let path = recorder.anomaly_dir.join(format!(
        "{timestamp}-{:03}-{item_name}-r{}-c{}-{match_error:.6}.png",
        recorder.next_anomaly, slot.row, slot.col
    ));
    recorder.next_anomaly += 1;
    image::imageops::crop_imm(screenshot, slot.x, slot.y, slot.w, slot.h)
        .to_image()
        .save(&path)
        .with_context(|| format!("failed to save matcher anomaly {}", path.display()))
}

pub(super) fn collect_slot<'a>(
    slot: &Slot,
    ranked: impl Iterator<
        Item = (
            &'a str,
            Option<StratagemCategory>,
            &'a SemanticExtraction,
            MatchResult,
        ),
    >,
    accepted: bool,
    threshold: f64,
) -> SlotDiagnostics {
    let comparisons = ranked
        .enumerate()
        .map(
            |(index, (template, category, extraction, result))| ComparisonDiagnostics {
                template: template.to_string(),
                category,
                rank: index + 1,
                accepted: accepted && index == 0,
                result,
                semantic_mode: extraction.mode,
                primary_endpoint: extraction.primary_endpoint,
                secondary_endpoint: extraction.secondary_endpoint,
                primary_mass: extraction.primary_mass,
                secondary_mass: extraction.secondary_mass,
            },
        )
        .collect::<Vec<_>>();
    let best_margin = comparisons.get(1).map_or(f64::INFINITY, |second| {
        second.result.score - comparisons[0].result.score
    });

    SlotDiagnostics {
        row: slot.row,
        col: slot.col,
        x: slot.x,
        y: slot.y,
        width: slot.w,
        height: slot.h,
        threshold,
        best_margin,
        comparisons,
    }
}

pub(super) fn record_frame<'a>(slots: impl IntoIterator<Item = &'a SlotDiagnostics>) -> Result<()> {
    let recorder = RECORDER.get().context("diagnostics were not initialized")?;
    let mut recorder = recorder
        .lock()
        .map_err(|_| anyhow!("diagnostics lock was poisoned"))?;
    let frame = recorder.next_frame;
    recorder.next_frame += 1;
    let Some(writer) = recorder.writer.as_mut() else {
        return Ok(());
    };

    let result = (|| -> Result<()> {
        for slot in slots {
            for comparison in &slot.comparisons {
                let result = comparison.result;
                let record = ScoreRecord {
                    frame,
                    row: slot.row,
                    col: slot.col,
                    x: slot.x,
                    y: slot.y,
                    width: slot.width,
                    height: slot.height,
                    template: &comparison.template,
                    category: comparison.category,
                    rank: comparison.rank,
                    accepted: comparison.accepted,
                    score: result.score,
                    threshold: slot.threshold,
                    best_margin: slot.best_margin,
                    evidence: result.evidence,
                    white_evidence: result.white_evidence,
                    class_evidence: result.class_evidence,
                    spatial: result.spatial,
                    qx: result.qx,
                    qy: result.qy,
                    evaluations: result.evaluations,
                    global_white: result.global_white,
                    local_white: result.local_white,
                    global_class: result.global_class,
                    local_class: result.local_class,
                    overlap_white: result.overlap_white,
                    overlap_class: result.overlap_class,
                    white_gain: result.white_gain,
                    class_gain: result.class_gain,
                    semantic_mode: comparison.semantic_mode,
                    primary_endpoint: comparison.primary_endpoint,
                    secondary_endpoint: comparison.secondary_endpoint,
                    primary_mass: comparison.primary_mass,
                    secondary_mass: comparison.secondary_mass,
                };
                serde_json::to_writer(&mut *writer, &record)
                    .context("failed to serialize diagnostic scores")?;
                writer
                    .write_all(b"\n")
                    .context("failed to write diagnostic scores")?;
            }
        }
        writer.flush().context("failed to flush diagnostic scores")
    })();
    if result.is_err() {
        recorder.writer = None;
    }
    result
}
