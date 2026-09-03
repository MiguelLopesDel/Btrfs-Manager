//! Snapshot name generation, shared by manual and scheduled snapshot
//! creation so both produce one consistent, human-readable scheme instead of
//! two different formats.

use std::path::Path;

use chrono::{DateTime, Local};

/// Replace every character that isn't alphanumeric/`-`/`_` with `-`, then
/// trim leading/trailing dashes. Used to turn a subvolume path segment (which
/// may start with `@`) into a plain label.
pub fn sanitize_label(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// `{label}-{YYYYMMDD-HHMMSS}` in local time, e.g. `home-20260825-143207`.
/// The single naming scheme for every snapshot the app creates, whether
/// triggered manually or by a scheduled policy.
pub fn snapshot_name(label: &str, when: DateTime<Local>) -> String {
    format!("{label}-{}", when.format("%Y%m%d-%H%M%S"))
}

/// Convenience wrapper: derive the label from a subvolume path's file name
/// (falling back to "root" when that's empty after sanitizing, e.g. for the
/// bare `@` subvolume) and build the name.
pub fn snapshot_name_from_path(path: &Path, when: DateTime<Local>) -> String {
    let raw = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("subvolume");
    let label = sanitize_label(raw);
    let label = if label.is_empty() { "root" } else { &label };
    snapshot_name(label, when)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_time() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 8, 25, 14, 32, 7).unwrap()
    }

    #[test]
    fn sanitize_label_replaces_non_alnum_and_trims_dashes() {
        assert_eq!(sanitize_label("@home"), "home");
        assert_eq!(sanitize_label("var/log stuff!"), "var-log-stuff");
        assert_eq!(sanitize_label("@"), "");
    }

    #[test]
    fn snapshot_name_from_path_uses_file_name_and_local_time() {
        let name = snapshot_name_from_path(Path::new("@home"), fixed_time());
        assert_eq!(name, "home-20260825-143207");
    }

    #[test]
    fn snapshot_name_from_path_falls_back_to_root_for_bare_at() {
        let name = snapshot_name_from_path(Path::new("@"), fixed_time());
        assert_eq!(name, "root-20260825-143207");
    }
}
