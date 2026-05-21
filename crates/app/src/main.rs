#[cfg(feature = "gui")]
mod dbus_client;

#[cfg(feature = "gui")]
mod gui;

#[cfg(feature = "gui")]
fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    if std::env::args().any(|arg| arg == "--check-gui") {
        gui::run_check();
    } else {
        gui::run();
    }
}

#[cfg(not(feature = "gui"))]
use btrfs_manager_helper::{Helper, HelperRequest, SystemCommandRunner};
#[cfg(not(feature = "gui"))]
use clap::{Parser, Subcommand};
#[cfg(not(feature = "gui"))]
use std::path::PathBuf;

#[cfg(not(feature = "gui"))]
#[derive(Debug, Parser)]
#[command(name = "btrfs-manager-app")]
#[command(about = "Btrfs Manager application shell")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[cfg(not(feature = "gui"))]
#[derive(Debug, Subcommand)]
enum Commands {
    List {
        #[arg(long)]
        mountpoint: PathBuf,
    },
}

#[cfg(not(feature = "gui"))]
fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::List { mountpoint }) => {
            let helper = Helper::new(SystemCommandRunner);
            let response = helper.handle(HelperRequest::ListSubvolumes { mountpoint })?;
            if let Some(data) = response.data {
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else {
                println!("{}", response.message);
            }
        }
        None => {
            println!("Btrfs Manager app shell");
            println!(
                "Try `list --mountpoint /mnt/btrfs-manager-test` or build with `--features gui`."
            );
        }
    }
    Ok(())
}
