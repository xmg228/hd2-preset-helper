//! Closed-loop equipment actions on the session's latest observation.
use super::{
    EQUIP_TIMEOUT, EquipmentItem, INPUT_HOLD_MS, INPUT_RELEASE_INTERVAL, InputAction,
    NAME_MATCH_THRESHOLD, NAVIGATION_RETRY_AFTER, NAVIGATION_TIMEOUT, Navigation,
    PAGE_STABLE_INTERVAL, Session, diagnostics,
};
use crate::{
    input::Key,
    item::EquipmentKind,
    vision::{equipment::EquipmentObservation, text::TextSample},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use std::time::Instant;

impl Session<'_, '_> {
    fn wait_for_release(&mut self) -> Result<()> {
        while self
            .last_release
            .is_some_and(|released| released.elapsed() < INPUT_RELEASE_INTERVAL)
        {
            self.refresh()?;
        }
        self.check_active()
    }

    // Only releases inputs injected by this session; no global key-release operation.
    fn press(&mut self, action: InputAction, point: Option<(u32, u32)>) -> Result<(Instant, f64)> {
        self.check_active()?;
        let started = Instant::now();
        let previous_release_ms = self
            .last_release
            .map(|released| started.duration_since(released).as_secs_f64() * 1000.0);
        match action {
            InputAction::Key(key) => self.automation.tap_key(key, INPUT_HOLD_MS)?,
            InputAction::ClickColumn(_) => self.automation.click_current(INPUT_HOLD_MS)?,
        }
        let released = Instant::now();
        self.last_release = Some(released);
        let input_ms = released.duration_since(started).as_secs_f64() * 1000.0;
        self.diagnostics.record(|| {
            json!({"event":"input", "action":action.name(), "point":point,
            "input_ms":input_ms, "previous_release_ms":previous_release_ms})
        });
        Ok((started, input_ms))
    }

    /// Hover changes the category heading without changing keyboard focus.
    pub(super) fn sync_category(&mut self) -> Result<()> {
        if self.page.category.is_none() {
            return Ok(());
        }
        let started = Instant::now();
        let mut cursor: Option<(u32, u32)> = None;
        let mut hover_confirmed = false;
        loop {
            self.check_active()?;
            if self.page.ready()
                && self.page.focus == self.page.hover
                && self
                    .page
                    .category
                    .as_ref()
                    .is_some_and(|title| title.has_text())
            {
                if cursor.is_some() && !hover_confirmed {
                    // The category heading can update after the hover border.
                    hover_confirmed = true;
                    self.refresh()?;
                    continue;
                }
                if cursor.is_some() {
                    self.diagnostics.record(|| {
                        json!({"event":"category_focus_confirmed",
                        "elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
                        "page":diagnostics::describe(&self.page)})
                    });
                }
                return Ok(());
            }
            hover_confirmed = false;
            if started.elapsed() >= NAVIGATION_TIMEOUT {
                self.diagnostics
                    .save(&self.frame, &self.page, "category-focus-timeout");
                bail!("category heading could not be associated with the keyboard focus");
            }
            if let Some(index) = self.page.focus {
                let point = self.page.slots[index].center();
                if cursor.is_none_or(|old| {
                    point.0.abs_diff(old.0).max(point.1.abs_diff(old.1)) as f64
                        > 3.0 * self.page.scale
                }) {
                    self.automation.move_cursor(point)?;
                    cursor = Some(point);
                    self.diagnostics
                        .record(|| json!({"event":"category_focus_move", "point":point}));
                }
            }
            self.refresh()?;
        }
    }

    pub(super) fn open(&mut self, kind: EquipmentKind) -> Result<()> {
        self.observer = self.observer.with_kind(kind);
        self.diagnostics.reset_page();
        let started = Instant::now();
        let entry = loop {
            self.check_active()?;
            let entry = self.observer.entry(&self.frame);
            if entry.confirmed() && entry.scores[kind.index()] >= 0.05 {
                self.diagnostics
                    .save_entry(&self.frame, &entry, "entry-before");
                break entry;
            }
            if started.elapsed() >= NAVIGATION_TIMEOUT {
                self.diagnostics
                    .save_entry(&self.frame, &entry, "entry-target-timeout");
                bail!("equipment entry target was not confirmed before timeout; no click was sent");
            }
            self.refresh()?;
        };
        self.check_active()?;
        let point = entry.point(kind);
        self.automation.click(point, INPUT_HOLD_MS)?;
        self.diagnostics
            .record(|| json!({"event":"entry_open_sent", "point":point, "entry":entry}));
        let mut settling: Option<(EquipmentObservation, Instant)> = None;
        loop {
            self.refresh()?;
            let ready = self.page.ready()
                && self
                    .page
                    .category
                    .as_ref()
                    .is_none_or(|title| title.has_text())
                && self.page.focus.is_some_and(|i| self.page.slots[i].equipped)
                && !self.at_entry();
            if ready {
                match &settling {
                    Some((anchor, since)) if !self.page.change_from(anchor).observed() => {
                        if since.elapsed() >= PAGE_STABLE_INTERVAL {
                            self.sync_category()?;
                            self.diagnostics.record(|| {
                                json!({"event":"entry_opened",
                                "elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
                                "page":diagnostics::describe(&self.page)})
                            });
                            self.diagnostics
                                .save(&self.frame, &self.page, "entry-opened");
                            return Ok(());
                        }
                    }
                    _ => settling = Some((self.page.clone(), Instant::now())),
                }
            } else {
                settling = None;
            }
            if started.elapsed() >= NAVIGATION_TIMEOUT {
                self.diagnostics
                    .save(&self.frame, &self.page, "entry-open-timeout");
                bail!(
                    "equipment list and its equipped focus were not confirmed after opening; no retry was sent"
                );
            }
        }
    }

    pub(super) fn return_to_entry(&mut self) -> Result<()> {
        self.press(InputAction::Key(Key::Escape), None)?;
        self.diagnostics.record(|| json!({"event":"return_sent"}));
        let started = Instant::now();
        loop {
            // Returning needs only entry evidence, not full list/text recognition each frame.
            self.check_active()?;
            let frame = self.automation.capture()?;
            let entry = self.observer.entry(&frame);
            if entry.confirmed() {
                self.diagnostics.record(|| {
                    json!({"event":"entry_returned", "entry":entry,
                    "elapsed_ms":started.elapsed().as_secs_f64()*1000.0})
                });
                self.diagnostics
                    .save_entry(&frame, &entry, "entry-returned");
                self.page = self.observer.observe(&frame);
                self.frame = frame;
                tracing::debug!("equipment entry confirmed");
                return Ok(());
            }
            if started.elapsed() >= NAVIGATION_TIMEOUT {
                self.diagnostics
                    .save_entry(&frame, &entry, "entry-return-timeout");
                bail!("equipment entry was not confirmed after Esc; no retry was sent");
            }
        }
    }

    pub fn navigate(&mut self, action: InputAction) -> Result<Navigation> {
        self.wait_for_release()?;
        ensure!(
            self.page.ready(),
            "no confirmed equipment focus/name; input was not sent"
        );
        if matches!(action, InputAction::Key(Key::Z | Key::C)) {
            self.sync_category()?;
        }
        let point = if let InputAction::ClickColumn(col) = action {
            Some(self.hover_column(col)?)
        } else {
            None
        };
        let before = self.page.clone();
        let (started, input_ms) = self.press(action, point)?;
        self.wait_for_input(action, &before, started, input_ms)
    }

    fn hover_column(&mut self, col: usize) -> Result<(u32, u32)> {
        let started = Instant::now();
        let mut cursor: Option<(u32, u32)> = None;
        loop {
            self.check_active()?;
            if started.elapsed() >= NAVIGATION_TIMEOUT {
                self.diagnostics
                    .save(&self.frame, &self.page, "hover-timeout");
                bail!("equipment target hover was not confirmed; no click was sent");
            }
            if self.page.ready() {
                let row = self.page.slots[self.page.focus.unwrap()].row;
                let (index, slot) = self
                    .page
                    .slots
                    .iter()
                    .enumerate()
                    .find(|(_, slot)| slot.row == row && slot.col == col)
                    .context("requested equipment column is no longer visible")?;
                let point = slot.center();
                if cursor.is_none_or(|old| {
                    point.0.abs_diff(old.0).max(point.1.abs_diff(old.1)) as f64
                        > 3.0 * self.page.scale
                }) {
                    self.automation.move_cursor(point)?;
                    cursor = Some(point);
                    self.diagnostics
                        .record(|| json!({"event":"hover_move", "col":col, "point":point}));
                } else if self.page.hover == Some(index) {
                    let point = cursor.unwrap();
                    self.diagnostics.record(|| {
                        json!({"event":"hover_confirmed", "col":col, "point":point,
                        "elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
                        "page":diagnostics::describe(&self.page)})
                    });
                    return Ok(point);
                }
            }
            self.refresh()?;
        }
    }

    fn wait_for_input(
        &mut self,
        action: InputAction,
        before: &EquipmentObservation,
        started: Instant,
        input_ms: f64,
    ) -> Result<Navigation> {
        let (mut list_moved, mut responded, mut retried) = (false, false, false);
        let mut settling: Option<(EquipmentObservation, Instant)> = None;
        let equipping = matches!(action, InputAction::Key(Key::Space));
        let timeout = if equipping {
            EQUIP_TIMEOUT
        } else {
            NAVIGATION_TIMEOUT
        };
        loop {
            self.refresh()?;
            let change = self.page.change_from(before);
            list_moved |= change.geometry_changed;
            responded |= change.observed();
            let complete = if equipping {
                change.name_cosine >= NAME_MATCH_THRESHOLD
                    && self.page.focus.is_some_and(|i| self.page.slots[i].equipped)
            } else if !self.page.ready()
                || !(list_moved || change.focus_changed)
                || matches!(action, InputAction::ClickColumn(col) if self.page.focus.is_none_or(|i| self.page.slots[i].col != col))
            {
                settling = None;
                false
            } else if list_moved
                || !change.name_changed
                || settling.is_some()
                || matches!(action, InputAction::Key(Key::Z | Key::C))
                || matches!(action, InputAction::Key(Key::Up | Key::Down))
                    && self.page.focus == self.page.hover
            {
                match &settling {
                    Some((anchor, since)) if !self.page.change_from(anchor).observed() => {
                        since.elapsed() >= PAGE_STABLE_INTERVAL
                    }
                    _ => {
                        settling = Some((self.page.clone(), Instant::now()));
                        false
                    }
                }
            } else {
                true
            };
            if complete {
                let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
                let stable_ms = settling
                    .as_ref()
                    .map(|(_, since)| since.elapsed().as_secs_f64() * 1000.0);
                self.diagnostics.confirm_input(
                    &self.page,
                    diagnostics::InputConfirmation {
                        action,
                        elapsed_ms,
                        input_ms,
                        stable_ms,
                        list_moved,
                        retried,
                        change: &change,
                    },
                );
                if equipping {
                    self.diagnostics.save(&self.frame, &self.page, "equipped");
                }
                let wrapped = !retried
                    && match action {
                        InputAction::Key(key @ (Key::Up | Key::Down)) => {
                            let old = &before.slots[before.focus.unwrap()];
                            let current = &self.page.slots[self.page.focus.unwrap()];
                            let dy =
                                (current.rect[1] + current.rect[3] - old.rect[1] - old.rect[3])
                                    * 0.5;
                            let height = old.rect[3] - old.rect[1];
                            // A normal row step moves with the key or stays near its
                            // scroll anchor. Small snapping/jitter is not a wrap.
                            let opposite = if key == Key::Down { -dy } else { dy };
                            opposite > height * 0.75
                        }
                        _ => false,
                    };
                tracing::debug!(
                    action = action.name(),
                    elapsed_s = elapsed_ms / 1000.0,
                    stable_s = stable_ms.map(|ms| ms / 1000.0),
                    name_cosine = change.name_cosine,
                    list_moved,
                    retried,
                    wrapped,
                    "equipment input confirmed"
                );
                return Ok(Navigation {
                    action,
                    retried,
                    wrapped,
                });
            }
            if started.elapsed() >= timeout {
                self.diagnostics.save(
                    &self.frame,
                    &self.page,
                    if equipping {
                        "equip-timeout"
                    } else {
                        "timeout"
                    },
                );
                if equipping {
                    bail!(
                        "target equipment marker was not confirmed within {:.3}s; no retry or return was sent",
                        started.elapsed().as_secs_f64()
                    );
                }
                bail!(
                    "{} navigation did not finish within {:.3}s; retried={retried}",
                    action.name(),
                    started.elapsed().as_secs_f64()
                );
            }
            if !responded
                && !retried
                && self.page.ready()
                && started.elapsed() >= NAVIGATION_RETRY_AFTER
                && let InputAction::Key(
                    key @ (Key::Up | Key::Down | Key::Left | Key::Right | Key::Z | Key::C),
                ) = action
            {
                self.check_active()?;
                let retry_started = Instant::now();
                let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
                self.automation.tap_key(key, INPUT_HOLD_MS)?;
                self.last_release = Some(Instant::now());
                retried = true;
                self.diagnostics.record(|| {
                    json!({"event":"input_retry", "action":key.name(), "elapsed_ms":elapsed_ms,
                    "input_ms":retry_started.elapsed().as_secs_f64()*1000.0})
                });
                tracing::info!(
                    action = key.name(),
                    elapsed_s = elapsed_ms / 1000.0,
                    "retrying unresponsive equipment navigation once"
                );
            }
        }
    }

    pub(super) fn equip_and_return(
        &mut self,
        target: &EquipmentItem,
    ) -> Result<Option<TextSample>> {
        self.wait_for_release()?;
        self.confirm_target(target, false)?;
        if self.page.focus.is_some_and(|i| self.page.slots[i].equipped) {
            self.diagnostics.record(
                || json!({"event":"already_equipped", "page":diagnostics::describe(&self.page)}),
            );
            tracing::info!("target already equipped; skipping Space");
        } else {
            let before = self.page.clone();
            let action = InputAction::Key(Key::Space);
            let (started, input_ms) = self.press(action, None)?;
            self.wait_for_input(action, &before, started, input_ms)?;
            self.wait_for_release()?;
        }
        self.sync_category()?;
        self.confirm_target(target, true)?;
        let corrected_category = self
            .page
            .category
            .as_ref()
            .filter(|category| {
                category.has_text()
                    && target
                        .category
                        .as_ref()
                        .is_none_or(|saved| category.cosine(saved) < NAME_MATCH_THRESHOLD)
            })
            .cloned();
        self.return_to_entry()?;
        Ok(corrected_category)
    }

    fn confirm_target(&mut self, target: &EquipmentItem, require_equipped: bool) -> Result<()> {
        let matches =
            self.page.ready() && self.page.name.cosine(&target.name) >= NAME_MATCH_THRESHOLD;
        let equipped = self.page.focus.is_some_and(|i| self.page.slots[i].equipped);
        if !matches || require_equipped && !equipped {
            self.diagnostics
                .save(&self.frame, &self.page, "equip-target-lost");
            bail!("equipment target or its confirmed marker changed; input was not sent");
        }
        Ok(())
    }
}
