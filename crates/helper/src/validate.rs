use crate::HelperError;
#[cfg(test)]
use btrfs_manager_core::paths::PathSafetyError;
use btrfs_manager_core::paths::validate_absolute_no_traversal;
use std::path::{Path, PathBuf};

pub(crate) fn validate_path(path: &Path) -> Result<(), HelperError> {
    validate_absolute_no_traversal(path).map_err(HelperError::from)
}

pub(crate) fn validate_relative_btrfs_path(path: &Path, label: &str) -> Result<(), HelperError> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(HelperError::InvalidPolicy(format!(
            "{label} must be relative to the Btrfs top-level and must not contain traversal: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(crate) fn validate_mount_subvolume_option_path(path: &Path) -> Result<(), HelperError> {
    if path.to_string_lossy().contains(',') {
        return Err(HelperError::InvalidPolicy(format!(
            "mount subvolume path must not contain commas: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn validate_managed_mount_target_with_roots(
    path: &Path,
    roots: &[PathBuf],
) -> Result<(), HelperError> {
    if roots.iter().any(|root| path.starts_with(root)) {
        Ok(())
    } else {
        Err(PathSafetyError::Traversal.into())
    }
}

pub(crate) fn runtime_dir_from_env() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("XDG_RUNTIME_DIR") {
        return Some(PathBuf::from(value));
    }
    for key in ["PKEXEC_UID", "SUDO_UID", "UID"] {
        if let Some(uid) = std::env::var_os(key).and_then(|value| value.into_string().ok()) {
            if uid.chars().all(|character| character.is_ascii_digit()) {
                return Some(PathBuf::from("/run/user").join(uid));
            }
        }
    }
    None
}
