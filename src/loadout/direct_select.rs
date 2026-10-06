mod click_plan;
mod home_activation;
mod hover;
mod list_map;
mod page_navigation;

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use tracing::{debug, debug_span, info, info_span, warn};

use crate::app_events::{AppEvent, AppEventSink};
use crate::automation::AutomationSession;
use crate::item::ItemKind;
use crate::preset::stratagems::{LocalTemplate, StratagemPreset, load_template_sample};
use crate::vision::{
    ImageSample, ItemAvailability, RecognizerSession, RoiObservation, Slot, SlotKind,
    TemplateClassifier,
};

use super::home::{find_home_row, home_booster_slot, wait_for_filled_home};

use self::click_plan::{DirectClickTarget, find_visible_target, next_visible_target};
use self::home_activation::{HomeOpenTarget, home_booster_target, open_slot_list};
use self::hover::{HoverSample, HoverVerifier};
use self::list_map::{ListMap, LocatedPage, NavigationHint, SlotId};
use self::page_navigation::{PageNavigator, PageSnapshot, PageTurnInput, PageTurnResult};

const MAX_WHEEL_INPUTS: u32 = 20;
const CLICK_HOLD_MS: u64 = 45;
const MAX_TARGET_CLICK_ATTEMPTS: usize = 3;
const MAX_HOVER_ATTEMPTS: usize = 4;
const TARGET_POSITION_TOLERANCE: u32 = 2;
const POST_CLICK_CONFIRM_TIMEOUT: Duration = Duration::from_millis(400);
const POST_CLICK_MIN_OBSERVATIONS: u32 = 2;
const TERMINAL_SETTLE_TIMEOUT: Duration = Duration::from_millis(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoosterApplyOutcome {
    Applied,
    Unavailable,
}

enum ListSelectionOutcome {
    Applied(RoiObservation),
    Unavailable,
}

#[derive(Clone, Copy)]
enum ListSelectionMode {
    Stratagems { count: usize, in_saved_order: bool },
    FirstAvailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollDirection {
    Up,
    Down,
}

impl ScrollDirection {
    const fn boundary_label(self) -> &'static str {
        match self {
            Self::Up => "top",
            Self::Down => "bottom",
        }
    }

    const fn opposite(self) -> Self {
        match self {
            Self::Up => Self::Down,
            Self::Down => Self::Up,
        }
    }
}

pub fn apply_stratagem_preset_from_home(
    recognizer: RecognizerSession,
    automation: &mut AutomationSession<'_>,
    events: &AppEventSink,
    mut home: RoiObservation,
    presets_path: &Path,
    preset: &StratagemPreset,
    apply_in_saved_order: bool,
) -> Result<Option<BoosterApplyOutcome>> {
    let mut booster_sources = Vec::new();
    if let Some(preferred) = &preset.booster {
        for template in std::iter::once(preferred).chain(preset.fallback_booster.as_ref()) {
            booster_sources.push((
                template.path.clone(),
                load_template_sample(presets_path, template)
                    .context("failed to load booster template")?,
            ));
        }
    }
    let mut booster_confirmed = false;
    if let Some((preferred, _)) = booster_sources.first()
        && home_booster_slot(&home).is_some_and(|slot| slot.kind == SlotKind::HomeBooster)
    {
        TemplateClassifier::new(
            ItemKind::Booster,
            &booster_sources,
            recognizer.ui_scale(),
            &home,
        )?
        .classify_batch(&mut home)
        .context("failed to identify equipped booster")?;
        if let Some(equipped) = home_booster_slot(&home)
            .and_then(|slot| slot.classification.as_ref())
            .filter(|matched| matched.item_id == *preferred)
        {
            info!(
                item_id = %equipped.item_id,
                match_error = equipped.match_error,
                "preferred booster is already equipped; skipping list entry"
            );
            booster_confirmed = true;
        }
    }
    events.emit(AppEvent::StratagemsApplyStarted {
        stratagems: preset
            .stratagems
            .iter()
            .map(|item| item.path.clone())
            .collect(),
        booster: preset.booster.as_ref().map(|item| item.path.clone()),
        booster_confirmed,
    });
    let home = apply_stratagems_from_home(
        recognizer,
        automation,
        events,
        home,
        presets_path,
        &preset.stratagems,
        apply_in_saved_order,
    )
    .context("failed to apply stratagems from home")?;

    // Selecting Stratagems does not change an already-confirmed Booster.
    if booster_sources.is_empty() {
        Ok(None)
    } else if booster_confirmed {
        Ok(Some(BoosterApplyOutcome::Applied))
    } else {
        apply_booster_from_home(recognizer, automation, events, home, &booster_sources)
            .map(Some)
            .context("failed to apply booster from home")
    }
}

fn apply_stratagems_from_home(
    recognizer: RecognizerSession,
    automation: &mut AutomationSession<'_>,
    events: &AppEventSink,
    mut home: RoiObservation,
    presets_path: &Path,
    templates: &[LocalTemplate],
    apply_in_saved_order: bool,
) -> Result<RoiObservation> {
    if templates.len() != 4 {
        bail!(
            "stratagem preset application requires exactly 4 stratagems, got {}",
            templates.len()
        );
    }

    let sources = templates
        .iter()
        .map(|template| {
            load_template_sample(presets_path, template)
                .map(|sample| (template.path.clone(), sample))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut home_classifier = None;
    let mut previous_missing = templates.len() + 1;
    loop {
        let (slots, _) = find_home_row(&home).context("missing home loadout row")?;
        let empty_count = slots
            .iter()
            .filter(|slot| slot.kind == SlotKind::StratagemEmpty)
            .count();
        if empty_count < 4 {
            if home_classifier.is_none() {
                home_classifier = Some(TemplateClassifier::new(
                    ItemKind::Stratagem,
                    &sources,
                    recognizer.ui_scale(),
                    &home,
                )?);
            }
            home_classifier
                .as_ref()
                .unwrap()
                .classify_batch(&mut home)?;
        }
        let (slots, _) = find_home_row(&home).context("missing home loadout row")?;
        let equipped = slots
            .iter()
            .filter_map(|slot| slot.classification.as_ref())
            .map(|classification| classification.item_id.as_str())
            .collect::<Vec<_>>();
        ensure!(
            equipped
                .iter()
                .enumerate()
                .all(|(index, id)| !equipped[..index].contains(id)),
            "equipped Stratagem identities are ambiguous; clear the slots and try again"
        );
        let missing = sources
            .iter()
            .filter(|(id, _)| !equipped.contains(&id.as_str()))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        events.emit(AppEvent::StratagemsProgress {
            remaining: missing.clone(),
        });
        if missing.is_empty() {
            info!("all preset Stratagems are already equipped");
            return Ok(home);
        }
        ensure!(
            missing.len() < previous_missing,
            "Stratagem replacement made no confirmed progress; clear the slots and try again"
        );
        previous_missing = missing.len();

        // Replace an incorrect item first, then fill the empties in the same
        // list visit. Once full, each further replacement returns home.
        let entry = slots
            .iter()
            .find(|slot| slot.kind == SlotKind::Stratagem && slot.classification.is_none())
            .or_else(|| {
                slots
                    .iter()
                    .find(|slot| slot.kind == SlotKind::StratagemEmpty)
            })
            .context("no home slot available for a missing Stratagem")?;
        let count = empty_count + usize::from(entry.kind == SlotKind::Stratagem);
        info!(
            kept = equipped.len(),
            missing = missing.len(),
            entry_col = entry.col,
            selections_until_full = count,
            "applying missing Stratagems"
        );
        let opened_list = open_slot_list(
            automation,
            recognizer,
            HomeOpenTarget {
                item_kind: ItemKind::Stratagem,
                point: entry.center(),
            },
        )?;
        let classifier = TemplateClassifier::new(
            ItemKind::Stratagem,
            &sources,
            recognizer.ui_scale(),
            &opened_list,
        )?;
        home = match select_items_from_open_list(
            PageNavigator::new(recognizer, ItemKind::Stratagem, classifier),
            automation,
            events,
            &missing,
            opened_list,
            ListSelectionMode::Stratagems {
                count,
                in_saved_order: apply_in_saved_order && empty_count == 4,
            },
            empty_count == 4,
        )? {
            ListSelectionOutcome::Applied(home) => home,
            ListSelectionOutcome::Unavailable => {
                unreachable!("stratagem candidates do not have an unavailable state")
            }
        };
        if count == missing.len() {
            return Ok(home);
        }
    }
}

fn apply_booster_from_home(
    recognizer: RecognizerSession,
    automation: &mut AutomationSession<'_>,
    events: &AppEventSink,
    home: RoiObservation,
    sources: &[(String, ImageSample)],
) -> Result<BoosterApplyOutcome> {
    events.emit(AppEvent::BoosterProgress {
        item_id: sources[0].0.clone(),
        confirmed: false,
    });
    let target = home_booster_target(&home)?;
    let opened_list = open_slot_list(automation, recognizer, target)?;
    let classifier = TemplateClassifier::new(
        ItemKind::Booster,
        sources,
        recognizer.ui_scale(),
        &opened_list,
    )?;
    let item_ids = sources.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
    match select_items_from_open_list(
        PageNavigator::new(recognizer, ItemKind::Booster, classifier),
        automation,
        events,
        &item_ids,
        opened_list,
        ListSelectionMode::FirstAvailable,
        home.slots
            .iter()
            .any(|slot| slot.kind == SlotKind::HomeBoosterEmpty),
    )? {
        ListSelectionOutcome::Applied(_) => Ok(BoosterApplyOutcome::Applied),
        ListSelectionOutcome::Unavailable => Ok(BoosterApplyOutcome::Unavailable),
    }
}

fn select_items_from_open_list(
    navigator: PageNavigator,
    automation: &mut AutomationSession<'_>,
    events: &AppEventSink,
    items: &[String],
    initial_observation: RoiObservation,
    mode: ListSelectionMode,
    starts_at_top: bool,
) -> Result<ListSelectionOutcome> {
    let item_kind = navigator.item_kind();
    let requested_items = match mode {
        ListSelectionMode::Stratagems { count, .. } => count,
        ListSelectionMode::FirstAvailable => 1,
    };
    let span = info_span!(
        "preset_list_selection",
        item_kind = %item_kind.label(),
        requested_items,
        starts_at_top,
        mode = match mode {
            ListSelectionMode::Stratagems { in_saved_order: true, .. } => "saved_order",
            ListSelectionMode::Stratagems { in_saved_order: false, .. } => "visible_first",
            ListSelectionMode::FirstAvailable => "first_available",
        }
    );
    let _guard = span.enter();
    let mut remaining = items.to_vec();
    let mut selections_left = requested_items;
    let mut wheel_attempts = 0u32;
    let mut boundary_candidate = None;
    // Replacing an equipped item can open the list anywhere. Unknown targets
    // are searched downward first, then upward; mapped targets navigate directly.
    let mut top_reached = starts_at_top;
    let mut bottom_reached = false;

    let mut current_page = {
        let span = debug_span!("scan_page", wheel_attempts);
        let _guard = span.enter();
        navigator.prepare_page(initial_observation)?
    };
    let mut list_map = ListMap::new(&current_page, item_kind);

    loop {
        let page_span = debug_span!(
            "selection_page",
            wheel_attempts,
            remaining_items = remaining.len(),
            ?boundary_candidate
        );
        let _page_guard = page_span.enter();
        let target = if matches!(
            mode,
            ListSelectionMode::Stratagems {
                in_saved_order: false,
                ..
            }
        ) {
            next_visible_target(&current_page.roi, &remaining, item_kind, |slot| {
                list_map.can_select_slot(slot)
            })
            .or_else(|| {
                remaining
                    .iter()
                    .find_map(|item_id| list_map.visible_mapped_target(item_id, &current_page.roi))
            })
        } else {
            find_visible_target(&current_page.roi, &remaining[0], item_kind)
                .filter(|target| list_map.can_select_slot(&target.slot))
                .or_else(|| list_map.visible_mapped_target(&remaining[0], &current_page.roi))
        };
        if let Some(target) = target {
            #[cfg(feature = "diagnostics")]
            if target.fallback {
                crate::vision::save_fallback_slot(
                    &current_page.roi.image,
                    &target.slot,
                    &target.item_id,
                    f64::from(target.match_error),
                )?;
            }
            let span = debug_span!("select_visible_item", item_id = %target.item_id);
            let _guard = span.enter();
            let selected_item_id = target.item_id.clone();
            // Local row numbers can change during hover relocation; map identity cannot.
            let selected_id = list_map
                .current_slot_id(&target.slot)
                .context("target slot is absent from the temporary list map")?;
            let final_requested_item = selections_left == 1;
            let outcome = select_preset_target(
                automation,
                &navigator,
                &mut list_map,
                selected_id,
                target,
                &mut current_page,
                final_requested_item,
            )?;
            if let TargetSelectionOutcome::Unavailable { brightness_ratio } = outcome {
                info!(
                    item_id = %selected_item_id,
                    brightness_ratio,
                    "booster target is already in use"
                );
                if matches!(mode, ListSelectionMode::FirstAvailable) && remaining.len() > 1 {
                    remaining.remove(0);
                    events.emit(AppEvent::BoosterProgress {
                        item_id: remaining[0].clone(),
                        confirmed: false,
                    });
                    wheel_attempts = 0;
                    boundary_candidate = None;
                    debug!(
                        next_item_id = %remaining[0],
                        "trying the fallback booster"
                    );
                    continue;
                }
                return Ok(ListSelectionOutcome::Unavailable);
            }
            selections_left -= 1;
            remaining.retain(|item_id| item_id != &selected_item_id);
            events.emit(match item_kind {
                ItemKind::Stratagem => AppEvent::StratagemsProgress {
                    remaining: remaining.clone(),
                },
                ItemKind::Booster => AppEvent::BoosterProgress {
                    item_id: selected_item_id.clone(),
                    confirmed: true,
                },
            });
            list_map.mark_selected(selected_id);
            match outcome {
                TargetSelectionOutcome::List { page, placement } => {
                    let vertical_shift = placement.vertical_shift;
                    list_map.commit(&page, placement, "post-click");
                    current_page = page;
                    wheel_attempts = 0;
                    boundary_candidate = None;
                    debug!(
                        item_id = %selected_item_id,
                        remaining_items = remaining.len(),
                        vertical_shift,
                        "single-item selection confirmed"
                    );
                    continue;
                }
                TargetSelectionOutcome::Home(home) => {
                    debug!(
                        item_id = %selected_item_id,
                        remaining_items = remaining.len(),
                        "final item selection confirmed after returning home"
                    );
                    debug_assert_eq!(selections_left, 0);
                    return Ok(ListSelectionOutcome::Applied(home));
                }
                TargetSelectionOutcome::Unavailable { .. } => unreachable!(),
            }
        }

        let item_id = &remaining[0];
        if top_reached
            && bottom_reached
            && !list_map.contains_item(item_id)
            && let Some(score) = list_map.activate_best_candidate(item_id)
        {
            warn!(
                item_id,
                score, "using the best accumulated slot from the completed list scan"
            );
            continue;
        }
        if wheel_attempts >= MAX_WHEEL_INPUTS {
            bail!(
                "{} preset item {item_id} not found after {} wheel inputs",
                item_kind.label(),
                MAX_WHEEL_INPUTS,
            );
        }

        let hint = list_map.navigation_hint(item_id);
        let direction = match hint {
            NavigationHint::Unmapped => {
                if bottom_reached {
                    ScrollDirection::Up
                } else {
                    ScrollDirection::Down
                }
            }
            NavigationHint::Scroll(direction) => direction,
            NavigationHint::ExpectedVisible => {
                bail!(
                    "mapped target item {item_id} was not recognized in its expected visible row"
                );
            }
        };
        let input = if boundary_candidate == Some(direction) {
            PageTurnInput::Nudge(direction)
        } else {
            PageTurnInput::Full(direction)
        };
        wheel_attempts += 1;

        match navigator.turn_page(
            automation,
            &mut list_map,
            &current_page,
            input,
            wheel_attempts,
        )? {
            PageTurnResult::Moved { page, short } => {
                current_page = page;
                boundary_candidate = (input.is_full() && short).then_some(direction);
                if boundary_candidate.is_some() {
                    debug!(
                        remaining_items = remaining.len(),
                        ?direction,
                        "short page turn accepted; list boundary will be nudged after processing this page"
                    );
                } else if input.is_nudge() {
                    debug!(
                        remaining_items = remaining.len(),
                        ?direction,
                        "nudge moved the viewport; normal page turns will resume"
                    );
                }
            }
            PageTurnResult::Recovered(page) => {
                current_page = page;
                if input.is_nudge() {
                    bail!("page navigation remained ambiguous after a boundary probe");
                }
                boundary_candidate = Some(direction);
                debug!(
                    remaining_items = remaining.len(),
                    ?direction,
                    "temporary list map recovered; the next forward turn will use a nudge"
                );
            }
            PageTurnResult::NoMovement(last_page) => {
                current_page = last_page;
                if input.is_nudge() {
                    match direction {
                        ScrollDirection::Up => top_reached = true,
                        ScrollDirection::Down => bottom_reached = true,
                    }
                    boundary_candidate = None;
                    if !(top_reached && bottom_reached) && !list_map.contains_item(item_id) {
                        info!(
                            ?direction,
                            "list boundary confirmed; searching the other side before enabling fallback"
                        );
                        wheel_attempts = 0;
                        continue;
                    }
                    if top_reached
                        && bottom_reached
                        && let Some(score) = list_map.activate_best_candidate(item_id)
                    {
                        warn!(
                            item_id,
                            score,
                            "no candidate passed the threshold; using the best observed slot"
                        );
                        continue;
                    }
                    debug!(
                        remaining_items = remaining.len(),
                        ?direction,
                        "list boundary confirmed by a conservative probe"
                    );
                    bail!(
                        "target item {item_id} was not found before the {} of the {} list",
                        direction.boundary_label(),
                        item_kind.label(),
                    );
                }
                boundary_candidate = Some(direction);
                debug!(
                    remaining_items = remaining.len(),
                    ?direction,
                    "full page turn produced no movement; scheduling a small boundary nudge"
                );
            }
        }
    }
}

#[allow(clippy::large_enum_variant)]
enum TargetSelectionOutcome {
    List {
        page: PageSnapshot,
        placement: LocatedPage,
    },
    Home(RoiObservation),
    Unavailable {
        brightness_ratio: f32,
    },
}

fn select_preset_target(
    automation: &mut AutomationSession<'_>,
    navigator: &PageNavigator,
    list_map: &mut ListMap,
    slot_id: SlotId,
    initial_target: DirectClickTarget,
    current_page: &mut PageSnapshot,
    final_requested_item: bool,
) -> Result<TargetSelectionOutcome> {
    let item_kind = navigator.item_kind();
    let item_id = initial_target.item_id.clone();
    if let ItemAvailability::Unavailable { brightness_ratio } =
        navigator.item_availability(&current_page.roi.image, &initial_target.slot, &item_id)?
    {
        return Ok(TargetSelectionOutcome::Unavailable { brightness_ratio });
    }
    let prepared = relocate_and_wait_hover(
        automation,
        navigator,
        list_map,
        slot_id,
        initial_target,
        current_page,
    )?;
    if let ItemAvailability::Unavailable { brightness_ratio } = prepared.availability {
        return Ok(TargetSelectionOutcome::Unavailable { brightness_ratio });
    }
    let mut target = prepared.target;
    let before = prepared.sample;
    let mut last_after_score = None;

    for attempt in 1..=MAX_TARGET_CLICK_ATTEMPTS {
        let (x, y) = target.slot.center();
        debug!(
            item_id = %target.item_id,
            attempt,
            x,
            y,
            match_error = target.match_error,
            match_margin = target.match_margin,
            gate_quality = target.gate_quality,
            hover_score = before.target_score(),
            "clicking preset item after hover confirmation"
        );

        automation.click_current(CLICK_HOLD_MS)?;
        if final_requested_item {
            if let Some(home) = wait_for_filled_home(
                automation,
                navigator.recognizer(),
                POST_CLICK_CONFIRM_TIMEOUT,
            )? {
                debug!(
                    item_kind = %item_kind.label(),
                    "terminal item selection returned to the loadout home"
                );
                return Ok(TargetSelectionOutcome::Home(home));
            }

            if let Some(retry_target) = unchanged_terminal_target(
                automation, navigator, list_map, slot_id, &target, &before,
            )? {
                debug!(
                    item_id = %item_id,
                    attempt,
                    "final click left the target unchanged; retrying in place"
                );
                target = retry_target;
                continue;
            }

            if let Some(home) =
                wait_for_filled_home(automation, navigator.recognizer(), TERMINAL_SETTLE_TIMEOUT)?
            {
                debug!(
                    item_kind = %item_kind.label(),
                    "terminal item selection returned to the loadout home after settling"
                );
                return Ok(TargetSelectionOutcome::Home(home));
            }
            bail!(
                "final {} click changed the list state but did not return to a confirmed loadout home",
                item_kind.label()
            );
        }

        match observe_post_click_state(automation, navigator, list_map, slot_id, &target, &before)?
        {
            PostClickObservation::Selected { page, placement } => {
                debug!(
                    item_id = %item_id,
                    attempt,
                    vertical_shift = placement.vertical_shift,
                    "preset item selected state confirmed"
                );
                return Ok(TargetSelectionOutcome::List { page, placement });
            }
            PostClickObservation::Unchanged { slot, after_score } => {
                debug!(
                    item_id = %item_id,
                    attempt,
                    "click left the target at the same position and brightness; retrying in place"
                );
                target.slot = slot;
                last_after_score = Some(after_score);
            }
        }
    }

    if final_requested_item {
        bail!(
            "final target item {item_id} remained unchanged after {MAX_TARGET_CLICK_ATTEMPTS} confirmed click attempts"
        );
    }
    let after_score = last_after_score.unwrap_or(before.target_score());
    bail!(
        "target item {item_id} neither moved nor dimmed after {MAX_TARGET_CLICK_ATTEMPTS} click attempts (before={:.1}, after={after_score:.1}, required drop={:.1})",
        before.target_score(),
        before.required_score_drop(),
    )
}

enum PostClickObservation {
    Selected {
        page: PageSnapshot,
        placement: LocatedPage,
    },
    Unchanged {
        slot: Slot,
        after_score: f32,
    },
}

fn observe_post_click_state(
    automation: &mut AutomationSession<'_>,
    navigator: &PageNavigator,
    list_map: &ListMap,
    slot_id: SlotId,
    clicked_target: &DirectClickTarget,
    before: &HoverSample,
) -> Result<PostClickObservation> {
    let started = Instant::now();
    let mut observation = 0u32;
    let item_id = clicked_target.item_id.as_str();
    loop {
        observation += 1;
        let image = automation.capture()?;
        let page = navigator.scan_direct_page(image)?;

        let Some(placement) = list_map.locate_after_click(&page, slot_id) else {
            debug!(
                item_id,
                observation, "post-click page could not be located in the temporary list map"
            );
            if started.elapsed() >= POST_CLICK_CONFIRM_TIMEOUT
                && observation >= POST_CLICK_MIN_OBSERVATIONS
            {
                bail!(
                    "target item {item_id} could not be confirmed because the post-click page could not be located"
                );
            }
            continue;
        };

        if placement.has_moved() {
            debug!(
                item_id,
                observation,
                vertical_shift = placement.vertical_shift,
                "post-click success confirmed by viewport movement"
            );
            return Ok(PostClickObservation::Selected { page, placement });
        }

        let Some(slot) = list_map.slot_after_placement(&placement, &page.roi, slot_id) else {
            debug!(
                item_id,
                observation, "post-click target slot is absent after map placement"
            );
            if started.elapsed() >= POST_CLICK_CONFIRM_TIMEOUT
                && observation >= POST_CLICK_MIN_OBSERVATIONS
            {
                bail!("target item {item_id} could not be located after clicking");
            }
            continue;
        };

        let sample = HoverVerifier::sample_current_frame(&page.roi.image, &slot)?;
        let brightness_dropped = sample.is_dimmer_than(before);

        if brightness_dropped {
            debug!(
                item_id,
                observation,
                before_score = before.target_score(),
                after_score = sample.target_score(),
                "post-click success confirmed by brightness drop"
            );
            return Ok(PostClickObservation::Selected { page, placement });
        }

        if started.elapsed() >= POST_CLICK_CONFIRM_TIMEOUT
            && observation >= POST_CLICK_MIN_OBSERVATIONS
        {
            debug!(
                item_id,
                observation,
                before_score = before.target_score(),
                after_score = sample.target_score(),
                "post-click target remained bright"
            );
            return Ok(PostClickObservation::Unchanged {
                slot,
                after_score: sample.target_score(),
            });
        }
    }
}

struct PreparedHover {
    target: DirectClickTarget,
    sample: HoverSample,
    availability: ItemAvailability,
}

fn relocate_and_wait_hover(
    automation: &mut AutomationSession<'_>,
    navigator: &PageNavigator,
    list_map: &mut ListMap,
    slot_id: SlotId,
    mut target: DirectClickTarget,
    current_page: &mut PageSnapshot,
) -> Result<PreparedHover> {
    let item_id = target.item_id.clone();
    let mut last_hover_error = None;
    for attempt in 1..=MAX_HOVER_ATTEMPTS {
        if attempt > 1 {
            let image = automation.capture()?;
            let page = navigator.scan_direct_page(image)?;
            let Some(placement) = list_map.locate_page(&page, None) else {
                last_hover_error = Some(anyhow::anyhow!(
                    "target item {item_id} could not be relocated in the temporary list map"
                ));
                continue;
            };
            let slot = list_map
                .slot_after_placement(&placement, &page.roi, slot_id)
                .with_context(|| {
                    format!(
                        "target item {item_id} left the visible viewport while relocating its hover position"
                    )
                })?;
            target.slot = slot;
            // Use the relocated viewport as the baseline for the upcoming click.
            list_map.commit(&page, placement, "hover-relocation");
            *current_page = page;
        }
        automation.move_cursor(target.slot.center())?;

        match HoverVerifier::wait_at_current_position(
            automation,
            &current_page.roi.slots,
            &target.slot,
        ) {
            Ok((sample, image)) => {
                let availability = navigator.item_availability(&image, &target.slot, &item_id)?;
                return Ok(PreparedHover {
                    target,
                    sample,
                    availability,
                });
            }
            Err(error) => {
                debug!(
                    item_id,
                    attempt,
                    error = %error,
                    "hover confirmation failed; rescanning and relocating target"
                );
                last_hover_error = Some(error);
            }
        }
    }

    let suffix = last_hover_error
        .map(|error| format!(": {error:#}"))
        .unwrap_or_default();
    bail!(
        "target item {item_id} was not confirmed under the cursor after {MAX_HOVER_ATTEMPTS} attempts{suffix}"
    )
}

fn unchanged_terminal_target(
    automation: &mut AutomationSession<'_>,
    navigator: &PageNavigator,
    list_map: &ListMap,
    slot_id: SlotId,
    clicked_target: &DirectClickTarget,
    before: &HoverSample,
) -> Result<Option<DirectClickTarget>> {
    let image = automation.capture()?;
    let Ok(page) = navigator.scan_direct_page(image) else {
        return Ok(None);
    };
    let target = find_visible_target(&page.roi, &clicked_target.item_id, navigator.item_kind())
        .or_else(|| {
            let placement = list_map.locate_after_click(&page, slot_id)?;
            let slot = list_map.slot_after_placement(&placement, &page.roi, slot_id)?;
            Some(DirectClickTarget {
                slot,
                ..clicked_target.clone()
            })
        });
    let Some(target) = target else {
        return Ok(None);
    };
    let moved = clicked_target.slot.x.abs_diff(target.slot.x) > TARGET_POSITION_TOLERANCE
        || clicked_target.slot.y.abs_diff(target.slot.y) > TARGET_POSITION_TOLERANCE;
    let sample = HoverVerifier::sample_current_frame(&page.roi.image, &target.slot)?;
    let brightness_dropped = !moved && sample.is_dimmer_than(before);

    if moved || brightness_dropped {
        debug!(
            item_id = %clicked_target.item_id,
            moved,
            brightness_dropped,
            before_score = before.target_score(),
            after_score = sample.target_score(),
            "final click changed the target state; continuing to wait for home"
        );
        return Ok(None);
    }

    Ok(Some(target))
}
