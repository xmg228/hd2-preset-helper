//! Search policy: complete cold scan, verified cached routing, bounded local repair.
use anyhow::{Context, Result, ensure};
use serde::Serialize;

use super::{
    EquipmentItem, InputAction, NAME_IDENTITY_THRESHOLD, NAME_MATCH_THRESHOLD, Navigation,
    cache::{Location, Name, SearchCache, Target},
    survey::{Step, Survey},
};
use crate::{input::Key, vision::equipment::EquipmentObservation};

const MAX_VISITS: usize = 1024;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Category,
    Scan,
    Cached,
    Repair,
}

#[derive(Clone, Serialize)]
pub(super) struct Visit {
    pub index: usize,
    phase: Phase,
    pub row: usize,
    pub col: usize,
    pub columns: usize,
    pub target_cosine: f32,
    pub category_cosine: Option<f32>,
}

pub(super) enum Next {
    Input(InputAction),
    Found,
    NotFound,
}

enum CategorySearch {
    Preferred,
    All { searched: Vec<usize> },
    Target(Target),
}

pub(super) struct Search<'a> {
    target: &'a EquipmentItem,
    cache: SearchCache<'a>,
    category_search: CategorySearch,
    category_origin: Option<usize>,
    survey: Option<Survey>,
    phase: Phase,
    expected_col: Option<usize>,
    visits: usize,
}

impl<'a> Search<'a> {
    pub fn new(target: &'a EquipmentItem, cache: SearchCache<'a>) -> Self {
        let prefer_category = cache.kind.has_categories() && target.category.is_some();
        Self {
            target,
            cache,
            category_search: if prefer_category {
                CategorySearch::Preferred
            } else {
                CategorySearch::All {
                    searched: Vec::new(),
                }
            },
            category_origin: None,
            survey: None,
            phase: Phase::Cached,
            expected_col: None,
            visits: 0,
        }
    }

    /// Active cache work, keyed by category so presentation changes only on transitions.
    pub fn caching(&self) -> Option<(usize, bool)> {
        self.survey
            .as_ref()
            .map(|survey| (survey.category, matches!(self.phase, Phase::Repair)))
    }

    /// Once per confirmed navigation, not per capture frame. Category hover is synchronized by the caller.
    pub fn advance(
        &mut self,
        page: &EquipmentObservation,
        navigation: Option<Navigation>,
    ) -> Result<(Visit, Next)> {
        ensure!(
            self.visits < MAX_VISITS,
            "search exceeded {MAX_VISITS} observations without finding the target or completing all categories"
        );
        ensure!(page.ready(), "search requires a visible focus and name");
        let focus = &page.slots[page.focus.context("search focus missing")?];
        if let Some(expected) = self.expected_col.take() {
            ensure!(
                focus.col == expected,
                "search expected column {expected}, observed {}; traversal stopped",
                focus.col
            );
        }
        let location = self.cache.observe(page, navigation)?;
        self.visits += 1;
        let score = page.name.cosine(&self.target.name);
        let category_cosine = self
            .target
            .category
            .as_ref()
            .map(|title| title.cosine(&page.category));
        let same_category = self
            .target
            .category
            .as_ref()
            .is_none_or(|title| self.cache.category_hint(title) == Some(location.category));
        let next = self.next(navigation, location, score, same_category)?;
        if let Next::Input(InputAction::ClickColumn(col)) = next {
            self.expected_col = Some(col);
        }
        Ok((
            Visit {
                index: self.visits,
                phase: self.phase,
                row: focus.row,
                col: focus.col,
                columns: location.width,
                target_cosine: score,
                category_cosine,
            },
            next,
        ))
    }

