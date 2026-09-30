//! Optional equipment diagnostics. File failures never decide an automation outcome.
use super::{
    EquipmentItem, InputAction, NAME_IDENTITY_THRESHOLD, NAME_MATCH_THRESHOLD, search::Visit,
};
use crate::vision::{
    equipment::{EquipmentEntry, EquipmentObservation, PageChange, equipped_marker_rect},
    text::TextSample,
};
use anyhow::{Context, Result};
use image::{Rgba, RgbaImage};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Default)]
pub(crate) struct Diagnostics {
    output: Option<Output>,
    failure_directory: Option<PathBuf>,
    best_match: Option<(Visit, TextSample)>,
    search_visits: usize,
}

impl Diagnostics {
    #[cfg(feature = "diagnostics")]
    pub fn new(directory: &Path) -> Result<Self> {
        Ok(Self {
            output: Some(Output::new(directory)?),
            ..Self::default()
        })
    }

    pub fn set_failure_directory(&mut self, directory: PathBuf) {
        self.failure_directory = Some(directory);
    }

    fn write(&mut self, action: impl FnOnce(&mut Output) -> Result<()>) {
        if let Some(output) = &mut self.output
            && let Err(error) = action(output)
        {
            tracing::warn!(error = %format!("{error:#}"), "equipment diagnostics disabled after write failure");
            self.output = None;
        }
    }

    pub fn record(&mut self, event: impl FnOnce() -> Value) {
        self.write(|output| output.record(event()));
    }

    pub fn observe(&mut self, page: &EquipmentObservation, capture_ms: f64, observe_ms: f64) {
        self.write(|output| {
            output.capture_ms = capture_ms;
            output.observe_ms = observe_ms;
            if output
                .previous
                .as_ref()
                .is_none_or(|old| page.change_from(old).observed())
            {
                output.page_number += 1;
                output.record(json!({"event":"page", "page_number":output.page_number,
                    "capture_ms":capture_ms, "observe_ms":observe_ms, "page":describe(page)}))?;
                output.previous = Some(page.clone());
            }
            Ok(())
        });
    }

    pub(super) fn confirm_input(
        &mut self,
        page: &EquipmentObservation,
        result: InputConfirmation<'_>,
    ) {
        self.write(|output| output.record(json!({
            "event":if matches!(result.action, InputAction::Key(crate::input::Key::Space)) { "equipped" } else { "navigation" },
            "action":result.action.name(), "elapsed_ms":result.elapsed_ms, "input_ms":result.input_ms,
            "stable_ms":result.stable_ms, "list_moved":result.list_moved, "retried":result.retried,
            "capture_ms":output.capture_ms, "observe_ms":output.observe_ms,
            "change":result.change, "page":describe(page),
        })));
    }

    pub fn reset_page(&mut self) {
        if let Some(output) = &mut self.output {
            output.previous = None;
        }
    }

    pub fn save(&mut self, frame: &RgbaImage, page: &EquipmentObservation, reason: &str) {
        self.write(|output| output.save(frame, page, reason));
    }

    pub fn save_entry(&mut self, frame: &RgbaImage, entry: &EquipmentEntry, reason: &str) {
        self.write(|output| output.save_entry(frame, entry, reason));
    }

    pub fn start_search(&mut self, item: &EquipmentItem) {
        self.best_match = None;
        self.search_visits = 0;
        self.write(|output| output.start_search(item));
    }

    pub(super) fn search_visit(
        &mut self,
        visit: &Visit,
        page: &EquipmentObservation,
        evaluate_ms: f64,
    ) {
        self.search_visits = visit.index;
        if self
            .best_match
            .as_ref()
            .is_none_or(|(best, _)| visit.target_cosine > best.target_cosine)
        {
            self.best_match = Some((visit.clone(), page.name.clone()));
        }
        self.write(|output| output.search_visit(visit, page, evaluate_ms));
    }

