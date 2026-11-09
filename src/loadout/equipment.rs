//! Equipment capture/application, independent of the UI and preset persistence.
mod cache;
mod evidence;
mod navigation;
mod row;
mod scan;
mod search;

use crate::item_cache::{EquipmentItem, EquipmentSet};
use evidence::EquipmentEvidence;

use crate::{
    automation::{AutomationSession, Cancellation},
    capture::CaptureSource,
    game_settings::GameColorSettings,
    game_window,
    input::Key,
    item::{EquipmentKind, EquipmentSelection},
    item_cache::ItemCache,
    vision::equipment::{
        CapturedEquipmentItem, CapturedEquipmentSet, EquipmentObservation, EquipmentObserver,
    },
    vision::text::TextSample,
    window::WindowTarget,
};
use anyhow::{Context, Result, bail, ensure};
use search::{Next, Search};
use std::time::{Duration, Instant};

// One threshold for smoothed name identities and saved-target acceptance.
const NAME_MATCH_THRESHOLD: f32 = 0.96;
const CATEGORY_MATCH_THRESHOLD: f32 = 0.80;
const NAME_CONTINUITY_THRESHOLD: f32 = 0.80;

#[derive(Clone, Copy, Debug)]
pub(crate) enum InputAction {
    Key(Key),
    ClickColumn(usize),
}

#[derive(Clone, Copy)]
pub(crate) struct Navigation {
    action: InputAction,
    retried: bool,
    /// Large opposite-direction focus jump after a settled vertical input.
    wrapped: bool,
}

pub(super) enum NavigationWait {
    Responsive,
    Settled,
}

impl InputAction {
    pub fn name(self) -> &'static str {
        match self {
            Self::Key(key) => key.name(),
            Self::ClickColumn(_) => "Click",
        }
    }

    pub fn column(self) -> Option<usize> {
        match self {
            Self::ClickColumn(col) => Some(col),
            Self::Key(_) => None,
        }
    }
}

const NAVIGATION_TIMEOUT: Duration = Duration::from_millis(1500);
const NAVIGATION_RETRY_AFTER: Duration = Duration::from_millis(300);
const EQUIP_TIMEOUT: Duration = Duration::from_millis(1500);
const INPUT_HOLD_MS: u64 = 45;
const INPUT_RELEASE_INTERVAL: Duration = Duration::from_millis(20);
const PAGE_STABLE_INTERVAL: Duration = Duration::from_millis(40);

pub(crate) fn with_session<T>(
    capture: &mut CaptureSource,
    target: WindowTarget,
    colors: GameColorSettings,
    evidence: &crate::evidence::OperationEvidence,
    cancellation: Cancellation,
    operation: impl FnOnce(&mut Session<'_>) -> Result<T>,
) -> Result<T> {
    let (width, height) = capture.output_size();
    let (observer, resolved) = EquipmentObserver::resolve(width, height, EquipmentKind::Helmet)?;
    let region = capture.region(resolved.rect, colors)?;
    let automation = AutomationSession::new(region, target, evidence, cancellation)?;
    let mut session = Session::new(automation, observer, (width, height))?;
    ensure!(
        session.at_entry(),
        "open the six-card equipment page before using equipment actions"
    );
    operation(&mut session)
}

pub(crate) struct Session<'a> {
    automation: AutomationSession<'a>,
    observer: EquipmentObserver,
    frame: crate::capture::CapturedFrame,
    page: EquipmentObservation,
    evidence: EquipmentEvidence<'a>,
    client_size: (u32, u32),
    last_release: Option<Instant>,
}

pub(crate) enum ApplyProgress<'a> {
    Selecting(EquipmentKind),
    Caching {
        kind: EquipmentKind,
        category: &'a TextSample,
        items: usize,
    },
    Selected {
        kind: EquipmentKind,
        corrected_category: Option<TextSample>,
    },
}

impl<'a> Session<'a> {
    fn new(
        mut automation: AutomationSession<'a>,
        observer: EquipmentObserver,
        client_size: (u32, u32),
    ) -> Result<Self> {
        let frame = automation.capture()?;
        let page = observer.observe(&frame);
        let evidence = EquipmentEvidence::new(automation.evidence());

        Ok(Self {
            automation,
            observer,
            frame,
            page,
            evidence,
            client_size,
            last_release: None,
        })
    }

    fn check_active(&self) -> Result<()> {
        self.automation.check_cancelled()?;
        ensure!(
            game_window::is_game_foreground(),
            "game lost foreground; equipment operation stopped"
        );
        Ok(())
    }

    fn refresh(&mut self) -> Result<()> {
        self.check_active()?;
        let started = Instant::now();
        self.frame = self.automation.capture()?;
        let capture_ms = started.elapsed().as_secs_f64() * 1000.0;
        let started = Instant::now();
        self.page = self.observer.observe(&self.frame);
        let observe_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.evidence
            .observe(&self.frame, &self.page, capture_ms, observe_ms);
        Ok(())
    }

    fn at_entry(&self) -> bool {
        self.observer.entry(&self.frame).confirmed()
    }

