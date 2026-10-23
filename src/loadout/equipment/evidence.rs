//! Equipment-specific evidence; storage and retention belong to the operation.
use super::{
    EquipmentItem, InputAction, NAME_IDENTITY_THRESHOLD, NAME_MATCH_THRESHOLD, search::Visit,
};
use crate::{
    capture::CapturedFrame,
    evidence::OperationEvidence,
    item::EquipmentKind,
    vision::{
        equipment::{EquipmentEntry, EquipmentObservation, PageChange},
        text::TextGeometry,
    },
};
use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Serialize)]
struct SearchTarget {
    client_size: (u32, u32),
    name_geometry: TextGeometry,
    category_geometry: Option<TextGeometry>,
}

#[derive(Serialize)]
struct BestMatch {
    visit: Visit,
    frame_id: u64,
    name_geometry: TextGeometry,
}

pub(super) struct EquipmentEvidence<'a> {
    evidence: &'a OperationEvidence,
    previous: Option<EquipmentObservation>,
    target: Option<SearchTarget>,
    best_match: Option<BestMatch>,
    search_visits: usize,
    kind: EquipmentKind,
}

impl<'a> EquipmentEvidence<'a> {
    pub(super) fn new(evidence: &'a OperationEvidence) -> Self {
        Self {
            evidence,
            previous: None,
            target: None,
            best_match: None,
            search_visits: 0,
            kind: EquipmentKind::Helmet,
        }
    }

    pub(super) fn observe(
        &mut self,
        frame: &CapturedFrame,
        page: &EquipmentObservation,
        capture_ms: f64,
        observe_ms: f64,
    ) {
        if self
            .previous
            .as_ref()
            .is_none_or(|old| page.change_from(old).observed())
        {
            self.evidence.record("equipment-observation", Some(frame), json!({
                "kind": self.kind, "capture_s": capture_ms / 1000.0, "observe_s": observe_ms / 1000.0, "page": describe(page),
            }));
            self.previous = Some(page.clone());
        }
    }

    pub(super) fn confirm_input(
        &self,
        frame: &CapturedFrame,
        page: &EquipmentObservation,
        result: InputConfirmation<'_>,
    ) {
        self.evidence.record(
            "equipment-confirmation",
            Some(frame),
            json!({
                "kind": self.kind, "action": result.action.name(), "column": result.action.column(),
                "elapsed_s": result.elapsed_ms / 1000.0, "input_s": result.input_ms / 1000.0,
                "stable_s": result.stable_ms.map(|ms| ms / 1000.0), "list_moved": result.list_moved,
                "retried": result.retried, "change": result.change, "page": describe(page),
            }),
        );
    }

    pub(super) fn begin_item(&mut self, kind: EquipmentKind) {
        self.previous = None;
        self.kind = kind;
        self.target = None;
        self.best_match = None;
        self.search_visits = 0;
        for name in [
            "target-name",
            "target-foreground",
            "target-category",
            "best-name",
            "best-foreground",
        ] {
            self.evidence.remove_image(name);
        }
    }

    pub(super) fn checkpoint(
        &self,
        frame: &CapturedFrame,
        page: &EquipmentObservation,
        reason: &str,
    ) {
        self.evidence.record(
            reason,
            Some(frame),
            json!({"kind": self.kind, "page": describe(page)}),
        );
    }

    pub(super) fn entry(&self, frame: &CapturedFrame, entry: &EquipmentEntry, reason: &str) {
        self.evidence.record(
            reason,
            Some(frame),
            json!({"kind": self.kind, "entry": entry}),
        );
    }

    pub(super) fn start_search(&mut self, item: &EquipmentItem) {
        self.target = Some(SearchTarget {
            client_size: item.client_size,
            name_geometry: item.name.geometry,
            category_geometry: item.category.as_ref().map(|category| category.geometry),
        });
        self.evidence.record(
            "equipment-search-target",
            None,
            json!({"kind": self.kind, "target": self.target}),
        );
        self.evidence.image("target-name", item.name.image.clone());
        self.evidence
            .image("target-foreground", item.name.foreground());
        if let Some(category) = &item.category {
            self.evidence
                .image("target-category", category.foreground());
        }
    }

    pub(super) fn search_visit(
        &mut self,
        frame: &CapturedFrame,
        visit: &Visit,
        page: &EquipmentObservation,
        evaluate_ms: f64,
    ) {
        self.search_visits = visit.index;
        if self
            .best_match
            .as_ref()
            .is_none_or(|best| visit.target_cosine > best.visit.target_cosine)
        {
            self.best_match = Some(BestMatch {
                visit: visit.clone(),
                frame_id: frame.id,
                name_geometry: page.name.geometry,
            });
            self.evidence.image("best-name", page.name.image.clone());
            self.evidence
                .image("best-foreground", page.name.foreground());
        }
        self.evidence.record(
            "equipment-search-visit",
            Some(frame),
            json!({"kind": self.kind, "evaluate_s": evaluate_ms / 1000.0, "visit": visit, "page": describe(page)}),
        );
    }

    pub(super) fn finish_search(&self, result: &Result<bool>, elapsed_ms: f64) {
        let found = matches!(result, Ok(true));
        tracing::debug!(
            found,
            elapsed_s = elapsed_ms / 1000.0,
            visits = self.search_visits,
            best_target_cosine = self
                .best_match
                .as_ref()
                .map(|best| best.visit.target_cosine),
            match_threshold = NAME_MATCH_THRESHOLD,
            identity_threshold = NAME_IDENTITY_THRESHOLD,
            "equipment search finished"
        );
        self.evidence.record("equipment-search-finished", None,
            json!({"kind": self.kind, "found": found,
            "visits": self.search_visits, "elapsed_s": elapsed_ms / 1000.0,
            "target": self.target, "best": self.best_match,
            "match_threshold": NAME_MATCH_THRESHOLD, "identity_threshold": NAME_IDENTITY_THRESHOLD})
        );
    }
}

pub(super) struct InputConfirmation<'a> {
    pub action: InputAction,
    pub elapsed_ms: f64,
    pub input_ms: f64,
    pub stable_ms: Option<f64>,
    pub list_moved: bool,
    pub retried: bool,
    pub change: &'a PageChange,
}

fn describe(page: &EquipmentObservation) -> Value {
    json!({ "scale": page.scale, "slots": page.slots, "focus": page.focus, "hover": page.hover,
        "name_geometry": page.name.geometry, "category_geometry": page.category.geometry,
        "name_width": page.name.image.width(), "name_height": page.name.image.height(),
        "name_support": page.name.response.iter().filter(|&&v| v > 0.0).count(),
        "category_support": page.category.response.iter().filter(|&&v| v > 0.0).count(),
        "name_energy": page.name.response.iter().map(|v| v*v).sum::<f32>() })
}
