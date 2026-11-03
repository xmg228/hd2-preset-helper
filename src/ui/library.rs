//! A visible list projection; preset identities and previews remain in the source model.
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::mpsc::Sender;

use slint::{ComponentHandle, Model, ModelExt, ModelRc, VecModel};

use super::{EntryDropTarget, LibraryEntryRow, PresetPanel, PresetRow, UiRequest};
use crate::preset::EntryMetadata;

const SPACING: f32 = 4.0;
const FOLDER_HEIGHT: f32 = 36.0;
const FOLDER_DROP_EDGE: f32 = 6.0;

fn tracked_rows<T: Clone + 'static>(model: &ModelRc<T>) -> Vec<T> {
    model.model_tracker().track_row_count_changes();
    (0..model.row_count())
        .filter_map(|index| model.row_data_tracked(index))
        .collect()
}

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    Rc::new(VecModel::from(rows)).into()
}

pub(super) fn connect(window: &PresetPanel, send: Sender<UiRequest>) {
    window.on_project_entries(|presets, entries, query, title, height| {
        model(project(
            &tracked_rows(&presets),
            &tracked_rows(&entries),
            query.as_str(),
            title.as_str(),
            height,
        ))
    });
    window.on_visible_preset_indices(|entries| {
        model(
            tracked_rows(&entries)
                .into_iter()
                .filter(|row| !row.is_folder)
                .map(|row| row.preset_index)
                .collect(),
        )
    });
    window.on_visible_preset_index(|indices, current| {
        tracked_rows(&indices)
            .iter()
            .position(|&index| index == current)
            .map_or(-1, |index| index as i32)
    });
    window.on_adjacent_preset(|indices, current, delta| {
        let indices = tracked_rows(&indices);
        if indices.is_empty() {
            return -1;
        }
        if let Some(index) = indices.iter().position(|&index| index == current) {
            let next = (index as i64 + i64::from(delta)).clamp(0, indices.len() as i64 - 1);
            indices[next as usize]
        } else if delta > 0 {
            indices
                .iter()
                .copied()
                .find(|&index| index > current)
                .unwrap_or(*indices.last().unwrap())
        } else {
            indices
                .iter()
                .copied()
                .rfind(|&index| index < current)
                .unwrap_or(indices[0])
        }
    });
    window.on_find_entry(|entries, name| {
        tracked_rows(&entries)
            .into_iter()
            .find(|row| row.name == name)
            .unwrap_or_default()
    });
    window.on_entry_span(|entries, name| {
        let rows = tracked_rows(&entries);
        let Some(index) = rows.iter().position(|row| row.name == name) else {
            return 0.0;
        };
        let row = &rows[index];
        let end = if row.is_folder {
            group_end(&rows, index)
        } else {
            index + 1
        };
        let last = &rows[end - 1];
        last.top + last.height - row.top
    });
    window.on_entry_drop_target(|entries, source, outdent, y| {
        drop_target(&tracked_rows(&entries), source.as_str(), outdent, y)
    });
    let weak = window.as_weak();
    window.on_toggle_folder(move |folder| {
        if let Some(window) = weak.upgrade() {
            let entries = window.get_entries();
            if let Some((index, mut row)) = entries
                .iter()
                .enumerate()
                .find(|(_, row)| row.is_folder && row.name == folder)
            {
                row.expanded = !row.expanded;
                entries.set_row_data(index, row);
            }
        }
    });
    let create = send.clone();
    window.on_add_folder(move || {
        let _ = create.send(UiRequest::CreateFolder);
    });
    let weak = window.as_weak();
    let rename = send.clone();
    window.on_request_folder_rename(move |folder, label| {
        let label = label.trim();
        if let Some(window) = weak.upgrade()
            && window
                .get_entries()
                .iter()
                .any(|row| row.is_folder && row.name == folder && row.label != label)
        {
            let _ = rename.send(UiRequest::RenameFolder {
                folder: folder.to_string(),
                label: label.into(),
            });
        }
    });
    window.on_request_ungroup(move |folder| {
        let _ = send.send(UiRequest::UngroupFolder(folder.to_string()));
    });
}

pub(super) fn rows(
    entries: Vec<EntryMetadata>,
    previous: &ModelRc<LibraryEntryRow>,
) -> ModelRc<LibraryEntryRow> {
    let expanded: BTreeMap<_, _> = previous
        .iter()
        .map(|row| (row.name.to_string(), row.expanded))
        .collect();
    model(
        entries
            .into_iter()
            .map(|entry| LibraryEntryRow {
                number: entry.id.rsplit('_').next().unwrap_or("").into(),
                expanded: expanded.get(&entry.id).copied().unwrap_or(true),
                name: entry.id.into(),
                label: entry.label.into(),
                parent: entry.parent.into(),
                is_folder: entry.is_folder,
                preset_index: -1,
                ..Default::default()
            })
            .collect(),
    )
}

