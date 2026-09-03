//! Shared fixtures for the helper's unit tests: fake `CommandRunner`
//! implementations that simulate `btrfs`/`findmnt`/`systemctl` output, and
//! small setup helpers used across multiple test groups.

use crate::state::StateStore;
use crate::{CommandRunner, HelperError};
use btrfs_manager_core::models::{
    Snapshot, SnapshotOrigin, SnapshotPolicy, SnapshotState, SubvolumeId,
};
use chrono::Utc;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(crate) struct RecordingRunner {
    pub(crate) calls: RefCell<Vec<(String, Vec<String>)>>,
}

impl CommandRunner for RecordingRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<String, HelperError> {
        self.calls
            .borrow_mut()
            .push((program.to_string(), args.to_vec()));
        if program == "btrfs" && args.first().map(String::as_str) == Some("subvolume") {
            if args.get(1).map(String::as_str) == Some("get-default") {
                Ok("ID 256 gen 10 top level 5 path @\n".into())
            } else {
                Ok("ID 256 gen 10 top level 5 uuid db14ad1b-c411-f247-8770-e8386e647b88 path @data\nID 257 gen 9 top level 5 uuid 1a123437-56b1-a849-b390-b5ec0c89c707 path snapshots/snap-1\n".into())
            }
        } else if program == "findmnt" && args.iter().any(|arg| arg == "TARGET") {
            Ok(format!(
                "{}\n",
                std::env::temp_dir()
                    .join("btrfs-manager-browse")
                    .join("snapshot-296-test")
                    .display()
            ))
        } else if program == "findmnt" && args.iter().any(|arg| arg == "UUID,SOURCE,TARGET,OPTIONS")
        {
            Ok("UUID=\"550e8400-e29b-41d4-a716-446655440000\" SOURCE=\"/dev/mapper/cryptroot[/@]\" TARGET=\"/\" OPTIONS=\"rw,relatime,subvol=/@\"\nUUID=\"550e8400-e29b-41d4-a716-446655440000\" SOURCE=\"/dev/mapper/cryptroot[/@home]\" TARGET=\"/home\" OPTIONS=\"rw,relatime,subvol=/@home\"\n".into())
        } else if program == "findmnt" && args.iter().any(|arg| arg == "--mountpoint") {
            // Simulate "not mounted" so ensure_top_level_mount always mounts.
            Ok("".into())
        } else if program == "findmnt" && args.iter().any(|arg| arg == "UUID") {
            Ok("550e8400-e29b-41d4-a716-446655440000\n".into())
        } else if program == "findmnt" {
            Ok("/dev/mapper/cryptroot[/@]\n".into())
        } else {
            Ok("ok".into())
        }
    }
}

// Serialize tests that touch BTRFS_MANAGER_STATE_DB (process-global env var).
static DB_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

pub(crate) fn with_test_db<T>(f: impl FnOnce() -> T) -> T {
    let _g = DB_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let db_path = std::env::temp_dir().join(format!("btrfs-manager-test-{}.db", Uuid::new_v4()));
    // SAFETY: DB_LOCK serializes all callers; no other thread reads this var concurrently.
    unsafe {
        std::env::set_var("BTRFS_MANAGER_STATE_DB", &db_path);
    }
    let result = f();
    unsafe {
        std::env::remove_var("BTRFS_MANAGER_STATE_DB");
    }
    std::fs::remove_file(&db_path).ok();
    result
}

/// Sets up a fake top-level Btrfs mount (`BTRFS_MANAGER_TOPLEVEL_DIR`
/// pointing at a fresh temp dir with a `@btrfs-manager/state` subdir) plus
/// its `StateStore`, and tears both down afterward. Shared by every test
/// that exercises managed-snapshot delete/lock paths against a real
/// filesystem layout instead of just the in-memory test DB.
pub(crate) fn with_top_level_fixture<T>(
    prefix: &str,
    f: impl FnOnce(&Path, &StateStore) -> T,
) -> T {
    with_test_db(|| {
        let test_root =
            std::env::temp_dir().join(format!("btrfs-manager-{prefix}-{}", Uuid::new_v4()));
        let fs_uuid = "550e8400-e29b-41d4-a716-446655440000";
        let top = test_root.join(fs_uuid);
        std::fs::create_dir_all(top.join("@btrfs-manager/state")).unwrap();
        // SAFETY: with_test_db already serializes all callers via DB_LOCK.
        unsafe {
            std::env::set_var("BTRFS_MANAGER_TOPLEVEL_DIR", &test_root);
        }
        let store = StateStore::open_at(top.join("@btrfs-manager/state/state.db")).unwrap();
        let result = f(&top, &store);
        unsafe {
            std::env::remove_var("BTRFS_MANAGER_TOPLEVEL_DIR");
        }
        std::fs::remove_dir_all(&test_root).ok();
        result
    })
}

