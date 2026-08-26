//! GTK4/libadwaita application shell. Submodules own one concern each; this
//! module assembles the main window and wires top-level actions.

mod bulk;
mod dialogs;
mod discovery;
mod errors;
mod filters;
mod header;
mod helper_client;
mod i18n;
mod inventory;
mod inventory_query;
mod mounts;
mod notifications;
mod row;
mod state;
mod update_check;
mod usage;
mod widgets;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use btrfs_manager_helper::{FilesystemDiscovery, HelperRequest};
use gtk4::glib;
use gtk4::prelude::*;

use bulk::{build_bulk_action_bar, wire_select_mode};
use dialogs::check_pending_rollback;
use discovery::{discover_and_load, load_mountpoint, selected_mountpoint};
use errors::{show_toast, user_error};
use filters::{build_filter_controls, wire_filter_toggles};
use header::{
    HeaderControls, SummaryPanel, build_header, build_summary_panel, wire_header_actions,
};
use helper_client::handle_privileged;
use i18n::tr;
use inventory::render_inventory;
use mounts::{managed_mount_roots_exist, unmount_session_mounts};
use notifications::check_recent_policy_runs;
use state::{SnapshotFilter, TimeRangeFilter, UiState, ViewMode};
use update_check::{UpdateBanner, build_update_banner, check_for_update};
use widgets::set_status_row;

pub fn run() {
    let app = libadwaita::Application::builder()
        .application_id("org.btrfsmanager.App")
        .build();

    app.connect_activate(build_ui);
    app.run();
}

pub fn run_check() {
    let app = libadwaita::Application::builder()
        .application_id("org.btrfsmanager.App.Check")
        .build();

    app.connect_activate(|app| {
        build_ui(app);
        let app = app.clone();
        glib::timeout_add_seconds_local_once(1, move || app.quit());
    });
    app.run_with_args::<&str>(&[]);
}

fn build_ui(app: &libadwaita::Application) {
    let controls = build_header();
    let summary = build_summary_panel();
    let browse_row = build_browse_row();
    let filesystem_selector = browse_row.filesystem_selector.clone();
    let search = browse_row.search.clone();
    let filters = build_filter_controls();

    let (list, list_scroll) = build_list_view();
    let (bulk_bar, bulk_delete_btn, bulk_cancel_btn) = build_bulk_action_bar();
    let update_banner = build_update_banner();

    let toast_overlay = assemble_content(
        &controls,
        &summary,
        &browse_row.widget,
        &filters,
        &update_banner,
        &bulk_bar,
        &list_scroll,
    );

    let ui_state = build_ui_state(
        &controls,
        &summary,
        &toast_overlay,
        &bulk_bar,
        &bulk_delete_btn,
    );

    wire_ui(
        &ui_state,
        &controls,
        &filters,
        &list,
        &search,
        &filesystem_selector,
        &bulk_cancel_btn,
    );

    let window = build_window(app, &toast_overlay, &ui_state);

    run_startup_tasks(
        &window,
        &ui_state,
        &list,
        &filesystem_selector,
        &search,
        &update_banner,
    );
}

/// Construct the shared `UiState`, wiring in the widgets built earlier so
/// every clone shares the same underlying labels/spinner/bulk-bar handles.
fn build_ui_state(
    controls: &HeaderControls,
    summary: &SummaryPanel,
    toast_overlay: &libadwaita::ToastOverlay,
    bulk_bar: &gtk4::Revealer,
    bulk_delete_btn: &gtk4::Button,
) -> UiState {
    UiState {
        inventory: Rc::new(RefCell::new(None)),
        mounted_snapshots: Rc::new(RefCell::new(HashSet::new())),
        session_mounts: Rc::new(RefCell::new(HashSet::new())),
        filesystems: Rc::new(RefCell::new(FilesystemDiscovery {
            filesystems: Vec::new(),
        })),
        suppress_selector_signal: Rc::new(Cell::new(false)),
        toast_overlay: toast_overlay.clone(),
        filter: Rc::new(RefCell::new(SnapshotFilter::All)),
        time_range: Rc::new(RefCell::new(TimeRangeFilter::Last7Days)),
        view_mode: Rc::new(RefCell::new(ViewMode::ByDay)),
        summary_scope: summary.scope.clone(),
        summary_counts: summary.counts.clone(),
        summary_filters: summary.filters.clone(),
        spinner: controls.spinner.clone(),
        select_mode: Rc::new(Cell::new(false)),
        selected: Rc::new(RefCell::new(HashSet::new())),
        row_paths: Rc::new(RefCell::new(HashMap::new())),
        suppress_selection_signal: Rc::new(Cell::new(false)),
        bulk_bar: bulk_bar.clone(),
        bulk_delete_btn: bulk_delete_btn.clone(),
    }
}

/// Wire every top-level signal handler: filter chips, header buttons, the
/// multi-select toggle, and the filesystem selector/search box.
fn wire_ui(
    ui_state: &UiState,
    controls: &HeaderControls,
    filters: &filters::FilterControls,
    list: &gtk4::ListBox,
    search: &gtk4::SearchEntry,
    filesystem_selector: &gtk4::ComboBoxText,
    bulk_cancel_btn: &gtk4::Button,
) {
    wire_filter_toggles(ui_state, filters, list, search);
    wire_header_actions(ui_state, controls, list, search, filesystem_selector);
    wire_select_mode(
        ui_state,
        list,
        search,
        filesystem_selector,
        &filters.select_toggle,
        bulk_cancel_btn,
    );
    wire_selector_and_search(ui_state, list, search, filesystem_selector);
}

/// Filesystem selector + search entry, side by side above the filter chips.
struct BrowseRow {
    widget: gtk4::Box,
    filesystem_selector: gtk4::ComboBoxText,
    search: gtk4::SearchEntry,
}

