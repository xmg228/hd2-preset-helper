//! Background update jobs and application handoff, separate from automation.
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use tracing::{debug, info, warn};

use super::{ActionState, AppController};
use crate::ui::UpdatePhase;
use crate::update::{self, AvailableUpdate};

enum UpdateEvent {
    Checked(Result<Option<AvailableUpdate>>),
    Installing,
    Installed(Result<PathBuf>),
}

pub(super) struct Updates {
    pub automatic: bool,
    available: Option<AvailableUpdate>,
    next_check: Instant,
    events: Option<Receiver<UpdateEvent>>,
    installer: Option<thread::JoinHandle<()>>,
}

impl Updates {
    pub fn new(automatic: bool) -> Self {
        Self {
            automatic,
            available: None,
            next_check: Instant::now(),
            events: None,
            installer: None,
        }
    }

    pub fn installing(&self) -> bool {
        self.installer.is_some()
    }

    fn finish(&mut self) {
        self.events = None;
        if let Some(installer) = self.installer.take() {
            let _ = installer.join();
        }
    }
}

impl Drop for Updates {
    fn drop(&mut self) {
        // Also protect replacement if the UI event loop exits with an error.
        self.finish();
    }
}

impl AppController {
    pub(super) fn initialize_updates(&mut self) {
        self.ui
            .set_update_install_allowed(update::installation_supported());
        self.check_updates_automatically();
    }

    fn check_updates_automatically(&mut self) {
        if self.updates.automatic
            && update::automatic_checks_supported()
            && Instant::now() >= self.updates.next_check
        {
            self.check_updates();
        }
    }

    pub(super) fn check_updates(&mut self) {
        if self.exiting || self.updates.events.is_some() {
            return;
        }
        self.updates.next_check = Instant::now() + Duration::from_secs(24 * 60 * 60);
        let (send, events) = mpsc::channel();
        match thread::Builder::new().name("update".into()).spawn(move || {
            let _ = send.send(UpdateEvent::Checked(update::check()));
        }) {
            Ok(_) => {
                self.updates.events = Some(events);
                self.present_update(UpdatePhase::Checking, "");
            }
            Err(error) => self.update_failed(&error),
        }
    }

    pub(super) fn install_update(&mut self) -> Result<()> {
        if self.exiting || self.updates.events.is_some() || !matches!(self.state, ActionState::Idle)
        {
            return Ok(());
        }
        let Some(available) = self.updates.available.clone() else {
            return Ok(());
        };
        if !update::installation_supported() {
            return Ok(());
        }
        // Capture the original path before self-replace renames the loaded image.
        let executable = std::env::current_exe()?;
        self.hotkeys.set_enabled(false)?;
        self.panel_hotkey.set_enabled(false)?;
        let (send, events) = mpsc::channel();
        let installer = thread::Builder::new()
            .name("update".into())
            .spawn(move || {
                info!(version = %available.version, "update installation started");
                let progress = send.clone();
                let result = update::install(&available, &executable, move || {
                    let _ = progress.send(UpdateEvent::Installing);
                })
                .map(|()| executable);
                let _ = send.send(UpdateEvent::Installed(result));
            })?;
        self.updates.installer = Some(installer);
        self.updates.events = Some(events);
        self.present_update(UpdatePhase::Downloading, "");
        Ok(())
    }

    pub(super) fn view_update_notes(&self) {
        if let Some(available) = &self.updates.available {
            self.open_link(&available.notes_url());
        }
    }

    pub(super) fn poll_updates(&mut self) -> Result<Option<PathBuf>> {
        loop {
            let event = match self.updates.events.as_ref().map(Receiver::try_recv) {
                Some(Ok(event)) => event,
                Some(Err(TryRecvError::Disconnected)) => {
                    self.updates.finish();
                    self.update_failed("the update worker stopped unexpectedly");
                    break;
                }
                _ => break,
            };
            match event {
                UpdateEvent::Checked(result) => {
                    self.updates.finish();
                    match result {
                        Ok(available) => {
                            self.updates.available = available;
                            let phase = if let Some(update) = &self.updates.available {
                                info!(version = %update.version, "update available");
                                UpdatePhase::Available
                            } else {
                                debug!("no newer stable release available");
                                UpdatePhase::UpToDate
                            };
                            self.present_update(phase, "");
                        }
                        Err(error) => self.update_failed(&error),
                    }
                }
                UpdateEvent::Installing => self.present_update(UpdatePhase::Installing, ""),
                UpdateEvent::Installed(result) => {
                    self.updates.finish();
                    match result {
                        Ok(executable) => {
                            info!(restart = !self.exiting, "update installed");
                            if !self.exiting {
                                self.present_update(UpdatePhase::Restarting, "");
                                self.request_exit()?;
                                return Ok(Some(executable));
                            }
                        }
                        Err(error) => self.update_failed(&error),
                    }
                }
            }
        }
        self.check_updates_automatically();
        Ok(None)
    }

    fn present_update(&self, phase: UpdatePhase, error: &str) {
        self.ui.set_update_state(
            phase,
            self.updates
                .available
                .as_ref()
                .map_or("", |update| &update.version),
            error,
        );
    }

    pub(super) fn update_failed(&self, error: impl std::fmt::Display) {
        // Checking/installing errors are not preset failures and have no image report.
        let error = format!("{error:#}");
        warn!(error, "update request failed");
        self.present_update(UpdatePhase::Failed, &error);
    }
}
