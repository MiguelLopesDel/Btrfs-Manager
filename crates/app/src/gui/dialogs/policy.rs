//! Snapshot policy dialog: schedule, retention counts, preview, and run logs.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use btrfs_manager_core::{
    PolicyRunLog, PolicySchedule, RetentionPreview, SnapshotPolicy, Subvolume,
};
use btrfs_manager_helper::HelperRequest;
use gtk4::prelude::*;
use uuid::Uuid;

use crate::gui::errors::{show_error_toast, show_toast};
use crate::gui::helper_client::handle_privileged;
use crate::gui::state::UiState;
use crate::gui::widgets::{dialog_content_box, labeled_widget, retention_spin};

/// Form widgets whose values define the policy being edited.
struct PolicyForm {
    enabled: gtk4::Switch,
    schedule: gtk4::ComboBoxText,
    snapshot_root: gtk4::Entry,
    keep_hourly: gtk4::SpinButton,
    keep_daily: gtk4::SpinButton,
    keep_weekly: gtk4::SpinButton,
    keep_monthly: gtk4::SpinButton,
}

pub(crate) fn open_policy_dialog(state: UiState, mountpoint: PathBuf, subvolume: Subvolume) {
    // source_path is relative to the Btrfs volume root (e.g. "@", "@home").
    // The helper will mount subvolid=5 internally to access it.
    let source_path = subvolume.path.clone();
    let existing = load_policy_for_subvolume(subvolume.id.0, &source_path);
    let policy_id = existing
        .as_ref()
        .map(|policy| policy.id)
        .unwrap_or_else(Uuid::new_v4);

    let window = gtk4::Window::builder()
        .title("Snapshot Policy")
        .default_width(520)
        .default_height(620)
        .modal(true)
        .build();
    let content = dialog_content_box();

    let title = gtk4::Label::builder()
        .label(format!("Policy for {}", subvolume.path.display()))
        .halign(gtk4::Align::Start)
        .css_classes(["title-3"])
        .build();
    content.append(&title);

    let form = build_policy_form(&content, existing.as_ref());

    let preview = gtk4::Label::builder()
        .label("Preview not loaded")
        .halign(gtk4::Align::Start)
        .wrap(true)
        .build();
    content.append(&preview);

    let logs = gtk4::Label::builder()
        .label("No logs loaded")
        .halign(gtk4::Align::Start)
        .wrap(true)
        .build();
    content.append(&logs);

    let actions = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk4::Align::End)
        .build();
    let preview_button = gtk4::Button::with_label("Preview");
    let save_button = gtk4::Button::with_label("Save");
    let run_button = gtk4::Button::with_label("Run Now");
    let logs_button = gtk4::Button::with_label("Logs");
    save_button.set_sensitive(false);
    run_button.set_sensitive(false);
    actions.append(&preview_button);
    actions.append(&save_button);
    actions.append(&run_button);
    actions.append(&logs_button);
    content.append(&actions);
    window.set_child(Some(&content));

    let build_policy = build_policy_factory(&form, policy_id, subvolume, source_path, mountpoint);

    let preview_valid = Rc::new(Cell::new(false));
    connect_policy_invalidation(&form, &save_button, &run_button, preview_valid.clone());
    wire_preview_button(
        &preview_button,
        &preview,
        &state,
        build_policy.clone(),
        preview_valid.clone(),
        &save_button,
        &run_button,
    );
    wire_save_button(&save_button, &state, build_policy.clone(), &preview_valid);
    wire_run_button(&run_button, &state, build_policy, &preview_valid);
    wire_logs_button(&logs_button, &logs, &state, policy_id);

    window.present();
}

