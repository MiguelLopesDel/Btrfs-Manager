//! Snapshot/subvolume query helpers: kind checks, search matching, time-range
//! matching, and the date/hour grouping + label logic used when rendering.

use std::path::Path;

use btrfs_manager_core::{Subvolume, SubvolumeKind};
use chrono::{DateTime, Datelike, Local, Timelike, Utc};

use super::state::{TimeRangeFilter, ViewMode};

pub(crate) fn is_snapshot_kind(kind: &SubvolumeKind) -> bool {
    matches!(
        kind,
        SubvolumeKind::Snapshot | SubvolumeKind::ExternalSnapshot { .. }
    )
}

pub(crate) fn matches_query(subvolume: &Subvolume, query: &str) -> bool {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return true;
    }
    if subvolume
        .path
        .to_string_lossy()
        .to_ascii_lowercase()
        .contains(&q)
    {
        return true;
    }
    if subvolume
        .tags
        .iter()
        .any(|tag| tag.to_ascii_lowercase().contains(&q))
    {
        return true;
    }
    // Date matching: group label (Today, Monday, March…) + ISO date + time.
    if snapshot_date_group(subvolume.created_at.as_ref())
        .to_ascii_lowercase()
        .contains(&q)
    {
        return true;
    }
    if let Some(dt) = subvolume.created_at {
        let local: chrono::DateTime<Local> = DateTime::from(dt);
        let formatted = local
            .format("%Y-%m-%d %H:%M %B")
            .to_string()
            .to_ascii_lowercase();
        if formatted.contains(&q) {
            return true;
        }
    }
    false
}

pub(crate) fn time_range_matches(subvolume: &Subvolume, time_range: &TimeRangeFilter) -> bool {
    if *time_range == TimeRangeFilter::AllHistory {
        return true;
    }
    let Some(dt) = subvolume.created_at else {
        return false;
    };
    let local: chrono::DateTime<Local> = DateTime::from(dt);
    let today = Local::now().date_naive();
    let days_ago = (today - local.date_naive()).num_days();
    match time_range {
        TimeRangeFilter::Today => days_ago == 0,
        TimeRangeFilter::Last7Days => days_ago < 7,
        TimeRangeFilter::Last30Days => days_ago < 30,
        TimeRangeFilter::AllHistory => true,
    }
}

/// Returns "Today", "Yesterday", weekday name, or None for dates ≥7 days ago.
pub(crate) fn relative_day_label(local: chrono::DateTime<Local>) -> Option<String> {
    let today = Local::now().date_naive();
    let date = local.date_naive();
    let days_ago = (today - date).num_days();
    if days_ago == 0 {
        Some("Today".into())
    } else if days_ago == 1 {
        Some("Yesterday".into())
    } else if days_ago < 7 {
        Some(local.format("%A").to_string())
    } else {
        None
    }
}

pub(crate) fn snapshot_hour_group(created_at: Option<&DateTime<Utc>>) -> String {
    let Some(dt) = created_at else {
        return "Unknown time".into();
    };
    let local: chrono::DateTime<Local> = DateTime::from(*dt);
    let hour = local.hour();
    let date_part = relative_day_label(local).unwrap_or_else(|| {
        if local.date_naive().year() == Local::now().date_naive().year() {
            local.format("%B %d").to_string()
        } else {
            local.format("%b %d, %Y").to_string()
        }
    });
    format!("{date_part}  —  {:02}:00–{:02}:59", hour, hour)
}

pub(crate) fn snapshot_time_key(
    created_at: Option<&DateTime<Utc>>,
    view_mode: &ViewMode,
) -> (i64, String) {
    let Some(dt) = created_at else {
        return (i64::MAX, "Unknown date".into());
    };
    let local: chrono::DateTime<Local> = DateTime::from(*dt);
    let naive = local.date_naive();
    match view_mode {
        ViewMode::ByDay => {
            let ordinal = -(naive.num_days_from_ce() as i64);
            let label = snapshot_date_group(Some(dt));
            (ordinal, label)
        }
        ViewMode::ByHour => {
            let days = naive.num_days_from_ce() as i64;
            let hour = local.hour() as i64;
            let ordinal = -(days * 24 + hour);
            let label = snapshot_hour_group(Some(dt));
            (ordinal, label)
        }
    }
}

pub(crate) fn snapshot_date_group(created_at: Option<&DateTime<Utc>>) -> String {
    let Some(dt) = created_at else {
        return "Unknown date".into();
    };
    let local: chrono::DateTime<Local> = DateTime::from(*dt);
    relative_day_label(local).unwrap_or_else(|| {
        // ≥7 days: show month (same year) or month+year
        if local.date_naive().year() == Local::now().date_naive().year() {
            local.format("%B").to_string()
        } else {
            local.format("%B %Y").to_string()
        }
    })
}

pub(crate) fn snapshot_display_title(snapshot: &Subvolume) -> String {
    let name = snapshot
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("snapshot");
    // "managed-<source>-YYYY-MM-DD_HH-MM-SS" → strip prefix + 20-char suffix.
    let source = if let Some(rest) = name.strip_prefix("managed-") {
        if rest.len() > 20 {
            rest[..rest.len() - 20].trim_end_matches('-')
        } else {
            rest
        }
    } else if name.len() > 16 {
        // Policy snapshots without "managed-": strip "-YYYYMMDD-HHMMSS" (16 chars).
        let suffix = &name[name.len() - 16..];
        if suffix.starts_with('-')
            && suffix[1..9].bytes().all(|b| b.is_ascii_digit())
            && suffix.as_bytes()[9] == b'-'
            && suffix[10..].bytes().all(|b| b.is_ascii_digit())
        {
            name[..name.len() - 16].trim_end_matches('-')
        } else {
            name
        }
    } else {
        name
    };
    match snapshot.created_at {
        Some(dt) => {
            let local: chrono::DateTime<Local> = DateTime::from(dt);
            format!("{} · {}", source, local.format("%H:%M"))
        }
        None => source.to_string(),
    }
}

pub(crate) fn snapshot_subtitle(
    id: u64,
    path: &Path,
    unlocked: bool,
    mounted: bool,
    mount_target: &Path,
    tags: &[String],
) -> String {
    let mut parts: Vec<String> = vec![format!("ID {id}"), path.display().to_string()];
    if unlocked {
        parts.push("Writable".into());
    }
    if !tags.is_empty() {
        parts.push(tags.join(", "));
    }
    if mounted {
        parts.push(format!("mounted at {}", mount_target.display()));
    }
    parts.join(" · ")
}

pub(crate) fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}
