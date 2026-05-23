use crate::{Helper, HelperRequest, SystemCommandRunner};
use std::process::Command;
use zbus::{Connection, fdo, interface, message::Header};
use tracing;

pub const SERVICE_NAME: &str = "org.btrfsmanager.Helper";
pub const OBJECT_PATH: &str = "/org/btrfsmanager/Helper";
pub const INTERFACE_NAME: &str = "org.btrfsmanager.Helper";

pub const ACTION_DISCOVERY: &str = "org.btrfsmanager.helper.discovery";
pub const ACTION_SNAPSHOT_CREATE: &str = "org.btrfsmanager.helper.snapshot.create";
pub const ACTION_SNAPSHOT_DELETE: &str = "org.btrfsmanager.helper.snapshot.delete";
pub const ACTION_SNAPSHOT_READONLY: &str = "org.btrfsmanager.helper.snapshot.readonly";
pub const ACTION_MOUNT: &str = "org.btrfsmanager.helper.mount";
pub const ACTION_ROLLBACK: &str = "org.btrfsmanager.helper.rollback";
pub const ACTION_POLICY_READ: &str = "org.btrfsmanager.helper.policy.read";
pub const ACTION_POLICY_WRITE: &str = "org.btrfsmanager.helper.policy.write";

pub struct HelperService {
    connection: Connection,
}

impl HelperService {
    pub fn new(connection: Connection) -> Self {
        Self { connection }
    }

    // Returns the caller's UID on success so the helper can scope mount roots correctly.
    async fn authorize(&self, header: &Header<'_>, request: &HelperRequest) -> fdo::Result<u32> {
        let sender = header
            .sender()
            .ok_or_else(|| fdo::Error::AuthFailed("D-Bus sender is missing".into()))?;
        let sender = sender.to_owned();
        let dbus = zbus::fdo::DBusProxy::new(&self.connection)
            .await
            .map_err(to_failed)?;
        let pid = dbus
            .get_connection_unix_process_id(sender.clone().into())
            .await
            .map_err(to_failed)?;
        let uid = dbus
            .get_connection_unix_user(sender.into())
            .await
            .map_err(to_failed)?;
        let start_time = linux_process_start_time(pid).map_err(to_failed)?;
        let subject = format!("{pid},{start_time},{uid}");
        let action = action_for_request(request);

        tracing::debug!(uid, pid, action, "checking Polkit authorization");

        // pkcheck is a blocking subprocess — run it off the async executor to
        // avoid blocking tokio worker threads during authentication prompts.
        let authorized = tokio::task::spawn_blocking(move || {
            Command::new("pkcheck")
                .arg("--action-id")
                .arg(action)
                .arg("--process")
                .arg(&subject)
                .arg("--allow-user-interaction")
                .status()
                .map(|s| s.success())
        })
        .await
        .map_err(|e| fdo::Error::Failed(e.to_string()))?
        .map_err(to_failed)?;

        if authorized {
            tracing::debug!(uid, action, "Polkit authorized");
            Ok(uid)
        } else {
            tracing::warn!(uid, action, "Polkit denied");
            Err(fdo::Error::AuthFailed(format!(
                "Polkit denied action {action}"
            )))
        }
    }
}

