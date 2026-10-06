//! Equipment capture/application, independent of the UI and preset persistence.
mod cache;
pub(crate) mod diagnostics;
mod navigation;
mod search;
mod survey;

pub(crate) use crate::preset::equipment::{EquipmentItem, EquipmentSet};
pub(crate) use cache::EquipmentCache;
pub(crate) use diagnostics::Diagnostics;

use crate::{
    automation::AutomationSession,
    game_window,
    input::Key,
    item::EquipmentKind,
    vision::equipment::{EquipmentObservation, EquipmentObserver},
    vision::text::TextSample,
};
use anyhow::{Context, Result, bail, ensure};
use image::RgbaImage;
use search::{Next, Search};
use serde_json::json;
use std::time::{Duration, Instant};

// Candidate/continuity tolerance is not enough to merge identities or skip a search.
const NAME_MATCH_THRESHOLD: f32 = 0.80;
// Cross-resolution rasterization can lower an identical name to about 0.93.
// Cache identity also requires separation from the runner-up.
const NAME_IDENTITY_THRESHOLD: f32 = 0.90;

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

impl InputAction {
    pub fn name(self) -> &'static str {
        match self {
            Self::Key(key) => key.name(),
            Self::ClickColumn(_) => "Click",
        }
    }
}

const NAVIGATION_TIMEOUT: Duration = Duration::from_millis(1500);
const NAVIGATION_RETRY_AFTER: Duration = Duration::from_millis(300);
const EQUIP_TIMEOUT: Duration = Duration::from_millis(1500);
const INPUT_HOLD_MS: u64 = 45;
const INPUT_RELEASE_INTERVAL: Duration = Duration::from_millis(20);
const PAGE_STABLE_INTERVAL: Duration = Duration::from_millis(40);

pub(crate) struct Session<'capture, 'operation> {
    automation: AutomationSession<'capture>,
    observer: EquipmentObserver,
    frame: RgbaImage,
    page: EquipmentObservation,
    diagnostics: &'operation mut Diagnostics,
    client_size: (u32, u32),
    last_release: Option<Instant>,
}

pub(crate) enum ApplyProgress<'a> {
    Selecting(EquipmentKind),
    Caching {
        category: &'a TextSample,
        repairing: bool,
    },
    Selected {
        kind: EquipmentKind,
        corrected_category: Option<TextSample>,
    },
}

impl<'capture, 'operation> Session<'capture, 'operation> {
    pub fn new(
        mut automation: AutomationSession<'capture>,
        observer: EquipmentObserver,
        client_size: (u32, u32),
        diagnostics: &'operation mut Diagnostics,
    ) -> Result<Self> {
        let frame = automation.capture()?;
        let page = observer.observe(&frame);
        diagnostics.record(|| {
            json!({"event":"timing", "input_hold_ms":INPUT_HOLD_MS,
            "input_release_ms":INPUT_RELEASE_INTERVAL.as_millis(),
            "page_stable_ms":PAGE_STABLE_INTERVAL.as_millis(),
            "navigation_timeout_ms":NAVIGATION_TIMEOUT.as_millis(),
            "navigation_retry_after_ms":NAVIGATION_RETRY_AFTER.as_millis(),
            "equip_timeout_ms":EQUIP_TIMEOUT.as_millis()})
        });
        Ok(Self {
            automation,
            observer,
            frame,
            page,
            diagnostics,
            client_size,
            last_release: None,
        })
    }

    fn check_active(&self) -> Result<()> {
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
        self.diagnostics.observe(&self.page, capture_ms, observe_ms);
        Ok(())
    }

    pub fn at_entry(&self) -> bool {
        self.observer.entry(&self.frame).confirmed()
    }

    fn sample_current(&mut self) -> Result<EquipmentItem> {
        if !self.page.ready() {
            self.diagnostics.save(&self.frame, &self.page, "not-ready");
            bail!("equipment focus or name is not readable; operation stopped");
        }
        self.sync_category()?;
        self.diagnostics.save(&self.frame, &self.page, "remembered");
        Ok(EquipmentItem {
            name: self.page.name.clone(),
            category: Some(self.page.category.clone()),
            client_size: self.client_size,
        })
    }

