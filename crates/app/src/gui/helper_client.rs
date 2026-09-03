//! Bridge to the privileged helper. All requests go over D-Bus; the in-process
//! fallback only activates in dev mode when the service is not installed.

use btrfs_manager_helper::{Helper, HelperRequest, HelperResponse, SystemCommandRunner};

use super::i18n::tr;
use crate::dbus_client;

pub(crate) async fn handle_privileged_async(
    request: HelperRequest,
) -> anyhow::Result<HelperResponse> {
    gio::spawn_blocking(move || handle_privileged(request))
        .await
        .map_err(|_| anyhow::anyhow!("helper thread panicked"))?
}

pub(crate) fn handle_privileged(request: HelperRequest) -> anyhow::Result<HelperResponse> {
    match dbus_client::handle(&request) {
        Ok(response) => Ok(response),
        Err(dbus_client::HelperBusError::Request(error)) => Err(error),
        Err(dbus_client::HelperBusError::Unavailable(error)) => {
            if dev_local_helper_enabled() {
                let helper = Helper::new(SystemCommandRunner);
                helper.handle(request).map_err(anyhow::Error::from)
            } else {
                anyhow::bail!("{}: {error}", tr("service_hint"));
            }
        }
    }
}

fn dev_local_helper_enabled() -> bool {
    matches!(
        std::env::var("BTRFS_MANAGER_DEV_LOCAL_HELPER").as_deref(),
        Ok("1" | "true" | "yes" | "on")
    )
}
