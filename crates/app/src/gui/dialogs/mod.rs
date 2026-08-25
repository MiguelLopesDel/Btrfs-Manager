//! Secondary windows and dialogs opened from the main inventory view.

mod create_snapshot;
mod diagnostics;
mod policy;
mod rollback;

pub(crate) use create_snapshot::open_create_snapshot_dialog;
pub(crate) use diagnostics::open_diagnostics_window;
pub(crate) use policy::open_policy_dialog;
pub(crate) use rollback::{check_pending_rollback, open_rollback_status_window};
