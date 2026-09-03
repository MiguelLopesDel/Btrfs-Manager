//! Inventory rendering: filtering, grouping, and the snapshot/subvolume lists.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use btrfs_manager_core::{Subvolume, SubvolumeKind};
use btrfs_manager_helper::SubvolumeInventory;
use gtk4::prelude::*;
use libadwaita::prelude::*;

use super::bulk::update_bulk_bar;
use super::dialogs::{open_create_snapshot_dialog, open_policy_dialog};
use super::inventory_query::{
    capitalize_first, is_snapshot_kind, matches_query, snapshot_time_key, time_range_matches,
};
use super::row::render_snapshot_row;
use super::state::{
    SnapshotFilter, TimeRangeFilter, UiState, filter_label, time_range_label, view_mode_label,
};
use super::widgets::{
    append_date_subheader, append_info_row, append_section_header, clear_list, linked_button_group,
    set_status_row,
};

pub(crate) fn render_inventory(
    list: &gtk4::ListBox,
    inventory: &SubvolumeInventory,
    query: &str,
    state: UiState,
) {
    // Suppressed while we tear down and rebuild rows below, so the row
    // destruction/recreation doesn't fire connect_selected_rows_changed with
    // a transient, incomplete GTK selection and clobber `state.selected`.
    state.suppress_selection_signal.set(true);
    clear_list(list);
    state.row_paths.borrow_mut().clear();
    if inventory.subvolumes.is_empty() {
        state
            .summary_scope
            .set_label(&inventory.mountpoint.display().to_string());
        state.summary_counts.set_label("Snapshots 0 · Subvolumes 0");
        state.summary_filters.set_label("No subvolumes discovered");
        set_status_row(
            list,
            "No subvolumes found",
            &inventory.mountpoint.display().to_string(),
        );
        state.suppress_selection_signal.set(false);
        return;
    }

    let active_filter = state.filter.borrow().clone();
    let active_time_range = state.time_range.borrow().clone();

    let all_snapshots: Vec<_> = inventory
        .subvolumes
        .iter()
        .filter(|s| is_snapshot_kind(&s.kind))
        .collect();
    let managed_snapshot_count = all_snapshots.iter().filter(|s| s.managed).count();
    let external_snapshot_count = all_snapshots.len().saturating_sub(managed_snapshot_count);

    let snapshots: Vec<_> = all_snapshots
        .iter()
        .copied()
        .filter(|s| match &active_filter {
            SnapshotFilter::All => true,
            SnapshotFilter::Managed => s.managed,
            SnapshotFilter::External => !s.managed,
        })
        .filter(|s| time_range_matches(s, &active_time_range))
        .filter(|s| matches_query(s, query))
        .collect();

    let subvolumes: Vec<_> = inventory
        .subvolumes
        .iter()
        .filter(|s| !is_snapshot_kind(&s.kind))
        .filter(|s| matches_query(s, query))
        .collect();

    prune_hidden_selection(&state, &snapshots);

    update_inventory_summary(
        &state,
        inventory,
        InventorySummaryCounts {
            visible_snapshots: snapshots.len(),
            total_snapshots: all_snapshots.len(),
            managed_snapshots: managed_snapshot_count,
            external_snapshots: external_snapshot_count,
            visible_subvolumes: subvolumes.len(),
        },
        query,
    );

    if snapshots.is_empty() {
        render_empty_snapshots(list, query, &active_filter, &active_time_range);
    } else {
        render_snapshot_sections(list, inventory, &snapshots, &state);
    }

    render_subvolume_rows(list, inventory, &subvolumes, &state);
    state.suppress_selection_signal.set(false);
}

/// Keep the batch selection in sync with what is actually visible: a snapshot
/// hidden by a filter/search/time-range change must not stay queued for
/// deletion behind the user's back. Only visible managed snapshots can be
/// (re)checked, so drop everything else from the selection.
fn prune_hidden_selection(state: &UiState, snapshots: &[&Subvolume]) {
    if !state.select_mode.get() {
        return;
    }
    let visible_managed: HashSet<PathBuf> = snapshots
        .iter()
        .filter(|s| s.managed)
        .map(|s| s.path.clone())
        .collect();
    state
        .selected
        .borrow_mut()
        .retain(|path| visible_managed.contains(path));
    update_bulk_bar(state);
}

fn render_empty_snapshots(
    list: &gtk4::ListBox,
    query: &str,
    active_filter: &SnapshotFilter,
    active_time_range: &TimeRangeFilter,
) {
    append_section_header(list, "Snapshots (0)");
    let (empty_title, empty_sub) = if !query.is_empty() {
        (
            "No snapshots match your search",
            "Try a different query or clear the search",
        )
    } else if *active_filter != SnapshotFilter::All {
        ("No snapshots in this category", "Try a different filter")
    } else if *active_time_range != TimeRangeFilter::AllHistory {
        (
            "No snapshots in this time range",
            "Try a wider range or select All",
        )
    } else {
        (
            "No snapshots found",
            "Create or import snapshots to show them here",
        )
    };
    append_info_row(list, empty_title, empty_sub);
}