    /// The caller replaces its old set only after all six samples and returns succeed.
    pub fn sample_all(&mut self) -> Result<EquipmentSet> {
        let started = Instant::now();
        let mut items = std::array::from_fn(|_| None);
        self.diagnostics
            .record(|| json!({"event":"remember_all_start", "client_size":self.client_size}));
        for kind in EquipmentKind::ALL {
            let item = (|| -> Result<EquipmentItem> {
                self.check_active()?;
                self.diagnostics
                    .record(|| json!({"event":"remember_item_start", "kind":kind}));
                self.open(kind)?;
                let item = self.sample_current()?;
                self.diagnostics.record(|| {
                    json!({"event":"remember_item_sampled", "kind":kind,
                    "page":diagnostics::describe(&self.page)})
                });
                self.return_to_entry()?;
                self.check_active()?;
                Ok(item)
            })()
            .with_context(|| {
                format!("failed to remember {kind:?}; previous equipment records retained")
            })?;
            items[kind.index()] = Some(item);
            tracing::info!(?kind, completed = kind.index() + 1, "equipped item sampled");
        }
        self.check_active()?;
        self.diagnostics.record(|| {
            json!({"event":"remember_all", "count":items.len(),
            "client_size":self.client_size, "elapsed_ms":started.elapsed().as_secs_f64()*1000.0})
        });
        Ok(items)
    }

    /// Check the complete set before opening or changing any equipment.
    pub fn apply_all(
        &mut self,
        items: &EquipmentSet,
        cache: &mut EquipmentCache,
        mut progress: impl FnMut(ApplyProgress<'_>),
    ) -> Result<()> {
        if let Some(kind) = EquipmentKind::ALL
            .iter()
            .find(|&&kind| items[kind.index()].is_none())
        {
            bail!("no saved item for {kind:?}; remember it before applying");
        }
        let started = Instant::now();
        self.diagnostics.record(
            || json!({"event":"apply_start", "kinds":EquipmentKind::ALL, "from_entry":true}),
        );
        for kind in EquipmentKind::ALL {
            progress(ApplyProgress::Selecting(kind));
            self.diagnostics
                .record(|| json!({"event":"apply_item_start", "kind":kind}));
            self.open(kind)
                .with_context(|| format!("failed to open {kind:?} for application"))?;
            let corrected_category = self
                .apply_item(
                    items[kind.index()].as_ref().unwrap(),
                    cache.search(kind, self.page.scale),
                    &mut progress,
                )
                .with_context(|| format!("failed to apply {kind:?}"))?;
            progress(ApplyProgress::Selected {
                kind,
                corrected_category,
            });
            self.diagnostics
                .record(|| json!({"event":"apply_item_completed", "kind":kind}));
            tracing::info!(?kind, "equipment item applied");
        }
        self.diagnostics.record(|| {
            json!({"event":"apply_completed", "from_entry":true,
            "elapsed_ms":started.elapsed().as_secs_f64()*1000.0})
        });
        tracing::info!("equipment application completed");
        Ok(())
    }

    fn apply_item(
        &mut self,
        item: &EquipmentItem,
        cache: cache::SearchCache<'_>,
        progress: &mut impl FnMut(ApplyProgress<'_>),
    ) -> Result<Option<TextSample>> {
        ensure!(
            self.find_item(item, cache, progress)?,
            "saved equipment was not found in the list"
        );
        self.equip_and_return(item)
    }

    fn find_item(
        &mut self,
        target: &EquipmentItem,
        cache: cache::SearchCache<'_>,
        progress: &mut impl FnMut(ApplyProgress<'_>),
    ) -> Result<bool> {
        let kind = cache.kind;
        let mut search = Search::new(target, cache);
        self.diagnostics.start_search(target);
        let started = Instant::now();
        let mut navigation = None;
        let mut caching = None;
        let result = (|| loop {
            self.sync_category()?;
            let evaluate_started = Instant::now();
            let (visit, next) = match search.advance(&self.page, navigation.take()) {
                Ok(step) => step,
                Err(error) => {
                    self.diagnostics
                        .save(&self.frame, &self.page, "search-error");
                    return Err(error);
                }
            };
            self.diagnostics.search_visit(
                &visit,
                &self.page,
                evaluate_started.elapsed().as_secs_f64() * 1000.0,
            );
            // The survey may briefly cross into another category to find its boundary.
            // Keep its original title until cache work actually switches categories.
            let active = search.caching();
            if active != caching {
                progress(match active {
                    Some((_, repairing)) => ApplyProgress::Caching {
                        category: &self.page.category,
                        repairing,
                    },
                    None => ApplyProgress::Selecting(kind),
                });
                caching = active;
            }
            match next {
                Next::Input(action) => navigation = Some(self.navigate(action)?),
                Next::Found | Next::NotFound => {
                    let found = matches!(next, Next::Found);
                    self.diagnostics.save(
                        &self.frame,
                        &self.page,
                        if found { "found" } else { "not-found" },
                    );
                    return Ok(found);
                }
            }
        })();
        self.diagnostics
            .finish_search(target, &result, started.elapsed().as_secs_f64() * 1000.0);
        result
    }
}
