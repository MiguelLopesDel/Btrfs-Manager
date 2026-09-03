//! Multi-select mode: the batch-action bar and its wiring for deleting
//! several managed snapshots at once.

use std::collections::HashSet;
use std::path::PathBuf;

use btrfs_manager_helper::HelperRequest;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita::prelude::*;

use super::discovery::discover_and_load;
use super::errors::{show_error_toast, show_toast};
use super::helper_client::handle_privileged_async;
use super::i18n::tr;
use super::inventory::render_inventory;
use super::state::UiState;

/// Build the revealer-wrapped batch-action bar. Returns (revealer, delete, cancel).
pub(crate) fn build_bulk_action_bar() -> (gtk4::Revealer, gtk4::Button, gtk4::Button) {
    let bulk_delete_btn = gtk4::Button::builder()
        .label(tr("delete_selected"))
        .css_classes(["destructive-action"])
        .sensitive(false)
        .build();
    let bulk_cancel_btn = gtk4::Button::builder().label(tr("cancel")).build();
    let bulk_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk4::Align::End)
        .build();
    bulk_box.append(&bulk_cancel_btn);
    bulk_box.append(&bulk_delete_btn);
    let bulk_bar = gtk4::Revealer::builder()
        .transition_type(gtk4::RevealerTransitionType::SlideDown)
        .reveal_child(false)
        .child(&bulk_box)
        .build();
    (bulk_bar, bulk_delete_btn, bulk_cancel_btn)
}

pub(crate) fn update_bulk_bar(state: &UiState) {
    let count = state.selected.borrow().len();
    state.bulk_delete_btn.set_sensitive(count > 0);
    state
        .bulk_delete_btn
        .set_label(&format!("{} ({count})", tr("delete_selected")));
}

/// Wire the multi-select toggle, cancel button, and batch-delete button.
pub(crate) fn wire_select_mode(
    ui_state: &UiState,
    list: &gtk4::ListBox,
    search: &gtk4::SearchEntry,
    filesystem_selector: &gtk4::ComboBoxText,
    select_toggle: &gtk4::ToggleButton,
    bulk_cancel_btn: &gtk4::Button,
) {
    wire_select_toggle(ui_state, list, search, select_toggle);
    wire_row_selection_sync(ui_state, list);

    let cancel_toggle = select_toggle.clone();
    bulk_cancel_btn.connect_clicked(move |_| {
        cancel_toggle.set_active(false);
    });

    wire_bulk_delete_button(ui_state, list, search, filesystem_selector, select_toggle);
}

/// Toggling select mode: switch the list's GTK selection mode (which is what
/// actually gives us ctrl/shift-click, ctrl+A, and shift-arrow range select
/// for free), reveal the batch bar, and re-render. Toggling off clears the
/// selection.
fn wire_select_toggle(
    ui_state: &UiState,
    list: &gtk4::ListBox,
    search: &gtk4::SearchEntry,
    select_toggle: &gtk4::ToggleButton,
) {
    let list_for_select = list.clone();
    let search_for_select = search.clone();
    let state_for_select = ui_state.clone();
    select_toggle.connect_toggled(move |btn| {
        let active = btn.is_active();
        state_for_select.select_mode.set(active);
        state_for_select.selected.borrow_mut().clear();
        state_for_select.bulk_bar.set_reveal_child(active);
        list_for_select.set_selection_mode(if active {
            gtk4::SelectionMode::Multiple
        } else {
            gtk4::SelectionMode::None
        });
        if !active {
            list_for_select.unselect_all();
        }
        update_bulk_bar(&state_for_select);
        if let Some(inventory) = state_for_select.inventory.borrow().as_ref() {
            render_inventory(
                &list_for_select,
                inventory,
                search_for_select.text().as_str(),
                state_for_select.clone(),
            );
        }
    });
}

/// The single source of truth for `selected` while in select mode: whatever
/// GTK reports as the list's selected rows (via click, ctrl-click,
/// shift-click, shift-arrow, or ctrl+A), translated back to snapshot paths
/// through `row_paths`. Suppressed during render_inventory's clear/rebuild —
/// see `UiState::suppress_selection_signal`.
fn wire_row_selection_sync(ui_state: &UiState, list: &gtk4::ListBox) {
    let state = ui_state.clone();
    list.connect_selected_rows_changed(move |list| {
        if state.suppress_selection_signal.get() {
            return;
        }
        let row_paths = state.row_paths.borrow();
        let selected: HashSet<PathBuf> = list
            .selected_rows()
            .into_iter()
            .filter_map(|row| row_paths.get(&row).cloned())
            .collect();
        drop(row_paths);
        *state.selected.borrow_mut() = selected;
        update_bulk_bar(&state);
    });
}

/// Batch-delete button: confirm via dialog, then call the helper for every
/// selected path and refresh the inventory from disk.
fn wire_bulk_delete_button(
    ui_state: &UiState,
    list: &gtk4::ListBox,
    search: &gtk4::SearchEntry,
    filesystem_selector: &gtk4::ComboBoxText,
    select_toggle: &gtk4::ToggleButton,
) {
    let list_for_bulk = list.clone();
    let search_for_bulk = search.clone();
    let selector_for_bulk = filesystem_selector.clone();
    let state_for_bulk = ui_state.clone();
    let toggle_for_bulk = select_toggle.clone();
    ui_state.bulk_delete_btn.connect_clicked(move |btn| {
        let paths: Vec<PathBuf> = state_for_bulk.selected.borrow().iter().cloned().collect();
        if paths.is_empty() {
            return;
        }
        let Some(mountpoint) = state_for_bulk
            .inventory
            .borrow()
            .as_ref()
            .map(|inv| inv.mountpoint.clone())
        else {
            return;
        };
        let dialog = libadwaita::AlertDialog::builder()
            .heading(tr("delete_selected"))
            .body(format!(
                "Permanently delete {} snapshot(s)? This cannot be undone.",
                paths.len()
            ))
            .build();
        dialog.add_response("cancel", tr("cancel"));
        dialog.add_response("delete", tr("delete_selected"));
        dialog.set_response_appearance("delete", libadwaita::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));

        let list_c = list_for_bulk.clone();
        let search_c = search_for_bulk.clone();
        let selector_c = selector_for_bulk.clone();
        let state_c = state_for_bulk.clone();
        let toggle_c = toggle_for_bulk.clone();
        dialog.connect_response(None, move |_, response| {
            if response != "delete" {
                return;
            }
            let mountpoint = mountpoint.clone();
            let paths = paths.clone();
            let list_c = list_c.clone();
            let search_c = search_c.clone();
            let selector_c = selector_c.clone();
            let state_c = state_c.clone();
            let toggle_c = toggle_c.clone();
            state_c.spinner.start();
            glib::MainContext::default().spawn_local(async move {
                let result = handle_privileged_async(HelperRequest::DeleteManagedSnapshots {
                    mountpoint,
                    subvolume_paths: paths,
                })
                .await;
                state_c.spinner.stop();
                match result {
                    Ok(response) => show_toast(&state_c.toast_overlay, &response.message),
                    Err(err) => show_error_toast(&state_c.toast_overlay, "delete_snapshots", &err),
                }
                // Leave select mode and refresh from disk so pruned rows and any
                // partial failures are reflected accurately.
                toggle_c.set_active(false);
                discover_and_load(list_c, state_c, selector_c, search_c.text().to_string());
            });
        });
        let window = btn.root().and_downcast::<gtk4::Window>();
        dialog.present(window.as_ref());
    });
}
