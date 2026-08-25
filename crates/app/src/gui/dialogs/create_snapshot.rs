//! Dialog for creating a managed snapshot of a subvolume.

use std::path::PathBuf;

use btrfs_manager_core::Subvolume;
use btrfs_manager_helper::HelperRequest;
use gtk4::prelude::*;
use libadwaita::prelude::*;

use crate::gui::discovery::load_mountpoint;
use crate::gui::errors::{show_toast, user_error};
use crate::gui::helper_client::handle_privileged;
use crate::gui::state::UiState;
use crate::gui::widgets::{dialog_content_box, labeled_widget};

/// Widgets whose values/handlers are needed after the dialog body is built.
struct SnapshotDialogBody {
    content: gtk4::Box,
    snap_root_entry: gtk4::Entry,
    tags_entry: gtk4::Entry,
    cancel_btn: gtk4::Button,
    create_btn: gtk4::Button,
}

/// Build the dialog body: subvolume title, snapshot-root entry, help text,
/// tags entry, and the Cancel/Create button row.
fn build_dialog_body(subvolume: &Subvolume) -> SnapshotDialogBody {
    let content = dialog_content_box();

    let title = gtk4::Label::builder()
        .label(format!("Snapshot of {}", subvolume.path.display()))
        .halign(gtk4::Align::Start)
        .css_classes(["title-3"])
        .build();
    content.append(&title);

    // Snapshot root is relative to the Btrfs volume root (e.g. "@snapshots").
    // The helper will mount the top-level, find this dir, and create the snapshot there.
    let snap_root_entry = gtk4::Entry::builder()
        .text("@btrfs-manager")
        .hexpand(true)
        .build();
    content.append(&labeled_widget("Snapshot root", &snap_root_entry));

    let info = gtk4::Label::builder()
        .label("Path relative to the Btrfs volume root. Created automatically if it does not exist. Snapshot name: managed-<timestamp>.")
        .halign(gtk4::Align::Start)
        .wrap(true)
        .css_classes(["caption", "dim-label"])
        .build();
    content.append(&info);

    let tags_entry = gtk4::Entry::builder()
        .placeholder_text("Optional: comma-separated tags")
        .hexpand(true)
        .build();
    content.append(&labeled_widget("Tags", &tags_entry));

    let buttons = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk4::Align::End)
        .margin_top(8)
        .build();
    let cancel_btn = gtk4::Button::builder().label("Cancel").build();
    let create_btn = gtk4::Button::builder()
        .label("Create")
        .css_classes(["suggested-action"])
        .build();
    buttons.append(&cancel_btn);
    buttons.append(&create_btn);
    content.append(&buttons);

    SnapshotDialogBody {
        content,
        snap_root_entry,
        tags_entry,
        cancel_btn,
        create_btn,
    }
}

pub(crate) fn open_create_snapshot_dialog(
    state: UiState,
    list: gtk4::ListBox,
    mountpoint: PathBuf,
    subvolume: Subvolume,
) {
    let window = libadwaita::Window::builder()
        .title("Create Snapshot")
        .default_width(420)
        .modal(true)
        .build();

    let SnapshotDialogBody {
        content,
        snap_root_entry,
        tags_entry,
        cancel_btn,
        create_btn,
    } = build_dialog_body(&subvolume);

    let header = libadwaita::HeaderBar::new();
    let toolbar_view = libadwaita::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content));

    window.set_content(Some(&toolbar_view));

    let window_for_cancel = window.clone();
    cancel_btn.connect_clicked(move |_| window_for_cancel.close());

    let window_for_create = window.clone();
    create_btn.connect_clicked(move |_| {
        let snapshot_root = PathBuf::from(snap_root_entry.text().as_str().trim());
        let tags: Vec<String> = tags_entry
            .text()
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();

        match handle_privileged(HelperRequest::CreateManagedSnapshot {
            mountpoint: mountpoint.clone(),
            subvolume_path: subvolume.path.clone(),
            snapshot_root,
            tags,
        }) {
            Ok(_) => {
                window_for_create.close();
                show_toast(&state.toast_overlay, "Snapshot created");
                load_mountpoint(
                    list.clone(),
                    state.clone(),
                    String::new(),
                    mountpoint.clone(),
                );
            }
            Err(err) => show_toast(&state.toast_overlay, &user_error("create_snapshot", &err)),
        }
    });

    window.present();
}
