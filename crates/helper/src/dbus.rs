use crate::{Helper, HelperRequest, SystemCommandRunner};
use std::process::Command;
use zbus::{Connection, fdo, interface, message::Header};

pub const SERVICE_NAME: &str = "org.btrfsmanager.Helper";
pub const OBJECT_PATH: &str = "/org/btrfsmanager/Helper";
pub const INTERFACE_NAME: &str = "org.btrfsmanager.Helper";

pub const ACTION_DISCOVERY: &str = "org.btrfsmanager.helper.discovery";
pub const ACTION_SNAPSHOT: &str = "org.btrfsmanager.helper.snapshot";
pub const ACTION_MOUNT: &str = "org.btrfsmanager.helper.mount";
pub const ACTION_ROLLBACK: &str = "org.btrfsmanager.helper.rollback";
pub const ACTION_POLICY: &str = "org.btrfsmanager.helper.policy";

pub struct HelperService {
    helper: Helper<SystemCommandRunner>,
    connection: Connection,
}

impl HelperService {
    pub fn new(connection: Connection) -> Self {
        Self {
            helper: Helper::new(SystemCommandRunner),
            connection,
        }
    }

    async fn authorize(&self, header: &Header<'_>, request: &HelperRequest) -> fdo::Result<()> {
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
        let status = Command::new("pkcheck")
            .arg("--action-id")
            .arg(action)
            .arg("--process")
            .arg(subject)
            .arg("--allow-user-interaction")
            .status()
            .map_err(to_failed)?;

        if status.success() {
            Ok(())
        } else {
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
        self.authorize(&header, &request).await?;
        let response = self
            .helper
            .handle(request)
            .map_err(|err| fdo::Error::Failed(err.to_string()))?;
        serde_json::to_string(&response).map_err(to_failed)
    }
}

pub fn action_for_request(request: &HelperRequest) -> &'static str {
    match request {
        HelperRequest::DiscoverFilesystems | HelperRequest::ListSubvolumes { .. } => {
            ACTION_DISCOVERY
        }
        HelperRequest::CreateSnapshot { .. }
        | HelperRequest::DeleteSnapshot { .. }
        | HelperRequest::SetSnapshotReadOnly { .. } => ACTION_SNAPSHOT,
        HelperRequest::MountSnapshot { .. }
        | HelperRequest::MountTopLevel { .. }
        | HelperRequest::UnmountSnapshot { .. }
        | HelperRequest::CleanupManagedMounts => ACTION_MOUNT,
        HelperRequest::StageRollback { .. } => ACTION_ROLLBACK,
        HelperRequest::ListSnapshotPolicies
        | HelperRequest::UpsertSnapshotPolicy { .. }
        | HelperRequest::SetSnapshotPolicyEnabled { .. }
        | HelperRequest::PreviewRetention { .. }
        | HelperRequest::PreviewRetentionForPolicy { .. }
        | HelperRequest::RunRetentionPolicy { .. }
        | HelperRequest::ListPolicyRunLogs { .. } => ACTION_POLICY,
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
            ACTION_SNAPSHOT
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
            ACTION_POLICY
        );
    }
}
