use btrfs_manager_core::{
    PolicyRunLog, PolicySchedule, RetentionPreview, SnapshotPolicy, Subvolume, SubvolumeKind,
};
use btrfs_manager_helper::{
    FilesystemDiscovery, Helper, HelperRequest, HelperResponse, SubvolumeInventory,
    SystemCommandRunner,
};
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use uuid::Uuid;

use crate::dbus_client;

#[derive(Clone)]
struct UiState {
    inventory: Rc<RefCell<Option<SubvolumeInventory>>>,
    mounted_snapshots: Rc<RefCell<HashSet<PathBuf>>>,
    session_mounts: Rc<RefCell<HashSet<PathBuf>>>,
    filesystems: Rc<RefCell<FilesystemDiscovery>>,
    suppress_selector_signal: Rc<Cell<bool>>,
    toast_overlay: libadwaita::ToastOverlay,
}

pub fn run() {
    let app = libadwaita::Application::builder()
        .application_id("org.btrfsmanager.App")
        .build();

    app.connect_activate(build_ui);
    app.run();
}

pub fn run_check() {
    let app = libadwaita::Application::builder()
        .application_id("org.btrfsmanager.App.Check")
        .build();

    app.connect_activate(|app| {
        build_ui(app);
        let app = app.clone();
        glib::timeout_add_seconds_local_once(1, move || app.quit());
    });
    app.run_with_args::<&str>(&[]);
}

fn build_ui(app: &libadwaita::Application) {
    let inventory_state: Rc<RefCell<Option<SubvolumeInventory>>> = Rc::new(RefCell::new(None));
    let mounted_snapshots: Rc<RefCell<HashSet<PathBuf>>> = Rc::new(RefCell::new(HashSet::new()));
    let session_mounts: Rc<RefCell<HashSet<PathBuf>>> = Rc::new(RefCell::new(HashSet::new()));
    let filesystem_state: Rc<RefCell<FilesystemDiscovery>> =
        Rc::new(RefCell::new(FilesystemDiscovery {
            filesystems: Vec::new(),
        }));
    let suppress_selector_signal = Rc::new(Cell::new(false));

    let header = libadwaita::HeaderBar::new();
    header.set_title_widget(Some(
        &libadwaita::WindowTitle::builder()
            .title("Btrfs Manager")
            .subtitle("Snapshots")
            .build(),
    ));

    let refresh = gtk4::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Refresh")
        .build();
    let cleanup = gtk4::Button::builder()
        .icon_name("media-eject-symbolic")
        .tooltip_text("Unmount temporary browse mounts")
        .build();
    header.pack_end(&cleanup);
    header.pack_end(&refresh);

    let title = gtk4::Label::builder()
        .label("Snapshots")
        .halign(gtk4::Align::Start)
        .css_classes(["title-1"])
        .build();
    let search = gtk4::SearchEntry::builder()
        .placeholder_text("Search snapshots")
        .hexpand(true)
        .build();
    let filesystem_selector = gtk4::ComboBoxText::builder()
        .tooltip_text("Btrfs filesystem")
        .hexpand(true)
        .build();

    let list = gtk4::ListBox::builder()
        .selection_mode(gtk4::SelectionMode::Single)
        .css_classes(["boxed-list"])
        .vexpand(true)
        .build();
    let list_scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .min_content_height(260)
        .vexpand(true)
        .child(&list)
        .build();

    set_status_row(
        &list,
        "No mountpoint loaded",
        "Use Refresh to list Btrfs subvolumes",
    );

    let content = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .margin_top(14)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .vexpand(true)
        .build();
    content.append(&title);
    content.append(&filesystem_selector);
    content.append(&search);
    content.append(&list_scroll);

    let root = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .vexpand(true)
        .build();
    root.append(&header);
    root.append(&content);
    let toast_overlay = libadwaita::ToastOverlay::new();
    toast_overlay.set_child(Some(&root));

    let ui_state = UiState {
        inventory: inventory_state,
        mounted_snapshots,
        session_mounts,
        filesystems: filesystem_state,
        suppress_selector_signal,
        toast_overlay: toast_overlay.clone(),
    };

    let state_for_cleanup = ui_state.clone();
    cleanup.connect_clicked(move |_| {
        match handle_privileged(HelperRequest::CleanupManagedMounts) {
            Ok(response) => {
                state_for_cleanup.mounted_snapshots.borrow_mut().clear();
                state_for_cleanup.session_mounts.borrow_mut().clear();
                show_toast(&state_for_cleanup.toast_overlay, &response.message);
            }
            Err(err) => show_toast(
                &state_for_cleanup.toast_overlay,
                &format!("Failed to unmount temporary mounts: {err}"),
            ),
        }
    });

    let list_for_refresh = list.clone();
    let search_for_refresh = search.clone();
    let state_for_refresh = ui_state.clone();
    let selector_for_refresh = filesystem_selector.clone();
    refresh.connect_clicked(move |_| {
        discover_and_load(
            &list_for_refresh,
            state_for_refresh.clone(),
            &selector_for_refresh,
            search_for_refresh.text().as_str(),
        );
    });

    let list_for_selector = list.clone();
    let search_for_selector = search.clone();
    let state_for_selector = ui_state.clone();
    filesystem_selector.connect_changed(move |selector| {
        if state_for_selector.suppress_selector_signal.get() {
            return;
        }
        let Some(index) = selector.active().map(|index| index as usize) else {
            return;
        };
        let Some(mountpoint) = selected_mountpoint(&state_for_selector.filesystems.borrow(), index)
        else {
            return;
        };
        load_mountpoint(
            &list_for_selector,
            state_for_selector.clone(),
            search_for_selector.text().as_str(),
            mountpoint,
        );
    });

    let list_for_search = list.clone();
    let state_for_search = ui_state.clone();
    search.connect_search_changed(move |entry| {
        if let Some(inventory) = state_for_search.inventory.borrow().as_ref() {
            render_inventory(
                &list_for_search,
                inventory,
                entry.text().as_str(),
                state_for_search.clone(),
            );
        }
    });

    let window = libadwaita::ApplicationWindow::builder()
        .application(app)
        .title("Btrfs Manager")
        .default_width(980)
        .default_height(680)
        .content(&toast_overlay)
        .build();
    let state_for_close = ui_state.clone();
    window.connect_close_request(move |_| match unmount_session_mounts(&state_for_close) {
        Ok(()) => glib::Propagation::Proceed,
        Err(err) => {
            show_toast(
                &state_for_close.toast_overlay,
                &format!("Failed to unmount session browse mounts: {err}"),
            );
            glib::Propagation::Stop
        }
    });
    window.present();

    if managed_mount_roots_exist() {
        match handle_privileged(HelperRequest::CleanupManagedMounts) {
            Ok(_) => {
                ui_state.mounted_snapshots.borrow_mut().clear();
                ui_state.session_mounts.borrow_mut().clear();
            }
            Err(err) => show_toast(
                &ui_state.toast_overlay,
                &format!("Failed to cleanup stale browse mounts: {err}"),
            ),
        }
    }

    discover_and_load(
        &list,
        ui_state,
        &filesystem_selector,
        search.text().as_str(),
    );
}