/// Append the editable fields to the dialog and return them as a form.
fn build_policy_form(content: &gtk4::Box, existing: Option<&SnapshotPolicy>) -> PolicyForm {
    let enabled = gtk4::Switch::builder()
        .active(existing.map(|policy| policy.enabled).unwrap_or(true))
        .halign(gtk4::Align::Start)
        .build();
    content.append(&labeled_widget("Enabled", &enabled));

    let schedule = gtk4::ComboBoxText::new();
    for item in ["hourly", "daily", "weekly", "monthly"] {
        schedule.append(Some(item), item);
    }
    schedule.set_active_id(Some(
        existing
            .map(|policy| policy.schedule.as_str())
            .unwrap_or("hourly"),
    ));
    content.append(&labeled_widget("Schedule", &schedule));

    let snapshot_root = gtk4::Entry::builder()
        .text(
            existing
                .map(|policy| policy.snapshot_root.display().to_string())
                .unwrap_or_else(|| "@btrfs-manager".into()),
        )
        .build();
    content.append(&labeled_widget("Snapshot root", &snapshot_root));

    let keep_hourly = retention_spin(existing.map(|policy| policy.keep_hourly).unwrap_or(24));
    let keep_daily = retention_spin(existing.map(|policy| policy.keep_daily).unwrap_or(7));
    let keep_weekly = retention_spin(existing.map(|policy| policy.keep_weekly).unwrap_or(4));
    let keep_monthly = retention_spin(existing.map(|policy| policy.keep_monthly).unwrap_or(6));
    content.append(&labeled_widget("Keep hourly", &keep_hourly));
    content.append(&labeled_widget("Keep daily", &keep_daily));
    content.append(&labeled_widget("Keep weekly", &keep_weekly));
    content.append(&labeled_widget("Keep monthly", &keep_monthly));

    PolicyForm {
        enabled,
        schedule,
        snapshot_root,
        keep_hourly,
        keep_daily,
        keep_weekly,
        keep_monthly,
    }
}

/// Closure that snapshots the current form values into a SnapshotPolicy.
fn build_policy_factory(
    form: &PolicyForm,
    policy_id: Uuid,
    subvolume: Subvolume,
    source_path: PathBuf,
    mountpoint: PathBuf,
) -> Rc<dyn Fn() -> SnapshotPolicy> {
    let schedule = form.schedule.clone();
    let snapshot_root = form.snapshot_root.clone();
    let enabled = form.enabled.clone();
    let keep_hourly = form.keep_hourly.clone();
    let keep_daily = form.keep_daily.clone();
    let keep_weekly = form.keep_weekly.clone();
    let keep_monthly = form.keep_monthly.clone();
    Rc::new(move || SnapshotPolicy {
        id: policy_id,
        filesystem_id: None,
        subvolume_id: subvolume.id.clone(),
        source_path: source_path.clone(),
        mountpoint: mountpoint.clone(),
        snapshot_root: PathBuf::from(snapshot_root.text().as_str()),
        schedule: schedule
            .active_id()
            .as_deref()
            .unwrap_or("hourly")
            .parse()
            .unwrap_or(PolicySchedule::Hourly),
        keep_hourly: keep_hourly.value() as usize,
        keep_daily: keep_daily.value() as usize,
        keep_weekly: keep_weekly.value() as usize,
        keep_monthly: keep_monthly.value() as usize,
        enabled: enabled.is_active(),
    })
}

/// Any form edit invalidates the preview, disabling Save/Run until the user
/// previews the retention effect of the new values again.
fn connect_policy_invalidation(
    form: &PolicyForm,
    save: &gtk4::Button,
    run: &gtk4::Button,
    preview_valid: Rc<Cell<bool>>,
) {
    let invalidate = {
        let save = save.clone();
        let run = run.clone();
        move || {
            preview_valid.set(false);
            save.set_sensitive(false);
            run.set_sensitive(false);
        }
    };

    let invalidate_for_entry = invalidate.clone();
    form.snapshot_root
        .connect_changed(move |_| invalidate_for_entry());
    let invalidate_for_schedule = invalidate.clone();
    form.schedule
        .connect_changed(move |_| invalidate_for_schedule());
    let invalidate_for_enabled = invalidate.clone();
    form.enabled
        .connect_active_notify(move |_| invalidate_for_enabled());
    for spin in [
        &form.keep_hourly,
        &form.keep_daily,
        &form.keep_weekly,
        &form.keep_monthly,
    ] {
        let invalidate_for_spin = invalidate.clone();
        spin.connect_value_changed(move |_| invalidate_for_spin());
    }
}

