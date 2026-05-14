CREATE TABLE IF NOT EXISTS managed_snapshots (
    id TEXT PRIMARY KEY NOT NULL,
    policy_id TEXT,
    source_subvolume_id INTEGER NOT NULL,
    path TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    tags_json TEXT NOT NULL DEFAULT '[]',
    origin_tool TEXT,
    state TEXT NOT NULL CHECK (state IN ('readonly', 'unlocked', 'dirty_unlocked', 'rollback_anchor'))
);

CREATE TABLE IF NOT EXISTS snapshot_policies (
    id TEXT PRIMARY KEY NOT NULL,
    filesystem_id TEXT,
    subvolume_id INTEGER NOT NULL,
    source_path TEXT NOT NULL,
    mountpoint TEXT NOT NULL,
    snapshot_root TEXT NOT NULL DEFAULT '.snapshots',
    enabled INTEGER NOT NULL DEFAULT 1,
    schedule TEXT NOT NULL CHECK (schedule IN ('hourly', 'daily', 'weekly', 'monthly')),
    keep_hourly INTEGER NOT NULL DEFAULT 24,
    keep_daily INTEGER NOT NULL DEFAULT 7,
    keep_weekly INTEGER NOT NULL DEFAULT 4,
    keep_monthly INTEGER NOT NULL DEFAULT 6,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS policy_run_logs (
    id TEXT PRIMARY KEY NOT NULL,
    policy_id TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('success', 'failed')),
    created_snapshot TEXT,
    deleted_snapshots_json TEXT NOT NULL DEFAULT '[]',
    error TEXT
);

CREATE TABLE IF NOT EXISTS rollback_transactions (
    id TEXT PRIMARY KEY NOT NULL,
    source_snapshot_id TEXT NOT NULL,
    prepared_subvolume_path TEXT NOT NULL,
    return_snapshot_path TEXT NOT NULL,
    boot_integration TEXT NOT NULL CHECK (boot_integration IN ('grub_btrfs', 'refind_btrfs', 'conservative')),
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