/// Group: managed first (by date), then external tools alphabetically.
fn render_snapshot_sections(
    list: &gtk4::ListBox,
    inventory: &SubvolumeInventory,
    snapshots: &[&Subvolume],
    state: &UiState,
) {
    let mut managed: Vec<&Subvolume> = Vec::new();
    let mut by_tool: BTreeMap<String, Vec<&Subvolume>> = BTreeMap::new();
    for snapshot in snapshots {
        match &snapshot.kind {
            SubvolumeKind::Snapshot => managed.push(snapshot),
            SubvolumeKind::ExternalSnapshot { tool } => {
                let label = capitalize_first(tool.as_deref().unwrap_or("external"));
                by_tool.entry(label).or_default().push(snapshot);
            }
            _ => {}
        }
    }

    if !managed.is_empty() {
        append_section_header(list, &format!("Managed ({})", managed.len()));
        render_grouped_rows(list, managed, inventory, state);
    }

    for (tool_label, group) in by_tool {
        append_section_header(list, &format!("{tool_label} ({})", group.len()));
        render_grouped_rows(list, group, inventory, state);
    }
}

/// Sort newest-first, group by time key (day or hour), render with subheaders.
fn render_grouped_rows(
    list: &gtk4::ListBox,
    mut snapshots: Vec<&Subvolume>,
    inventory: &SubvolumeInventory,
    state: &UiState,
) {
    let view_mode = state.view_mode.borrow().clone();
    snapshots.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.path.cmp(&b.path)));
    let mut by_date: BTreeMap<(i64, String), Vec<&Subvolume>> = BTreeMap::new();
    for snap in &snapshots {
        let (ordinal, label) = snapshot_time_key(snap.created_at.as_ref(), &view_mode);
        by_date.entry((ordinal, label)).or_default().push(snap);
    }
    for ((_ord, date_label), group) in &by_date {
        append_date_subheader(list, date_label);
        for snap in group {
            render_snapshot_row(list, snap, &inventory.mountpoint, state.clone());
        }
    }
}

fn render_subvolume_rows(
    list: &gtk4::ListBox,
    inventory: &SubvolumeInventory,
    subvolumes: &[&Subvolume],
    state: &UiState,
) {
    append_section_header(list, &format!("Subvolumes ({})", subvolumes.len()));
    for subvolume in subvolumes {
        let mountpoint = inventory.mountpoint.clone();
        let row = libadwaita::ActionRow::builder()
            .title(subvolume.path.display().to_string())
            .subtitle(format!("ID {}", subvolume.id.0))
            .build();
        row.add_prefix(
            &gtk4::Image::builder()
                .icon_name("folder-symbolic")
                .pixel_size(20)
                .valign(gtk4::Align::Center)
                .build(),
        );
        // Subvolumes aren't snapshots — never selectable for batch delete.
        row.set_selectable(false);

        let snapshot_btn = gtk4::Button::builder()
            .icon_name("camera-photo-symbolic")
            .tooltip_text("Create snapshot")
            .valign(gtk4::Align::Center)
            .build();
        let state_for_snap = state.clone();
        let mountpoint_for_snap = mountpoint.clone();
        let subvolume_for_snap = (*subvolume).clone();
        let list_for_snap = list.clone();
        snapshot_btn.connect_clicked(move |_| {
            open_create_snapshot_dialog(
                state_for_snap.clone(),
                list_for_snap.clone(),
                mountpoint_for_snap.clone(),
                subvolume_for_snap.clone(),
            );
        });

        let schedule = gtk4::Button::builder()
            .icon_name("alarm-symbolic")
            .tooltip_text("Snapshot policy")
            .valign(gtk4::Align::Center)
            .build();
        let state_for_policy = state.clone();
        let subvolume_for_policy = (*subvolume).clone();
        schedule.connect_clicked(move |_| {
            open_policy_dialog(
                state_for_policy.clone(),
                mountpoint.clone(),
                subvolume_for_policy.clone(),
            );
        });
        let actions = linked_button_group();
        actions.append(&snapshot_btn);
        actions.append(&schedule);
        row.add_suffix(&actions);
        list.append(&row);
    }
}

struct InventorySummaryCounts {
    visible_snapshots: usize,
    total_snapshots: usize,
    managed_snapshots: usize,
    external_snapshots: usize,
    visible_subvolumes: usize,
}

fn update_inventory_summary(
    state: &UiState,
    inventory: &SubvolumeInventory,
    counts: InventorySummaryCounts,
    query: &str,
) {
    state
        .summary_scope
        .set_label(&inventory.mountpoint.display().to_string());
    state.summary_counts.set_label(&format!(
        "{} visible snapshots · {} total · {} managed · {} external · {} subvolumes",
        counts.visible_snapshots,
        counts.total_snapshots,
        counts.managed_snapshots,
        counts.external_snapshots,
        counts.visible_subvolumes,
    ));
    let filter = state.filter.borrow();
    let time_range = state.time_range.borrow();
    let view_mode = state.view_mode.borrow();
    let search = if query.trim().is_empty() {
        "No search".to_string()
    } else {
        format!("Search: {}", query.trim())
    };
    state.summary_filters.set_label(&format!(
        "{} · {} · {} · {}",
        filter_label(&filter),
        time_range_label(&time_range),
        view_mode_label(&view_mode),
        search
    ));
}
