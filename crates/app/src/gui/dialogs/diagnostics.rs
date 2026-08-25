//! Read-only diagnostics window: helper, Btrfs, rollback, and scheduler checks.

use btrfs_manager_helper::{
    DiagnosticCheck, DiagnosticStatus, DiagnosticsReport, HelperRequest, HelperResponse,
};
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita::prelude::*;

use crate::gui::errors::{show_error_toast, user_error};
use crate::gui::helper_client::handle_privileged_async;
use crate::gui::i18n::tr;
use crate::gui::state::UiState;
use crate::gui::widgets::{append_icon_row, clear_list, dialog_content_box, section_label};

pub(crate) fn open_diagnostics_window(parent: &gtk4::Window, state: UiState) {
    let window = libadwaita::Window::builder()
        .title("Diagnostics")
        .default_width(640)
        .default_height(680)
        .modal(true)
        .build();
    let header = libadwaita::HeaderBar::new();
    header.set_title_widget(Some(
        &libadwaita::WindowTitle::builder()
            .title("Diagnostics")
            .subtitle("System readiness")
            .build(),
    ));

    let content = dialog_content_box();
    content.set_spacing(14);
    let list = gtk4::ListBox::builder()
        .selection_mode(gtk4::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    append_icon_row(
        &list,
        "Running checks",
        "Inspecting helper, Btrfs, rollback, and scheduler state",
        "view-refresh-symbolic",
    );
    content.append(&section_label("Checks"));
    content.append(&list);

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

    let list_for_result = list.clone();
    let state_for_result = state.clone();
    glib::MainContext::default().spawn_local(async move {
        match handle_privileged_async(HelperRequest::RunDiagnostics).await {
            Ok(response) => match diagnostics_report_from_response(response) {
                Ok(report) => render_diagnostics_report(&list_for_result, &report),
                Err(err) => render_diagnostics_error(&list_for_result, &err),
            },
            Err(err) => {
                render_diagnostics_error(&list_for_result, &err);
                show_error_toast(&state_for_result.toast_overlay, "run_diagnostics", &err);
            }
        }
    });
}

fn render_diagnostics_error(list: &gtk4::ListBox, err: &anyhow::Error) {
    clear_list(list);
    append_icon_row(
        list,
        tr("run_diagnostics"),
        &user_error("run_diagnostics", err),
        "dialog-error-symbolic",
    );
}

fn diagnostics_report_from_response(response: HelperResponse) -> anyhow::Result<DiagnosticsReport> {
    let data = response
        .data
        .ok_or_else(|| anyhow::anyhow!("helper returned no diagnostics data"))?;
    Ok(serde_json::from_value(data)?)
}

fn render_diagnostics_report(list: &gtk4::ListBox, report: &DiagnosticsReport) {
    clear_list(list);
    for check in &report.checks {
        append_diagnostic_row(list, check);
    }
}

fn append_diagnostic_row(list: &gtk4::ListBox, check: &DiagnosticCheck) {
    let subtitle = if check.details.is_empty() {
        check.message.clone()
    } else {
        format!("{} · {}", check.message, check.details.join(" · "))
    };
    append_icon_row(
        list,
        &check.name,
        &subtitle,
        diagnostic_status_icon(&check.status),
    );
}

fn diagnostic_status_icon(status: &DiagnosticStatus) -> &'static str {
    match status {
        DiagnosticStatus::Ok => "emblem-ok-symbolic",
        DiagnosticStatus::Warning => "dialog-warning-symbolic",
        DiagnosticStatus::Error => "dialog-error-symbolic",
    }
}