fn build_browse_row() -> BrowseRow {
    let search = gtk4::SearchEntry::builder()
        .placeholder_text(tr("search_placeholder"))
        .hexpand(true)
        .build();
    let filesystem_selector = gtk4::ComboBoxText::builder()
        .tooltip_text(tr("btrfs_filesystem"))
        .hexpand(true)
        .build();
    let widget = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(10)
        .build();
    widget.append(&filesystem_selector);
    widget.append(&search);

    BrowseRow {
        widget,
        filesystem_selector,
        search,
    }
}

/// The scrollable inventory list, seeded with the initial placeholder status.
fn build_list_view() -> (gtk4::ListBox, gtk4::ScrolledWindow) {
    let list = gtk4::ListBox::builder()
        .selection_mode(gtk4::SelectionMode::None)
        .css_classes(["boxed-list"])
        .vexpand(true)
        .build();
    let list_scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .min_content_height(260)
        .vexpand(true)
        .child(&list)
        .build();

    set_status_row(
        &list,
        "No mountpoint loaded",
        "Use Refresh to list Btrfs subvolumes",
    );

    (list, list_scroll)
}

/// Assemble the page content Box and wrap it in the ToolbarView/ToastOverlay.
fn assemble_content(
    controls: &HeaderControls,
    summary: &SummaryPanel,
    browse_row: &gtk4::Box,
    filters: &filters::FilterControls,
    update_banner: &UpdateBanner,
    bulk_bar: &gtk4::Revealer,
    list_scroll: &gtk4::ScrolledWindow,
) -> libadwaita::ToastOverlay {
    let page_title = gtk4::Label::builder()
        .label(tr("snapshot_inventory"))
        .halign(gtk4::Align::Start)
        .css_classes(["title-1"])
        .build();

    let content = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .margin_top(14)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .vexpand(true)
        .build();
    content.append(&update_banner.revealer);
    content.append(&page_title);
    content.append(&summary.widget);
    content.append(browse_row);
    content.append(&filters.filter_row);
    content.append(&filters.time_range_row);
    content.append(bulk_bar);
    content.append(list_scroll);

    let toolbar_view = libadwaita::ToolbarView::new();
    toolbar_view.add_top_bar(&controls.header);
    toolbar_view.set_content(Some(&content));
    let toast_overlay = libadwaita::ToastOverlay::new();
    toast_overlay.set_child(Some(&toolbar_view));
    toast_overlay
}

/// Construct and present the application window, wiring the best-effort
/// session-mount cleanup that runs on close.
fn build_window(
    app: &libadwaita::Application,
    toast_overlay: &libadwaita::ToastOverlay,
    ui_state: &UiState,
) -> libadwaita::ApplicationWindow {
    let window = libadwaita::ApplicationWindow::builder()
        .application(app)
        .title("Btrfs Manager")
        .default_width(980)
        .default_height(680)
        .content(toast_overlay)
        .build();
    let state_for_close = ui_state.clone();
    window.connect_close_request(move |_| {
        // Always allow the window to close — cleanup is best-effort.
        // Stale mounts are recovered by CleanupManagedMounts on next launch.
        if let Err(err) = unmount_session_mounts(&state_for_close) {
            tracing::error!(error = %err, "failed to unmount session browse mounts on close");
        }
        glib::Propagation::Proceed
    });
    window.present();
    window
}

/// Stale-mount cleanup, pending-rollback check, and the initial discovery
/// kickoff — everything that happens once, right after the window is shown.
fn run_startup_tasks(
    window: &libadwaita::ApplicationWindow,
    ui_state: &UiState,
    list: &gtk4::ListBox,
    filesystem_selector: &gtk4::ComboBoxText,
    search: &gtk4::SearchEntry,
    update_banner: &UpdateBanner,
) {
    if managed_mount_roots_exist() {
        match handle_privileged(HelperRequest::CleanupManagedMounts) {
            Ok(_) => {
                ui_state.mounted_snapshots.borrow_mut().clear();
                ui_state.session_mounts.borrow_mut().clear();
            }
            Err(err) => show_toast(
                &ui_state.toast_overlay,
                &user_error("cleanup_stale_mounts", &err),
            ),
        }
    }

    check_pending_rollback(window.upcast_ref(), &ui_state.toast_overlay);
    check_recent_policy_runs(ui_state);
    check_for_update(update_banner);

    discover_and_load(
        list.clone(),
        ui_state.clone(),
        filesystem_selector.clone(),
        search.text().to_string(),
    );
}

fn wire_selector_and_search(
    ui_state: &UiState,
    list: &gtk4::ListBox,
    search: &gtk4::SearchEntry,
    filesystem_selector: &gtk4::ComboBoxText,
) {
    let list_for_selector = list.clone();
    let search_for_selector = search.clone();
    let state_for_selector = ui_state.clone();
    filesystem_selector.connect_changed(move |selector| {
        if state_for_selector.suppress_selector_signal.get() {
            return;
        }
        let Some(index) = selector.active().map(|index| index as usize) else {
            return;
        };
        let Some(mountpoint) = selected_mountpoint(&state_for_selector.filesystems.borrow(), index)
        else {
            return;
        };
        load_mountpoint(
            list_for_selector.clone(),
            state_for_selector.clone(),
            search_for_selector.text().to_string(),
            mountpoint,
        );
    });

    let list_for_search = list.clone();
    let state_for_search = ui_state.clone();
    search.connect_search_changed(move |entry| {
        if let Some(inventory) = state_for_search.inventory.borrow().as_ref() {
            render_inventory(
                &list_for_search,
                inventory,
                entry.text().as_str(),
                state_for_search.clone(),
            );
        }
    });
}
