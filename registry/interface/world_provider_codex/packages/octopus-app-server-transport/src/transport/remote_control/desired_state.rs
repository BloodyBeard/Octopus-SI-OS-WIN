use super::RemoteControlHandle;
use octopus_appserver_proto::RemoteControlStatusChangedNotification;
use std::io;
use std::io::ErrorKind;
use tokio::sync::Semaphore;
use tokio::sync::SemaphorePermit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RemoteControlDesiredState {
    Disabled,
    Enabled,
}
impl RemoteControlDesiredState {
    pub(super) fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

pub(super) async fn acquire_persistence_lock(lock: &Semaphore) -> SemaphorePermit<'_> {
    lock.acquire().await.unwrap_or_else(|_| unreachable!())
}

impl RemoteControlHandle {
    pub async fn resolve_persisted_preference(
        &self,
        _app_server_client_name: Option<&str>,
    ) -> io::Result<bool> {
        if self.ensure_remote_control_allowed().is_err() {
            return Ok(false);
        }
        let _transition = self
            .desired_state_rpc_lock
            .acquire()
            .await
            .unwrap_or_else(|_| unreachable!());
        Ok(self.desired_state_tx.borrow().is_enabled())
    }

    pub async fn enable(
        &self,
        _app_server_client_name: Option<&str>,
    ) -> io::Result<RemoteControlStatusChangedNotification> {
        self.ensure_remote_control_allowed()
            .map_err(|err| io::Error::new(io::ErrorKind::PermissionDenied, err))?;
        Err(io::Error::new(
            ErrorKind::Unsupported,
            "durable remote-control enable is not supported by the Octopus transport; use \
             enable_ephemeral or route durable preference through Octopus ledger artifacts",
        ))
    }
}
