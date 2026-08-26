//! A single snapshot row: browse/unmount, lock/unlock, delete, and rollback
//! actions, each wired by its own builder to keep the row assembly readable.

use std::path::{Path, PathBuf};

use btrfs_manager_core::Subvolume;
use btrfs_manager_helper::HelperRequest;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita::prelude::*;

use super::discovery::load_mountpoint;
use super::errors::{show_toast, user_error};
use super::helper_client::{handle_privileged, handle_privileged_async};
use super::i18n::tr;
use super::inventory_query::{snapshot_display_title, snapshot_subtitle};
use super::mounts::{browse_mount_target, browse_snapshot_readonly};
use super::state::UiState;
use super::widgets::{linked_button_group, snapshot_prefix_icon};

pub(crate) fn render_snapshot_row(
    list: &gtk4::ListBox,
    snapshot: &Subvolume,
    mountpoint: &Path,
    state: UiState,
) {
    let mountpoint = mountpoint.to_path_buf();
    let target = browse_mount_target(&snapshot.path);
    let is_mounted = state.mounted_snapshots.borrow().contains(&target);
    let subtitle = snapshot_subtitle(
        snapshot.id.0,
        &snapshot.path,
        snapshot.unlocked,
        is_mounted,
        &target,
        &snapshot.tags,
    );
    let row = libadwaita::ActionRow::builder()
        .title(snapshot_display_title(snapshot))
        .subtitle(subtitle)
        .build();
    row.add_prefix(&snapshot_prefix_icon(snapshot, is_mounted));
    if snapshot.managed {
        state
            .row_paths
            .borrow_mut()
            .insert(row.clone().upcast(), snapshot.path.clone());
    } else {
        // Only managed snapshots can be batch-deleted — external ones must
        // never be selectable via click/ctrl+A/shift-click.
        row.set_selectable(false);
    }
    attach_browse_actions(&row, snapshot, &mountpoint, target, is_mounted, &state);
    if snapshot.managed {
        attach_managed_actions(&row, list, snapshot, &mountpoint, &state);
    }
    list.append(&row);
    // Restore GTK's row selection after a rebuild (filter/search change while
    // in select mode) so previously-checked rows stay visually selected.
    if snapshot.managed && state.selected.borrow().contains(&snapshot.path) {
        list.select_row(Some(&row));
    }
}

fn attach_browse_actions(
    row: &libadwaita::ActionRow,
    snapshot: &Subvolume,
    mountpoint: &Path,
    target: PathBuf,
    is_mounted: bool,
    state: &UiState,
) {
    let browse = gtk4::Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text("Browse read-only")
        .valign(gtk4::Align::Center)
        .sensitive(!is_mounted)
        .build();
    let unmount = gtk4::Button::builder()
        .icon_name("media-eject-symbolic")
        .tooltip_text("Unmount browse view")
        .valign(gtk4::Align::Center)
        .sensitive(is_mounted)
        .build();

    let mountpoint = mountpoint.to_path_buf();
    wire_browse_button(&browse, &unmount, row, snapshot, mountpoint, state);

    let snapshot_id = snapshot.id.0;
    let unlocked = snapshot.unlocked;
    let row_for_unmount = row.clone();
    let browse_for_unmount = browse.clone();
    let unmount_for_unmount = unmount.clone();
    let state_for_unmount = state.clone();
    let tags_for_unmount = snapshot.tags.clone();
    let snap_path_for_unmount = snapshot.path.clone();
    unmount.connect_clicked(move |_| {
        let target = target.clone();
        let row = row_for_unmount.clone();
        let browse_btn = browse_for_unmount.clone();
        let unmount_btn = unmount_for_unmount.clone();
        let state = state_for_unmount.clone();
        let tags = tags_for_unmount.clone();
        let snap_path = snap_path_for_unmount.clone();
        glib::MainContext::default().spawn_local(async move {
            match handle_privileged_async(HelperRequest::UnmountSnapshot {
                target: target.clone(),
            })
            .await
            {
                Ok(_) => {
                    state.mounted_snapshots.borrow_mut().remove(&target);
                    row.set_subtitle(&snapshot_subtitle(
                        snapshot_id,
                        &snap_path,
                        unlocked,
                        false,
                        &target,
                        &tags,
                    ));
                    browse_btn.set_sensitive(true);
                    unmount_btn.set_sensitive(false);
                    show_toast(&state.toast_overlay, "Snapshot unmounted");
                }
                Err(err) => show_toast(&state.toast_overlay, &user_error("unmount_snapshot", &err)),
            }
        });
    });

    let browse_actions = linked_button_group();
    browse_actions.append(&browse);
    browse_actions.append(&unmount);
    row.add_suffix(&browse_actions);
}

