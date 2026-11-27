//! Collect both card states from scans without making previews a navigation requirement.
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use image::RgbaImage;

use super::Session;
use crate::{
    capture::CapturedFrame,
    item::EquipmentKind,
    vision::{
        ScrollbarObservation,
        equipment::{EquipmentObservation, EquipmentSlot, equipped_marker_center},
        equipment_thumbnail::{EquipmentCard, IgnoreRegion},
    },
};

const STILL_INTERVAL: Duration = Duration::from_millis(30);
const ARTWORK_LOAD_TIMEOUT: Duration = Duration::from_secs(10);
const ALTERNATE_STATE_TIMEOUT: Duration = Duration::from_millis(500);

// Card interior coordinates, excluding the border inset in EquipmentCard.
const WEAPON_LEVEL: IgnoreRegion = IgnoreRegion::Rect {
    left: 0.0,
    top: 0.0,
    right: 0.45,
    bottom: 0.225,
};
const EQUIPPED_MARKER_RADIUS: f64 = 7.25; // Reference pixels, including the outer ring.

struct CardFrame {
    sample: EquipmentCard,
    captured_at: Instant,
}

enum Artwork {
    Loading,
    StableSince(Instant),
    Ready,
}

struct CardPair {
    slot: EquipmentSlot,
    frames: [Option<CardFrame>; 2],
    artwork: Artwork,
    load_timed_out: bool,
}

impl CardPair {
    fn matches(&self, slot: &EquipmentSlot, tolerance: f64) -> bool {
        // Column x and slot dimensions are fixed within an equipment list.
        slot.col == self.slot.col && (slot.rect[1] - self.slot.rect[1]).abs() <= tolerance
    }

    fn observe(&mut self, frame: &CapturedFrame, yellow: bool) -> bool {
        let index = usize::from(yellow);
        if let Some(reference) = &self.frames[index] {
            if self.ready() {
                return true;
            }
            if reference.sample.stable_in_frame(frame) {
                if matches!(self.artwork, Artwork::Loading)
                    && frame.captured_at.duration_since(reference.captured_at) >= STILL_INTERVAL
                {
                    self.artwork = Artwork::StableSince(reference.captured_at);
                }
                return true;
            }
            // A change in the same background makes both earlier crops suspect.
            self.frames = [None, None];
            self.artwork = Artwork::Loading;
        }

        let sample = &mut self.frames[index];
        *sample = EquipmentCard::capture(frame, self.slot.rect, yellow).map(|sample| CardFrame {
            sample,
            captured_at: frame.captured_at,
        });
        sample.is_some()
    }

    fn confirm_artwork(&mut self) -> bool {
        // Passive frames only establish readiness; the scan decides when to lock it.
        if let Artwork::StableSince(since) = self.artwork {
            for frame in &mut self.frames {
                if frame
                    .as_ref()
                    .is_some_and(|frame| frame.captured_at < since)
                {
                    *frame = None;
                }
            }
            self.artwork = Artwork::Ready;
        }
        self.ready()
    }

    fn ready(&self) -> bool {
        matches!(self.artwork, Artwork::Ready)
    }

    fn complete(&self) -> bool {
        self.ready() && self.frames.iter().all(Option::is_some)
    }

    fn finish(mut self, kind: EquipmentKind, scale: f64) -> Option<Arc<RgbaImage>> {
        let ready = self.confirm_artwork();
        let [normal, yellow] = self.frames;
        let pair = normal.zip(yellow).filter(|_| ready);
        if let Some((normal, yellow)) = &pair {
            let mut ignored = Vec::new();
            if kind == EquipmentKind::Primary {
                ignored.push(WEAPON_LEVEL);
            }
            if self.slot.equipped {
                ignored.push(IgnoreRegion::Circle {
                    center: equipped_marker_center(self.slot.rect, scale),
                    radius: EQUIPPED_MARKER_RADIUS * scale,
                });
            }
            if let Some(image) = EquipmentCard::thumbnail(&normal.sample, &yellow.sample, &ignored)
            {
                return Some(Arc::new(image));
            }
        }
        let reason = if pair.is_some() {
            "pair_rejected"
        } else if ready {
            "alternate_state_missing"
        } else if self.load_timed_out {
            "load_timeout"
        } else {
            return None;
        };
        tracing::info!(
            ?kind,
            viewport_row = self.slot.row,
            column = self.slot.col,
            reason,
            "equipment thumbnail skipped"
        );
        None
    }
}

pub(super) struct ThumbnailRow {
    pairs: Vec<CardPair>,
    scrollbar: Option<ScrollbarObservation>,
}

impl ThumbnailRow {
    fn new(page: &EquipmentObservation) -> Option<Self> {
        let row = page.slots.get(page.focus?)?.row;
        let pairs = page
            .slots
            .iter()
            .filter(|slot| slot.row == row)
            .cloned()
            .map(|slot| CardPair {
                slot,
                frames: [None, None],
                artwork: Artwork::Loading,
                load_timed_out: false,
            })
            .collect();
        Some(Self {
            pairs,
            scrollbar: page.scrollbar,
        })
    }

