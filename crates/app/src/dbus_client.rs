use btrfs_manager_helper::dbus::{INTERFACE_NAME, OBJECT_PATH, SERVICE_NAME};
use btrfs_manager_helper::{HelperRequest, HelperResponse};

#[derive(Debug)]
pub enum HelperBusError {
    Unavailable(anyhow::Error),
    Request(anyhow::Error),
}

impl std::fmt::Display for HelperBusError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(error) | Self::Request(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for HelperBusError {}

pub fn handle(request: &HelperRequest) -> Result<HelperResponse, HelperBusError> {
    let connection = zbus::blocking::Connection::system()
        .map_err(|error| HelperBusError::Unavailable(error.into()))?;
    let request_json = serde_json::to_string(request)?;
    let reply = connection
        .call_method(
            Some(SERVICE_NAME),
            OBJECT_PATH,
            Some(INTERFACE_NAME),
            "Handle",
            &(request_json.as_str()),
        )
        .map_err(classify_call_error)?;
    let response_json = reply
        .body()
        .deserialize::<String>()
        .map_err(|error| HelperBusError::Request(error.into()))?;
    let response = serde_json::from_str::<HelperResponse>(&response_json)
        .map_err(|error| HelperBusError::Request(error.into()))?;
    Ok(response)
}

impl From<serde_json::Error> for HelperBusError {
    fn from(error: serde_json::Error) -> Self {
        Self::Request(error.into())
    }
}

fn classify_call_error(error: zbus::Error) -> HelperBusError {
    match &error {
        zbus::Error::MethodError(name, _, _) if is_service_unavailable_error(name.as_str()) => {
            HelperBusError::Unavailable(error.into())
        }
        _ => HelperBusError::Request(error.into()),
    }
}

fn is_service_unavailable_error(name: &str) -> bool {
    matches!(
        name,
        "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner"
            | "org.freedesktop.DBus.Error.Spawn.ExecFailed"
            | "org.freedesktop.DBus.Error.Spawn.ChildExited"
    )
}