#[interface(name = "org.btrfsmanager.Helper")]
impl HelperService {
    async fn handle(
        &self,
        request_json: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<String> {
        let request = serde_json::from_str::<HelperRequest>(request_json)
            .map_err(|err| fdo::Error::InvalidArgs(err.to_string()))?;
        let action = action_for_request(&request);
        let uid = self.authorize(&header, &request).await?;
        let is_unmount = matches!(request, HelperRequest::UnmountSnapshot { .. });
        let request_debug = format!("{request:?}");
        tracing::info!(uid, action, "handling request");
        // Helper::handle() executes btrfs/mount/systemctl — blocking calls that
        // must not run on the async executor.
        let result =
            tokio::task::spawn_blocking(move || {
                Helper::new(SystemCommandRunner).with_caller_uid(uid).handle(request)
            })
                .await
                .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        match result {
            Ok(response) => {
                tracing::debug!(uid, action, "request completed: {}", response.message);
                serde_json::to_string(&response).map_err(to_failed)
            }
            Err(err) => {
                // UnmountSnapshot failures are often expected (pre-unmount cleanup
                // when nothing was mounted yet). Log at WARN, not ERROR.
                if is_unmount {
                    tracing::warn!(uid, action, error = %err, "unmount failed (may be expected)");
                } else {
                    tracing::error!(uid, action, request = %request_debug, error = %err, "request failed");
                }
                Err(fdo::Error::Failed(err.to_string()))
            }
        }
    }
}

pub fn action_for_request(request: &HelperRequest) -> &'static str {
    match request {
        HelperRequest::DiscoverFilesystems | HelperRequest::ListSubvolumes { .. } => {
            ACTION_DISCOVERY
        }
        HelperRequest::CreateSnapshot { .. } => ACTION_SNAPSHOT_CREATE,
        HelperRequest::DeleteSnapshot { .. } => ACTION_SNAPSHOT_DELETE,
        HelperRequest::SetSnapshotReadOnly { .. } => ACTION_SNAPSHOT_READONLY,
        HelperRequest::MountSnapshot { .. }
        | HelperRequest::MountSubvolume { .. }
        | HelperRequest::MountTopLevel { .. }
        | HelperRequest::UnmountSnapshot { .. }
        | HelperRequest::CleanupManagedMounts => ACTION_MOUNT,
        HelperRequest::CreateManagedSnapshot { .. } => ACTION_SNAPSHOT_CREATE,
        HelperRequest::ListManagedSnapshots => ACTION_POLICY_READ,
        HelperRequest::SetManagedSnapshotReadOnly { .. } => ACTION_SNAPSHOT_READONLY,
        HelperRequest::DeleteManagedSnapshot { .. } => ACTION_SNAPSHOT_DELETE,
        HelperRequest::StageRollback { .. } => ACTION_ROLLBACK,
        HelperRequest::ListSnapshotPolicies
        | HelperRequest::PreviewRetention { .. }
        | HelperRequest::PreviewRetentionForPolicy { .. }
        | HelperRequest::ListPolicyRunLogs { .. } => ACTION_POLICY_READ,
        HelperRequest::UpsertSnapshotPolicy { .. }
        | HelperRequest::SetSnapshotPolicyEnabled { .. }
        | HelperRequest::RunRetentionPolicy { .. } => ACTION_POLICY_WRITE,
    }
}

fn linux_process_start_time(pid: u32) -> std::io::Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let after_name = stat
        .rsplit_once(") ")
        .map(|(_, value)| value)
        .unwrap_or(stat.as_str());
    let fields = after_name.split_whitespace().collect::<Vec<_>>();
    fields
        .get(19)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| std::io::Error::other("failed to parse process start time"))
}

fn to_failed(error: impl std::fmt::Display) -> fdo::Error {
    fdo::Error::Failed(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use uuid::Uuid;

    #[test]
    fn maps_requests_to_polkit_action_classes() {
        assert_eq!(
            action_for_request(&HelperRequest::ListSubvolumes {
                mountpoint: PathBuf::from("/")
            }),
            ACTION_DISCOVERY
        );
        assert_eq!(
            action_for_request(&HelperRequest::CreateSnapshot {
                source: PathBuf::from("/a"),
                destination: PathBuf::from("/b"),
                readonly: true,
            }),
            ACTION_SNAPSHOT_CREATE
        );
        assert_eq!(
            action_for_request(&HelperRequest::DeleteSnapshot {
                path: PathBuf::from("/a"),
            }),
            ACTION_SNAPSHOT_DELETE
        );
        assert_eq!(
            action_for_request(&HelperRequest::SetSnapshotReadOnly {
                path: PathBuf::from("/a"),
                readonly: true,
            }),
            ACTION_SNAPSHOT_READONLY
        );
        assert_eq!(
            action_for_request(&HelperRequest::CleanupManagedMounts),
            ACTION_MOUNT
        );
        assert_eq!(
            action_for_request(&HelperRequest::StageRollback {
                snapshot: PathBuf::from("/a"),
                prepared_subvolume: PathBuf::from("/b"),
                return_snapshot: PathBuf::from("/c"),
            }),
            ACTION_ROLLBACK
        );
        assert_eq!(
            action_for_request(&HelperRequest::RunRetentionPolicy {
                policy_id: Uuid::new_v4()
            }),
            ACTION_POLICY_WRITE
        );
        assert_eq!(
            action_for_request(&HelperRequest::ListSnapshotPolicies),
            ACTION_POLICY_READ
        );
    }
}
