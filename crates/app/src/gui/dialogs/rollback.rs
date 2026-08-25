//! Rollback prompts and the rollback status window.

use std::path::Path;

use btrfs_manager_core::{RollbackPlan, RollbackPrompt, Subvolume};
use btrfs_manager_helper::{HelperRequest, HelperResponse, SubvolumeInventory};
use chrono::{DateTime, Local};
use gtk4::prelude::*;
use libadwaita::prelude::*;

use crate::gui::errors::{show_error_toast, show_toast, user_error};
use crate::gui::helper_client::handle_privileged;
use crate::gui::state::UiState;
use crate::gui::widgets::{append_icon_row, dialog_content_box, section_label};

pub(crate) fn check_pending_rollback(
    window: &gtk4::Window,
    toast_overlay: &libadwaita::ToastOverlay,
) {
    if let Ok(response) = handle_privileged(HelperRequest::GetPendingRollback) {
        if let Some(prompt) = rollback_prompt_from_response(response) {
            show_pending_rollback_dialog(window, toast_overlay, prompt);
        }
    }
}

fn rollback_prompt_from_response(response: HelperResponse) -> Option<RollbackPrompt> {
    let data = response.data?;
    if let Ok(prompt) = serde_json::from_value::<RollbackPrompt>(data.clone()) {
        Some(prompt)
    } else {
        serde_json::from_value::<RollbackPlan>(data)
            .ok()
            .map(|plan| RollbackPrompt {
                plan,
                rebooted_since_staging: false,
            })
    }
}

fn show_pending_rollback_dialog(
    window: &gtk4::Window,
    toast_overlay: &libadwaita::ToastOverlay,
    prompt: RollbackPrompt,
) {
    let plan = prompt.plan;
    let plan_id = plan.id;
    let src = plan
        .source_snapshot_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("snapshot")
        .to_string();
    let description = plan
        .description
        .as_deref()
        .unwrap_or("Previous system snapshot");
    let dialog = if prompt.rebooted_since_staging {
        libadwaita::AlertDialog::builder()
            .heading("Rollback booted")
            .body(format!(
                "The system appears to have rebooted into {src}.\n\nKeep this restored system, or revert to the saved return snapshot: {description}."
            ))
            .build()
    } else {
        libadwaita::AlertDialog::builder()
            .heading("Rollback staged")
            .body(format!(
                "A rollback to {src} is staged for the next reboot.\n\nReboot to activate it, or revert now to cancel and restore the saved return snapshot."
            ))
            .build()
    };
    if prompt.rebooted_since_staging {
        dialog.add_response("revert", "Revert");
        dialog.add_response("commit", "Keep restored system");
        dialog.add_response("later", "Decide later");
        dialog.set_default_response(Some("commit"));
        dialog.set_response_appearance("commit", libadwaita::ResponseAppearance::Suggested);
    } else {
        dialog.add_response("revert", "Cancel rollback");
        dialog.add_response("ok", "Keep staged");
        dialog.set_default_response(Some("ok"));
    }
    dialog.set_response_appearance("revert", libadwaita::ResponseAppearance::Destructive);

    let toast_overlay = toast_overlay.clone();
    let window_ref = window.clone();
    dialog.connect_response(None, move |_, response| {
        match response {
            "commit" => match handle_privileged(HelperRequest::CommitRollback { plan_id }) {
                Ok(_) => show_toast(&toast_overlay, "Rollback kept as the current system"),
                Err(err) => show_error_toast(&toast_overlay, "keep_rollback", &err),
            },
            "revert" => match handle_privileged(HelperRequest::RevertRollback { plan_id }) {
                Ok(_) => show_toast(
                    &toast_overlay,
                    "Rollback reverted — reboot to return to the previous system",
                ),
                Err(err) => show_error_toast(&toast_overlay, "revert_rollback", &err),
            },
            _ => return,
        }
        let _ = window_ref.activate_action("win.refresh", None);
    });
    dialog.present(Some(window));
}