fn discover_and_load(
    list: &gtk4::ListBox,
    state: UiState,
    selector: &gtk4::ComboBoxText,
    query: &str,
) {
    if let Some(mountpoint) = configured_mountpoint_override() {
        state.suppress_selector_signal.set(true);
        selector.remove_all();
        selector.append_text(&format!("Override: {}", mountpoint.display()));
        selector.set_active(Some(0));
        state.suppress_selector_signal.set(false);
        selector.set_sensitive(false);
        load_mountpoint(list, state, query, mountpoint);
        return;
    }

    selector.set_sensitive(true);
    match handle_privileged(HelperRequest::DiscoverFilesystems) {
        Ok(response) => match response.data {
            Some(data) => match serde_json::from_value::<FilesystemDiscovery>(data) {
                Ok(discovery) => {
                    state.suppress_selector_signal.set(true);
                    selector.remove_all();
                    for filesystem in &discovery.filesystems {
                        selector.append_text(&filesystem_label(filesystem));
                    }
                    let active_index = preferred_filesystem_index(&discovery);
                    *state.filesystems.borrow_mut() = discovery;
                    if let Some(index) = active_index {
                        selector.set_active(Some(index as u32));
                        state.suppress_selector_signal.set(false);
                        let mountpoint = {
                            let filesystems = state.filesystems.borrow();
                            selected_mountpoint(&filesystems, index)
                        };
                        if let Some(mountpoint) = mountpoint {
                            load_mountpoint(list, state, query, mountpoint);
                        }
                    } else {
                        state.suppress_selector_signal.set(false);
                        set_status_row(
                            list,
                            "No Btrfs filesystems found",
                            "Discovery returned no mountpoints",
                        );
                    }
                }
                Err(err) => set_status_row(
                    list,
                    "Failed to read filesystem discovery",
                    &err.to_string(),
                ),
            },
            None => set_status_row(list, "No filesystem discovery returned", &response.message),
        },
        Err(err) => set_status_row(list, "Filesystem discovery failed", &err.to_string()),
    }
}

fn configured_mountpoint_override() -> Option<PathBuf> {
    std::env::var_os("BTRFS_MANAGER_MOUNTPOINT").map(PathBuf::from)
}

