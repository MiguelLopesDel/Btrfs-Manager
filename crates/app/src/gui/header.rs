//! Header bar and summary card: top-of-window controls and the card that
//! shows scope/counts/filters above the inventory list.

use gtk4::glib;
use gtk4::prelude::*;

use super::dialogs::{open_diagnostics_window, open_rollback_status_window};
use super::discovery::discover_and_load;
use super::errors::{show_toast, user_error};
use super::helper_client::handle_privileged_async;
use super::i18n::tr;
use super::state::UiState;
use btrfs_manager_helper::HelperRequest;

/// Header-bar widgets that outlive build_header.
pub(crate) struct HeaderControls {
    pub(crate) header: libadwaita::HeaderBar,
    pub(crate) spinner: gtk4::Spinner,
    pub(crate) refresh: gtk4::Button,
    pub(crate) cleanup: gtk4::Button,
    pub(crate) rollback_status: gtk4::Button,
    pub(crate) diagnostics: gtk4::Button,
}

/// Summary card labels updated on every render.
pub(crate) struct SummaryPanel {
    pub(crate) widget: gtk4::Box,
    pub(crate) scope: gtk4::Label,
    pub(crate) counts: gtk4::Label,
    pub(crate) filters: gtk4::Label,
}

pub(crate) fn build_header() -> HeaderControls {
    let header = libadwaita::HeaderBar::new();
    header.set_title_widget(Some(
        &libadwaita::WindowTitle::builder()
            .title("Btrfs Manager")
            .subtitle(tr("snapshots"))
            .build(),
    ));

    let spinner = gtk4::Spinner::new();
    let refresh = gtk4::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text(tr("refresh"))
        .build();
    let cleanup = gtk4::Button::builder()
        .icon_name("media-eject-symbolic")
        .tooltip_text(tr("cleanup_mounts"))
        .build();
    let rollback_status = gtk4::Button::builder()
        .icon_name("document-open-recent-symbolic")
        .tooltip_text(tr("rollback_status"))
        .build();
    let diagnostics = gtk4::Button::builder()
        .icon_name("utilities-system-monitor-symbolic")
        .tooltip_text(tr("system_diagnostics"))
        .build();
    header.pack_end(&cleanup);
    header.pack_end(&diagnostics);
    header.pack_end(&rollback_status);
    header.pack_end(&refresh);
    header.pack_start(&spinner);

    HeaderControls {
        header,
        spinner,
        refresh,
        cleanup,
        rollback_status,
        diagnostics,
    }
}

pub(crate) fn build_summary_panel() -> SummaryPanel {
    let scope = gtk4::Label::builder()
        .label(tr("no_filesystem_selected"))
        .halign(gtk4::Align::Start)
        .hexpand(true)
        .ellipsize(gtk4::pango::EllipsizeMode::Middle)
        .css_classes(["heading"])
        .build();
    let counts = gtk4::Label::builder()
        .label("Snapshots 0 · Subvolumes 0")
        .halign(gtk4::Align::Start)
        .css_classes(["caption", "dim-label"])
        .build();
    let filters = gtk4::Label::builder()
        .label("All · 7 days · By day")
        .halign(gtk4::Align::Start)
        .css_classes(["caption", "dim-label"])
        .build();
    let text = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .margin_top(12)
        .margin_bottom(12)
        .margin_end(14)
        .build();
    text.append(&scope);
    text.append(&counts);
    text.append(&filters);
    let icon = gtk4::Image::builder()
        .icon_name("drive-harddisk-symbolic")
        .pixel_size(32)
        .valign(gtk4::Align::Center)
        .margin_start(14)
        .margin_top(12)
        .margin_bottom(12)
        .css_classes(["accent"])
        .build();
    let widget = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(14)
        .margin_top(2)
        .margin_bottom(4)
        .css_classes(["card"])
        .build();
    widget.append(&icon);
    widget.append(&text);

    SummaryPanel {
        widget,
        scope,
        counts,
        filters,
    }
}

pub(crate) fn wire_header_actions(
    ui_state: &UiState,
    controls: &HeaderControls,
    list: &gtk4::ListBox,
    search: &gtk4::SearchEntry,
    filesystem_selector: &gtk4::ComboBoxText,
) {
    let state_for_cleanup = ui_state.clone();
    controls.cleanup.connect_clicked(move |_| {
        let state = state_for_cleanup.clone();
        glib::MainContext::default().spawn_local(async move {
            match handle_privileged_async(HelperRequest::CleanupManagedMounts).await {
                Ok(response) => {
                    state.mounted_snapshots.borrow_mut().clear();
                    state.session_mounts.borrow_mut().clear();
                    show_toast(&state.toast_overlay, &response.message);
                }
                Err(err) => show_toast(
                    &state.toast_overlay,
                    &user_error("unmount_temporary_mounts", &err),
                ),
            }
        });
    });

    let list_for_refresh = list.clone();
    let search_for_refresh = search.clone();
    let state_for_refresh = ui_state.clone();
    let selector_for_refresh = filesystem_selector.clone();
    controls.refresh.connect_clicked(move |_| {
        discover_and_load(
            list_for_refresh.clone(),
            state_for_refresh.clone(),
            selector_for_refresh.clone(),
            search_for_refresh.text().to_string(),
        );
    });

    let state_for_rollback_status = ui_state.clone();
    controls.rollback_status.connect_clicked(move |btn| {
        if let Some(window) = btn.root().and_downcast::<gtk4::Window>() {
            open_rollback_status_window(&window, state_for_rollback_status.clone());
        }
    });

    let state_for_diagnostics = ui_state.clone();
    controls.diagnostics.connect_clicked(move |btn| {
        if let Some(window) = btn.root().and_downcast::<gtk4::Window>() {
            open_diagnostics_window(&window, state_for_diagnostics.clone());
        }
    });
}
