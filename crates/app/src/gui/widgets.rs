//! Small reusable widget builders shared by the main window and dialogs.

use btrfs_manager_core::Subvolume;
use gtk4::prelude::*;
use libadwaita::prelude::*;

pub(crate) fn clear_list(list: &gtk4::ListBox) {
    while let Some(row) = list.first_child() {
        list.remove(&row);
    }
}

/// Replace the list content with a centered status placeholder (empty,
/// loading, and error states), in the spirit of adw::StatusPage.
pub(crate) fn set_status_row(list: &gtk4::ListBox, title: &str, subtitle: &str) {
    clear_list(list);
    let icon = gtk4::Image::builder()
        .icon_name("drive-harddisk-symbolic")
        .pixel_size(56)
        .css_classes(["dim-label"])
        .build();
    let title_label = gtk4::Label::builder()
        .label(title)
        .halign(gtk4::Align::Center)
        .justify(gtk4::Justification::Center)
        .wrap(true)
        .css_classes(["title-2"])
        .build();
    let subtitle_label = gtk4::Label::builder()
        .label(subtitle)
        .halign(gtk4::Align::Center)
        .justify(gtk4::Justification::Center)
        .wrap(true)
        .max_width_chars(60)
        .css_classes(["dim-label"])
        .build();
    let page = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(10)
        .margin_top(64)
        .margin_bottom(64)
        .margin_start(24)
        .margin_end(24)
        .halign(gtk4::Align::Center)
        .valign(gtk4::Align::Center)
        .build();
    page.append(&icon);
    page.append(&title_label);
    page.append(&subtitle_label);
    let row = gtk4::ListBoxRow::builder()
        .selectable(false)
        .activatable(false)
        .child(&page)
        .build();
    list.append(&row);
}

pub(crate) fn append_info_row(list: &gtk4::ListBox, title: &str, subtitle: &str) {
    append_icon_row(list, title, subtitle, "dialog-information-symbolic");
}

pub(crate) fn append_icon_row(list: &gtk4::ListBox, title: &str, subtitle: &str, icon: &str) {
    let row = libadwaita::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .build();
    row.add_prefix(
        &gtk4::Image::builder()
            .icon_name(icon)
            .pixel_size(20)
            .valign(gtk4::Align::Center)
            .build(),
    );
    row.set_selectable(false);
    list.append(&row);
}

pub(crate) fn append_date_subheader(list: &gtk4::ListBox, label: &str) {
    let lbl = gtk4::Label::builder()
        .label(label)
        .halign(gtk4::Align::Start)
        .margin_top(6)
        .margin_bottom(2)
        .margin_start(18)
        .margin_end(12)
        .css_classes(["caption", "dim-label"])
        .build();
    let row = gtk4::ListBoxRow::builder()
        .selectable(false)
        .activatable(false)
        .child(&lbl)
        .build();
    list.append(&row);
}

pub(crate) fn append_section_header(list: &gtk4::ListBox, title: &str) {
    let label = gtk4::Label::builder()
        .label(title)
        .halign(gtk4::Align::Start)
        .margin_top(14)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .css_classes(["heading", "dim-label"])
        .build();
    let row = gtk4::ListBoxRow::builder()
        .selectable(false)
        .activatable(false)
        .child(&label)
        .build();
    list.append(&row);
}

pub(crate) fn section_label(label: &str) -> gtk4::Label {
    gtk4::Label::builder()
        .label(label)
        .halign(gtk4::Align::Start)
        .css_classes(["heading", "dim-label"])
        .build()
}

pub(crate) fn snapshot_prefix_icon(snapshot: &Subvolume, is_mounted: bool) -> gtk4::Image {
    let icon_name = if is_mounted {
        "drive-harddisk-symbolic"
    } else if snapshot.unlocked {
        "changes-allow-symbolic"
    } else if snapshot.managed {
        "camera-photo-symbolic"
    } else {
        "document-open-recent-symbolic"
    };
    gtk4::Image::builder()
        .icon_name(icon_name)
        .pixel_size(20)
        .valign(gtk4::Align::Center)
        .build()
}

pub(crate) fn linked_button_group() -> gtk4::Box {
    gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(0)
        .valign(gtk4::Align::Center)
        .css_classes(["linked"])
        .build()
}

pub(crate) fn labeled_widget(label: &str, widget: &impl IsA<gtk4::Widget>) -> gtk4::Box {
    let row = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(12)
        .build();
    let label = gtk4::Label::builder()
        .label(label)
        .halign(gtk4::Align::Start)
        .hexpand(true)
        .build();
    row.append(&label);
    row.append(widget);
    row
}

pub(crate) fn retention_spin(value: usize) -> gtk4::SpinButton {
    let spin = gtk4::SpinButton::with_range(0.0, 500.0, 1.0);
    spin.set_value(value as f64);
    spin
}

pub(crate) fn dialog_content_box() -> gtk4::Box {
    gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build()
}
