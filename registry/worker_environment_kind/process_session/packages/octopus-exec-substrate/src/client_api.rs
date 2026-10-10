use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;

use crate::DirectExecServerAuth;
use crate::ExecServerError;
use crate::HttpRequestParams;
use crate::HttpRequestResponse;
use crate::HttpResponseBodyStream;
use crate::NoiseChannelIdentity;
use crate::NoiseChannelPublicKey;

pub(crate) const DEFAULT_REMOTE_EXEC_SERVER_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const DEFAULT_REMOTE_EXEC_SERVER_INITIALIZE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecServerClientConnectOptions {
    pub client_name: String,
    pub initialize_timeout: Duration,
    pub resume_session_id: Option<String>,
    pub claim_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteExecServerConnectArgs {
    pub websocket_url: String,
    pub client_name: String,
    pub connect_timeout: Duration,
    pub initialize_timeout: Duration,
    pub resume_session_id: Option<String>,
}

pub struct NoiseRendezvousConnectBundle {
    pub websocket_url: String,
    pub environment_id: String,
    pub executor_registration_id: String,
    pub executor_public_key: NoiseChannelPublicKey,
    pub harness_key_authorization: String,
}

pub struct NoiseRendezvousConnectArgs {
    pub bundle: NoiseRendezvousConnectBundle,
    pub harness_identity: NoiseChannelIdentity,
    pub client_name: String,
    pub connect_timeout: Duration,
    pub initialize_timeout: Duration,
    pub resume_session_id: Option<String>,
}

pub trait NoiseRendezvousConnectProvider: Send + Sync {
    fn connect_bundle(
        &self,
        harness_public_key: NoiseChannelPublicKey,
    ) -> BoxFuture<'_, Result<NoiseRendezvousConnectBundle, ExecServerError>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StdioExecServerConnectArgs {
    pub command: StdioExecServerCommand,
    pub client_name: String,
    pub initialize_timeout: Duration,
    pub resume_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StdioExecServerCommand {
    pub program: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
}

#[derive(Clone)]
pub(crate) enum ExecServerTransportParams {
    WebSocketUrl {
        websocket_url: String,
        connect_timeout: Duration,
        initialize_timeout: Duration,
        resume_session_id: Option<String>,
    },
    AuthenticatedWebSocketUrl {
        websocket_url: String,
        connect_timeout: Duration,
        initialize_timeout: Duration,
        auth: DirectExecServerAuth,
        resume_session_id: Option<String>,
        claim_session_id: Option<String>,
    },
    NoiseRendezvous {
        provider: Arc<dyn NoiseRendezvousConnectProvider>,
        identity: NoiseChannelIdentity,
    },
    #[allow(dead_code)]
    StdioCommand {
        command: StdioExecServerCommand,
        initialize_timeout: Duration,
    },
}

impl std::fmt::Debug for ExecServerTransportParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WebSocketUrl {
                websocket_url,
                connect_timeout,
                initialize_timeout,
                resume_session_id,
            } => f
                .debug_struct("WebSocketUrl")
                .field("websocket_url", websocket_url)
                .field("connect_timeout", connect_timeout)
                .field("initialize_timeout", initialize_timeout)
                .field("resume_session_id", resume_session_id)
                .finish(),
            Self::AuthenticatedWebSocketUrl {
                websocket_url,
                connect_timeout,
                initialize_timeout,
                resume_session_id,
                claim_session_id,
                ..
            } => f
                .debug_struct("AuthenticatedWebSocketUrl")
                .field("websocket_url", websocket_url)
                .field("connect_timeout", connect_timeout)
                .field("initialize_timeout", initialize_timeout)
                .field("resume_session_id", resume_session_id)
                .field("claim_session_id", claim_session_id)
                .finish_non_exhaustive(),
            Self::NoiseRendezvous { .. } => {
                f.debug_struct("NoiseRendezvous").finish_non_exhaustive()
            }
            Self::StdioCommand {
                command,
                initialize_timeout,
            } => f
                .debug_struct("StdioCommand")
                .field("command", command)
                .field("initialize_timeout", initialize_timeout)
                .finish(),
        }
    }
}

