use crate::HelperError;
use rusqlite::Connection;
use std::path::PathBuf;

pub(crate) struct StateStore {
    pub(super) connection: Connection,
}

impl StateStore {
    pub(crate) fn open_at(path: PathBuf) -> Result<Self, HelperError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        // Each request opens its own connection, and writers can overlap (the
        // retention timer, GUI operations, and the reconcile pass in
        // list_subvolumes). Wait for the lock instead of failing immediately
        // with SQLITE_BUSY.
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) fn open() -> Result<Self, HelperError> {
        Self::open_at(state_db_path())
    }

    fn migrate(&self) -> Result<(), HelperError> {
        self.connection.execute_batch(
            r#"
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
                snapshot_root TEXT NOT NULL,
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
            CREATE TABLE IF NOT EXISTS rollback_plans (
                id TEXT PRIMARY KEY NOT NULL,
                mountpoint TEXT NOT NULL,
                source_snapshot_path TEXT NOT NULL,
                replaced_subvol_path TEXT NOT NULL,
                return_snapshot_path TEXT NOT NULL,
                boot_integration TEXT NOT NULL,
                status TEXT NOT NULL CHECK (status IN ('awaiting_reboot', 'activated', 'reverted', 'failed')),
                created_at TEXT NOT NULL,
                created_boot_id TEXT,
                description TEXT
            );
            "#,
        )?;
        add_column_if_missing(&self.connection, "managed_snapshots", "policy_id", "TEXT")?;
        for (column, definition) in [
            ("filesystem_id", "TEXT"),
            ("source_path", "TEXT NOT NULL DEFAULT ''"),
            ("mountpoint", "TEXT NOT NULL DEFAULT '/'"),
            ("snapshot_root", "TEXT NOT NULL DEFAULT '.snapshots'"),
            ("created_at", "TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP"),
            ("updated_at", "TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP"),
        ] {
            add_column_if_missing(&self.connection, "snapshot_policies", column, definition)?;
        }
        add_column_if_missing(
            &self.connection,
            "rollback_plans",
            "created_boot_id",
            "TEXT",
        )?;
        add_column_if_missing(&self.connection, "rollback_plans", "description", "TEXT")?;
        Ok(())
    }
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), HelperError> {
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
            rusqlite::params![table, column],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if !exists {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn state_db_path() -> PathBuf {
    std::env::var_os("BTRFS_MANAGER_STATE_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/btrfs-manager/state.db"))
}