/// Points `XDG_RUNTIME_DIR` at a fresh temp dir for the duration of `f`, so
/// tests can exercise the `/run/user/<uid>/...`-shaped paths this crate
/// trusts (see `validate::runtime_dir_from_env`) without touching the real
/// `/run/user/<uid>` on the machine running the tests.
pub(crate) fn with_runtime_dir<T>(f: impl FnOnce(&Path) -> T) -> T {
    with_test_db(|| {
        let runtime_dir =
            std::env::temp_dir().join(format!("btrfs-manager-runtime-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&runtime_dir).unwrap();
        // SAFETY: with_test_db already serializes all callers via DB_LOCK.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
        }
        let result = f(&runtime_dir);
        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
        }
        std::fs::remove_dir_all(&runtime_dir).ok();
        result
    })
}

pub(crate) fn find_snap(store: &StateStore, id: Uuid) -> Snapshot {
    store
        .list_all_managed_snapshots()
        .unwrap()
        .into_iter()
        .find(|s| s.id == id)
        .unwrap()
}

pub(crate) struct RollbackRunner {
    pub(crate) calls: RefCell<Vec<(String, Vec<String>)>>,
}

impl CommandRunner for RollbackRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<String, HelperError> {
        self.calls
            .borrow_mut()
            .push((program.to_string(), args.to_vec()));
        if program == "findmnt" && args.iter().any(|arg| arg == "UUID") {
            Ok("550e8400-e29b-41d4-a716-446655440000\n".into())
        } else if program == "findmnt" && args.iter().any(|arg| arg == "--mountpoint") {
            Ok("mounted\n".into())
        } else if program == "findmnt" && args.iter().any(|arg| arg == "OPTIONS") {
            Ok("rw,relatime,subvol=/@\n".into())
        } else if program == "findmnt" && args.iter().any(|arg| arg == "SOURCE") {
            Ok("/dev/loop-test\n".into())
        } else if program == "btrfs"
            && args.first().map(String::as_str) == Some("subvolume")
            && args.get(1).map(String::as_str) == Some("snapshot")
        {
            let source = PathBuf::from(args.get(2).expect("snapshot source"));
            let destination = PathBuf::from(args.get(3).expect("snapshot destination"));
            if !source.exists() {
                return Err(HelperError::InvalidPolicy(format!(
                    "missing snapshot source {}",
                    source.display()
                )));
            }
            std::fs::create_dir_all(destination)?;
            Ok("snapshot created\n".into())
        } else {
            Ok("ok\n".into())
        }
    }
}

fn retention_handle_findmnt(args: &[String]) -> Option<String> {
    if args.iter().any(|arg| arg == "UUID") {
        Some("550e8400-e29b-41d4-a716-446655440000\n".into())
    } else if args.iter().any(|arg| arg == "--mountpoint") {
        Some("mounted\n".into())
    } else if args.iter().any(|arg| arg == "SOURCE") {
        Some("/dev/loop-test\n".into())
    } else {
        None
    }
}

fn retention_handle_create(args: &[String]) -> Result<String, HelperError> {
    let path = PathBuf::from(args.get(2).expect("subvolume create path"));
    std::fs::create_dir_all(path)?;
    Ok("Create subvolume\n".into())
}

fn retention_handle_show(args: &[String]) -> Result<String, HelperError> {
    let path = PathBuf::from(args.get(2).expect("subvolume show path"));
    if path.exists() {
        Ok("Name: @btrfs-manager\n".into())
    } else {
        Err(HelperError::InvalidPolicy(format!(
            "missing subvolume {}",
            path.display()
        )))
    }
}

fn retention_handle_snapshot(args: &[String]) -> Result<String, HelperError> {
    let source = PathBuf::from(args.get(3).expect("snapshot source"));
    let destination = PathBuf::from(args.get(4).expect("snapshot destination"));
    if !source.exists() {
        return Err(HelperError::InvalidPolicy(format!(
            "missing snapshot source {}",
            source.display()
        )));
    }
    std::fs::create_dir_all(destination)?;
    Ok("snapshot created\n".into())
}

fn retention_handle_delete(args: &[String]) -> Result<String, HelperError> {
    let path = PathBuf::from(args.get(2).expect("subvolume delete path"));
    std::fs::remove_dir_all(path)?;
    Ok("deleted\n".into())
}

pub(crate) struct RetentionRunner {
    pub(crate) calls: RefCell<Vec<(String, Vec<String>)>>,
}

impl CommandRunner for RetentionRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<String, HelperError> {
        self.calls
            .borrow_mut()
            .push((program.to_string(), args.to_vec()));
        if program == "findmnt" {
            if let Some(output) = retention_handle_findmnt(args) {
                return Ok(output);
            }
        } else if program == "btrfs" && args.first().map(String::as_str) == Some("subvolume") {
            match args.get(1).map(String::as_str) {
                Some("create") => return retention_handle_create(args),
                Some("show") => return retention_handle_show(args),
                Some("snapshot") => return retention_handle_snapshot(args),
                Some("delete") => return retention_handle_delete(args),
                _ => {}
            }
        }
        Ok("ok\n".into())
    }
}

