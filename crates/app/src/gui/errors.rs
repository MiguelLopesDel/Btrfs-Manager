//! User-facing error presentation: every failure gets a translated action
//! label plus a hint keyed off the underlying error text.

use super::i18n::tr;
use super::widgets::set_status_row;

pub(crate) fn user_error(action_key: &'static str, err: &anyhow::Error) -> String {
    let detail = err.to_string();
    format!("{}. {}\n{}", tr(action_key), error_hint(&detail), detail)
}

fn error_hint(detail: &str) -> &'static str {
    let lower = detail.to_ascii_lowercase();
    if lower.contains("system service is not available")
        || lower.contains("org.btrfsmanager.helper")
    {
        tr("service_hint")
    } else if lower.contains("polkit denied") || lower.contains("auth") {
        tr("polkit_hint")
    } else if lower.contains("exists but is not a btrfs subvolume") {
        tr("btrfs_subvolume_hint")
    } else if lower.contains("unsafe path")
        || lower.contains("traversal")
        || lower.contains("must be relative")
    {
        tr("path_hint")
    } else if lower.contains("systemctl") {
        tr("systemctl_hint")
    } else {
        tr("generic_hint")
    }
}

pub(crate) fn show_toast(toast_overlay: &libadwaita::ToastOverlay, message: &str) {
    toast_overlay.add_toast(libadwaita::Toast::new(message));
}

pub(crate) fn show_error_toast(
    toast_overlay: &libadwaita::ToastOverlay,
    action_key: &'static str,
    err: &anyhow::Error,
) {
    show_toast(toast_overlay, &user_error(action_key, err));
}

pub(crate) fn set_error_status_row(
    list: &gtk4::ListBox,
    action_key: &'static str,
    err: &anyhow::Error,
) {
    set_status_row(list, tr(action_key), &user_error(action_key, err));
}
