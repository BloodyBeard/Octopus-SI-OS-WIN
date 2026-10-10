use std::path::PathBuf;

use futures_util::{SinkExt, StreamExt};
use octopus_interfaces::DeliveryVerdict;
use tokio_tungstenite::tungstenite::Message as WsMessage;

pub const TERMINAL_LISTEN_ENV: &str = "OCTOPUS_TERMINAL_LISTEN";

const DAEMON_CONTROL_SOCKET_DIR_NAME: &str = "app-server-control";
const DAEMON_CONTROL_SOCKET_FILE_NAME: &str = "app-server-control.sock";

pub fn daemon_control_socket_path(octopus_home: &std::path::Path) -> PathBuf {
    octopus_home
        .join(DAEMON_CONTROL_SOCKET_DIR_NAME)
        .join(DAEMON_CONTROL_SOCKET_FILE_NAME)
}

pub fn interface_control_socket_path(
    octopus_home: &std::path::Path,
    interface: &octopus_descriptors::InterfaceId,
) -> PathBuf {
    octopus_interface_activation_app_server_transport::interface_control_socket_path(
        octopus_home,
        interface,
    )
}

const DAEMON_LEDGER_DIR_NAME: &str = "ledger";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonEndpoint {
    pub control_socket: PathBuf,
    pub durable_ledger_root: PathBuf,
    pub artifact_hand_in_root: PathBuf,
}

pub fn terminal_hand_in_root_path(octopus_home: &std::path::Path) -> PathBuf {
    octopus_interface_activation_app_server_transport::conventional_hand_in_root_path(octopus_home)
        .join("world_terminal")
}

impl DaemonEndpoint {
    pub fn local_default(octopus_home: &std::path::Path) -> Self {
        Self {
            control_socket: daemon_control_socket_path(octopus_home),
            durable_ledger_root: octopus_home.join(DAEMON_LEDGER_DIR_NAME),
            artifact_hand_in_root: terminal_hand_in_root_path(octopus_home),
        }
    }

    pub fn with_control_socket(octopus_home: &std::path::Path, control_socket: PathBuf) -> Self {
        Self {
            control_socket,
            durable_ledger_root: octopus_home.join(DAEMON_LEDGER_DIR_NAME),
            artifact_hand_in_root: terminal_hand_in_root_path(octopus_home),
        }
    }

    pub fn with_durable_ledger_root(mut self, durable_ledger_root: PathBuf) -> Self {
        self.durable_ledger_root = durable_ledger_root;
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonArrivalError {
    #[error(
        "the octopus-app daemon socket is not configured (${TERMINAL_LISTEN_ENV} is unset); the \
         terminal cannot reach the runtime — start the octopus-app daemon"
    )]
    NoDaemonSocket,
    #[error("the daemon socket path '{path}' is not usable: {reason}")]
    BadSocketPath { path: String, reason: String },
    #[error("could not connect the octopus-app daemon socket '{path}': {source}")]
    Connect {
        path: String,
        source: std::io::Error,
    },
    #[error("the daemon transport websocket upgrade failed: {0}")]
    Upgrade(String),
    #[error("the input artifact manifest is not a JSON object: {0}")]
    Manifest(String),
    #[error("sending the interface artifact notification to the daemon failed: {0}")]
    Send(String),
    #[error("the runtime refused the interface artifact hand-in: {0}")]
    Refused(String),
}

#[derive(Clone, Debug)]
pub struct TerminalDaemonEmitter {
    socket_path: Option<PathBuf>,
}

impl TerminalDaemonEmitter {
    pub fn from_env() -> Self {
        Self {
            socket_path: std::env::var_os(TERMINAL_LISTEN_ENV).map(PathBuf::from),
        }
    }

    pub fn with_socket_path(socket_path: PathBuf) -> Self {
        Self {
            socket_path: Some(socket_path),
        }
    }

    pub fn has_daemon_socket(&self) -> bool {
        self.socket_path.is_some()
    }

