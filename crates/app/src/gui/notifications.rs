//! Startup check for scheduled-policy activity that happened while the GUI
//! wasn't open. The retention timer runs as a headless root systemd service
//! with no user session to push a desktop notification to (see
//! docs/product-roadmap.md on why that boundary isn't crossed), so instead
//! the GUI compares policy run logs against the last time it checked and
//! surfaces a toast per run since then — no new privileged surface needed.

use std::path::PathBuf;

use btrfs_manager_core::{PolicyRunLog, PolicyRunStatus, SnapshotPolicy};
use btrfs_manager_helper::HelperRequest;
use chrono::{DateTime, Utc};
use gtk4::glib;

use super::errors::show_toast;
use super::helper_client::handle_privileged_async;
use super::state::UiState;

pub(crate) fn check_recent_policy_runs(ui_state: &UiState) {
    let state = ui_state.clone();
    glib::MainContext::default().spawn_local(async move {
        let since = read_last_check();
        let now = Utc::now();
        for (policy, log) in recent_runs_since(since).await {
            show_toast(&state.toast_overlay, &policy_run_message(&policy, &log));
        }
        write_last_check(now);
    });
}

async fn recent_runs_since(since: DateTime<Utc>) -> Vec<(SnapshotPolicy, PolicyRunLog)> {
    let Ok(response) = handle_privileged_async(HelperRequest::ListSnapshotPolicies).await else {
        return Vec::new();
    };
    let Some(data) = response.data else {
        return Vec::new();
    };
    let Ok(policies) = serde_json::from_value::<Vec<SnapshotPolicy>>(data) else {
        return Vec::new();
    };

    let mut recent = Vec::new();
    for policy in policies {
        let Ok(response) = handle_privileged_async(HelperRequest::ListPolicyRunLogs {
            policy_id: policy.id,
        })
        .await
        else {
            continue;
        };
        let Some(data) = response.data else {
            continue;
        };
        let Ok(logs) = serde_json::from_value::<Vec<PolicyRunLog>>(data) else {
            continue;
        };
        for log in logs {
            if log.finished_at > since {
                recent.push((policy.clone(), log));
            }
        }
    }
    recent.sort_by_key(|(_, log)| log.finished_at);
    recent
}

fn policy_run_message(policy: &SnapshotPolicy, log: &PolicyRunLog) -> String {
    let label = policy
        .source_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("subvolume");
    match log.status {
        PolicyRunStatus::Success => {
            let mut parts = Vec::new();
            if log.created_snapshot.is_some() {
                parts.push("created 1 snapshot".to_string());
            }
            if !log.deleted_snapshots.is_empty() {
                parts.push(format!("removed {} expired", log.deleted_snapshots.len()));
            }
            if parts.is_empty() {
                format!("Scheduled policy for {label} ran — no changes")
            } else {
                format!("Scheduled policy for {label}: {}", parts.join(", "))
            }
        }
        PolicyRunStatus::Failed => format!(
            "Scheduled policy for {label} failed: {}",
            log.error.as_deref().unwrap_or("unknown error")
        ),
    }
}

fn last_check_file() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("btrfs-manager").join("last-policy-check")
}

/// Defaults to one hour ago on first run, so a fresh install doesn't dump
/// every historical run as a toast flood.
fn read_last_check() -> DateTime<Utc> {
    std::fs::read_to_string(last_check_file())
        .ok()
        .and_then(|text| DateTime::parse_from_rfc3339(text.trim()).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|| Utc::now() - chrono::Duration::hours(1))
}

fn write_last_check(now: DateTime<Utc>) {
    let path = last_check_file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, now.to_rfc3339());
}