/// Wire the "browse read-only" button: mount the snapshot, then flip sensitivity.
fn wire_browse_button(
    browse: &gtk4::Button,
    unmount: &gtk4::Button,
    row: &libadwaita::ActionRow,
    snapshot: &Subvolume,
    mountpoint: PathBuf,
    state: &UiState,
) {
    let snapshot_id = snapshot.id.0;
    let unlocked = snapshot.unlocked;
    let row_for_browse = row.clone();
    let browse_for_browse = browse.clone();
    let unmount_for_browse = unmount.clone();
    let state_for_browse = state.clone();
    let tags_for_browse = snapshot.tags.clone();
    let snap_path_for_browse = snapshot.path.clone();
    browse.connect_clicked(move |_| {
        let mountpoint = mountpoint.clone();
        let relative_path = snap_path_for_browse.clone();
        let row = row_for_browse.clone();
        let browse_btn = browse_for_browse.clone();
        let unmount_btn = unmount_for_browse.clone();
        let state = state_for_browse.clone();
        let tags = tags_for_browse.clone();
        state.spinner.start();
        glib::MainContext::default().spawn_local(async move {
            let snap_path = relative_path.clone();
            let result = gio::spawn_blocking(move || {
                browse_snapshot_readonly(mountpoint, relative_path, unlocked)
            })
            .await
            .map_err(|_| anyhow::anyhow!("browse thread panicked"))
            .and_then(|r| r);
            state.spinner.stop();
            match result {
                Ok(mounted) => {
                    state
                        .mounted_snapshots
                        .borrow_mut()
                        .insert(mounted.target.clone());
                    state
                        .session_mounts
                        .borrow_mut()
                        .extend(mounted.created_mounts.iter().cloned());
                    row.set_subtitle(&snapshot_subtitle(
                        snapshot_id,
                        &snap_path,
                        unlocked,
                        true,
                        &mounted.target,
                        &tags,
                    ));
                    browse_btn.set_sensitive(false);
                    unmount_btn.set_sensitive(true);
                    let msg = mounted.warning.as_deref().unwrap_or("Snapshot mounted");
                    show_toast(&state.toast_overlay, msg);
                }
                Err(err) => show_toast(&state.toast_overlay, &user_error("browse_snapshot", &err)),
            }
        });
    });
}

fn attach_managed_actions(
    row: &libadwaita::ActionRow,
    list: &gtk4::ListBox,
    snapshot: &Subvolume,
    mountpoint: &Path,
    state: &UiState,
) {
    let managed_actions = linked_button_group();
    let (unlock_btn, lock_btn) = build_lock_buttons(row, list, snapshot, mountpoint, state);
    managed_actions.append(&unlock_btn);
    managed_actions.append(&lock_btn);
    // Visual indicator for unlocked state
    if snapshot.unlocked {
        row.add_css_class("warning");
    }
    managed_actions.append(&build_delete_button(row, list, snapshot, mountpoint, state));
    managed_actions.append(&build_rollback_button(snapshot, mountpoint, state));
    row.add_suffix(&managed_actions);
}