fn load_mountpoint(list: &gtk4::ListBox, state: UiState, query: &str, mountpoint: PathBuf) {
    clear_list(list);
    set_status_row(list, "Loading", &mountpoint.display().to_string());

    match handle_privileged(HelperRequest::ListSubvolumes { mountpoint }) {
        Ok(response) => match response.data {
            Some(data) => match serde_json::from_value::<SubvolumeInventory>(data) {
                Ok(inventory) => {
                    *state.inventory.borrow_mut() = Some(inventory.clone());
                    render_inventory(list, &inventory, query, state);
                }
                Err(err) => set_status_row(list, "Failed to read inventory", &err.to_string()),
            },
            None => set_status_row(list, "No structured data returned", &response.message),
        },
        Err(err) => set_status_row(list, "Discovery failed", &err.to_string()),
    }
}

fn preferred_filesystem_index(discovery: &FilesystemDiscovery) -> Option<usize> {
    discovery
        .filesystems
        .iter()
        .position(|filesystem| filesystem.mounts.iter().any(|mount| mount.is_active_root))
        .or_else(|| (!discovery.filesystems.is_empty()).then_some(0))
}

fn selected_mountpoint(discovery: &FilesystemDiscovery, index: usize) -> Option<PathBuf> {
    let filesystem = discovery.filesystems.get(index)?;
    filesystem
        .mounts
        .iter()
        .find(|mount| mount.is_active_root)
        .or_else(|| filesystem.mounts.first())
        .map(|mount| mount.mountpoint.clone())
}

fn filesystem_label(filesystem: &btrfs_manager_core::models::FilesystemSummary) -> String {
    let primary_mount = filesystem
        .mounts
        .iter()
        .find(|mount| mount.is_active_root)
        .or_else(|| filesystem.mounts.first());
    let mountpoint = primary_mount
        .map(|mount| mount.mountpoint.display().to_string())
        .unwrap_or_else(|| "not mounted".to_string());
    let device = filesystem
        .devices
        .first()
        .map(|device| device.display().to_string())
        .unwrap_or_else(|| "unknown device".to_string());
    format!("{mountpoint} on {device}")
}

fn handle_privileged(request: HelperRequest) -> anyhow::Result<HelperResponse> {
    let can_try_local = can_run_unprivileged(&request);
    let dbus_error = match dbus_client::handle(&request) {
        Ok(response) => return Ok(response),
        Err(error) => error,
    };

    if matches!(&dbus_error, dbus_client::HelperBusError::Request(_)) {
        return Err(anyhow::Error::new(dbus_error));
    }

    if std::env::var_os("BTRFS_MANAGER_REQUIRE_DBUS").is_some() {
        return Err(anyhow::anyhow!("D-Bus helper request failed: {dbus_error}"));
    }

    let local_error = if can_try_local {
        let helper = Helper::new(SystemCommandRunner);
        match helper.handle(request.clone()) {
            Ok(response) => return Ok(response),
            Err(err) => Some(anyhow::Error::from(err)),
        }
    } else {
        None
    };

    let helper_path = helper_binary_path();

    if command_exists("pkexec") && helper_path.exists() {
        let mut command = Command::new("pkexec");
        command.arg(helper_path);
        append_helper_cli_args(&mut command, &request)?;
        let output = command.output()?;
        if output.status.success() {
            let response = serde_json::from_slice::<HelperResponse>(&output.stdout)?;
            return Ok(response);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if let Some(err) = local_error {
            anyhow::bail!(
                "helper authorization or execution failed: {stderr}; local fallback also failed: {err}"
            );
        }
        if !can_try_local {
            anyhow::bail!("helper authorization or execution failed: {stderr}");
        }
    } else if let Some(err) = local_error {
        anyhow::bail!("local helper fallback failed and pkexec helper is unavailable: {err}");
    }

    let helper = Helper::new(SystemCommandRunner);
    helper.handle(request).map_err(anyhow::Error::from)
}

fn can_run_unprivileged(request: &HelperRequest) -> bool {
    matches!(
        request,
        HelperRequest::DiscoverFilesystems | HelperRequest::ListSubvolumes { .. }
    )
}

fn append_helper_cli_args(command: &mut Command, request: &HelperRequest) -> anyhow::Result<()> {
    match request {
        HelperRequest::DiscoverFilesystems => {
            command.arg("discover-filesystems");
            Ok(())
        }
        HelperRequest::ListSubvolumes { mountpoint } => {
            command.arg("list-subvolumes").arg(mountpoint);
            Ok(())
        }
        HelperRequest::MountSnapshot { source, target } => {
            command.arg("mount-snapshot").arg(source).arg(target);
            Ok(())
        }
        HelperRequest::MountTopLevel { mountpoint, target } => {
            command.arg("mount-top-level").arg(mountpoint).arg(target);
            Ok(())
        }
        HelperRequest::UnmountSnapshot { target } => {
            command.arg("unmount-snapshot").arg(target);
            Ok(())
        }
        HelperRequest::CleanupManagedMounts => {
            command.arg("cleanup-managed-mounts");
            Ok(())
        }
        HelperRequest::ListSnapshotPolicies => {
            command.arg("list-snapshot-policies");
            Ok(())
        }
        HelperRequest::UpsertSnapshotPolicy { policy } => {
            command
                .arg("upsert-snapshot-policy")
                .arg("--json")
                .arg(serde_json::to_string(policy)?);
            Ok(())
        }
        HelperRequest::SetSnapshotPolicyEnabled { policy_id, enabled } => {
            command
                .arg("set-snapshot-policy-enabled")
                .arg(policy_id.to_string())
                .arg(enabled.to_string());
            Ok(())
        }
        HelperRequest::PreviewRetention { policy_id } => {
            command.arg("preview-retention").arg(policy_id.to_string());
            Ok(())
        }
        HelperRequest::PreviewRetentionForPolicy { policy } => {
            command
                .arg("preview-retention-for-policy")
                .arg("--json")
                .arg(serde_json::to_string(policy)?);
            Ok(())
        }
        HelperRequest::RunRetentionPolicy { policy_id } => {
            command
                .arg("run-retention-policy")
                .arg("--policy-id")
                .arg(policy_id.to_string());
            Ok(())
        }
        HelperRequest::ListPolicyRunLogs { policy_id } => {
            command
                .arg("list-policy-run-logs")
                .arg(policy_id.to_string());
            Ok(())
        }
        _ => anyhow::bail!("helper request is not wired to the GUI yet"),
    }
}

fn helper_binary_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent()
                .map(|parent| parent.join("btrfs-manager-helper"))
        })
        .unwrap_or_else(|| PathBuf::from("target/debug/btrfs-manager-helper"))
}

