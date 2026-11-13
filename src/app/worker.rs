use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use anyhow::{Context, Result};
use tracing::{debug, error, info_span, warn};

use super::GameAction;
use super::failure::save_failure_report;
use crate::app_events::{AppEvent, AppEventSink};
use crate::app_paths::AppPaths;
use crate::capture::CaptureSessionManager;
use crate::item_cache::{
    CachedChoices, ItemCache,
    preview::{ItemCandidates, item_candidates},
};
use crate::preset::{
    MigrationGroupSummary, MigrationPair, MigrationReport, MigrationSelection, PresetMigration,
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
    InspectMigration(scan::ScanScope),
    LoadMigrationPreview {
        migration: Arc<PresetMigration>,
        scanned: Option<Arc<ItemCache>>,
        selection: MigrationSelection,
    },
    LoadItemCandidates,
    SaveItemChoices {
        preset: String,
        choices: Box<CachedChoices>,
    },
    CommitMigration {
        draft: Box<MigrationDraft>,
        migrate_presets: bool,
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
    MigrationInspected(std::result::Result<MigrationInspection, String>),
    MigrationPreviewLoaded {
        selection: MigrationSelection,
        result: std::result::Result<Vec<MigrationPair>, String>,
    },
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
    MigrationReady(Box<MigrationDraft>),
}

pub(super) struct MigrationDraft {
    scanned: Arc<ItemCache>,
    migration: Arc<PresetMigration>,
    groups: Vec<MigrationGroupSummary>,
    summary: ScanSummary,
}

pub(super) struct MigrationInspection {
    pub migration: Arc<PresetMigration>,
    pub groups: Vec<MigrationGroupSummary>,
}

impl MigrationDraft {
    pub fn report(&self) -> &MigrationReport {
        self.migration.report()
    }

    pub fn groups(&self) -> &[MigrationGroupSummary] {
        &self.groups
    }

    pub fn preview_work(&self, selection: MigrationSelection) -> Work {
        Work::LoadMigrationPreview {
            migration: self.migration.clone(),
            scanned: Some(self.scanned.clone()),
            selection,
        }
    }
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
                    // Queued work can supersede preparation, but Discard must
                    // release capture before any later command runs.
                    while matches!(command, Work::Prepare(_)) {
                        match receiver.try_recv() {
                            Ok(next) => command = next,
                            Err(_) => break,
                        }
                    }
                    match command {
                        Work::Prepare(target) => prepare_capture(&mut capture_session, target),
                        Work::Discard => capture_session.cancel_prepare(),
                        Work::LoadPreviews => {
                            let previews = preview::load(&paths.presets)
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
                        Work::InspectMigration(scope) => {
                            let result = PresetMigration::inspect(&paths.presets, &item_cache, &scope.kinds(&item_cache))
                                .map(|migration| MigrationInspection {
                                    groups: migration.groups(&item_cache, None),
                                    migration: Arc::new(migration),
                                }).map_err(|error| format!("{error:#}"));
                            if let Err(error) = &result {
                                warn!(%error, "could not check presets for migration");
                            }
                            if event_tx.send(WorkerEvent::MigrationInspected(result)).is_err() { break; }
                        }
                        Work::LoadMigrationPreview { migration, scanned, selection } => {
                            let result = migration.preview(&paths.presets, &item_cache, scanned.as_deref(), &selection)
                                .map_err(|error| format!("{error:#}"));
                            if event_tx.send(WorkerEvent::MigrationPreviewLoaded { selection, result }).is_err() { break; }
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
                        Work::CommitMigration { draft, migrate_presets } => {
                            let result = (|| -> Result<ScanSummary> {
                                let migration = if migrate_presets {
                                    Some(Arc::try_unwrap(draft.migration).map_err(|_| anyhow::anyhow!("migration previews are still in use"))?)
                                } else {
                                    None
                                };
                                let scanned = Arc::try_unwrap(draft.scanned)
                                    .map_err(|_| anyhow::anyhow!("scanned item previews are still in use"))?;
                                item_cache.commit_scanned(&cache_path, scanned)
                                    .context("could not update the scanned item cache")?;
                                let mut summary = draft.summary;
                                if let Some(migration) = migration {
                                    summary.migrated_items = Some(
                                        migration.apply(&paths.presets, &item_cache)
                                            .context("item cache updated; preset migration did not complete")?,
                                    );
                                }
                                Ok(summary)
                            })().map_err(|error| format!("{error:#}"));
                            if let Err(error) = &result {
                                error!(%error, "could not finish preset migration");
                            }
                            if event_tx.send(WorkerEvent::Finished(WorkCompletion::Scan(result))).is_err() { break; }
                        }
                        Work::Shutdown => break,
                        Work::Run { action, target } => {
                            let span = info_span!("game_action", ?action);
                            let _guard = span.enter();
                            let evidence = crate::evidence::OperationEvidence::new();
                            let start = Instant::now();
                            let completion = match &action {
                                GameAction::Preset { preset, command, options, cancel } => {
                                    let mut context = PresetActionContext {
                                        presets: &paths.presets,
                                        options: *options,
                                        events: &sink,
                                        item_cache: &mut item_cache,
                                        evidence: &evidence,
                                        cancel,
                                    };
                                    let outcome = execute_preset(&runtime, &mut context, preset, *command, &mut capture_session, target);
                                    let outcome = match outcome {
                                        Ok((outcome, completion)) => {
                                            tracing::info!(elapsed_s = start.elapsed().as_secs_f64(), ?completion, "preset action completed");
                                            sink.emit(AppEvent::PresetDone { preset: preset.clone(), completion });
                                            Ok(outcome)
                                        }
                                        Err(error) => {
                                            let message = format!("{error:#}");
                                            if cancel.is_cancelled() {
                                                debug!("preset action cancelled");
                                                sink.emit(AppEvent::PresetCancelled { preset: preset.clone(), reason: message.clone() });
                                            } else {
                                                error!(elapsed_s = start.elapsed().as_secs_f64(), error = %message, "preset action failed");
                                                sink.emit(AppEvent::PresetFailed { preset: preset.clone(), error: message.clone() });
                                                save_failure_report(&paths.last_failure, &action, &error, Some(&evidence));
                                            }
                                            Err(message)
                                        }
                                    };
                                    if let Err(error) = item_cache.save_if_changed(&cache_path) {
                                        warn!(error = %format!("{error:#}"), "could not save learned item cache");
                                    }
                                    WorkCompletion::Preset(outcome)
                                }
                                GameAction::Scan { scope, migration, cancel } => {
                                    let progress = event_tx.clone();
                                    let kinds = scope.kinds(&item_cache);
                                    let mut scanned = ItemCache::default();
                                    let result = scan::execute(
                                        &runtime, &mut capture_session,
                                        target, &kinds, cancel,
                                        move |event| { let _ = progress.send(WorkerEvent::ScanProgress(event)); },
                                        &evidence,
                                        |list| {
                                            if migration.is_some() {
                                                list.store(&mut scanned);
                                                Ok(())
                                            } else {
                                                list.commit(&mut item_cache, &cache_path)
                                            }
                                        },
                                    ).and_then(|summary| {
                                        if let Some(migration) = migration {
                                            let migration = migration.as_ref().clone().check(&item_cache, &scanned);
                                            cancel.check()?;
                                            let groups = migration.groups(&item_cache, Some(&scanned));
                                            Ok(WorkCompletion::MigrationReady(Box::new(MigrationDraft {
                                                scanned: Arc::new(scanned), migration: Arc::new(migration), groups, summary,
                                            })))
                                        } else {
                                            Ok(WorkCompletion::Scan(Ok(summary)))
                                        }
                                    }).map_err(|error| {
                                        if migration.is_some() {
                                            error.context("migration scan failed; cache and presets were not changed")
                                        } else {
                                            error
                                        }
                                    });
                                    let cancelled = result.is_err() && cancel.is_cancelled();
                                    if !cancelled && let Err(error) = &result {
                                        error!(elapsed_s = start.elapsed().as_secs_f64(), error = %format!("{error:#}"), "item scan failed");
                                        save_failure_report(&paths.last_failure, &action, error, Some(&evidence));
                                    }
                                    if cancelled {
                                        WorkCompletion::ScanCancelled
                                    } else {
                                        result.unwrap_or_else(|error| WorkCompletion::Scan(Err(format!("{error:#}"))))
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
