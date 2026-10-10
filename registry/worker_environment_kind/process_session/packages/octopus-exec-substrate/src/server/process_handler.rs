use crate::jsonrpc_lite::JSONRPCErrorError;

use crate::local_process::LocalProcess;
use crate::protocol::ExecParams;
use crate::protocol::ExecResponse;
use crate::protocol::ReadParams;
use crate::protocol::ReadResponse;
use crate::protocol::SignalParams;
use crate::protocol::SignalResponse;
use crate::protocol::TerminateParams;
use crate::protocol::TerminateResponse;
use crate::protocol::WriteParams;
use crate::protocol::WriteResponse;
use crate::rpc::RpcNotificationSender;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

#[derive(Clone)]
pub(crate) struct ProcessHandler {
    process: LocalProcess,
    start_gate: Arc<RwLock<()>>,
}

impl ProcessHandler {
    pub(crate) fn new(notifications: RpcNotificationSender) -> Self {
        Self {
            process: LocalProcess::new(notifications),
            start_gate: Arc::new(RwLock::new(())),
        }
    }

    pub(crate) async fn shutdown(&self, timeout: Duration) -> Result<(), JSONRPCErrorError> {
        let _gate = self.start_gate.write().await;
        self.process.shutdown(timeout).await
    }

    pub(crate) async fn has_live_processes(&self) -> bool {
        self.process.has_live_processes().await
    }

    pub(crate) async fn wait_until_no_live_processes(&self) {
        self.process.wait_until_no_live_processes().await;
    }

    pub(crate) fn set_notification_sender(&self, notifications: Option<RpcNotificationSender>) {
        self.process.set_notification_sender(notifications);
    }

    pub(crate) async fn exec(&self, params: ExecParams) -> Result<ExecResponse, JSONRPCErrorError> {
        let _gate = self.start_gate.read().await;
        self.process.exec(params).await
    }

    pub(crate) async fn exec_read(
        &self,
        params: ReadParams,
    ) -> Result<ReadResponse, JSONRPCErrorError> {
        self.process.exec_read(params).await
    }

    pub(crate) async fn exec_write(
        &self,
        params: WriteParams,
    ) -> Result<WriteResponse, JSONRPCErrorError> {
        self.process.exec_write(params).await
    }

    pub(crate) async fn signal(
        &self,
        params: SignalParams,
    ) -> Result<SignalResponse, JSONRPCErrorError> {
        self.process.signal_process(params).await
    }

    pub(crate) async fn terminate(
        &self,
        params: TerminateParams,
    ) -> Result<TerminateResponse, JSONRPCErrorError> {
        self.process.terminate_process(params).await
    }
}