#[allow(clippy::too_many_arguments)]
fn wire_preview_button(
    preview_button: &gtk4::Button,
    preview: &gtk4::Label,
    state: &UiState,
    build_policy: Rc<dyn Fn() -> SnapshotPolicy>,
    preview_valid: Rc<Cell<bool>>,
    save_button: &gtk4::Button,
    run_button: &gtk4::Button,
) {
    let preview = preview.clone();
    let state = state.clone();
    let save_button = save_button.clone();
    let run_button = run_button.clone();
    preview_button.connect_clicked(move |_| {
        let policy = build_policy();
        match handle_privileged(HelperRequest::PreviewRetentionForPolicy { policy }) {
            Ok(response) => match response.data {
                Some(data) => match serde_json::from_value::<RetentionPreview>(data) {
                    Ok(value) => {
                        preview.set_label(&format!(
                            "Next: {}\nWould delete: {}\nWould keep: {}",
                            value.next_snapshot_path.display(),
                            value.delete.len(),
                            value.keep.len()
                        ));
                        preview_valid.set(true);
                        save_button.set_sensitive(true);
                        run_button.set_sensitive(true);
                    }
                    Err(err) => {
                        let err = anyhow::Error::from(err);
                        show_error_toast(&state.toast_overlay, "preview_policy", &err);
                    }
                },
                None => preview.set_label(&response.message),
            },
            Err(err) => show_error_toast(&state.toast_overlay, "preview_policy", &err),
        }
    });
}

fn wire_save_button(
    save_button: &gtk4::Button,
    state: &UiState,
    build_policy: Rc<dyn Fn() -> SnapshotPolicy>,
    preview_valid: &Rc<Cell<bool>>,
) {
    let state = state.clone();
    let preview_valid = preview_valid.clone();
    save_button.connect_clicked(move |_| {
        if !preview_valid.get() {
            show_toast(&state.toast_overlay, "Preview retention before saving");
            return;
        }
        let policy = build_policy();
        match handle_privileged(HelperRequest::UpsertSnapshotPolicy { policy }) {
            Ok(response) => show_toast(&state.toast_overlay, &response.message),
            Err(err) => show_error_toast(&state.toast_overlay, "save_policy", &err),
        }
    });
}

fn wire_run_button(
    run_button: &gtk4::Button,
    state: &UiState,
    build_policy: Rc<dyn Fn() -> SnapshotPolicy>,
    preview_valid: &Rc<Cell<bool>>,
) {
    let state = state.clone();
    let preview_valid = preview_valid.clone();
    run_button.connect_clicked(move |_| {
        if !preview_valid.get() {
            show_toast(&state.toast_overlay, "Preview retention before running");
            return;
        }
        let policy = build_policy();
        match handle_privileged(HelperRequest::UpsertSnapshotPolicy {
            policy: policy.clone(),
        })
        .and_then(|_| {
            handle_privileged(HelperRequest::RunRetentionPolicy {
                policy_id: policy.id,
            })
        }) {
            Ok(response) => show_toast(&state.toast_overlay, &response.message),
            Err(err) => show_error_toast(&state.toast_overlay, "run_policy", &err),
        }
    });
}

fn wire_logs_button(
    logs_button: &gtk4::Button,
    logs: &gtk4::Label,
    state: &UiState,
    policy_id: Uuid,
) {
    let state = state.clone();
    let logs = logs.clone();
    logs_button.connect_clicked(move |_| {
        match handle_privileged(HelperRequest::ListPolicyRunLogs { policy_id }) {
            Ok(response) => match response.data {
                Some(data) => match serde_json::from_value::<Vec<PolicyRunLog>>(data) {
                    Ok(values) => logs.set_label(&format_policy_logs(&values)),
                    Err(err) => {
                        let err = anyhow::Error::from(err);
                        show_error_toast(&state.toast_overlay, "load_policy_logs", &err);
                    }
                },
                None => logs.set_label(&response.message),
            },
            Err(err) => show_error_toast(&state.toast_overlay, "load_policy_logs", &err),
        }
    });
}

fn load_policy_for_subvolume(
    subvolume_id: u64,
    source_path: &std::path::Path,
) -> Option<SnapshotPolicy> {
    let response = handle_privileged(HelperRequest::ListSnapshotPolicies).ok()?;
    let policies = serde_json::from_value::<Vec<SnapshotPolicy>>(response.data?).ok()?;
    policies
        .into_iter()
        .find(|policy| policy.subvolume_id.0 == subvolume_id && policy.source_path == source_path)
}

fn format_policy_logs(logs: &[PolicyRunLog]) -> String {
    if logs.is_empty() {
        return "No policy runs yet".into();
    }
    logs.iter()
        .take(5)
        .map(|log| {
            format!(
                "{} · {:?} · created: {} · deleted: {}",
                log.started_at,
                log.status,
                log.created_snapshot
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "-".into()),
                log.deleted_snapshots.len()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
