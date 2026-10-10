use crate::AppServerTarget;
use octopus_world_terminal_client::DaemonEndpoint;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoteConnectionStatus {
    pub(crate) address: String,
    pub(crate) version: String,
}

pub(crate) fn remote_connection_status_value(
    app_server_target: &AppServerTarget,
    server_version: Option<&str>,
) -> Option<RemoteConnectionStatus> {
    let endpoint = match app_server_target {
        AppServerTarget::Embedded => return None,
        AppServerTarget::LocalDaemon { endpoint } | AppServerTarget::Remote { endpoint } => {
            endpoint
        }
    };
    let version = server_version
        .map(|version| format!("v{version}"))
        .unwrap_or_else(|| "unknown".to_string());
    Some(RemoteConnectionStatus {
        address: endpoint_display_address(endpoint),
        version,
    })
}

fn endpoint_display_address(endpoint: &DaemonEndpoint) -> String {
    format!("unix://{}", endpoint.control_socket.display())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn remote_connection_status_value_formats_display_value() {
        assert_eq!(
            remote_connection_status_value(&AppServerTarget::Embedded, Some("1.2.3")),
            None
        );

        let endpoint = DaemonEndpoint::local_default(Path::new("/tmp/octopus-home"));
        let expected_address = format!("unix://{}", endpoint.control_socket.display());
        let daemon_target = AppServerTarget::LocalDaemon { endpoint };
        assert_eq!(
            remote_connection_status_value(&daemon_target, None),
            Some(RemoteConnectionStatus {
                address: expected_address.clone(),
                version: "unknown".to_string(),
            })
        );

        let remote_endpoint = DaemonEndpoint::with_control_socket(
            Path::new("/tmp/octopus-home"),
            Path::new("/remote/daemon.sock").to_path_buf(),
        );
        let remote_target = AppServerTarget::Remote {
            endpoint: remote_endpoint,
        };
        assert_eq!(
            remote_connection_status_value(&remote_target, Some("1.2.3")),
            Some(RemoteConnectionStatus {
                address: "unix:///remote/daemon.sock".to_string(),
                version: "v1.2.3".to_string(),
            })
        );
    }
}