    fn next(
        &mut self,
        mut navigation: Option<Navigation>,
        mut location: Location,
        score: f32,
        same_category: bool,
    ) -> Result<Next> {
        // A strong name match can override a stale category hint immediately.
        if self.visits == 1
            && score >= NAME_IDENTITY_THRESHOLD
            && (!self.cache.is_complete(location.category)
                || self
                    .cache
                    .target([location.category], self.target.name.signature())
                    .is_some_and(|target| target.name == location.name))
        {
            return Ok(Next::Found);
        }
        if self.visits == 1 {
            // A reliable cached identity outranks the saved category hint.
            self.select_target(self.cache.category_indices(), false);
        }
        loop {
            if let Some(survey) = &mut self.survey {
                let category = survey.category;
                let step = survey.advance(location, navigation.take());
                match step {
                    Step::Input(action) => return Ok(Next::Input(action)),
                    Step::Complete { order, position } => {
                        location = self.cache.replace(category, order, position);
                        self.survey = None;
                        let single_category =
                            !self.cache.kind.has_categories() || self.cache.is_circular(category);
                        let accept_weak = single_category
                            || matches!(self.category_search, CategorySearch::Target(_));
                        if !self.select_target([category], accept_weak) {
                            if single_category {
                                return Ok(Next::NotFound);
                            }
                            self.search_all_categories();
                            if let CategorySearch::All { searched } = &mut self.category_search {
                                searched.push(category);
                            }
                            // A completed scan may already have crossed into the next category.
                            if location.category == category {
                                self.phase = Phase::Category;
                                return Ok(Next::Input(InputAction::Key(Key::C)));
                            }
                        }
                    }
                    Step::Rebuild => {
                        self.survey = None;
                        self.cache.take_order(category);
                        // Crossing during a retry can leave this category incomplete.
                        // Start a fresh pass rather than treating a later revisit as closure.
                        if location.category != category
                            && let CategorySearch::All { searched } = &mut self.category_search
                        {
                            searched.clear();
                        }
                        tracing::info!(
                            category,
                            "equipment continuity lost; restarting category scan"
                        );
                    }
                }
                continue;
            }
            match &mut self.category_search {
                CategorySearch::Preferred if !same_category => {
                    if self.category_origin == Some(location.category) {
                        self.search_all_categories();
                        continue;
                    }
                    self.category_origin.get_or_insert(location.category);
                    let key = self
                        .target
                        .category
                        .as_ref()
                        .and_then(|title| self.cache.category_hint(title))
                        .and_then(|category| self.cache.category_key(category));
                    self.phase = Phase::Category;
                    return Ok(Next::Input(InputAction::Key(key.unwrap_or(Key::C))));
                }
                CategorySearch::All { searched } if searched.contains(&location.category) => {
                    // Weak matches are compared across the whole pass, never first-come-first-served.
                    let categories = searched.clone();
                    if !self.select_target(categories, true) {
                        return Ok(Next::NotFound);
                    }
                    continue;
                }
                CategorySearch::Target(target) if target.category != location.category => {
                    if self.category_origin == Some(location.category) {
                        // Cached category navigation looped; fall back to live exploration.
                        self.cache.take_order(target.category);
                        self.search_all_categories();
                        continue;
                    }
                    self.category_origin.get_or_insert(location.category);
                    self.phase = Phase::Category;
                    let key = self.cache.category_key(target.category).unwrap_or(Key::C);
                    return Ok(Next::Input(InputAction::Key(key)));
                }
                _ => self.category_origin = None,
            }
            if let CategorySearch::Target(target) = self.category_search {
                if location.position.is_some() && navigation.is_none_or(|n| !n.retried) {
                    return self.route(location, target.name, score);
                }
            } else if self.select_target([location.category], !self.cache.kind.has_categories()) {
                continue;
            }
            let old = self.cache.take_order(location.category);
            self.survey = Some(
                if matches!(self.category_search, CategorySearch::Target(_))
                    && !old.is_empty()
                    && navigation.is_none_or(|n| !n.retried)
                {
                    self.phase = Phase::Repair;
                    Survey::repair(location, self.cache.kind.list_columns(), old)
                } else {
                    self.phase = Phase::Scan;
                    Survey::build(location, self.cache.kind.list_columns())
                },
            );
            navigation = None;
        }
    }

    /// Weak candidates need a completed comparison, or a target already chosen before repair.
    fn select_target(
        &mut self,
        categories: impl IntoIterator<Item = usize>,
        accept_weak: bool,
    ) -> bool {
        let Some(target) = self
            .cache
            .target(categories, self.target.name.signature())
            .filter(|target| accept_weak || target.score >= NAME_IDENTITY_THRESHOLD)
        else {
            return false;
        };
        self.category_search = CategorySearch::Target(target);
        self.category_origin = None;
        true
    }

    fn search_all_categories(&mut self) {
        if !matches!(self.category_search, CategorySearch::All { .. }) {
            tracing::info!("equipment category hint was insufficient; searching all categories");
            self.category_search = CategorySearch::All {
                searched: Vec::new(),
            };
            self.category_origin = None;
        }
    }

    fn route(&mut self, location: Location, target: Name, score: f32) -> Result<Next> {
        self.phase = Phase::Cached;
        if location.position.is_some() && location.name == target {
            ensure!(
                score >= NAME_MATCH_THRESHOLD,
                "mapped equipment target did not match its saved name"
            );
            return Ok(Next::Found);
        }
        self.cache
            .next(target)
            .map(Next::Input)
            .context("equipment map could not locate the confirmed focus")
    }
}
