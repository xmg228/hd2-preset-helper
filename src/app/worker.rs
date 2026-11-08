use std::collections::BTreeMap;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use anyhow::{Context, Result};
use tracing::{debug, error, info_span, warn};

use super::GameAction;
use super::failure::save_failure_report;
#[cfg(feature = "diagnostics")]
use super::failure::save_scan_diagnostics;
use crate::app_events::{AppEvent, AppEventSink};
use crate::app_paths::AppPaths;
use crate::capture::CaptureSessionManager;
use crate::item_cache::{
    CachedChoices, ItemCache,
    preview::{ItemCandidates, item_candidates},
};
use crate::preset::{
    PresetScope,
    preview::{self, PresetPreview},
};
use crate::preset_action::{PresetActionContext, PresetActionOutcome, execute_preset};
use crate::scan::{self, ScanProgress, ScanSummary};
use crate::vision::RecognizerRuntime;
use crate::window::{WindowIdentity, WindowTarget};

pub(super) enum Work {
    Prepare(WindowIdentity),
    Discard,
    LoadPreviews,
    CheckPreset {
        preset: String,
        scope: PresetScope,
    },
    LoadItemCandidates,
    SaveItemChoices {
        preset: String,
        choices: Box<CachedChoices>,
    },
    Run {
        action: GameAction,
        target: WindowIdentity,
    },
    Shutdown,
}

pub(super) enum WorkerEvent {
    Progress(AppEvent),
    ScanProgress(ScanProgress),
    Finished(WorkCompletion),
    PreviewsLoaded(std::result::Result<BTreeMap<String, PresetPreview>, String>),
    PresetChecked(std::result::Result<usize, String>),
    ItemCandidatesLoaded(std::result::Result<ItemCandidates, String>),
    ItemChoicesSaved {
        preset: String,
        result: std::result::Result<(), String>,
    },
}

pub(super) enum WorkCompletion {
    Preset(std::result::Result<PresetActionOutcome, String>),
    Scan(std::result::Result<ScanSummary, String>),
    ScanCancelled,
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
            .name("action".into())
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
                let cache_path = paths.presets.with_file_name("item-cache.json");
                let mut item_cache = ItemCache::load(&cache_path);
                while let Ok(mut command) = receiver.recv() {
                    // Only the latest pending preparation hint matters. Other
                    // commands are boundaries and are never coalesced away.
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
                            let previews = load_previews(&paths.presets, &item_cache)
                                .map_err(|error| format!("{error:#}"));
                            if event_tx.send(WorkerEvent::PreviewsLoaded(previews)).is_err() { break; }
                        }
                        Work::LoadItemCandidates => {
                            let result = item_candidates(&item_cache)
                                .map_err(|error| format!("{error:#}"));
                            if event_tx.send(WorkerEvent::ItemCandidatesLoaded(result)).is_err() { break; }
                        }
                        Work::CheckPreset { preset, scope } => {
                            let result = crate::preset::uncached_type_count(&paths.presets, &preset, scope, &item_cache)
                                .map_err(|error| format!("{error:#}"));
                            if event_tx.send(WorkerEvent::PresetChecked(result)).is_err() { break; }
                        }
                        Work::SaveItemChoices { preset, choices } => {
                            let result = crate::preset::save_cached(
                                &paths.presets, &preset, &item_cache, &choices,
                            ).map_err(|error| format!("{error:#}"));
                            if let Err(error) = &result {
                                warn!(%preset, %error, "could not save selected items");
                            }
                            if event_tx.send(WorkerEvent::ItemChoicesSaved { preset, result }).is_err() { break; }
                        }
                        Work::Shutdown => break,
                        Work::Run { action, target } => {
                            let span = info_span!("game_action", ?action);
                            let _guard = span.enter();
                            let evidence = crate::evidence::OperationEvidence::new();
                            let start = Instant::now();
                            let completion = match &action {
                                GameAction::Preset { preset, command, options } => {
                                    let mut context = PresetActionContext {
                                        presets: &paths.presets,
                                        options: *options,
                                        events: &sink,
                                        item_cache: &mut item_cache,
                                        evidence: &evidence,
                                    };
                                    let outcome = execute_preset(&runtime, &mut context, preset, *command, &mut capture_session, target);
                                    if let Err(error) = &outcome {
                                        let message = format!("{error:#}");
                                        error!(elapsed_s = start.elapsed().as_secs_f64(), error = %message, "preset action failed");
                                        sink.emit(AppEvent::PresetFailed { preset: preset.clone(), error: message });
                                        save_failure_report(&paths.last_failure, &action, error, Some(&evidence));
                                    }
                                    if let Err(error) = item_cache.save_if_changed(&cache_path) {
                                        warn!(error = %format!("{error:#}"), "could not save learned item cache");
                                    }
                                    WorkCompletion::Preset(outcome.map_err(|error| format!("{error:#}")))
                                }
                                GameAction::Scan { scope, cancel } => {
                                    let progress = event_tx.clone();
                                    #[cfg(feature = "diagnostics")]
                                    let mut diagnostics = crate::loadout::equipment::ScanDiagnostics::default();
                                    let result = scan::execute(
                                        &runtime, &mut item_cache, &cache_path, &mut capture_session,
                                        target, *scope, cancel,
                                        move |event| { let _ = progress.send(WorkerEvent::ScanProgress(event)); },
                                        &evidence,
                                        #[cfg(feature = "diagnostics")]
                                        &mut diagnostics,
                                    );
                                    let cancelled = result.is_err() && cancel.is_cancelled();
                                    if !cancelled && let Err(error) = &result {
                                        error!(elapsed_s = start.elapsed().as_secs_f64(), error = %format!("{error:#}"), "item scan failed");
                                        save_failure_report(&paths.last_failure, &action, error, Some(&evidence));
                                    }
                                    #[cfg(feature = "diagnostics")]
                                    if !diagnostics.is_empty() {
                                        save_scan_diagnostics(
                                            &paths.last_failure.with_file_name("last_scan.zip"),
                                            &action, result.as_ref().err(), &evidence, &diagnostics,
                                        );
                                    }
                                    if cancelled {
                                        WorkCompletion::ScanCancelled
                                    } else {
                                        WorkCompletion::Scan(result.map_err(|error| format!("{error:#}")))
                                    }
                                }
                            };
                            capture_session.finish_action();
                            // Completion follows all progress on the same FIFO channel.
                            if event_tx.send(WorkerEvent::Finished(completion)).is_err() { break; }
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

fn load_previews(path: &Path, cache: &ItemCache) -> Result<BTreeMap<String, PresetPreview>> {
    Ok(preview::load(path)?
        .map(|(name, mut loaded)| {
            if !loaded.equipment_names.is_empty() {
                loaded.preview.equipment_cache_matches = Some({
                    loaded
                        .equipment_names
                        .iter()
                        .filter(|(kind, sample)| cache.has_strong_match(*kind, sample.signature()))
                        .count()
                });
            }
            (name, loaded.preview)
        })
        .collect())
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