/// The unlock/lock button pair — exactly one is visible at a time.
fn build_lock_toggle_buttons(is_unlocked: bool) -> (gtk4::Button, gtk4::Button) {
    let unlock = gtk4::Button::builder()
        .icon_name("changes-allow-symbolic")
        .tooltip_text("Unlock snapshot (make writable)")
        .valign(gtk4::Align::Center)
        .visible(!is_unlocked)
        .build();
    let lock = gtk4::Button::builder()
        .icon_name("changes-prevent-symbolic")
        .tooltip_text("Lock snapshot (make read-only)")
        .valign(gtk4::Align::Center)
        .visible(is_unlocked)
        .build();
    (unlock, lock)
}

fn build_lock_buttons(
    row: &libadwaita::ActionRow,
    list: &gtk4::ListBox,
    snapshot: &Subvolume,
    mountpoint: &Path,
    state: &UiState,
) -> (gtk4::Button, gtk4::Button) {
    let (unlock, lock) = build_lock_toggle_buttons(snapshot.unlocked);

    // Unlock: confirm via dialog, then call the helper and flip button/row state.
    let state_for_unlock = state.clone();
    let list_for_unlock = list.clone();
    let path_for_unlock = snapshot.path.clone();
    let mount_for_unlock = mountpoint.to_path_buf();
    let row_for_unlock = row.clone();
    let unlock_c = unlock.clone();
    let lock_c = lock.clone();
    unlock.connect_clicked(move |_| {
        let dialog = libadwaita::AlertDialog::builder()
            .heading("Unlock snapshot?")
            .body(format!(
                "Make {} writable?\n\nWriting to a snapshot breaks its integrity. Only unlock if you know what you are doing.",
                path_for_unlock.display()
            ))
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("unlock", "Unlock");
        dialog.set_response_appearance("unlock", libadwaita::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        let state_c = state_for_unlock.clone();
        let list_c = list_for_unlock.clone();
        let path_c = path_for_unlock.clone();
        let mount_c = mount_for_unlock.clone();
        let row_c = row_for_unlock.clone();
        let unlock_btn_c = unlock_c.clone();
        let lock_btn_c = lock_c.clone();
        let window_ref = unlock_btn_c.root().and_downcast::<gtk4::Window>();
        dialog.connect_response(None, move |_, response| {
            if response != "unlock" {
                return;
            }
            match handle_privileged(HelperRequest::SetManagedSnapshotReadOnly {
                mountpoint: mount_c.clone(),
                subvol_path: path_c.clone(),
                readonly: false,
            }) {
                Ok(_) => {
                    unlock_btn_c.set_visible(false);
                    lock_btn_c.set_visible(true);
                    row_c.add_css_class("warning");
                    show_toast(&state_c.toast_overlay, "Snapshot unlocked — handle with care");
                    load_mountpoint(
                        list_c.clone(),
                        state_c.clone(),
                        String::new(),
                        mount_c.clone(),
                    );
                }
                Err(err) => show_toast(&state_c.toast_overlay, &user_error("unlock_snapshot", &err)),
            }
        });
        dialog.present(window_ref.as_ref());
    });

    // Lock: call the helper directly and flip the button pair + row styling.
    let state_for_lock = state.clone();
    let list_for_lock = list.clone();
    let path_for_lock = snapshot.path.clone();
    let mount_for_lock = mountpoint.to_path_buf();
    let row_for_lock = row.clone();
    let unlock_c2 = unlock.clone();
    let lock_c2 = lock.clone();
    lock.connect_clicked(move |_| {
        match handle_privileged(HelperRequest::SetManagedSnapshotReadOnly {
            mountpoint: mount_for_lock.clone(),
            subvol_path: path_for_lock.clone(),
            readonly: true,
        }) {
            Ok(_) => {
                lock_c2.set_visible(false);
                unlock_c2.set_visible(true);
                row_for_lock.remove_css_class("warning");
                show_toast(&state_for_lock.toast_overlay, "Snapshot locked");
                load_mountpoint(
                    list_for_lock.clone(),
                    state_for_lock.clone(),
                    String::new(),
                    mount_for_lock.clone(),
                );
            }
            Err(err) => show_toast(
                &state_for_lock.toast_overlay,
                &user_error("lock_snapshot", &err),
            ),
        }
    });

    (unlock, lock)
}

fn build_delete_button(
    row: &libadwaita::ActionRow,
    list: &gtk4::ListBox,
    snapshot: &Subvolume,
    mountpoint: &Path,
    state: &UiState,
) -> gtk4::Button {
    let delete = gtk4::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Delete managed snapshot")
        .valign(gtk4::Align::Center)
        .css_classes(["destructive-action"])
        .build();
    let row_for_delete = row.clone();
    let state_for_delete = state.clone();
    let list_for_delete = list.clone();
    let path_for_delete = snapshot.path.clone();
    let mountpoint = mountpoint.to_path_buf();
    delete.connect_clicked(move |btn| {
        let dialog = libadwaita::AlertDialog::builder()
            .heading("Delete snapshot?")
            .body(format!(
                "Permanently delete {}? This cannot be undone.",
                path_for_delete.display()
            ))
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("delete", "Delete");
        dialog.set_response_appearance("delete", libadwaita::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        let row_c = row_for_delete.clone();
        let state_c = state_for_delete.clone();
        let list_c = list_for_delete.clone();
        let path_c = path_for_delete.clone();
        let mount_c = mountpoint.clone();
        dialog.connect_response(None, move |_, response| {
            if response != "delete" {
                return;
            }
            match handle_privileged(HelperRequest::DeleteManagedSnapshot {
                mountpoint: mount_c.clone(),
                subvolume_path: path_c.clone(),
            }) {
                Ok(_) => {
                    list_c.remove(&row_c);
                    show_toast(&state_c.toast_overlay, "Snapshot deleted");
                }
                Err(err) => {
                    show_toast(&state_c.toast_overlay, &user_error("delete_snapshot", &err))
                }
            }
        });
        let window = btn.root().and_downcast::<gtk4::Window>();
        dialog.present(window.as_ref());
    });
    delete
}

/// Rollback button — stages a reversible rollback to this snapshot.
fn build_rollback_button(snapshot: &Subvolume, mountpoint: &Path, state: &UiState) -> gtk4::Button {
    let rollback_btn = gtk4::Button::builder()
        .icon_name("system-reboot-symbolic")
        .tooltip_text("Stage rollback to this snapshot")
        .valign(gtk4::Align::Center)
        .build();
    let state_for_rb = state.clone();
    let path_for_rb = snapshot.path.clone();
    let mount_for_rb = mountpoint.to_path_buf();
    rollback_btn.connect_clicked(move |btn| {
        let window = btn.root().and_downcast::<gtk4::Window>();
        let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let snap_name = path_for_rb
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("snap");
        let anchor = PathBuf::from(format!("@btrfs-manager/return-{timestamp}"));
        let dialog = libadwaita::AlertDialog::builder()
            .heading(tr("stage_rollback_heading"))
            .body(tr("stage_rollback_body").replace("{name}", snap_name))
            .build();
        dialog.add_response("cancel", tr("cancel"));
        dialog.add_response("rollback", tr("stage_rollback_confirm"));
        dialog.set_response_appearance("rollback", libadwaita::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        let state_c = state_for_rb.clone();
        let mount_c = mount_for_rb.clone();
        let path_c = path_for_rb.clone();
        dialog.connect_response(None, move |_, response| {
            if response != "rollback" {
                return;
            }
            match handle_privileged(HelperRequest::StageRollback {
                mountpoint: mount_c.clone(),
                snapshot_path: path_c.clone(),
                return_snapshot_path: anchor.clone(),
            }) {
                Ok(_) => show_toast(&state_c.toast_overlay, tr("stage_rollback_staged")),
                Err(err) => show_toast(&state_c.toast_overlay, &user_error("stage_rollback", &err)),
            }
        });
        dialog.present(window.as_ref());
    });
    rollback_btn
}
