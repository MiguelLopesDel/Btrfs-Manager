use crate::models::Snapshot;
use chrono::{Datelike, Timelike};
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionClass {
    Hourly,
    Daily,
    Weekly,
    Monthly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub hourly: usize,
    pub daily: usize,
    pub weekly: usize,
    pub monthly: usize,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            hourly: 24,
            daily: 7,
            weekly: 4,
            monthly: 6,
        }
    }
}

pub fn retention_keep_set(snapshots: &[Snapshot], policy: &RetentionPolicy) -> HashSet<Uuid> {
    let mut ordered = snapshots.to_vec();
    ordered.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.created_at));

    let mut keep = HashSet::new();
    keep_bucketed(&ordered, policy.hourly, bucket_hour, &mut keep);
    keep_bucketed(&ordered, policy.daily, bucket_day, &mut keep);
    keep_bucketed(&ordered, policy.weekly, bucket_week, &mut keep);
    keep_bucketed(&ordered, policy.monthly, bucket_month, &mut keep);
    keep
}

fn keep_bucketed<K: Eq>(
    snapshots: &[Snapshot],
    limit: usize,
    bucket: impl Fn(&Snapshot) -> K,
    keep: &mut HashSet<Uuid>,
) {
    if limit == 0 {
        return;
    }
    let mut buckets: Vec<K> = Vec::new();
    for snapshot in snapshots {
        let current = bucket(snapshot);
        if buckets.contains(&current) {
            continue;
        }
        buckets.push(current);
        keep.insert(snapshot.id);
        if buckets.len() >= limit {
            break;
        }
    }
}

fn bucket_hour(snapshot: &Snapshot) -> (i32, u32, u32, u32) {
    let at = snapshot.created_at;
    (at.year(), at.month(), at.day(), at.hour())
}

fn bucket_day(snapshot: &Snapshot) -> (i32, u32, u32) {
    let at = snapshot.created_at;
    (at.year(), at.month(), at.day())
}

fn bucket_week(snapshot: &Snapshot) -> (i32, u32) {
    let at = snapshot.created_at.iso_week();
    (at.year(), at.week())
}

fn bucket_month(snapshot: &Snapshot) -> (i32, u32) {
    let at = snapshot.created_at;
    (at.year(), at.month())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{SnapshotOrigin, SnapshotState, SubvolumeId};
    use chrono::{Duration, Utc};
    use std::path::PathBuf;

    #[test]
    fn keeps_newest_snapshot_per_bucket() {
        let now = Utc::now();
        let snapshots: Vec<_> = (0..48)
            .map(|hour| Snapshot {
                id: Uuid::new_v4(),
                source_subvolume: SubvolumeId(256),
                path: PathBuf::from(format!("@snapshots/{hour}")),
                created_at: now - Duration::hours(hour),
                tags: Vec::new(),
                origin: SnapshotOrigin::Managed,
                state: SnapshotState::ReadOnly,
            })
            .collect();
        let keep = retention_keep_set(
            &snapshots,
            &RetentionPolicy {
                hourly: 3,
                daily: 0,
                weekly: 0,
                monthly: 0,
            },
        );
        assert_eq!(keep.len(), 3);
        assert!(keep.contains(&snapshots[0].id));
    }
}
