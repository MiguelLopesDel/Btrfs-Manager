//! Browse-mount lifecycle: mounting snapshots read-only for inspection,
//! opening them in the file manager, and cleaning session mounts up.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;
use btrfs_manager_helper::HelperRequest;

use super::helper_client::handle_privileged;
use super::state::UiState;

pub(crate) struct MountedBrowse {
    pub(crate) target: PathBuf,
    pub(crate) created_mounts: Vec<PathBuf>,
    pub(crate) warning: Option<String>,
}

pub(crate) fn unmount_session_mounts(state: &UiState) -> anyhow::Result<()> {
    let mut targets = state
        .session_mounts
        .borrow()
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    // Longest paths first: unmount browse mounts before top-level mounts.
    targets.sort_by_key(|target| std::cmp::Reverse(target.as_os_str().len()));
    let mut first_err: Option<anyhow::Error> = None;
    for target in &targets {
        if let Err(err) = handle_privileged(HelperRequest::UnmountSnapshot {
            target: target.clone(),
        }) {
            // Log and continue — try to unmount remaining mounts even if one fails.
            tracing::warn!(target = %target.display(), error = %err, "unmount failed during session cleanup");
            if first_err.is_none() {
                first_err = Some(err);
            }
        }
    }
    state.session_mounts.borrow_mut().clear();
    state.mounted_snapshots.borrow_mut().clear();
    first_err.map_or(Ok(()), Err)
}

pub(crate) fn browse_mount_root() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .map(|runtime_dir| runtime_dir.join("btrfs-manager").join("browse"))
        .unwrap_or_else(|| std::env::temp_dir().join("btrfs-manager-browse"))
}

pub(crate) fn managed_mount_roots_exist() -> bool {
    browse_mount_root().exists()
}

pub(crate) fn browse_mount_target(source: &Path) -> PathBuf {
    browse_mount_root().join(short_snapshot_mount_name(source))
}

pub(crate) fn browse_snapshot_readonly(
    mountpoint: PathBuf,
    relative_path: PathBuf,
    unlocked: bool,
) -> anyhow::Result<MountedBrowse> {
    tracing::debug!(
        mountpoint = %mountpoint.display(),
        relative_path = %relative_path.display(),
        unlocked,
        "browse_snapshot_readonly: mounting subvolume"
    );
    let target = ensure_browse_target(&relative_path)?;
    // Pre-unmount if something is already mounted there (ignore errors).
    let _ = handle_privileged(HelperRequest::UnmountSnapshot {
        target: target.clone(),
    });
    // Mount the snapshot as a proper btrfs subvolume. The subvolume's own ro
    // property determines writability — no need to force -o ro.
    handle_privileged(HelperRequest::MountSubvolume {
        mountpoint,
        subvol_path: relative_path,
        target: target.clone(),
    })?;
    tracing::debug!(target = %target.display(), as_root = unlocked, "browse: opening file manager");
    let warning = open_in_filemanager(&target, unlocked)?;
    Ok(MountedBrowse {
        target: target.clone(),
        created_mounts: vec![target],
        warning,
    })
}

/// Returns None on success, or Some(warning) on partial success (opened without root).
fn open_in_filemanager(path: &Path, as_root: bool) -> anyhow::Result<Option<String>> {
    // Dev sandbox: app was launched via sudo, open as the original user.
    if let Ok(sudo_user) = std::env::var("SUDO_USER") {
        if !sudo_user.is_empty() {
            Command::new("runuser")
                .args(["-u", &sudo_user, "--", "xdg-open"])
                .arg(path)
                .spawn()?;
            return Ok(None);
        }
    }

    if as_root {
        // Delegate to the helper (which runs as root). The helper spawns the
        // file manager with the user's display environment, so the window
        // appears on the user's desktop. Works on X11 and Wayland without
        // any extra dependencies.
        let display = std::env::var("DISPLAY").unwrap_or_default();
        let wayland_display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
        let xdg_runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
        tracing::debug!(
            "requesting root file manager via helper: {}",
            path.display()
        );
        handle_privileged(HelperRequest::OpenFileManager {
            path: path.to_path_buf(),
            display,
            wayland_display,
            xdg_runtime_dir,
        })?;
        return Ok(None);
    }

    Command::new("xdg-open").arg(path).spawn()?;
    Ok(None)
}

// Creates the browse target directory, falling back to /tmp if the XDG path is
// not writable (e.g., a previous session ran with sudo -E and created the parent
// owned by root).
fn ensure_browse_target(relative_path: &Path) -> anyhow::Result<PathBuf> {
    let preferred = browse_mount_target(relative_path);
    if std::fs::create_dir_all(&preferred).is_ok() {
        return Ok(preferred);
    }
    tracing::warn!(
        preferred = %preferred.display(),
        "XDG browse dir not writable (parent may be root-owned); falling back to /tmp"
    );
    let fallback = std::env::temp_dir()
        .join("btrfs-manager-browse")
        .join(short_snapshot_mount_name(relative_path));
    std::fs::create_dir_all(&fallback)
        .with_context(|| format!("creating browse dir {}", fallback.display()))?;
    Ok(fallback)
}

fn short_snapshot_mount_name(path: &Path) -> String {
    let mut components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    // Pop trivial leaf identifiers: Snapper ends in "snapshot", Timeshift ends
    // in "@" or "@home". Neither carries useful name information.
    if let Some(&last) = components.last() {
        if last == "snapshot" || last.starts_with('@') {
            components.pop();
        }
    }
    let label = components.pop().unwrap_or("snapshot");
    format!("snapshot-{}-{:08x}", sanitize_name(label), path_hash(path))
}

fn sanitize_name(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn path_hash(path: &Path) -> u32 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish() as u32
}