fn command_exists(command: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {command} >/dev/null 2>&1"))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn unmount_session_mounts(state: &UiState) -> anyhow::Result<()> {
    let mut targets = state
        .session_mounts
        .borrow()
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    targets.sort_by_key(|target| std::cmp::Reverse(target.as_os_str().len()));
    for target in &targets {
        handle_privileged(HelperRequest::UnmountSnapshot {
            target: target.clone(),
        })?;
    }
    state.session_mounts.borrow_mut().clear();
    state.mounted_snapshots.borrow_mut().clear();
    Ok(())
}

fn browse_mount_root() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .map(|runtime_dir| runtime_dir.join("btrfs-manager").join("browse"))
        .unwrap_or_else(|| std::env::temp_dir().join("btrfs-manager-browse"))
}

fn managed_mount_roots_exist() -> bool {
    browse_mount_root().exists() || std::env::temp_dir().join("btrfs-manager-toplevel").exists()
}

fn snapshot_subtitle(id: u64, mounted: bool, target: &std::path::Path) -> String {
    if mounted {
        format!("ID {id} · mounted at {}", target.display())
    } else {
        format!("ID {id}")
    }
}

fn show_toast(toast_overlay: &libadwaita::ToastOverlay, message: &str) {
    toast_overlay.add_toast(libadwaita::Toast::new(message));
}

fn top_level_mount_target(mountpoint: &std::path::Path) -> PathBuf {
    let label = if mountpoint == std::path::Path::new("/") {
        "root".to_string()
    } else {
        mountpoint
            .file_name()
            .and_then(|name| name.to_str())
            .map(sanitize_name)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "mount".to_string())
    };
    std::env::temp_dir()
        .join("btrfs-manager-toplevel")
        .join(format!("{label}-{:08x}", path_hash(mountpoint)))
}

fn short_snapshot_mount_name(path: &std::path::Path) -> String {
    let mut components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    let leaf = components.pop().unwrap_or("snapshot");
    let label = if leaf == "snapshot" {
        components.pop().unwrap_or("snapshot")
    } else {
        leaf
    };
    format!("snapshot-{}-{:08x}", sanitize_name(label), path_hash(path))
}

fn sanitize_name(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn path_hash(path: &std::path::Path) -> u32 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish() as u32
}

fn set_status_row(list: &gtk4::ListBox, title: &str, subtitle: &str) {
    clear_list(list);
    append_info_row(list, title, subtitle);
}

fn append_info_row(list: &gtk4::ListBox, title: &str, subtitle: &str) {
    let row = libadwaita::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .build();
    list.append(&row);
}

fn append_section_header(list: &gtk4::ListBox, title: &str) {
    let label = gtk4::Label::builder()
        .label(title)
        .halign(gtk4::Align::Start)
        .margin_top(14)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .css_classes(["heading", "dim-label"])
        .build();
    let row = gtk4::ListBoxRow::builder()
        .selectable(false)
        .activatable(false)
        .child(&label)
        .build();
    list.append(&row);
}

fn is_snapshot_kind(kind: &SubvolumeKind) -> bool {
    matches!(
        kind,
        SubvolumeKind::Snapshot | SubvolumeKind::ExternalSnapshot { .. }
    )
}

