//! Shared UI state and the filter enums that drive inventory rendering.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use btrfs_manager_helper::{FilesystemDiscovery, SubvolumeInventory};

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) enum SnapshotFilter {
    #[default]
    All,
    Managed,
    External,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) enum TimeRangeFilter {
    Today,
    #[default]
    Last7Days,
    Last30Days,
    AllHistory,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) enum ViewMode {
    #[default]
    ByDay,
    ByHour,
}

#[derive(Clone)]
pub(crate) struct UiState {
    pub(crate) inventory: Rc<RefCell<Option<SubvolumeInventory>>>,
    pub(crate) mounted_snapshots: Rc<RefCell<HashSet<PathBuf>>>,
    pub(crate) session_mounts: Rc<RefCell<HashSet<PathBuf>>>,
    pub(crate) filesystems: Rc<RefCell<FilesystemDiscovery>>,
    pub(crate) suppress_selector_signal: Rc<Cell<bool>>,
    pub(crate) toast_overlay: libadwaita::ToastOverlay,
    pub(crate) filter: Rc<RefCell<SnapshotFilter>>,
    pub(crate) time_range: Rc<RefCell<TimeRangeFilter>>,
    pub(crate) view_mode: Rc<RefCell<ViewMode>>,
    pub(crate) summary_scope: gtk4::Label,
    pub(crate) summary_counts: gtk4::Label,
    pub(crate) summary_filters: gtk4::Label,
    pub(crate) spinner: gtk4::Spinner,
    pub(crate) select_mode: Rc<Cell<bool>>,
    pub(crate) selected: Rc<RefCell<HashSet<PathBuf>>>,
    pub(crate) bulk_bar: gtk4::Revealer,
    pub(crate) bulk_delete_btn: gtk4::Button,
}

pub(crate) fn filter_label(filter: &SnapshotFilter) -> &'static str {
    match filter {
        SnapshotFilter::All => "All",
        SnapshotFilter::Managed => "Managed",
        SnapshotFilter::External => "External",
    }
}

pub(crate) fn time_range_label(time_range: &TimeRangeFilter) -> &'static str {
    match time_range {
        TimeRangeFilter::Today => "Today",
        TimeRangeFilter::Last7Days => "7 days",
        TimeRangeFilter::Last30Days => "30 days",
        TimeRangeFilter::AllHistory => "All history",
    }
}

pub(crate) fn view_mode_label(view_mode: &ViewMode) -> &'static str {
    match view_mode {
        ViewMode::ByDay => "By day",
        ViewMode::ByHour => "By hour",
    }
}