impl ExecServerTransportParams {
    pub(crate) fn websocket_url(websocket_url: String, connect_timeout: Duration) -> Self {
        Self::WebSocketUrl {
            websocket_url,
            connect_timeout,
            initialize_timeout: DEFAULT_REMOTE_EXEC_SERVER_INITIALIZE_TIMEOUT,
            resume_session_id: None,
        }
    }

    pub(crate) fn authenticated_websocket_url(
        websocket_url: String,
        connect_timeout: Duration,
        auth: DirectExecServerAuth,
    ) -> Self {
        Self::AuthenticatedWebSocketUrl {
            websocket_url,
            connect_timeout,
            initialize_timeout: DEFAULT_REMOTE_EXEC_SERVER_INITIALIZE_TIMEOUT,
            auth,
            resume_session_id: None,
            claim_session_id: None,
        }
    }

    pub(crate) fn authenticated_websocket_url_with_resume(
        websocket_url: String,
        connect_timeout: Duration,
        auth: DirectExecServerAuth,
        resume_session_id: String,
    ) -> Self {
        Self::AuthenticatedWebSocketUrl {
            websocket_url,
            connect_timeout,
            initialize_timeout: DEFAULT_REMOTE_EXEC_SERVER_INITIALIZE_TIMEOUT,
            auth,
            resume_session_id: Some(resume_session_id),
            claim_session_id: None,
        }
    }

    pub(crate) fn authenticated_websocket_url_with_claim(
        websocket_url: String,
        connect_timeout: Duration,
        auth: DirectExecServerAuth,
        claim_session_id: String,
    ) -> Self {
        Self::AuthenticatedWebSocketUrl {
            websocket_url,
            connect_timeout,
            initialize_timeout: DEFAULT_REMOTE_EXEC_SERVER_INITIALIZE_TIMEOUT,
            auth,
            resume_session_id: None,
            claim_session_id: Some(claim_session_id),
        }
    }

    pub(crate) fn with_resume_session_id(mut self, session_id: Option<String>) -> Self {
        match &mut self {
            Self::WebSocketUrl {
                resume_session_id, ..
            } => *resume_session_id = session_id,
            Self::AuthenticatedWebSocketUrl {
                resume_session_id,
                claim_session_id,
                ..
            } => {
                *resume_session_id = session_id;
                *claim_session_id = None;
            }
            Self::NoiseRendezvous { .. } | Self::StdioCommand { .. } => {}
        }
        self
    }
}

pub trait HttpClient: Send + Sync {
    fn http_request(
        &self,
        params: HttpRequestParams,
    ) -> BoxFuture<'_, Result<HttpRequestResponse, ExecServerError>>;

    fn http_request_stream(
        &self,
        params: HttpRequestParams,
    ) -> BoxFuture<'_, Result<(HttpRequestResponse, HttpResponseBodyStream), ExecServerError>>;
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn claimed_authenticated_transport_reconnects_by_resume_only() {
        let root = tempfile::tempdir().expect("private auth root");
        let secret = root.path().join("secret");
        std::fs::write(&secret, [7_u8; 32]).expect("write auth secret");
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600))
            .expect("protect auth secret");
        let auth = DirectExecServerAuth::load_private_file(&secret).expect("load auth secret");
        let transport = ExecServerTransportParams::authenticated_websocket_url_with_claim(
            "ws://127.0.0.1:1".to_string(),
            Duration::from_secs(1),
            auth,
            "octopus-claim_1".to_string(),
        )
        .with_resume_session_id(Some("octopus-claim_1".to_string()));

        let ExecServerTransportParams::AuthenticatedWebSocketUrl {
            resume_session_id,
            claim_session_id,
            ..
        } = transport
        else {
            panic!("expected authenticated transport");
        };
        assert_eq!(resume_session_id.as_deref(), Some("octopus-claim_1"));
        assert_eq!(claim_session_id, None);
    }
}