pub(crate) fn open_rollback_status_window(parent: &gtk4::Window, state: UiState) {
    let window = libadwaita::Window::builder()
        .title("Rollback Status")
        .default_width(620)
        .default_height(680)
        .modal(true)
        .build();

    let header = libadwaita::HeaderBar::new();
    header.set_title_widget(Some(
        &libadwaita::WindowTitle::builder()
            .title("Rollback Status")
            .subtitle("Current state and recovery history")
            .build(),
    ));

    let content = dialog_content_box();
    content.set_spacing(14);

    let status_list = boxed_list();
    let anchors_list = boxed_list();
    let discarded_list = boxed_list();

    let inventory = state.inventory.borrow().clone();
    let pending_response = handle_privileged(HelperRequest::GetPendingRollback);
    let pending = pending_response
        .as_ref()
        .ok()
        .cloned()
        .and_then(rollback_prompt_from_response);

    append_rollback_summary_rows(&status_list, inventory.as_ref(), pending.as_ref());
    content.append(&section_label("State"));
    content.append(&status_list);

    content.append(&section_label("Return Anchors"));
    append_rollback_history_rows(
        &anchors_list,
        rollback_history_entries(inventory.as_ref(), rollback_anchor_path),
        "No return anchors found",
        "A return anchor is created before staging rollback.",
        "edit-undo-symbolic",
    );
    content.append(&anchors_list);

    content.append(&section_label("Discarded Roots"));
    append_rollback_history_rows(
        &discarded_list,
        rollback_history_entries(inventory.as_ref(), discarded_root_path),
        "No discarded roots found",
        "Discarded roots appear after keeping or reverting a rollback.",
        "user-trash-symbolic",
    );
    content.append(&discarded_list);

    if let Some(prompt) = pending {
        content.append(&build_pending_actions(&window, &state, &prompt));
    } else if let Err(err) = pending_response {
        let warning = gtk4::Label::builder()
            .label(user_error("read_rollback_state", &err))
            .halign(gtk4::Align::Start)
            .wrap(true)
            .css_classes(["caption", "error"])
            .build();
        content.append(&warning);
    }

    let scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vexpand(true)
        .child(&content)
        .build();
    let toolbar_view = libadwaita::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&scroll));
    window.set_content(Some(&toolbar_view));
    window.set_transient_for(Some(parent));
    window.present();
}

fn boxed_list() -> gtk4::ListBox {
    gtk4::ListBox::builder()
        .selection_mode(gtk4::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build()
}

/// Action row shown at the bottom of the status window while a rollback is
/// pending: keep (after reboot), revert, or decide later.
fn build_pending_actions(
    window: &libadwaita::Window,
    state: &UiState,
    prompt: &RollbackPrompt,
) -> gtk4::Box {
    let actions = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk4::Align::End)
        .margin_top(4)
        .build();
    let later = gtk4::Button::builder().label("Decide Later").build();
    let revert_label = if prompt.rebooted_since_staging {
        "Revert"
    } else {
        "Cancel Rollback"
    };
    let revert = gtk4::Button::builder()
        .label(revert_label)
        .css_classes(["destructive-action"])
        .build();
    actions.append(&later);
    actions.append(&revert);

    let plan_id = prompt.plan.id;
    if prompt.rebooted_since_staging {
        let keep = gtk4::Button::builder()
            .label("Keep Restored System")
            .css_classes(["suggested-action"])
            .build();
        let state_for_keep = state.clone();
        let window_for_keep = window.clone();
        keep.connect_clicked(move |_| {
            match handle_privileged(HelperRequest::CommitRollback { plan_id }) {
                Ok(_) => {
                    show_toast(
                        &state_for_keep.toast_overlay,
                        "Rollback kept as the current system",
                    );
                    window_for_keep.close();
                }
                Err(err) => show_toast(
                    &state_for_keep.toast_overlay,
                    &user_error("keep_rollback", &err),
                ),
            }
        });
        actions.append(&keep);
    }

    let state_for_revert = state.clone();
    let window_for_revert = window.clone();
    revert.connect_clicked(move |_| {
        match handle_privileged(HelperRequest::RevertRollback { plan_id }) {
            Ok(_) => {
                show_toast(
                    &state_for_revert.toast_overlay,
                    "Rollback reverted — reboot to return to the previous system",
                );
                window_for_revert.close();
            }
            Err(err) => show_toast(
                &state_for_revert.toast_overlay,
                &user_error("revert_rollback", &err),
            ),
        }
    });

    let window_for_later = window.clone();
    later.connect_clicked(move |_| window_for_later.close());
    actions
}

