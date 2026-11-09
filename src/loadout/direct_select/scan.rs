//! Enumerate accepted list positions using the application navigator and map.
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};

use super::{
    CLICK_HOLD_MS, ScrollDirection,
    home_activation::{HomeOpenTarget, home_booster_target, open_slot_list},
    list_map::ListMap,
    page_navigation::{PageNavigator, PageSnapshot, PageTurnInput, PageTurnResult},
};
use crate::{
    automation::AutomationSession,
    input::Key,
    item::ItemKind,
    item_cache::{CachedIcon, IconCatalog},
    loadout::home::{UiState, find_home_row, wait_for_ui_state},
    scan::{ScanCancellation, ScanKind, ScanProgress},
    vision::{
        HOME_ICON_SIZE_LOGICAL, IconSample, ImageSample, RecognizerSession, RoiObservation, Slot,
        SlotKind, TemplateClassifier, crop_icon_sample,
    },
};

const MAX_SCAN_TURNS: usize = 256;
const SINGLE_PAGE_SETTLE: Duration = Duration::from_millis(150);
const SINGLE_PAGE_TIMEOUT: Duration = Duration::from_millis(1500);

pub(crate) fn scan_icon_list(
    recognizer: RecognizerSession,
    automation: &mut AutomationSession<'_>,
    kind: ItemKind,
    home: RoiObservation,
    progress: &mut impl FnMut(ScanProgress),
    cancel: &ScanCancellation,
) -> Result<IconCatalog> {
    cancel.check()?;
    let selected = home_samples(kind, recognizer, &home)?;
    let target = match kind {
        ItemKind::Stratagem => {
            let (slots, _) = find_home_row(&home).context("Stratagem home was not confirmed")?;
            HomeOpenTarget {
                item_kind: kind,
                point: slots[0].center(),
            }
        }
        ItemKind::Booster => home_booster_target(&home)?,
    };
    let navigator = PageNavigator::for_scan(recognizer, kind);
    let opened = open_slot_list(automation, recognizer, target)?;
    let mut page = navigator.prepare_page(opened)?;
    let point = page
        .roi
        .slots
        .first()
        .context("the opened icon list is empty")?
        .center();
    automation.move_cursor(point)?;
    let mut map = ListMap::new(&page, kind, automation.evidence());
    complete_open_list(
        &navigator, automation, &mut map, &mut page, progress, cancel,
    )?;
    let templates = materialize_icons(kind, &map, &selected)?;
    automation.tap_key(Key::Escape, CLICK_HOLD_MS)?;
    let returned = wait_for_ui_state(
        automation,
        recognizer,
        UiState::Home,
        Duration::from_millis(1500),
        |_| true,
    )?
    .context("could not confirm the Stratagem home after scanning")?;
    verify_home(kind, recognizer, &home, returned)?;
    Ok(templates)
}

pub(super) fn complete_open_list(
    navigator: &PageNavigator,
    automation: &mut AutomationSession<'_>,
    map: &mut ListMap,
    page: &mut PageSnapshot,
    progress: &mut impl FnMut(ScanProgress),
    cancel: &ScanCancellation,
) -> Result<()> {
    let kind = navigator.item_kind();
    let mut report = |map: &ListMap| {
        progress(ScanProgress {
            kind: match kind {
                ItemKind::Stratagem => ScanKind::Stratagems,
                ItemKind::Booster => ScanKind::Booster,
            },
            category: None,
            items: map.scan_count(),
        })
    };
    cancel.check()?;
    report(map);
    if page.roi.scrollbar.is_none() {
        confirm_single_page(navigator, automation, map, page, cancel)?;
        let placement = map
            .locate_page(page, None)
            .placement
            .context("single-page icon list lost its map position")?;
        map.commit(page, placement, "single-page", automation.evidence());
        map.confirm_single_page();
    } else {
        let mut turns = 0;
        while !map.boundaries().1 {
            *page = turn(
                navigator,
                automation,
                map,
                page,
                ScrollDirection::Down,
                &mut turns,
                cancel,
            )?;
            report(map);
        }
        while !map.boundaries().0 {
            *page = turn(
                navigator,
                automation,
                map,
                page,
                ScrollDirection::Up,
                &mut turns,
                cancel,
            )?;
            report(map);
        }
    }
    cancel.check()?;
    map.scan_samples()?;
    automation.evidence().record(
        "icon-scan-complete",
        Some(&page.roi.image),
        serde_json::json!({"kind":kind,"items":map.scan_count(),"scrollbar":page.roi.scrollbar}),
    );
    report(map);
    Ok(())
}