fn matches_query(path: &std::path::Path, query: &str) -> bool {
    let query = query.trim().to_ascii_lowercase();
    query.is_empty() || path.to_string_lossy().to_ascii_lowercase().contains(&query)
}

fn clear_list(list: &gtk4::ListBox) {
    while let Some(row) = list.first_child() {
        list.remove(&row);
    }
}

fn render_inventory(
    list: &gtk4::ListBox,
    inventory: &SubvolumeInventory,
    query: &str,
    state: UiState,
) {
    clear_list(list);
    if inventory.subvolumes.is_empty() {
        set_status_row(
            list,
            "No subvolumes found",
            &inventory.mountpoint.display().to_string(),
        );
        return;
    }

    let snapshots: Vec<_> = inventory
        .subvolumes
        .iter()
        .filter(|subvolume| is_snapshot_kind(&subvolume.kind))
        .filter(|subvolume| matches_query(&subvolume.path, query))
        .collect();
    let subvolumes: Vec<_> = inventory
        .subvolumes
        .iter()
        .filter(|subvolume| !is_snapshot_kind(&subvolume.kind))
        .filter(|subvolume| matches_query(&subvolume.path, query))
        .collect();

    append_section_header(list, &format!("Snapshots ({})", snapshots.len()));
    if snapshots.is_empty() {
        append_info_row(
            list,
            "No snapshots found",
            "Create or import snapshots to show them here",
        );
    } else {
        for snapshot in snapshots {
            let source_mountpoint = inventory.mountpoint.clone();
            let relative_path = snapshot.path.clone();
            let target = browse_mount_target(&relative_path);
            let is_mounted = state.mounted_snapshots.borrow().contains(&target);
            let row = libadwaita::ActionRow::builder()
                .title(snapshot.path.display().to_string())
                .subtitle(snapshot_subtitle(snapshot.id.0, is_mounted, &target))
                .build();
            let browse = gtk4::Button::builder()
                .icon_name("folder-open-symbolic")
                .tooltip_text("Browse read-only")
                .valign(gtk4::Align::Center)
                .sensitive(!is_mounted)
                .build();
            let unmount = gtk4::Button::builder()
                .icon_name("media-eject-symbolic")
                .tooltip_text("Unmount browse view")
                .valign(gtk4::Align::Center)
                .sensitive(is_mounted)
                .build();

            let row_for_browse = row.clone();
            let browse_for_browse = browse.clone();
            let unmount_for_browse = unmount.clone();
            let state_for_browse = state.clone();
            let snapshot_id = snapshot.id.0;
            browse.connect_clicked(move |_| {
                match browse_snapshot_readonly(source_mountpoint.clone(), relative_path.clone()) {
                    Ok(mounted) => {
                        state_for_browse
                            .mounted_snapshots
                            .borrow_mut()
                            .insert(mounted.target.clone());
                        state_for_browse
                            .session_mounts
                            .borrow_mut()
                            .extend(mounted.created_mounts.iter().cloned());
                        row_for_browse.set_subtitle(&snapshot_subtitle(
                            snapshot_id,
                            true,
                            &mounted.target,
                        ));
                        browse_for_browse.set_sensitive(false);
                        unmount_for_browse.set_sensitive(true);
                        show_toast(
                            &state_for_browse.toast_overlay,
                            "Snapshot mounted read-only",
                        );
                    }
                    Err(err) => show_toast(
                        &state_for_browse.toast_overlay,
                        &format!("Failed to browse snapshot: {err}"),
                    ),
                }
            });

            let row_for_unmount = row.clone();
            let browse_for_unmount = browse.clone();
            let unmount_for_unmount = unmount.clone();
            let state_for_unmount = state.clone();
            let target_for_unmount = target.clone();
            unmount.connect_clicked(move |_| {
                match handle_privileged(HelperRequest::UnmountSnapshot {
                    target: target_for_unmount.clone(),
                }) {
                    Ok(_) => {
                        state_for_unmount
                            .mounted_snapshots
                            .borrow_mut()
                            .remove(&target_for_unmount);
                        row_for_unmount.set_subtitle(&snapshot_subtitle(
                            snapshot_id,
                            false,
                            &target_for_unmount,
                        ));
                        browse_for_unmount.set_sensitive(true);
                        unmount_for_unmount.set_sensitive(false);
                        show_toast(&state_for_unmount.toast_overlay, "Snapshot unmounted");
                    }
                    Err(err) => show_toast(
                        &state_for_unmount.toast_overlay,
                        &format!("Failed to unmount snapshot: {err}"),
                    ),
                }
            });
            row.add_suffix(&browse);
            row.add_suffix(&unmount);
            list.append(&row);
        }
    }

    append_section_header(list, &format!("Subvolumes ({})", subvolumes.len()));
    for subvolume in subvolumes {
        let mountpoint = inventory.mountpoint.clone();
        let row = libadwaita::ActionRow::builder()
            .title(subvolume.path.display().to_string())
            .subtitle(format!("ID {}", subvolume.id.0))
            .build();
        let schedule = gtk4::Button::builder()
            .icon_name("alarm-symbolic")
            .tooltip_text("Snapshot policy")
            .valign(gtk4::Align::Center)
            .build();
        let state_for_policy = state.clone();
        let subvolume_for_policy = (*subvolume).clone();
        schedule.connect_clicked(move |_| {
            open_policy_dialog(
                state_for_policy.clone(),
                mountpoint.clone(),
                subvolume_for_policy.clone(),
            );
        });
        row.add_suffix(&schedule);
        list.append(&row);
    }
}