    pub(super) fn finish_search(
        &mut self,
        target: &EquipmentItem,
        result: &Result<bool>,
        elapsed_ms: f64,
    ) {
        let found = matches!(result, Ok(true));
        let visits = self.search_visits;
        let error = result.as_ref().err().map(|error| format!("{error:#}"));
        tracing::info!(
            found,
            elapsed_s = elapsed_ms / 1000.0,
            visits,
            best_target_cosine = self
                .best_match
                .as_ref()
                .map(|(visit, _)| visit.target_cosine),
            best_visit = self.best_match.as_ref().map(|(visit, _)| visit.index),
            match_threshold = NAME_MATCH_THRESHOLD,
            identity_threshold = NAME_IDENTITY_THRESHOLD,
            error,
            "equipment search finished"
        );
        self.write(|output| output.finish_search(found, elapsed_ms, visits));
        if !found
            && let Some(directory) = &self.failure_directory
            && let Some((visit, name)) = &self.best_match
        {
            let saved = (|| -> Result<()> {
                fs::create_dir_all(directory)?;
                name.image.save(directory.join("best-name.png"))?;
                name.foreground()
                    .save(directory.join("best-foreground.png"))?;
                target.name.image.save(directory.join("target-name.png"))?;
                target
                    .name
                    .foreground()
                    .save(directory.join("target-foreground.png"))?;
                serde_json::to_writer_pretty(
                    File::create(directory.join("summary.json"))?,
                    &json!({"visits":visits, "elapsed_ms":elapsed_ms, "error":error,
                        "match_threshold":NAME_MATCH_THRESHOLD,
                        "identity_threshold":NAME_IDENTITY_THRESHOLD, "best":visit,
                        "target_geometry":target.name.geometry, "best_geometry":name.geometry}),
                )?;
                Ok(())
            })();
            match saved {
                Ok(()) => {
                    tracing::info!(path = %directory.display(), "saved best equipment search match")
                }
                Err(error) => tracing::warn!(%error, "failed to save best equipment search match"),
            }
        }
    }
}

pub(super) struct InputConfirmation<'a> {
    pub action: InputAction,
    pub elapsed_ms: f64,
    pub input_ms: f64,
    pub stable_ms: Option<f64>,
    pub list_moved: bool,
    pub retried: bool,
    pub change: &'a PageChange,
}

struct Output {
    directory: PathBuf,
    events: File,
    snapshot: usize,
    search: usize,
    started: Instant,
    names: Vec<(TextSample, f32)>,
    last_visit: Value,
    previous: Option<EquipmentObservation>,
    page_number: u64,
    capture_ms: f64,
    observe_ms: f64,
}

