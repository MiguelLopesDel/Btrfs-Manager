//! Filter chips and toggles above the inventory list: type filter, select
//! mode, time range, and view mode (grouped by day/hour).

use gtk4::prelude::*;

use super::i18n::tr;
use super::inventory::render_inventory;
use super::state::{SnapshotFilter, TimeRangeFilter, UiState, ViewMode};
use super::widgets::linked_button_group;

/// Filter chips and toggles above the inventory list.
pub(crate) struct FilterControls {
    pub(crate) filter_row: gtk4::Box,
    pub(crate) time_range_row: gtk4::Box,
    pub(crate) filter_all: gtk4::ToggleButton,
    pub(crate) filter_managed: gtk4::ToggleButton,
    pub(crate) filter_external: gtk4::ToggleButton,
    pub(crate) select_toggle: gtk4::ToggleButton,
    pub(crate) tr_today: gtk4::ToggleButton,
    pub(crate) tr_7: gtk4::ToggleButton,
    pub(crate) tr_30: gtk4::ToggleButton,
    pub(crate) tr_all: gtk4::ToggleButton,
    pub(crate) vm_day: gtk4::ToggleButton,
    pub(crate) vm_hour: gtk4::ToggleButton,
}

pub(crate) fn chip_row_label(label: &str) -> gtk4::Label {
    gtk4::Label::builder()
        .label(label)
        .halign(gtk4::Align::Start)
        .valign(gtk4::Align::Center)
        .css_classes(["caption", "dim-label"])
        .build()
}

/// All/Managed/External linked toggle group, plus the select-mode toggle.
fn build_type_filter_row() -> (
    gtk4::Box,
    gtk4::ToggleButton,
    gtk4::ToggleButton,
    gtk4::ToggleButton,
    gtk4::ToggleButton,
) {
    let filter_all = gtk4::ToggleButton::builder()
        .label("All")
        .active(true)
        .build();
    let filter_managed = gtk4::ToggleButton::builder()
        .label("Managed")
        .group(&filter_all)
        .build();
    let filter_external = gtk4::ToggleButton::builder()
        .label("External")
        .group(&filter_all)
        .build();
    let filter_group = linked_button_group();
    filter_group.append(&filter_all);
    filter_group.append(&filter_managed);
    filter_group.append(&filter_external);
    let filter_row = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .build();
    filter_row.append(&chip_row_label("Type"));
    filter_row.append(&filter_group);

    // Multi-select toggle for batch deletion of managed snapshots.
    let select_toggle = gtk4::ToggleButton::builder()
        .label(tr("select"))
        .tooltip_text(tr("select_tooltip"))
        .hexpand(true)
        .halign(gtk4::Align::End)
        .build();
    filter_row.append(&select_toggle);

    (
        filter_row,
        filter_all,
        filter_managed,
        filter_external,
        select_toggle,
    )
}

/// Today/7/30/All linked toggle group.
fn build_time_range_row() -> (
    gtk4::Box,
    gtk4::ToggleButton,
    gtk4::ToggleButton,
    gtk4::ToggleButton,
    gtk4::ToggleButton,
) {
    let tr_today = gtk4::ToggleButton::builder().label("Today").build();
    let tr_7 = gtk4::ToggleButton::builder()
        .label("7 days")
        .group(&tr_today)
        .active(true)
        .build();
    let tr_30 = gtk4::ToggleButton::builder()
        .label("30 days")
        .group(&tr_today)
        .build();
    let tr_all = gtk4::ToggleButton::builder()
        .label("All")
        .group(&tr_today)
        .build();
    let tr_group = linked_button_group();
    tr_group.append(&tr_today);
    tr_group.append(&tr_7);
    tr_group.append(&tr_30);
    tr_group.append(&tr_all);
    let tr_left = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .hexpand(true)
        .build();
    tr_left.append(&chip_row_label("Range"));
    tr_left.append(&tr_group);

    (tr_left, tr_today, tr_7, tr_30, tr_all)
}

/// By-day/By-hour linked toggle group.
fn build_view_mode_group() -> (gtk4::Box, gtk4::ToggleButton, gtk4::ToggleButton) {
    let vm_day = gtk4::ToggleButton::builder()
        .label("By day")
        .active(true)
        .build();
    let vm_hour = gtk4::ToggleButton::builder()
        .label("By hour")
        .group(&vm_day)
        .build();
    let vm_group = linked_button_group();
    vm_group.append(&vm_day);
    vm_group.append(&vm_hour);
    let vm_right = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk4::Align::End)
        .build();
    vm_right.append(&chip_row_label("Group"));
    vm_right.append(&vm_group);

    (vm_right, vm_day, vm_hour)
}

pub(crate) fn build_filter_controls() -> FilterControls {
    let (filter_row, filter_all, filter_managed, filter_external, select_toggle) =
        build_type_filter_row();
    let (tr_left, tr_today, tr_7, tr_30, tr_all) = build_time_range_row();
    let (vm_right, vm_day, vm_hour) = build_view_mode_group();

    let time_range_row = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(14)
        .build();
    time_range_row.append(&tr_left);
    time_range_row.append(&vm_right);

    FilterControls {
        filter_row,
        time_range_row,
        filter_all,
        filter_managed,
        filter_external,
        select_toggle,
        tr_today,
        tr_7,
        tr_30,
        tr_all,
        vm_day,
        vm_hour,
    }
}

/// Wire a toggle chip so activating it applies `apply` to the state and
/// re-renders the inventory without issuing new Btrfs commands.
fn wire_rerender_toggle(
    btn: &gtk4::ToggleButton,
    state: &UiState,
    list: &gtk4::ListBox,
    search: &gtk4::SearchEntry,
    apply: impl Fn(&UiState) + 'static,
) {
    let state = state.clone();
    let list = list.clone();
    let search = search.clone();
    btn.connect_toggled(move |b| {
        if !b.is_active() {
            return;
        }
        apply(&state);
        if let Some(inventory) = state.inventory.borrow().as_ref() {
            render_inventory(&list, inventory, search.text().as_str(), state.clone());
        }
    });
}

pub(crate) fn wire_filter_toggles(
    state: &UiState,
    filters: &FilterControls,
    list: &gtk4::ListBox,
    search: &gtk4::SearchEntry,
) {
    for (btn, value) in [
        (&filters.filter_all, SnapshotFilter::All),
        (&filters.filter_managed, SnapshotFilter::Managed),
        (&filters.filter_external, SnapshotFilter::External),
    ] {
        wire_rerender_toggle(btn, state, list, search, move |state| {
            *state.filter.borrow_mut() = value.clone();
        });
    }

    for (btn, value) in [
        (&filters.tr_today, TimeRangeFilter::Today),
        (&filters.tr_7, TimeRangeFilter::Last7Days),
        (&filters.tr_30, TimeRangeFilter::Last30Days),
        (&filters.tr_all, TimeRangeFilter::AllHistory),
    ] {
        wire_rerender_toggle(btn, state, list, search, move |state| {
            *state.time_range.borrow_mut() = value.clone();
        });
    }

    for (btn, value) in [
        (&filters.vm_day, ViewMode::ByDay),
        (&filters.vm_hour, ViewMode::ByHour),
    ] {
        wire_rerender_toggle(btn, state, list, search, move |state| {
            *state.view_mode.borrow_mut() = value.clone();
        });
    }
}