    fn sample_current(&mut self) -> Result<CapturedEquipmentItem> {
        if !self.page.ready() {
            self.evidence
                .checkpoint(&self.frame, &self.page, "not-ready");
            bail!("equipment focus or name is not readable; operation stopped");
        }
        self.sync_category()?;
        self.evidence
            .checkpoint(&self.frame, &self.page, "remembered");
        Ok(CapturedEquipmentItem {
            name: self.page.name.clone(),
            category: Some(self.page.category.clone()),
            client_size: self.client_size,
        })
    }

    /// Publish only after every requested sample and return succeeds.
    pub fn sample(&mut self, selection: EquipmentSelection) -> Result<CapturedEquipmentSet> {
        let mut items = std::array::from_fn(|_| None);

        for (index, kind) in selection.iter().enumerate() {
            let span = tracing::info_span!("equipment_item", ?kind);
            let _guard = span.enter();
            let item = (|| -> Result<CapturedEquipmentItem> {
                self.check_active()?;

                self.open(kind)?;
                let item = self.sample_current()?;

                self.return_to_entry()?;
                self.check_active()?;
                Ok(item)
            })()
            .with_context(|| {
                format!("failed to remember {kind:?}; previous equipment records retained")
            })?;
            items[kind.index()] = Some(item);
            tracing::debug!(completed = index + 1, "equipped item sampled");
        }
        self.check_active()?;

        Ok(items)
    }

    /// Missing slots are outside this operation's scope.
    pub fn apply(
        &mut self,
        items: &EquipmentSet,
        cache: &mut ItemCache,
        mut progress: impl FnMut(ApplyProgress<'_>),
    ) -> Result<()> {
        ensure!(
            items.iter().any(Option::is_some),
            "no saved equipment in the selected range"
        );
        for (kind, item) in EquipmentKind::ALL
            .into_iter()
            .filter_map(|kind| items[kind.index()].as_ref().map(|item| (kind, item)))
        {
            let span = tracing::info_span!("equipment_item", ?kind);
            let _guard = span.enter();
            progress(ApplyProgress::Selecting(kind));

            self.open(kind)
                .with_context(|| format!("failed to open {kind:?} for application"))?;
            let corrected_category = self
                .apply_item(kind, item, cache, &mut progress)
                .with_context(|| format!("failed to apply {kind:?}"))?;
            progress(ApplyProgress::Selected {
                kind,
                corrected_category,
            });

            tracing::info!("equipment item applied");
        }

        tracing::debug!("equipment application completed");
        Ok(())
    }

    fn apply_item(
        &mut self,
        kind: EquipmentKind,
        item: &EquipmentItem,
        cache: &mut ItemCache,
        progress: &mut impl FnMut(ApplyProgress<'_>),
    ) -> Result<Option<TextSample>> {
        ensure!(
            self.find_item(kind, item, cache, progress)?,
            "saved equipment was not found in the list"
        );
        self.equip_and_return(item)
    }

    fn find_item(
        &mut self,
        kind: EquipmentKind,
        target: &EquipmentItem,
        cache: &mut ItemCache,
        progress: &mut impl FnMut(ApplyProgress<'_>),
    ) -> Result<bool> {
        self.evidence.start_search(target);
        let started = Instant::now();
        let result = (|| {
            let mut rebuilt = false;
            let mut position = None;
            loop {
                if let Some(catalog) = &cache.equipment[kind.index()] {
                    let mut search = Search::new(kind, target, catalog, position);
                    let mut navigation = None;
                    loop {
                        self.sync_category()?;
                        let evaluate_started = Instant::now();
                        let (visit, next) = search
                            .advance(&self.page, navigation.take())
                            .inspect_err(|_| {
                                self.evidence
                                    .checkpoint(&self.frame, &self.page, "search-error")
                            })?;
                        self.evidence.search_visit(
                            &self.frame,
                            &visit,
                            &self.page,
                            evaluate_started.elapsed().as_secs_f64() * 1000.0,
                        );
                        match next {
                            Next::Observe => self.refresh()?,
                            Next::Input(action) => {
                                navigation =
                                    Some(self.navigate(action, NavigationWait::Responsive)?);
                            }
                            Next::Found => {
                                self.evidence.checkpoint(&self.frame, &self.page, "found");
                                return Ok(true);
                            }
                            Next::Rebuild => break,
                        }
                    }
                }
                if rebuilt {
                    self.evidence
                        .checkpoint(&self.frame, &self.page, "not-found-after-scan");
                    return Ok(false);
                }
                tracing::info!(?kind, "building complete equipment catalog");
                progress(ApplyProgress::Caching {
                    kind,
                    category: &self.page.category,
                    items: 0,
                });
                let collection = self.collect(&mut |category, items| {
                    progress(ApplyProgress::Caching {
                        kind,
                        category,
                        items,
                    })
                })?;
                position = Some(collection.position);
                cache.set_equipment(kind, collection.catalog);
                rebuilt = true;
                progress(ApplyProgress::Selecting(kind));
            }
        })();
        self.evidence
            .finish_search(&result, started.elapsed().as_secs_f64() * 1000.0);
        result
    }
}