fn project(
    presets: &[PresetRow],
    entries: &[LibraryEntryRow],
    query: &str,
    default_title: &str,
    preset_height: f32,
) -> Vec<LibraryEntryRow> {
    let query = query.trim().to_lowercase();
    let searching = !query.is_empty();
    let indices: BTreeMap<_, _> = presets
        .iter()
        .enumerate()
        .map(|(index, preset)| (preset.name.as_str(), index))
        .collect();
    let by_id: BTreeMap<_, _> = entries.iter().map(|row| (row.name.as_str(), row)).collect();
    let mut matches = BTreeSet::new();
    if searching {
        for preset in presets {
            let title = if preset.label.is_empty() {
                default_title.replace("{}", preset.number.as_str())
            } else {
                preset.label.to_string()
            };
            if title.to_lowercase().contains(query.as_str())
                || preset.number.contains(query.as_str())
            {
                matches.insert(preset.name.as_str());
                let mut parent = by_id
                    .get(preset.name.as_str())
                    .map(|row| row.parent.as_str());
                while let Some(row) = parent.and_then(|parent| by_id.get(parent)) {
                    matches.insert(row.name.as_str());
                    parent = Some(row.parent.as_str());
                }
            }
        }
    }
    let mut top = 0.0;
    entries
        .iter()
        .filter_map(|entry| {
            if searching && !matches.contains(entry.name.as_str()) {
                return None;
            }
            let mut parent = entry.parent.as_str();
            while let Some(folder) = by_id.get(parent) {
                if !searching && !folder.expanded {
                    return None;
                }
                parent = folder.parent.as_str();
            }
            let mut row = entry.clone();
            row.preset_index = if row.is_folder {
                -1
            } else {
                *indices.get(row.name.as_str())? as i32
            };
            row.expanded |= searching;
            row.top = top;
            row.height = if row.is_folder {
                FOLDER_HEIGHT
            } else {
                preset_height
            };
            top += row.height + SPACING;
            Some(row)
        })
        .collect()
}

fn group_end(rows: &[LibraryEntryRow], start: usize) -> usize {
    rows[start + 1..]
        .iter()
        .position(|row| row.parent.is_empty())
        .map_or(rows.len(), |offset| start + 1 + offset)
}

fn after_row(rows: &[LibraryEntryRow], index: usize, parent: &str) -> EntryDropTarget {
    let row = &rows[index];
    EntryDropTarget {
        parent: parent.into(),
        before: rows[index + 1..]
            .iter()
            .find(|next| next.parent == parent)
            .map(|next| next.name.clone())
            .unwrap_or_default(),
        top: row.top + row.height + SPACING,
        valid: true,
        ..Default::default()
    }
}

fn drop_target(rows: &[LibraryEntryRow], source: &str, outdent: bool, y: f32) -> EntryDropTarget {
    let Some(source_index) = rows.iter().position(|row| row.name == source) else {
        return EntryDropTarget::default();
    };
    let source = &rows[source_index];
    let last = rows.len() - 1;
    let parent = if !source.is_folder && !outdent {
        rows[last].parent.as_str()
    } else {
        ""
    };
    let mut target = after_row(rows, last, parent);
    if let Some(mut index) = rows
        .iter()
        .position(|row| y < row.top + row.height + SPACING / 2.0)
    {
        // Both nesting levels remain reachable throughout the group-tail gap.
        if index > 0
            && rows[index].parent.is_empty()
            && !rows[index - 1].parent.is_empty()
            && y < rows[index].top
        {
            index -= 1;
        }
        let row = &rows[index];
        if source.is_folder {
            let start = (0..=index)
                .rev()
                .find(|&index| rows[index].parent.is_empty())
                .unwrap_or(index);
            let end = group_end(rows, start);
            let bottom = rows[end - 1].top + rows[end - 1].height + SPACING;
            if y < (rows[start].top + bottom) / 2.0 {
                target.before = rows[start].name.clone();
                target.top = rows[start].top;
            } else {
                target = after_row(rows, end - 1, "");
            }
        } else if row.is_folder {
            let end = group_end(rows, index);
            if y < row.top + FOLDER_DROP_EDGE {
                target.parent = row.parent.clone();
                target.before = row.name.clone();
                target.top = row.top;
            } else if end == index + 1 && y >= row.top + row.height - FOLDER_DROP_EDGE {
                target = after_row(rows, index, "");
            } else {
                target = after_row(rows, end - 1, row.name.as_str());
                target.inside = row.name.clone();
            }
        } else if y < row.top + row.height / 2.0 {
            target.parent = row.parent.clone();
            target.before = row.name.clone();
            target.top = row.top;
        } else {
            let group_tail = !row.parent.is_empty()
                && rows
                    .get(index + 1)
                    .is_none_or(|next| next.parent != row.parent);
            let parent = if group_tail && outdent {
                ""
            } else {
                row.parent.as_str()
            };
            target = after_row(rows, index, parent);
        }
    }
    let next_sibling = rows[source_index + 1..]
        .iter()
        .find(|row| row.parent == source.parent)
        .map(|row| row.name.as_str())
        .unwrap_or("");
    if target.before == source.name
        || (target.parent == source.parent && target.before == next_sibling)
        || target.parent == source.name
    {
        target.valid = false;
    }
    target.displace = target.valid
        && target.inside.is_empty()
        && (source.is_folder
            || (target.parent == source.parent
                && !rows.iter().any(|row| {
                    row.is_folder
                        && ((row.top >= target.top && row.top < source.top)
                            || (row.top > source.top + source.height && row.top < target.top))
                })));
    target
}
