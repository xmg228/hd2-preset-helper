//! Bounded, operation-owned evidence. Recording never captures, recognizes, or writes files.
use std::{cell::RefCell, collections::VecDeque, io::Cursor, time::Instant};

use anyhow::Context;
use image::{DynamicImage, ImageEncoder, ImageFormat, codecs::png::PngEncoder};
use serde::Serialize;
use serde_json::{Value, json};

use crate::capture::CapturedFrame;

const MAX_FRAMES: usize = 8;
// Three native 4K equipment ROIs (input baseline and the latest pair) fit here.
const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CHECKPOINTS: usize = 128;
const MAX_DETAIL_BYTES: usize = 512 * 1024;
const MAX_ATTACHMENTS: usize = 16;

pub(crate) struct OperationEvidence {
    started: Instant,
    started_unix_ms: u128,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    frames: VecDeque<RetainedFrame>,
    checkpoints: VecDeque<(Value, usize)>,
    images: Vec<(String, DynamicImage)>,
    next_checkpoint: u64,
    omitted_checkpoints: usize,
    detail_bytes: usize,
    latest: Option<Value>,
    input_reference: Option<u64>,
}

struct RetainedFrame {
    frame: CapturedFrame,
    checkpoint: bool,
}

impl OperationEvidence {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            started_unix_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            state: RefCell::new(State::default()),
        }
    }

    /// Keep the last two observations even if recognition fails before a checkpoint.
    pub fn observe(&self, frame: &CapturedFrame) {
        let mut state = self.state.borrow_mut();
        state.latest = Some(self.describe_frame(frame));
        state.frames.push_back(RetainedFrame {
            frame: frame.clone(),
            checkpoint: false,
        });
        while state
            .frames
            .iter()
            .filter(|frame| !frame.checkpoint)
            .count()
            > 2
        {
            let index = state
                .frames
                .iter()
                .position(|frame| !frame.checkpoint)
                .unwrap();
            state.frames.remove(index);
        }
        state.trim_images();
    }

    pub fn latest_frame(&self) -> Option<CapturedFrame> {
        self.state
            .borrow()
            .frames
            .back()
            .map(|item| item.frame.clone())
    }

    pub fn input(&self, before: Option<&CapturedFrame>, details: impl Serialize) {
        self.state.borrow_mut().input_reference = before.map(|frame| frame.id);
        self.record("input", before, details);
    }

    /// Details are the result already used by the caller, never recomputed for a report.
    pub fn record(&self, kind: &str, frame: Option<&CapturedFrame>, details: impl Serialize) {
        let Ok(details) = serde_json::to_value(details) else {
            return;
        };
        let mut state = self.state.borrow_mut();
        state.next_checkpoint += 1;
        let id = state.next_checkpoint;
        if let Some(frame) = frame {
            if let Some(retained) = state
                .frames
                .iter_mut()
                .find(|item| item.frame.id == frame.id)
            {
                retained.checkpoint = true;
            } else {
                let index = state
                    .frames
                    .iter()
                    .position(|item| item.frame.id > frame.id)
                    .unwrap_or(state.frames.len());
                state.frames.insert(
                    index,
                    RetainedFrame {
                        frame: frame.clone(),
                        checkpoint: true,
                    },
                );
            }
        }
        let mut checkpoint = json!({
            "id": id, "time_s": self.started.elapsed().as_secs_f64(), "kind": kind,
            "observation": frame.map(|frame| self.describe_frame(frame)), "details": details,
        });
        let mut bytes = checkpoint.to_string().len();
        if bytes > MAX_DETAIL_BYTES {
            checkpoint["details"] =
                json!({"omitted": "detail_budget_exceeded", "original_bytes": bytes});
            bytes = checkpoint.to_string().len();
        }
        state.detail_bytes += bytes;
        state.checkpoints.push_back((checkpoint, bytes));
        while state.checkpoints.len() > MAX_CHECKPOINTS || state.detail_bytes > MAX_DETAIL_BYTES {
            let (_, bytes) = state.checkpoints.pop_front().unwrap();
            state.detail_bytes -= bytes;
            state.omitted_checkpoints += 1;
        }
        state.trim_images();
        tracing::trace!(
            checkpoint = id,
            kind,
            frame_id = frame.map(|frame| frame.id),
            "operation checkpoint"
        );
    }

    /// Named domain attachments replace earlier values (e.g. the best search candidate).
    pub fn image(&self, name: &str, image: impl Into<DynamicImage>) {
        let mut state = self.state.borrow_mut();
        state.images.retain(|(old, _)| old != name);
        state.images.push((name.to_owned(), image.into()));
        state.trim_images();
    }

    pub fn remove_image(&self, name: &str) {
        self.state
            .borrow_mut()
            .images
            .retain(|(old, _)| old != name);
    }

    pub fn report(&self) -> Value {
        let state = self.state.borrow();
        json!({
            "started_unix_ms": self.started_unix_ms,
            "elapsed_s": self.started.elapsed().as_secs_f64(),
            "limits": { "frames": MAX_FRAMES, "image_bytes": MAX_IMAGE_BYTES,
                "checkpoints": MAX_CHECKPOINTS, "detail_bytes": MAX_DETAIL_BYTES, "attachments": MAX_ATTACHMENTS },
            "omitted_checkpoints": state.omitted_checkpoints,
            "last_observation": state.latest,
            "input_reference": state.input_reference,
            "checkpoints": state.checkpoints.iter().map(|(record, _)| {
                let mut record = record.clone();
                if let Some(id) = record["observation"]["id"].as_u64() {
                    record["observation"]["retained"] = json!(state.frames.iter().any(|item| item.frame.id == id));
                }
                record
            }).collect::<Vec<_>>(),
            "frames": state.frames.iter().map(|item| self.describe_frame(&item.frame)).collect::<Vec<_>>(),
            "attachments": state.images.iter().map(|(name, _)| format!("evidence/{name}.png")).collect::<Vec<_>>(),
        })
    }

    pub fn encode_images(
        &self,
        mut write: impl FnMut(&str, &[u8]) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let state = self.state.borrow();
        let mut png = Vec::new();
        for item in &state.frames {
            let name = format!("frames/{}.png", item.frame.id);
            png.clear();
            PngEncoder::new(&mut png)
                .write_image(
                    item.frame.as_raw(),
                    item.frame.width(),
                    item.frame.height(),
                    image::ColorType::Rgba8.into(),
                )
                .with_context(|| format!("failed to encode {name}"))?;
            write(&name, &png).with_context(|| format!("failed to write {name}"))?;
        }
        for (name, image) in &state.images {
            let name = format!("evidence/{name}.png");
            png.clear();
            image
                .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
                .with_context(|| format!("failed to encode {name}"))?;
            write(&name, &png).with_context(|| format!("failed to write {name}"))?;
        }
        Ok(())
    }

    fn describe_frame(&self, frame: &CapturedFrame) -> Value {
        json!({ "id": frame.id, "file": format!("frames/{}.png", frame.id),
            "time_s": frame.captured_at.saturating_duration_since(self.started).as_secs_f64(),
            "roi": frame.rect, "output_size": frame.output_size,
            "color_space": "normalized_ui_rgba8" })
    }
}

impl State {
    fn trim_images(&mut self) {
        while self.images.len() > MAX_ATTACHMENTS {
            self.images.remove(0);
        }
        while self.frames.len() > MAX_FRAMES || self.image_bytes() > MAX_IMAGE_BYTES {
            // Keep the current input's baseline and latest two observations.
            let older = self.frames.len().saturating_sub(2);
            let eligible = (0..older)
                .filter(|&index| Some(self.frames[index].frame.id) != self.input_reference);
            let index = eligible
                .clone()
                .find(|&index| !self.frames[index].checkpoint)
                .or_else(|| eligible.into_iter().next());
            if let Some(index) = index {
                self.frames.remove(index);
            } else if !self.images.is_empty() {
                self.images.remove(0);
            } else if !self.frames.is_empty() {
                self.frames.pop_front();
            } else {
                break;
            }
        }
    }

    fn image_bytes(&self) -> usize {
        self.frames
            .iter()
            .map(|item| item.frame.as_raw().len())
            .sum::<usize>()
            + self
                .images
                .iter()
                .map(|(_, image)| image.as_bytes().len())
                .sum::<usize>()
    }
}