pub(crate) struct DiagnosticsRunner;

impl CommandRunner for DiagnosticsRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<String, HelperError> {
        if program == "btrfs" && args.first().map(String::as_str) == Some("version") {
            Ok("btrfs-progs v6.16\n".into())
        } else if program == "btrfs"
            && args.first().map(String::as_str) == Some("subvolume")
            && args.get(1).map(String::as_str) == Some("show")
        {
            Ok("Name: @btrfs-manager\n".into())
        } else if program == "findmnt"
            && args.iter().any(|arg| arg == "FSTYPE,SOURCE,TARGET,OPTIONS")
        {
            Ok("FSTYPE=\"btrfs\" SOURCE=\"/dev/loop0[/@]\" TARGET=\"/\" OPTIONS=\"rw,relatime,subvol=/@\"\n".into())
        } else if program == "findmnt" && args.iter().any(|arg| arg == "UUID") {
            Ok("550e8400-e29b-41d4-a716-446655440000\n".into())
        } else if program == "findmnt" && args.iter().any(|arg| arg == "--mountpoint") {
            Ok("mounted\n".into())
        } else if program == "systemctl" && args.first().map(String::as_str) == Some("is-active") {
            Ok("active\n".into())
        } else {
            Ok("ok\n".into())
        }
    }
}

pub(crate) struct FailingSystemdRunner {
    pub(crate) calls: RefCell<Vec<(String, Vec<String>)>>,
}

impl CommandRunner for FailingSystemdRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<String, HelperError> {
        self.calls
            .borrow_mut()
            .push((program.to_string(), args.to_vec()));
        if program == "systemctl" && args.iter().any(|arg| arg == "enable" || arg == "disable") {
            return Err(HelperError::CommandFailed {
                program: program.into(),
                args: args.to_vec(),
                stderr: "systemd failed".into(),
            });
        }
        RetentionRunner {
            calls: RefCell::new(Vec::new()),
        }
        .run(program, args)
    }
}

pub(crate) fn test_policy(id: Uuid) -> SnapshotPolicy {
    SnapshotPolicy {
        id,
        filesystem_id: None,
        subvolume_id: SubvolumeId(256),
        source_path: PathBuf::from("@"),
        mountpoint: PathBuf::from("/mnt"),
        snapshot_root: PathBuf::from("@btrfs-manager/scheduled"),
        schedule: btrfs_manager_core::PolicySchedule::Hourly,
        keep_hourly: 1,
        keep_daily: 0,
        keep_weekly: 0,
        keep_monthly: 0,
        enabled: true,
    }
}

// Runner for reconciliation tests: returns a fixed `btrfs subvolume list` output
// so tests control which subvolumes are "present on disk".
pub(crate) struct ListReconcileRunner {
    pub(crate) list_output: String,
}

impl CommandRunner for ListReconcileRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<String, HelperError> {
        if program == "findmnt" && args.iter().any(|arg| arg == "UUID") {
            Ok("550e8400-e29b-41d4-a716-446655440000\n".into())
        } else if program == "findmnt" && args.iter().any(|arg| arg == "--mountpoint") {
            Ok("mounted\n".into())
        } else if program == "btrfs"
            && args.first().map(String::as_str) == Some("subvolume")
            && args.get(1).map(String::as_str) == Some("list")
        {
            Ok(self.list_output.clone())
        } else {
            Ok("ok\n".into())
        }
    }
}

pub(crate) fn managed_snapshot(path: &str, state: SnapshotState) -> Snapshot {
    Snapshot {
        id: Uuid::new_v4(),
        source_subvolume: SubvolumeId(256),
        path: PathBuf::from(path),
        created_at: Utc::now(),
        tags: Vec::new(),
        origin: SnapshotOrigin::Managed,
        state,
    }
}

/// `managed_snapshot`, but also creates the subvolume directory under `top`
/// and inserts the DB row — the combination almost every delete/lock test
/// needs to set up a snapshot that's actually there to be found.
pub(crate) fn place_managed_snapshot(
    store: &StateStore,
    top: &Path,
    path: &str,
    state: SnapshotState,
) -> Snapshot {
    let snapshot = managed_snapshot(path, state);
    std::fs::create_dir_all(top.join(&snapshot.path)).unwrap();
    store.insert_managed_snapshot(None, &snapshot).unwrap();
    snapshot
}

/// A placed rollback-anchor snapshot at a fixed path, for the tests that
/// check the anchor is protected from manual delete/lock/unlock.
pub(crate) fn place_rollback_anchor(store: &StateStore, top: &Path) -> Snapshot {
    place_managed_snapshot(
        store,
        top,
        "@btrfs-manager/anchor",
        SnapshotState::RollbackAnchor,
    )
}
