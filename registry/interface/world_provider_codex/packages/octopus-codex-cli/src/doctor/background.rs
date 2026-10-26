use std::path::Path;
use std::time::Duration;

use octopus_core::config::Config;

use super::CheckStatus;
use super::DoctorCheck;

const CONNECT_TIMEOUT: Duration = Duration::from_millis(750);

pub(super) async fn background_server_check(config: &Config) -> DoctorCheck {
    let mut details = Vec::new();

    let socket_path =
        match octopus_app_server_transport::app_server_control_socket_path(&config.codex_home) {
            Ok(socket_path) => socket_path,
            Err(err) => {
                return DoctorCheck::new(
                    "app_server.status",
                    "daemon",
                    CheckStatus::Warning,
                    "daemon socket path could not be resolved",
                )
                .details(details)
                .detail(err.to_string());
            }
        };

    details.push(format!("control socket: {}", socket_path.display()));
    let status = socket_status(socket_path.as_path()).await;
    details.push(format!("status: {}", status.detail_label()));
    if let SocketStatus::StaleOrUnreachable(reason) = &status {
        details.push(format!("error: {reason}"));
    }

    let mut check = DoctorCheck::new(
        "app_server.status",
        "daemon",
        status.check_status(),
        status.summary(),
    )
    .details(details);
    if status.check_status() == CheckStatus::Warning {
        check = check
            .remediation("Remove the stale socket or start the octopus-app daemon, then rerun.");
    }
    check
}

enum SocketStatus {
    NotRunning,
    Running,
    StaleOrUnreachable(String),
}

impl SocketStatus {
    fn check_status(&self) -> CheckStatus {
        match self {
            Self::NotRunning | Self::Running => CheckStatus::Ok,
            Self::StaleOrUnreachable(_) => CheckStatus::Warning,
        }
    }

    fn summary(&self) -> &'static str {
        match self {
            Self::NotRunning => "daemon is not running",
            Self::Running => "daemon socket accepts connections",
            Self::StaleOrUnreachable(_) => "daemon socket is stale or unreachable",
        }
    }

    fn detail_label(&self) -> &'static str {
        match self {
            Self::NotRunning => "not running",
            Self::Running => "running",
            Self::StaleOrUnreachable(_) => "stale or unreachable",
        }
    }
}

/// Unix: the daemon's socket file. Windows: the named pipe that
/// `octopus-local-socket` derives from the same path, which leaves no file, so
/// a pipe that is not found means the daemon is not running.
async fn socket_status(socket_path: &Path) -> SocketStatus {
    #[cfg(unix)]
    if !socket_path.exists() {
        return SocketStatus::NotRunning;
    }
    match tokio::time::timeout(
        CONNECT_TIMEOUT,
        octopus_local_socket::UnixStream::connect(socket_path),
    )
    .await
    {
        Ok(Ok(_stream)) => SocketStatus::Running,
        #[cfg(windows)]
        Ok(Err(err)) if err.kind() == std::io::ErrorKind::NotFound => SocketStatus::NotRunning,
        Ok(Err(err)) => SocketStatus::StaleOrUnreachable(err.to_string()),
        Err(_elapsed) => SocketStatus::StaleOrUnreachable("connect timed out".to_string()),
    }
}