impl Output {
    #[cfg(feature = "diagnostics")]
    pub fn new(directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory)?;
        Ok(Self {
            directory: directory.to_owned(),
            events: File::create(directory.join("observations.jsonl"))?,
            snapshot: 0,
            search: 0,
            started: Instant::now(),
            names: Vec::new(),
            last_visit: Value::Null,
            previous: None,
            page_number: 0,
            capture_ms: 0.0,
            observe_ms: 0.0,
        })
    }

    pub fn record(&mut self, mut event: Value) -> Result<()> {
        event["time_ms"] = json!(self.started.elapsed().as_secs_f64() * 1000.0);
        serde_json::to_writer(&mut self.events, &event)?;
        self.events.write_all(b"\n")?;
        Ok(())
    }

    pub fn start_search(&mut self, target: &EquipmentItem) -> Result<()> {
        self.search += 1;
        self.names.clear();
        self.last_visit = Value::Null;
        let directory = self.directory.join(format!("search-{:04}", self.search));
        fs::create_dir_all(&directory)?;
        target.name.image.save(directory.join("target-name.png"))?;
        target
            .name
            .foreground()
            .save(directory.join("target-foreground.png"))?;
        save_category(&directory, target.category.as_ref(), "target-")?;
        self.record(json!({"event":"search_start", "search":self.search,
            "match_threshold":NAME_MATCH_THRESHOLD,
            "identity_threshold":NAME_IDENTITY_THRESHOLD}))
    }

    pub fn search_visit(
        &mut self,
        visit: &Visit,
        page: &EquipmentObservation,
        evaluate_ms: f64,
    ) -> Result<()> {
        let directory = self.directory.join(format!("search-{:04}", self.search));
        page.name
            .image
            .save(directory.join(format!("{:04}-name.png", visit.index)))?;
        page.name
            .foreground()
            .save(directory.join(format!("{:04}-foreground.png", visit.index)))?;
        save_category(
            &directory,
            Some(&page.category),
            &format!("{:04}-", visit.index),
        )?;
        let known = self
            .names
            .iter()
            .enumerate()
            .map(|(index, (name, _))| (index, page.name.cosine(name)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .filter(|(_, score)| *score >= NAME_IDENTITY_THRESHOLD);
        let item = if let Some((index, _)) = known {
            self.names[index].1 = self.names[index].1.max(visit.target_cosine);
            index
        } else {
            self.names.push((page.name.clone(), visit.target_cosine));
            self.names.len() - 1
        };
        let mut scores = self
            .names
            .iter()
            .map(|(_, score)| *score)
            .collect::<Vec<_>>();
        scores.sort_unstable_by(|a, b| b.total_cmp(a));
        let mut detail = serde_json::to_value(visit)?;
        detail["item"] = json!(item);
        detail["best_observed"] = json!(scores[0]);
        detail["second_observed"] = json!(scores.get(1));
        self.last_visit = detail;
        self.record(json!({"event":"search_visit", "search":self.search,
            "evaluate_ms":evaluate_ms, "visit":self.last_visit}))
    }

    pub fn finish_search(&mut self, found: bool, elapsed_ms: f64, visits: usize) -> Result<()> {
        self.record(json!({"event":"search_end", "search":self.search,
            "found":found, "elapsed_ms":elapsed_ms, "visits":visits,
            "inputs":visits.saturating_sub(1), "last_visit":self.last_visit}))
    }

    pub fn save(
        &mut self,
        image: &RgbaImage,
        page: &EquipmentObservation,
        reason: &str,
    ) -> Result<()> {
        let directory = self.snapshot_directory(reason)?;
        image.save(directory.join("frame.png"))?;
        let mut annotated = image.clone();
        for (index, slot) in page.slots.iter().enumerate() {
            let color = if page.focus == Some(index) {
                [255, 80, 230, 255]
            } else {
                [50, 240, 255, 255]
            };
            outline(&mut annotated, slot.rect, color);
            if slot.equipped {
                let rect = equipped_marker_rect(slot.rect, page.scale);
                outline(
                    &mut annotated,
                    [
                        rect.x as f64,
                        rect.y as f64,
                        (rect.x + rect.w - 1) as f64,
                        (rect.y + rect.h - 1) as f64,
                    ],
                    [255, 130, 30, 255],
                );
            }
        }
        annotated.save(directory.join("annotated.png"))?;
        page.name.image.save(directory.join("name.png"))?;
        page.name
            .foreground()
            .save(directory.join("foreground.png"))?;
        save_category(&directory, Some(&page.category), "")?;
        let data = File::create(directory.join("observation.json"))?;
        serde_json::to_writer_pretty(data, &describe(page))
            .context("failed to write equipment observation")?;
        tracing::debug!(path = %directory.display(), "equipment snapshot saved");
        Ok(())
    }

    pub fn save_entry(
        &mut self,
        image: &RgbaImage,
        entry: &EquipmentEntry,
        reason: &str,
    ) -> Result<()> {
        let directory = self.snapshot_directory(reason)?;
        image.save(directory.join("frame.png"))?;
        let mut annotated = image.clone();
        for &rect in &entry.rects {
            outline(&mut annotated, rect, [50, 240, 255, 255]);
        }
        annotated.save(directory.join("annotated.png"))?;
        serde_json::to_writer_pretty(File::create(directory.join("observation.json"))?, entry)?;
        tracing::debug!(path = %directory.display(), "equipment entry snapshot saved");
        Ok(())
    }

    fn snapshot_directory(&mut self, reason: &str) -> Result<PathBuf> {
        self.snapshot += 1;
        let directory = self
            .directory
            .join(format!("{:04}-{reason}", self.snapshot));
        fs::create_dir_all(&directory)?;
        Ok(directory)
    }
}

pub(crate) fn describe(page: &EquipmentObservation) -> Value {
    json!({ "scale": page.scale, "slots": page.slots, "focus": page.focus, "hover": page.hover,
        "name_width": page.name.image.width(), "name_height": page.name.image.height(),
        "name_support": page.name.response.iter().filter(|&&v| v > 0.0).count(),
        "category_support": page.category.response.iter().filter(|&&v| v > 0.0).count(),
        "name_energy": page.name.response.iter().map(|v| v*v).sum::<f32>() })
}

fn save_category(directory: &Path, category: Option<&TextSample>, prefix: &str) -> Result<()> {
    if let Some(title) = category {
        title
            .image
            .save(directory.join(format!("{prefix}category.png")))?;
        title
            .foreground()
            .save(directory.join(format!("{prefix}category-foreground.png")))?;
    }
    Ok(())
}

fn outline(image: &mut RgbaImage, rect: [f64; 4], color: [u8; 4]) {
    let [l, t, r, b] = rect.map(|v| v.round() as u32);
    let (l, r) = (l.min(image.width() - 1), r.min(image.width() - 1));
    let (t, b) = (t.min(image.height() - 1), b.min(image.height() - 1));
    for x in l..=r {
        image.put_pixel(x, t, Rgba(color));
        image.put_pixel(x, b, Rgba(color));
    }
    for y in t..=b {
        image.put_pixel(l, y, Rgba(color));
        image.put_pixel(r, y, Rgba(color));
    }
}