    pub async fn emit_input_manifest(
        &self,
        manifest_bytes: &[u8],
    ) -> Result<DeliveryVerdict, DaemonArrivalError> {
        let socket_path = self
            .socket_path
            .as_ref()
            .ok_or(DaemonArrivalError::NoDaemonSocket)?;

        let payload: serde_json::Value = serde_json::from_slice(manifest_bytes)
            .map_err(|e| DaemonArrivalError::Manifest(e.to_string()))?;
        if !payload.is_object() {
            return Err(DaemonArrivalError::Manifest(
                "expected a JSON object artifact manifest".to_string(),
            ));
        }

        let stream = tokio::net::UnixStream::connect(socket_path)
            .await
            .map_err(|source| DaemonArrivalError::Connect {
                path: socket_path.display().to_string(),
                source,
            })?;
        let (mut ws, _response) =
            tokio_tungstenite::client_async("ws://localhost/artifact-hand-in", stream)
                .await
                .map_err(|e| DaemonArrivalError::Upgrade(e.to_string()))?;

        let text =
            serde_json::to_string(&payload).map_err(|e| DaemonArrivalError::Send(e.to_string()))?;
        ws.send(WsMessage::Text(text.into()))
            .await
            .map_err(|e| DaemonArrivalError::Send(e.to_string()))?;

        let reply = ws
            .next()
            .await
            .ok_or_else(|| {
                DaemonArrivalError::Send("daemon closed before delivery verdict".into())
            })?
            .map_err(|e| DaemonArrivalError::Send(e.to_string()))?;
        let text = reply
            .into_text()
            .map_err(|e| DaemonArrivalError::Send(e.to_string()))?;
        let verdict: DeliveryVerdict = serde_json::from_str(&text)
            .map_err(|e| DaemonArrivalError::Send(format!("malformed delivery verdict: {e}")))?;
        if let DeliveryVerdict::Refused { reason, detail } = &verdict {
            return Err(DaemonArrivalError::Refused(format!(
                "{}: {}",
                reason.as_str(),
                detail
            )));
        }

        let _ = ws.close(None).await;
        Ok(verdict)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn emit_without_daemon_socket_fails_closed() {
        let emitter = TerminalDaemonEmitter { socket_path: None };
        assert!(!emitter.has_daemon_socket());
        let err = emitter
            .emit_input_manifest(br#"{"label":"x","payload_path":"/tmp/p"}"#)
            .await
            .unwrap_err();
        assert!(matches!(err, DaemonArrivalError::NoDaemonSocket));
    }

    #[tokio::test]
    async fn emit_with_malformed_manifest_fails_closed() {
        let emitter = TerminalDaemonEmitter::with_socket_path(PathBuf::from("/nonexistent.sock"));
        let err = emitter.emit_input_manifest(b"not json").await.unwrap_err();
        assert!(matches!(err, DaemonArrivalError::Manifest(_)));
        let err = emitter.emit_input_manifest(b"[1,2,3]").await.unwrap_err();
        assert!(matches!(err, DaemonArrivalError::Manifest(_)));
    }

    #[test]
    fn resolves_daemon_control_socket_path_under_home() {
        let home = std::path::Path::new("/octo/home");
        let socket = daemon_control_socket_path(home);
        assert_eq!(
            socket,
            PathBuf::from("/octo/home/app-server-control/app-server-control.sock")
        );
    }

    #[test]
    fn terminal_connector_and_daemon_binder_agree_on_the_conventional_socket_path() {
        for home in [
            std::path::Path::new("/octo/home"),
            std::path::Path::new("/var/lib/octopus"),
            std::path::Path::new("/Users/someone/.octopus"),
        ] {
            assert_eq!(
                daemon_control_socket_path(home),
                octopus_interface_activation_app_server_transport::conventional_control_socket_path(
                    home
                ),
                "the terminal's daemon_control_socket_path and the daemon transport's \
                 conventional_control_socket_path must resolve the SAME path under {}",
                home.display()
            );
        }
    }

    #[tokio::test]
    async fn emit_with_dead_socket_fails_closed_on_connect() {
        let emitter =
            TerminalDaemonEmitter::with_socket_path(PathBuf::from("/nonexistent/dead.sock"));
        let err = emitter
            .emit_input_manifest(br#"{"label":"x","payload_path":"/tmp/p"}"#)
            .await
            .unwrap_err();
        assert!(matches!(err, DaemonArrivalError::Connect { .. }));
    }
}