fn labeled_widget(label: &str, widget: &impl IsA<gtk4::Widget>) -> gtk4::Box {
    let row = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(12)
        .build();
    let label = gtk4::Label::builder()
        .label(label)
        .halign(gtk4::Align::Start)
        .hexpand(true)
        .build();
    row.append(&label);
    row.append(widget);
    row
}

fn retention_spin(value: usize) -> gtk4::SpinButton {
    gtk4::SpinButton::with_range(0.0, 500.0, 1.0).tap(|spin| {
        spin.set_value(value as f64);
    })
}

trait Tap: Sized {
    fn tap(self, f: impl FnOnce(&Self)) -> Self {
        f(&self);
        self
    }
}

impl<T> Tap for T {}

fn load_policy_for_subvolume(
    subvolume_id: u64,
    source_path: &std::path::Path,
) -> Option<SnapshotPolicy> {
    let response = handle_privileged(HelperRequest::ListSnapshotPolicies).ok()?;
    let policies = serde_json::from_value::<Vec<SnapshotPolicy>>(response.data?).ok()?;
    policies
        .into_iter()
        .find(|policy| policy.subvolume_id.0 == subvolume_id && policy.source_path == source_path)
}

fn format_policy_logs(logs: &[PolicyRunLog]) -> String {
    if logs.is_empty() {
        return "No policy runs yet".into();
    }
    logs.iter()
        .take(5)
        .map(|log| {
            format!(
                "{} · {:?} · created: {} · deleted: {}",
                log.started_at,
                log.status,
                log.created_snapshot
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "-".into()),
                log.deleted_snapshots.len()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn connect_policy_invalidation(
    snapshot_root: &gtk4::Entry,
    schedule: &gtk4::ComboBoxText,
    enabled: &gtk4::Switch,
    spins: [&gtk4::SpinButton; 4],
    save: &gtk4::Button,
    run: &gtk4::Button,
    preview_valid: Rc<Cell<bool>>,
) {
    let preview_for_entry = preview_valid.clone();
    let save_for_entry = save.clone();
    let run_for_entry = run.clone();
    snapshot_root.connect_changed(move |_| {
        invalidate_policy_preview(&preview_for_entry, &save_for_entry, &run_for_entry)
    });

    let preview_for_schedule = preview_valid.clone();
    let save_for_schedule = save.clone();
    let run_for_schedule = run.clone();
    schedule.connect_changed(move |_| {
        invalidate_policy_preview(&preview_for_schedule, &save_for_schedule, &run_for_schedule)
    });

    let preview_for_enabled = preview_valid.clone();
    let save_for_enabled = save.clone();
    let run_for_enabled = run.clone();
    enabled.connect_active_notify(move |_| {
        invalidate_policy_preview(&preview_for_enabled, &save_for_enabled, &run_for_enabled)
    });

    for spin in spins {
        let preview_for_spin = preview_valid.clone();
        let save_for_spin = save.clone();
        let run_for_spin = run.clone();
        spin.connect_value_changed(move |_| {
            invalidate_policy_preview(&preview_for_spin, &save_for_spin, &run_for_spin)
        });
    }
}

fn invalidate_policy_preview(
    preview_valid: &Rc<Cell<bool>>,
    save: &gtk4::Button,
    run: &gtk4::Button,
) {
    preview_valid.set(false);
    save.set_sensitive(false);
    run.set_sensitive(false);
}

fn open_policy_dialog(state: UiState, mountpoint: PathBuf, subvolume: Subvolume) {
    let source_path = mountpoint.join(&subvolume.path);
    let existing = load_policy_for_subvolume(subvolume.id.0, &source_path);
    let policy_id = existing
        .as_ref()
        .map(|policy| policy.id)
        .unwrap_or_else(Uuid::new_v4);

    let window = gtk4::Window::builder()
        .title("Snapshot Policy")
        .default_width(520)
        .default_height(620)
        .modal(true)
        .build();
    let content = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();

    let title = gtk4::Label::builder()
        .label(format!("Policy for {}", subvolume.path.display()))
        .halign(gtk4::Align::Start)
        .css_classes(["title-3"])
        .build();
    content.append(&title);

    let enabled = gtk4::Switch::builder()
        .active(
            existing
                .as_ref()
                .map(|policy| policy.enabled)
                .unwrap_or(true),
        )
        .halign(gtk4::Align::Start)
        .build();
    content.append(&labeled_widget("Enabled", &enabled));

    let schedule = gtk4::ComboBoxText::new();
    for item in ["hourly", "daily", "weekly", "monthly"] {
        schedule.append(Some(item), item);
    }
    schedule.set_active_id(Some(
        existing
            .as_ref()
            .map(|policy| policy.schedule.as_str())
            .unwrap_or("hourly"),
    ));
    content.append(&labeled_widget("Schedule", &schedule));

    let snapshot_root = gtk4::Entry::builder()
        .text(
            existing
                .as_ref()
                .map(|policy| policy.snapshot_root.display().to_string())
                .unwrap_or_else(|| ".snapshots".into()),
        )
        .build();
    content.append(&labeled_widget("Snapshot root", &snapshot_root));

    let keep_hourly = retention_spin(
        existing
            .as_ref()
            .map(|policy| policy.keep_hourly)
            .unwrap_or(24),
    );
    let keep_daily = retention_spin(
        existing
            .as_ref()
            .map(|policy| policy.keep_daily)
            .unwrap_or(7),
    );
    let keep_weekly = retention_spin(
        existing
            .as_ref()
            .map(|policy| policy.keep_weekly)
            .unwrap_or(4),
    );
    let keep_monthly = retention_spin(
        existing
            .as_ref()
            .map(|policy| policy.keep_monthly)
            .unwrap_or(6),
    );
    content.append(&labeled_widget("Keep hourly", &keep_hourly));
    content.append(&labeled_widget("Keep daily", &keep_daily));
    content.append(&labeled_widget("Keep weekly", &keep_weekly));
    content.append(&labeled_widget("Keep monthly", &keep_monthly));

    let preview = gtk4::Label::builder()
        .label("Preview not loaded")
        .halign(gtk4::Align::Start)
        .wrap(true)
        .build();
    content.append(&preview);

    let logs = gtk4::Label::builder()
        .label("No logs loaded")
        .halign(gtk4::Align::Start)
        .wrap(true)
        .build();
    content.append(&logs);

    let actions = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk4::Align::End)
        .build();
    let preview_button = gtk4::Button::with_label("Preview");
    let save_button = gtk4::Button::with_label("Save");
    let run_button = gtk4::Button::with_label("Run Now");
    let logs_button = gtk4::Button::with_label("Logs");
    save_button.set_sensitive(false);
    run_button.set_sensitive(false);
    actions.append(&preview_button);
    actions.append(&save_button);
    actions.append(&run_button);
    actions.append(&logs_button);
    content.append(&actions);
    window.set_child(Some(&content));

    let build_policy = {
        let schedule = schedule.clone();
        let snapshot_root = snapshot_root.clone();
        let enabled = enabled.clone();
        let keep_hourly = keep_hourly.clone();
        let keep_daily = keep_daily.clone();
        let keep_weekly = keep_weekly.clone();
        let keep_monthly = keep_monthly.clone();
        move || SnapshotPolicy {
            id: policy_id,
            filesystem_id: None,
            subvolume_id: subvolume.id.clone(),
            source_path: source_path.clone(),
            mountpoint: mountpoint.clone(),
            snapshot_root: PathBuf::from(snapshot_root.text().as_str()),
            schedule: schedule
                .active_id()
                .as_deref()
                .unwrap_or("hourly")
                .parse()
                .unwrap_or(PolicySchedule::Hourly),
            keep_hourly: keep_hourly.value() as usize,
            keep_daily: keep_daily.value() as usize,
            keep_weekly: keep_weekly.value() as usize,
            keep_monthly: keep_monthly.value() as usize,
            enabled: enabled.is_active(),
        }
    };

    let build_policy = Rc::new(build_policy);
    let preview_valid = Rc::new(Cell::new(false));
    connect_policy_invalidation(
        &snapshot_root,
        &schedule,
        &enabled,
        [&keep_hourly, &keep_daily, &keep_weekly, &keep_monthly],
        &save_button,
        &run_button,
        preview_valid.clone(),
    );
    let preview_for_button = preview.clone();
    let state_for_preview = state.clone();
    let build_for_preview = build_policy.clone();
    let preview_valid_for_button = preview_valid.clone();
    let save_for_preview = save_button.clone();
    let run_for_preview = run_button.clone();
    preview_button.connect_clicked(move |_| {
        let policy = build_for_preview();
        match handle_privileged(HelperRequest::PreviewRetentionForPolicy { policy }) {
            Ok(response) => match response.data {
                Some(data) => match serde_json::from_value::<RetentionPreview>(data) {
                    Ok(value) => {
                        preview_for_button.set_label(&format!(
                            "Next: {}\nWould delete: {}\nWould keep: {}",
                            value.next_snapshot_path.display(),
                            value.delete.len(),
                            value.keep.len()
                        ));
                        preview_valid_for_button.set(true);
                        save_for_preview.set_sensitive(true);
                        run_for_preview.set_sensitive(true);
                    }
                    Err(err) => show_toast(&state_for_preview.toast_overlay, &err.to_string()),
                },
                None => preview_for_button.set_label(&response.message),
            },
            Err(err) => show_toast(&state_for_preview.toast_overlay, &err.to_string()),
        }
    });

    let state_for_save = state.clone();
    let build_for_save = build_policy.clone();
    let preview_valid_for_save = preview_valid.clone();
    save_button.connect_clicked(move |_| {
        if !preview_valid_for_save.get() {
            show_toast(
                &state_for_save.toast_overlay,
                "Preview retention before saving",
            );
            return;
        }
        let policy = build_for_save();
        match handle_privileged(HelperRequest::UpsertSnapshotPolicy { policy }) {
            Ok(response) => show_toast(&state_for_save.toast_overlay, &response.message),
            Err(err) => show_toast(&state_for_save.toast_overlay, &err.to_string()),
        }
    });

    let state_for_run = state.clone();
    let build_for_run = build_policy.clone();
    let preview_valid_for_run = preview_valid.clone();
    run_button.connect_clicked(move |_| {
        if !preview_valid_for_run.get() {
            show_toast(
                &state_for_run.toast_overlay,
                "Preview retention before running",
            );
            return;
        }
        let policy = build_for_run();
        match handle_privileged(HelperRequest::UpsertSnapshotPolicy {
            policy: policy.clone(),
        })
        .and_then(|_| {
            handle_privileged(HelperRequest::RunRetentionPolicy {
                policy_id: policy.id,
            })
        }) {
            Ok(response) => show_toast(&state_for_run.toast_overlay, &response.message),
            Err(err) => show_toast(&state_for_run.toast_overlay, &err.to_string()),
        }
    });

    let state_for_logs = state.clone();
    let logs_for_button = logs.clone();
    logs_button.connect_clicked(move |_| {
        match handle_privileged(HelperRequest::ListPolicyRunLogs { policy_id }) {
            Ok(response) => match response.data {
                Some(data) => match serde_json::from_value::<Vec<PolicyRunLog>>(data) {
                    Ok(values) => logs_for_button.set_label(&format_policy_logs(&values)),
                    Err(err) => show_toast(&state_for_logs.toast_overlay, &err.to_string()),
                },
                None => logs_for_button.set_label(&response.message),
            },
            Err(err) => show_toast(&state_for_logs.toast_overlay, &err.to_string()),
        }
    });

    window.present();
}

fn browse_snapshot_readonly(
    mountpoint: PathBuf,
    relative_path: PathBuf,
) -> anyhow::Result<MountedBrowse> {
    let resolved = resolve_subvolume_path(&mountpoint, &relative_path)?;
    let target = browse_mount_target(&relative_path);
    std::fs::create_dir_all(&target)?;
    let _ = handle_privileged(HelperRequest::UnmountSnapshot {
        target: target.clone(),
    });
    handle_privileged(HelperRequest::MountSnapshot {
        source: resolved.source,
        target: target.clone(),
    })?;
    Command::new("xdg-open").arg(&target).spawn()?;
    let mut created_mounts = resolved.created_mounts;
    created_mounts.push(target.clone());
    Ok(MountedBrowse {
        target,
        created_mounts,
    })
}

struct ResolvedSubvolumePath {
    source: PathBuf,
    created_mounts: Vec<PathBuf>,
}

struct MountedBrowse {
    target: PathBuf,
    created_mounts: Vec<PathBuf>,
}

fn resolve_subvolume_path(
    mountpoint: &std::path::Path,
    relative_path: &std::path::Path,
) -> anyhow::Result<ResolvedSubvolumePath> {
    let direct = mountpoint.join(relative_path);
    if direct.exists() {
        return Ok(ResolvedSubvolumePath {
            source: direct,
            created_mounts: Vec::new(),
        });
    }

    let top_level = top_level_mount_target(mountpoint);
    std::fs::create_dir_all(&top_level)?;
    let mut created_mounts = Vec::new();
    if !top_level.join(relative_path).exists() {
        handle_privileged(HelperRequest::MountTopLevel {
            mountpoint: mountpoint.to_path_buf(),
            target: top_level.clone(),
        })?;
        created_mounts.push(top_level.clone());
    }

    let resolved = top_level.join(relative_path);
    if resolved.exists() {
        Ok(ResolvedSubvolumePath {
            source: resolved,
            created_mounts,
        })
    } else {
        anyhow::bail!(
            "snapshot path is not accessible from current or top-level mount: {}",
            relative_path.display()
        );
    }
}

fn browse_mount_target(source: &std::path::Path) -> PathBuf {
    browse_mount_root().join(short_snapshot_mount_name(source))
}
