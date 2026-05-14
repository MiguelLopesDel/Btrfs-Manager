use anyhow::Context;
use btrfs_manager_helper::dbus::{HelperService, OBJECT_PATH, SERVICE_NAME};
use btrfs_manager_helper::{Helper, HelperRequest, SystemCommandRunner};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use zbus::Connection;

#[derive(Debug, Parser)]
#[command(name = "btrfs-manager-helper")]
#[command(about = "Privileged helper boundary for Btrfs Manager")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Service,
    ListSubvolumes {
        mountpoint: PathBuf,
    },
    DiscoverFilesystems,
    CreateSnapshot {
        source: PathBuf,
        destination: PathBuf,
        #[arg(long, default_value_t = true)]
        readonly: bool,
    },
    DeleteSnapshot {
        path: PathBuf,
    },
    SetReadonly {
        path: PathBuf,
        readonly: String,
    },
    MountSnapshot {
        source: PathBuf,
        target: PathBuf,
    },
    MountTopLevel {
        mountpoint: PathBuf,
        target: PathBuf,
    },
    UnmountSnapshot {
        target: PathBuf,
    },
    #[command(alias = "cleanup-managed-mount")]
    CleanupManagedMounts,
    ListSnapshotPolicies,
    UpsertSnapshotPolicy {
        #[arg(long)]
        json: String,
    },
    SetSnapshotPolicyEnabled {
        policy_id: uuid::Uuid,
        enabled: String,
    },
    PreviewRetention {
        policy_id: uuid::Uuid,
    },
    PreviewRetentionForPolicy {
        #[arg(long)]
        json: String,
    },
    RunRetentionPolicy {
        #[arg(long, alias = "policy")]
        policy_id: uuid::Uuid,
    },
    ListPolicyRunLogs {
        policy_id: uuid::Uuid,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if matches!(cli.command, Commands::Service) {
        let connection = Connection::system().await?;
        let service = HelperService::new(connection.clone());
        connection.object_server().at(OBJECT_PATH, service).await?;
        connection.request_name(SERVICE_NAME).await?;
        std::future::pending::<()>().await;
        return Ok(());
    }

    let helper = Helper::new(SystemCommandRunner);
    let request = match cli.command {
        Commands::Service => unreachable!(),
        Commands::ListSubvolumes { mountpoint } => HelperRequest::ListSubvolumes { mountpoint },
        Commands::DiscoverFilesystems => HelperRequest::DiscoverFilesystems,
        Commands::CreateSnapshot {
            source,
            destination,
            readonly,
        } => HelperRequest::CreateSnapshot {
            source,
            destination,
            readonly,
        },
        Commands::DeleteSnapshot { path } => HelperRequest::DeleteSnapshot { path },
        Commands::SetReadonly { path, readonly } => HelperRequest::SetSnapshotReadOnly {
            path,
            readonly: parse_bool(&readonly)?,
        },
        Commands::MountSnapshot { source, target } => {
            HelperRequest::MountSnapshot { source, target }
        }
        Commands::MountTopLevel { mountpoint, target } => {
            HelperRequest::MountTopLevel { mountpoint, target }
        }
        Commands::UnmountSnapshot { target } => HelperRequest::UnmountSnapshot { target },
        Commands::CleanupManagedMounts => HelperRequest::CleanupManagedMounts,
        Commands::ListSnapshotPolicies => HelperRequest::ListSnapshotPolicies,
        Commands::UpsertSnapshotPolicy { json } => HelperRequest::UpsertSnapshotPolicy {
            policy: serde_json::from_str(&json)?,
        },
        Commands::SetSnapshotPolicyEnabled { policy_id, enabled } => {
            HelperRequest::SetSnapshotPolicyEnabled {
                policy_id,
                enabled: parse_bool(&enabled)?,
            }
        }
        Commands::PreviewRetention { policy_id } => HelperRequest::PreviewRetention { policy_id },
        Commands::PreviewRetentionForPolicy { json } => HelperRequest::PreviewRetentionForPolicy {
            policy: serde_json::from_str(&json)?,
        },
        Commands::RunRetentionPolicy { policy_id } => {
            HelperRequest::RunRetentionPolicy { policy_id }
        }
        Commands::ListPolicyRunLogs { policy_id } => HelperRequest::ListPolicyRunLogs { policy_id },
    };
    let response = helper.handle(request).context("helper request failed")?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn parse_bool(value: &str) -> anyhow::Result<bool> {
    match value {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => anyhow::bail!("expected boolean value: true or false"),
    }
}