pub(super) fn materialize_icons(
    kind: ItemKind,
    map: &ListMap,
    selected: &[ImageSample],
) -> Result<IconCatalog> {
    let (layout, mut samples): (Vec<_>, Vec<_>) = map
        .scan_samples()?
        .into_iter()
        .map(|(position, slot)| {
            slot.native
                .as_ref()
                .map(|sample| (position, sample.as_ref().clone()))
                .context("scanned icon has no native sample")
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .unzip();
    let positions = TemplateClassifier::match_scanned(kind, selected, &samples)?;
    for (position, sample) in positions.iter().zip(selected) {
        samples[*position] = sample.clone();
    }
    let items = samples
        .into_iter()
        .zip(layout)
        .enumerate()
        .map(|(index, (sample, position))| {
            let icon = IconSample::extract(kind, sample).with_context(|| {
                format!(
                    "could not extract scanned {} item {}",
                    kind.label(),
                    index + 1
                )
            })?;
            Ok(CachedIcon { icon, position })
        })
        .collect::<Result<_>>()?;
    Ok(IconCatalog { items })
}

fn turn(
    navigator: &PageNavigator,
    automation: &mut AutomationSession<'_>,
    map: &mut ListMap,
    page: &PageSnapshot,
    direction: ScrollDirection,
    turns: &mut usize,
    cancel: &ScanCancellation,
) -> Result<PageSnapshot> {
    let mut current = None;
    for attempt in 0..2 {
        cancel.check()?;
        ensure!(
            *turns < MAX_SCAN_TURNS,
            "icon scan exceeded {MAX_SCAN_TURNS} page turns"
        );
        *turns += 1;
        let input = if attempt == 0 {
            PageTurnInput::Scan(direction)
        } else {
            PageTurnInput::Retry(direction)
        };
        match navigator.turn_page(
            automation,
            map,
            current.as_ref().unwrap_or(page),
            input,
            *turns as u32,
        )? {
            PageTurnResult::Confirmed(page) => return Ok(page),
            PageTurnResult::Recovered(page) => current = Some(page),
        }
    }
    anyhow::bail!("icon scan navigation remained ambiguous after recovery")
}

fn confirm_single_page(
    navigator: &PageNavigator,
    automation: &mut AutomationSession<'_>,
    map: &ListMap,
    page: &mut PageSnapshot,
    cancel: &ScanCancellation,
) -> Result<()> {
    let last_row = page
        .slot_samples
        .iter()
        .map(|slot| slot.row)
        .max()
        .context("icon list has no sampled rows")?;
    let last = page
        .slot_samples
        .iter()
        .filter(|slot| slot.row == last_row)
        .collect::<Vec<_>>();
    let scale = navigator.recognizer().ui_scale();
    let bottom = last
        .iter()
        .map(|slot| slot.page_y + slot.height as f32 * 0.5)
        .fold(0.0, f32::max);
    ensure!(
        last.len() < 4 || bottom + 80.0 * scale < page.roi.image.height() as f32,
        "the list extent could not be confirmed without a scrollbar"
    );
    for notches in [1, -1] {
        cancel.check()?;
        automation.scroll(notches)?;
        let started = Instant::now();
        let mut stable = 0;
        loop {
            cancel.check()?;
            let next = navigator.scan_direct_page(automation.capture()?)?;
            ensure!(
                next.roi.scrollbar.is_none(),
                "a scrollbar appeared while checking the single-page list"
            );
            let assessment = map.locate_page(&next, None);
            let unchanged = assessment
                .placement
                .as_ref()
                .is_some_and(|placement| !placement.has_moved())
                && next.slot_samples.len() == page.slot_samples.len();
            ensure!(
                unchanged,
                "the icon list moved while checking its single-page extent"
            );
            stable += 1;
            *page = next;
            if stable >= 2 && started.elapsed() >= SINGLE_PAGE_SETTLE {
                break;
            }
            ensure!(
                started.elapsed() < SINGLE_PAGE_TIMEOUT,
                "single-page icon list did not settle"
            );
        }
    }
    automation.evidence().record(
        "single-page-list-confirmed",
        Some(&page.roi.image),
        serde_json::json!({
            "kind": navigator.item_kind(), "slots": page.slot_samples.len(), "wheel_probes": 2,
        }),
    );
    Ok(())
}

fn verify_home(
    kind: ItemKind,
    recognizer: RecognizerSession,
    before: &RoiObservation,
    after: RoiObservation,
) -> Result<()> {
    let previous = home_slots(kind, before)?;
    let current = home_slots(kind, &after)?;
    ensure!(
        previous.iter().zip(&current).all(|(a, b)| a.kind == b.kind),
        "{} selection changed during scanning",
        kind.label(),
    );
    for (previous, current) in home_samples(kind, recognizer, before)?
        .iter()
        .zip(home_samples(kind, recognizer, &after)?)
    {
        ensure!(
            TemplateClassifier::same_item(kind, previous, &current)?,
            "equipped {} could not be confirmed after scanning",
            kind.label(),
        );
    }
    Ok(())
}

fn home_slots(kind: ItemKind, home: &RoiObservation) -> Result<Vec<&Slot>> {
    let (stratagems, booster) =
        find_home_row(home).context("loadout home is missing while scanning icons")?;
    Ok(match kind {
        ItemKind::Stratagem => stratagems.to_vec(),
        ItemKind::Booster => vec![booster],
    })
}

pub(super) fn home_samples(
    kind: ItemKind,
    recognizer: RecognizerSession,
    home: &RoiObservation,
) -> Result<Vec<ImageSample>> {
    home_slots(kind, home)?
        .into_iter()
        .filter(|slot| matches!(slot.kind, SlotKind::Stratagem | SlotKind::HomeBooster))
        .map(|slot| {
            crop_icon_sample(
                kind,
                &home.image,
                slot,
                HOME_ICON_SIZE_LOGICAL * recognizer.ui_scale(),
            )
        })
        .collect()
}