    fn scrollbar_moved(&self, page: &EquipmentObservation) -> bool {
        ScrollbarObservation::moved(self.scrollbar, page.scrollbar, page.scale.max(1.0))
    }

    pub(super) fn observe(&mut self, frame: &CapturedFrame, page: &EquipmentObservation) {
        if self.scrollbar_moved(page) {
            return;
        }
        let tolerance = page.scale.max(1.0);
        for pair in &mut self.pairs {
            if let Some((index, slot)) = page
                .slots
                .iter()
                .enumerate()
                .find(|(_, slot)| pair.matches(slot, tolerance))
            {
                pair.slot.equipped |= slot.equipped;
                let yellow = slot.has_yellow_border() && page.hover != Some(index);
                pair.observe(frame, yellow);
            }
        }
    }
}

impl Session<'_> {
    pub(super) fn begin_thumbnail_row(&mut self) {
        self.thumbnail_row = ThumbnailRow::new(&self.page);
        if let Some(row) = &mut self.thumbnail_row {
            row.observe(&self.frame, &self.page);
        }
    }

    pub(super) fn wait_for_thumbnail_artwork(&mut self) -> Result<()> {
        let Some(index) = self.page.focus else {
            return Ok(());
        };
        let focus = &self.page.slots[index];
        let Some(mut row) = self.thumbnail_row.take() else {
            return Ok(());
        };
        if !row.scrollbar_moved(&self.page)
            && let Some(pair) = row
                .pairs
                .iter_mut()
                .find(|pair| pair.matches(focus, self.page.scale.max(1.0)))
        {
            let deadline = Instant::now() + ARTWORK_LOAD_TIMEOUT;
            while !pair.confirm_artwork() {
                if Instant::now() >= deadline {
                    pair.load_timed_out = true;
                    break;
                }
                if !self.sample_thumbnail_card(pair, row.scrollbar)? {
                    break;
                }
            }
            if !pair.ready() {
                pair.frames = [None, None];
            }
        }
        self.thumbnail_row = Some(row);
        Ok(())
    }

    pub(super) fn finish_thumbnail_row(&mut self) -> Result<Vec<Option<Arc<RgbaImage>>>> {
        let Some(mut row) = self.thumbnail_row.take() else {
            return Ok(Vec::new());
        };
        let Some(focus) = self.page.focus.map(|index| &self.page.slots[index]) else {
            return Ok(Vec::new());
        };
        let tolerance = self.page.scale.max(1.0);
        if row.scrollbar_moved(&self.page)
            || row.pairs.iter().any(|pair| {
                !self
                    .page
                    .slots
                    .iter()
                    .any(|slot| slot.row == focus.row && pair.matches(slot, tolerance))
            })
        {
            return Ok(Vec::new());
        }
        if let Some(pair) = row.pairs.iter_mut().find(|pair| pair.slot.col == focus.col) {
            self.complete_last_thumbnail(pair, row.scrollbar)?;
        }
        let kind = self.observer.kind();
        let scale = self.page.scale;
        Ok(row
            .pairs
            .into_iter()
            .map(|pair| pair.finish(kind, scale))
            .collect())
    }

    fn complete_last_thumbnail(
        &mut self,
        pair: &mut CardPair,
        scrollbar: Option<ScrollbarObservation>,
    ) -> Result<()> {
        if !pair.ready() || pair.complete() {
            return Ok(());
        }
        let deadline = Instant::now() + ALTERNATE_STATE_TIMEOUT;
        let point = if pair.frames[0].is_none() {
            pair.slot.center()
        } else {
            // Blank space to the left of the list, inside the capture.
            (0, pair.slot.center().1)
        };
        if let Err(error) = self.automation.move_cursor(point) {
            self.automation.check_cancelled()?;
            tracing::debug!(%error, "equipment thumbnail cursor move unavailable");
            return Ok(());
        }
        while !pair.complete() && Instant::now() < deadline {
            if !self.sample_thumbnail_card(pair, scrollbar)? {
                break;
            }
        }
        Ok(())
    }

    fn sample_thumbnail_card(
        &mut self,
        pair: &mut CardPair,
        scrollbar: Option<ScrollbarObservation>,
    ) -> Result<bool> {
        // Extra frames only feed the card; navigation retains its last full observation.
        let frame = match self.automation.capture() {
            Ok(frame)
                if frame.rect == self.frame.rect && frame.output_size == self.frame.output_size =>
            {
                frame
            }
            Ok(_) => return Ok(false),
            Err(error) => {
                self.automation.check_cancelled()?;
                tracing::debug!(%error, "equipment thumbnail frame unavailable");
                return Ok(false);
            }
        };
        if scrollbar.is_some()
            && ScrollbarObservation::moved(
                scrollbar,
                self.observer.scrollbar(&frame),
                self.page.scale.max(1.0),
            )
        {
            return Ok(false);
        }
        let yellow = self.observer.has_yellow_border(&frame, pair.slot.rect);
        Ok(pair.observe(&frame, yellow))
    }
}
