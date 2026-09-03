//! Filesystem discovery and inventory loading for the main window.

use std::path::PathBuf;

use btrfs_manager_helper::{FilesystemDiscovery, HelperRequest, SubvolumeInventory};
use gtk4::glib;
use gtk4::prelude::*;

use super::errors::{set_error_status_row, show_toast};
use super::helper_client::handle_privileged_async;
use super::i18n::{reconciled_message, tr};
use super::inventory::render_inventory;
use super::state::UiState;
use super::widgets::{clear_list, set_status_row};

pub(crate) fn discover_and_load(
    list: gtk4::ListBox,
    state: UiState,
    selector: gtk4::ComboBoxText,
    query: String,
) {
    if let Some(mountpoint) = configured_mountpoint_override() {
        state.suppress_selector_signal.set(true);
        selector.remove_all();
        selector.append_text(&format!("Override: {}", mountpoint.display()));
        selector.set_active(Some(0));
        state.suppress_selector_signal.set(false);
        selector.set_sensitive(false);
        load_mountpoint(list, state, query, mountpoint);
        return;
    }

    set_status_row(&list, tr("loading"), "Discovering Btrfs filesystems…");
    state.spinner.start();
    selector.set_sensitive(false);

    glib::MainContext::default().spawn_local(async move {
        let result = handle_privileged_async(HelperRequest::DiscoverFilesystems).await;
        state.spinner.stop();
        selector.set_sensitive(true);
        match result {
            Ok(response) => match response.data {
                Some(data) => match serde_json::from_value::<FilesystemDiscovery>(data) {
                    Ok(discovery) => apply_discovery(list, state, selector, query, discovery),
                    Err(err) => {
                        let err = anyhow::Error::from(err);
                        set_error_status_row(&list, "read_filesystem_discovery", &err);
                    }
                },
                None => set_status_row(&list, tr("no_discovery_data"), &response.message),
            },
            Err(err) => set_error_status_row(&list, "discover_filesystems", &err),
        }
    });
}

fn apply_discovery(
    list: gtk4::ListBox,
    state: UiState,
    selector: gtk4::ComboBoxText,
    query: String,
    discovery: FilesystemDiscovery,
) {
    state.suppress_selector_signal.set(true);
    selector.remove_all();
    for filesystem in &discovery.filesystems {
        selector.append_text(&filesystem_label(filesystem));
    }
    let active_index = preferred_filesystem_index(&discovery);
    *state.filesystems.borrow_mut() = discovery;
    if let Some(index) = active_index {
        selector.set_active(Some(index as u32));
        state.suppress_selector_signal.set(false);
        let mountpoint = {
            let filesystems = state.filesystems.borrow();
            selected_mountpoint(&filesystems, index)
        };
        if let Some(mountpoint) = mountpoint {
            load_mountpoint(list, state, query, mountpoint);
        }
    } else {
        state.suppress_selector_signal.set(false);
        set_status_row(
            &list,
            tr("no_btrfs_filesystems"),
            tr("discovery_returned_no_mountpoints"),
        );
    }
}

pub(crate) fn configured_mountpoint_override() -> Option<PathBuf> {
    std::env::var_os("BTRFS_MANAGER_MOUNTPOINT").map(PathBuf::from)
}

pub(crate) fn load_mountpoint(
    list: gtk4::ListBox,
    state: UiState,
    query: String,
    mountpoint: PathBuf,
) {
    clear_list(&list);
    set_status_row(&list, tr("loading"), &mountpoint.display().to_string());
    state.spinner.start();

    glib::MainContext::default().spawn_local(async move {
        let result = handle_privileged_async(HelperRequest::ListSubvolumes {
            mountpoint: mountpoint.clone(),
        })
        .await;
        state.spinner.stop();
        match result {
            Ok(response) => match response.data {
                Some(data) => match serde_json::from_value::<SubvolumeInventory>(data) {
                    Ok(inventory) => {
                        if inventory.reconciled_external_deletions > 0 {
                            show_toast(
                                &state.toast_overlay,
                                &reconciled_message(inventory.reconciled_external_deletions),
                            );
                        }
                        *state.inventory.borrow_mut() = Some(inventory.clone());
                        render_inventory(&list, &inventory, &query, state);
                    }
                    Err(err) => {
                        let err = anyhow::Error::from(err);
                        set_error_status_row(&list, "read_inventory", &err);
                    }
                },
                None => set_status_row(&list, tr("no_structured_data"), &response.message),
            },
            Err(err) => set_error_status_row(&list, "read_inventory", &err),
        }
    });
}

pub(crate) fn preferred_filesystem_index(discovery: &FilesystemDiscovery) -> Option<usize> {
    discovery
        .filesystems
        .iter()
        .position(|filesystem| filesystem.mounts.iter().any(|mount| mount.is_active_root))
        .or_else(|| (!discovery.filesystems.is_empty()).then_some(0))
}

pub(crate) fn selected_mountpoint(
    discovery: &FilesystemDiscovery,
    index: usize,
) -> Option<PathBuf> {
    let filesystem = discovery.filesystems.get(index)?;
    filesystem
        .mounts
        .iter()
        .find(|mount| mount.is_active_root)
        .or_else(|| filesystem.mounts.first())
        .map(|mount| mount.mountpoint.clone())
}

pub(crate) fn filesystem_label(
    filesystem: &btrfs_manager_core::models::FilesystemSummary,
) -> String {
    filesystem
        .devices
        .first()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|| "unknown device".to_string())
}
