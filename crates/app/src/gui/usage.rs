//! Disk-usage button on a managed snapshot row: fetched on demand
//! (`btrfs filesystem du` can be slow) rather than eagerly for every row.

use std::path::{Path, PathBuf};

use btrfs_manager_core::{DiskUsage, Subvolume};
use btrfs_manager_helper::HelperRequest;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita::prelude::*;

use super::errors::{show_toast, user_error};
use super::helper_client::handle_privileged_async;
use super::state::UiState;

pub(crate) fn build_usage_button(
    row: &libadwaita::ActionRow,
    snapshot: &Subvolume,
    mountpoint: &Path,
    state: &UiState,
) -> gtk4::Button {
    let usage_btn = gtk4::Button::builder()
        .icon_name("drive-harddisk-symbolic")
        .tooltip_text("Show disk usage")
        .valign(gtk4::Align::Center)
        .build();
    let state_for_usage = state.clone();
    let mountpoint = mountpoint.to_path_buf();
    let subvolume_path = snapshot.path.clone();
    let subtitle_base = row.subtitle().map(|s| s.to_string()).unwrap_or_default();
    let row_for_usage = row.clone();
    usage_btn.connect_clicked(move |_| {
        wire_usage_click(
            &state_for_usage,
            mountpoint.clone(),
            subvolume_path.clone(),
            subtitle_base.clone(),
            row_for_usage.clone(),
        );
    });
    usage_btn
}

fn wire_usage_click(
    state: &UiState,
    mountpoint: PathBuf,
    subvolume_path: PathBuf,
    subtitle_base: String,
    row: libadwaita::ActionRow,
) {
    let state = state.clone();
    state.spinner.start();
    glib::MainContext::default().spawn_local(async move {
        let result = fetch_usage(mountpoint, subvolume_path).await;
        state.spinner.stop();
        match result {
            Ok(usage) => {
                row.set_subtitle(&format!("{subtitle_base} · {}", format_disk_usage(&usage)));
            }
            Err(err) => show_toast(&state.toast_overlay, &user_error("disk_usage", &err)),
        }
    });
}

async fn fetch_usage(mountpoint: PathBuf, subvolume_path: PathBuf) -> anyhow::Result<DiskUsage> {
    let response = handle_privileged_async(HelperRequest::SnapshotDiskUsage {
        mountpoint,
        subvolume_path,
    })
    .await?;
    let data = response
        .data
        .ok_or_else(|| anyhow::anyhow!("no usage data returned"))?;
    Ok(serde_json::from_value(data)?)
}

fn format_disk_usage(usage: &DiskUsage) -> String {
    format!(
        "{} total, {} exclusive",
        format_bytes(usage.total_bytes),
        format_bytes(usage.exclusive_bytes)
    )
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
