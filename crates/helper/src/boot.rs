use btrfs_manager_core::models::BootIntegration;
use std::path::Path;

pub(crate) fn detect_boot_integration() -> BootIntegration {
    if Path::new("/etc/default/grub-btrfs/config").exists()
        || Path::new("/etc/grub.d/41_snapshots-btrfs").exists()
    {
        BootIntegration::GrubBtrfs
    } else if Path::new("/boot/refind_linux.conf").exists()
        || Path::new("/boot/efi/EFI/refind/refind.conf").exists()
    {
        BootIntegration::RefindBtrfs
    } else {
        BootIntegration::Conservative
    }
}

pub(crate) fn current_boot_id() -> Option<String> {
    if let Ok(value) = std::env::var("BTRFS_MANAGER_BOOT_ID") {
        if !value.trim().is_empty() {
            return Some(value);
        }
    }
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