fn append_rollback_summary_rows(
    list: &gtk4::ListBox,
    inventory: Option<&SubvolumeInventory>,
    pending: Option<&RollbackPrompt>,
) {
    if let Some(prompt) = pending {
        let plan = &prompt.plan;
        let staged_at: chrono::DateTime<Local> = DateTime::from(plan.created_at);
        append_icon_row(
            list,
            if prompt.rebooted_since_staging {
                "Rollback booted"
            } else {
                "Rollback staged"
            },
            &format!(
                "Source: {} · return anchor: {}",
                plan.source_snapshot_path.display(),
                plan.return_snapshot_path.display()
            ),
            "system-reboot-symbolic",
        );
        append_icon_row(
            list,
            "Current root target",
            &format!(
                "{} · staged {}",
                plan.replaced_subvol_path.display(),
                staged_at.format("%Y-%m-%d %H:%M")
            ),
            "drive-harddisk-symbolic",
        );
        let next_step = if prompt.rebooted_since_staging {
            "Choose Keep Restored System to accept this boot, or Revert to restore the return anchor and reboot."
        } else {
            "Reboot to activate the restored snapshot, or cancel rollback before reboot."
        };
        append_icon_row(list, "Next step", next_step, "go-next-symbolic");
    } else {
        append_icon_row(
            list,
            "No pending rollback",
            "The system is not waiting for rollback confirmation.",
            "emblem-ok-symbolic",
        );
        let root = inventory
            .and_then(current_mount_subvolume)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "Unknown active subvolume".into());
        append_icon_row(list, "Current root", &root, "drive-harddisk-symbolic");
        append_icon_row(
            list,
            "Next step",
            "Stage rollback from a managed snapshot when you want to test or restore it.",
            "go-next-symbolic",
        );
    }
}

fn append_rollback_history_rows(
    list: &gtk4::ListBox,
    entries: Vec<&Subvolume>,
    empty_title: &str,
    empty_subtitle: &str,
    icon: &str,
) {
    if entries.is_empty() {
        append_icon_row(
            list,
            empty_title,
            empty_subtitle,
            "dialog-information-symbolic",
        );
        return;
    }
    for subvolume in entries {
        append_icon_row(
            list,
            &subvolume.path.display().to_string(),
            &rollback_entry_subtitle(subvolume),
            icon,
        );
    }
}

fn rollback_history_entries(
    inventory: Option<&SubvolumeInventory>,
    predicate: fn(&Path) -> bool,
) -> Vec<&Subvolume> {
    let mut entries = inventory
        .map(|inventory| {
            inventory
                .subvolumes
                .iter()
                .filter(|subvolume| predicate(&subvolume.path))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    entries.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.path.cmp(&b.path)));
    entries
}

fn rollback_entry_subtitle(subvolume: &Subvolume) -> String {
    let mut parts = vec![format!("ID {}", subvolume.id.0)];
    if let Some(created_at) = subvolume.created_at {
        let local: chrono::DateTime<Local> = DateTime::from(created_at);
        parts.push(local.format("%Y-%m-%d %H:%M").to_string());
    }
    if subvolume.readonly {
        parts.push("read-only".into());
    } else {
        parts.push("writable".into());
    }
    if !subvolume.tags.is_empty() {
        parts.push(subvolume.tags.join(", "));
    }
    parts.join(" · ")
}

fn current_mount_subvolume(inventory: &SubvolumeInventory) -> Option<&Path> {
    inventory
        .subvolumes
        .iter()
        .find(|subvolume| subvolume.mountpoint.as_deref() == Some(inventory.mountpoint.as_path()))
        .or_else(|| {
            inventory
                .subvolumes
                .iter()
                .find(|subvolume| subvolume.mountpoint.as_deref() == Some(Path::new("/")))
        })
        .map(|subvolume| subvolume.path.as_path())
}

fn rollback_anchor_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("return-"))
        && path.starts_with("@btrfs-manager")
}

fn discarded_root_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("discarded-"))
        && path.starts_with("@btrfs-manager")
}
