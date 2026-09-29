use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use anyhow::{Context, Result};
use tracing::{debug, error, warn};

use crate::app_events::{AppEvent, AppEventSink};
use crate::app_paths::AppPaths;
use crate::capture::CaptureSessionManager;
use crate::game_settings::read_color_settings;
use crate::image_rect::ImageRect;
use crate::preset_action::{
    PresetActionContext, PresetActionOptions, PresetActionOutcome, PresetCommand, execute_preset,
};
use crate::vision::RecognizerRuntime;
use crate::window::{WindowIdentity, WindowTarget};

pub(super) enum Work {
    Prepare(WindowIdentity),
    Discard,
    LoadPreviews,
    Run {
        preset: String,
        command: PresetCommand,
        settings: PresetActionOptions,
        target: WindowIdentity,
    },
    Shutdown,
}

pub(super) enum WorkerEvent {
    Progress(AppEvent),
    Finished {
        saved: bool,
    },
    PreviewsLoaded(
        std::result::Result<
            std::collections::BTreeMap<String, crate::preset::preview::PresetPreview>,
            String,
        >,
    ),
}

pub(super) struct ActionWorker {
    commands: Sender<Work>,
    pub events: Receiver<WorkerEvent>,
    thread: Option<JoinHandle<()>>,
}

impl ActionWorker {
    pub fn start(paths: AppPaths) -> Result<Self> {
        let (commands, receiver) = mpsc::channel();
        let (event_tx, events) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("hd2-preset-helper-action".into())
            .spawn(move || {
                let runtime = match RecognizerRuntime::load() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                if ready_tx.send(Ok(())).is_err() { return; }
                let progress = event_tx.clone();
                let sink = AppEventSink::new(move |event| {
                    let _ = progress.send(WorkerEvent::Progress(event));
                });
                let mut capture_session = CaptureSessionManager::new();
                let cache_path = paths.presets.with_file_name("equipment-cache.json");
                let mut equipment_cache = crate::loadout::equipment::EquipmentCache::load(&cache_path);
                while let Ok(mut command) = receiver.recv() {
                    // Only the latest pending preparation hint matters. A Run or
                    // Shutdown command is a boundary and is never coalesced away.
                    while matches!(command, Work::Prepare(_) | Work::Discard) {
                        match receiver.try_recv() {
                            Ok(next) => command = next,
                            Err(_) => break,
                        }
                    }
                    match command {
                        Work::Prepare(target) => prepare_capture(&mut capture_session, target),
                        Work::Discard => capture_session.cancel_prepare(),
                        Work::LoadPreviews => {
                            let previews = crate::preset::preview::load(&paths.presets)
                                .map_err(|error| format!("{error:#}"));
                            if event_tx.send(WorkerEvent::PreviewsLoaded(previews)).is_err() { break; }
                        }
                        Work::Shutdown => break,
                        Work::Run { preset, command, settings, target } => {
                            let mut context = PresetActionContext {
                                presets: &paths.presets,
                                options: settings,
                                events: &sink,
                                equipment_cache: &mut equipment_cache,
                            };
                            let start = Instant::now();
                            let outcome = execute_preset(&runtime, &mut context, &preset, command, &mut capture_session, target);
                            if let Err(error) = &outcome {
                                save_last_failure(&mut capture_session, &paths.last_failure, error);
                            }
                            capture_session.finish_action();
                            equipment_cache.save_if_changed(&cache_path);
                            let saved = matches!(&outcome, Ok(PresetActionOutcome::Saved));
                            if let Err(error) = outcome {
                                let error = format!("{error:#}");
                                error!(preset = %preset, elapsed = ?start.elapsed(), %error, "preset action failed");
                                sink.emit(AppEvent::PresetFailed { preset, error });
                            }
                            // Completion follows all progress on the same FIFO channel.
                            if event_tx.send(WorkerEvent::Finished { saved }).is_err() { break; }
                        }
                    }
                }
            })
            .context("failed to start action worker")?;
        let worker = Self {
            commands,
            events,
            thread: Some(thread),
        };
        ready_rx
            .recv()
            .context("action worker stopped during initialization")??;
        Ok(worker)
    }

    pub fn send(&self, command: Work) -> Result<()> {
        self.commands.send(command).context("action worker stopped")
    }

    pub fn shutdown(&mut self) {
        let _ = self.commands.send(Work::Shutdown);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("action worker panicked");
        }
    }
}

impl Drop for ActionWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn prepare_capture(capture_session: &mut CaptureSessionManager, identity: WindowIdentity) {
    // Opening the panel can change foreground before the worker consumes this hint.
    let result =
        WindowTarget::from_identity(identity).and_then(|target| capture_session.prepare(&target));

    if let Err(error) = result {
        debug!(
            error = %format!("{error:#}"),
            "capture preparation skipped"
        );
    }
}

fn save_last_failure(
    capture_session: &mut CaptureSessionManager,
    directory: &Path,
    action_error: &anyhow::Error,
) {
    let result = (|| -> Result<Option<PathBuf>> {
        let Some(capture) = capture_session.active_capture() else {
            return Ok(None);
        };
        // Include either loadout page; the equipment name is outside the Stratagem ROI.
        let (w, h) = capture.output_size();
        let mut region = capture.region(ImageRect { x: 0, y: 0, w, h }, read_color_settings()?)?;
        let image = region.capture()?;
        fs::create_dir_all(directory).with_context(|| {
            format!(
                "failed to create failure debug directory {}",
                directory.display()
            )
        })?;
        image
            .save(directory.join("frame.png"))
            .context("failed to save failure debug frame")?;
        fs::write(directory.join("error.txt"), format!("{action_error:#}"))
            .context("failed to save failure debug error")?;
        Ok(Some(directory.to_path_buf()))
    })();

    match result {
        Ok(Some(directory)) => warn!(
            path = %directory.display(),
            "saved last preset failure diagnostics"
        ),
        Ok(None) => debug!("preset failure has no active capture frame to save"),
        Err(error) => warn!(
            error = %format!("{error:#}"),
            "failed to save preset failure diagnostics"
        ),
    }
}
