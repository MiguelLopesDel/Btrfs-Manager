//! Unit tests for the helper, grouped by concern. Kept out of the runtime
//! modules so those stay reviewable; test files exercise crate-internal
//! items via `crate::` paths.

mod support;

mod diagnostics_tests;
mod mount_tests;
mod policy_tests;
mod retention_tests;
mod rollback_tests;
mod snapshot_tests;
mod subvolume_tests;
