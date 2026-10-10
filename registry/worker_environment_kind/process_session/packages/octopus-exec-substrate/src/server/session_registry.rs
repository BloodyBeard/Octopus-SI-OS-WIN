use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use crate::jsonrpc_lite::JSONRPCErrorError;
use tokio::sync::Mutex;
use tokio::sync::watch;
use uuid::Uuid;

use crate::rpc::RpcNotificationSender;
use crate::rpc::invalid_request;
use crate::rpc::session_already_attached;
use crate::server::process_handler::ProcessHandler;

#[cfg(test)]
const DETACHED_SESSION_TTL: Duration = Duration::from_millis(200);
#[cfg(not(test))]
const DETACHED_SESSION_TTL: Duration = Duration::from_secs(30);
#[cfg(test)]
const SESSION_RELEASE_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(not(test))]
const SESSION_RELEASE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct SessionRegistry {
    sessions: Mutex<HashMap<String, Arc<SessionEntry>>>,
}

struct SessionEntry {
    session_id: String,
    process: ProcessHandler,
    attachment: StdMutex<AttachmentState>,
    attachment_changed: watch::Sender<u64>,
}

struct AttachmentState {
    current_connection_id: Option<ConnectionId>,
    detached_connection_id: Option<ConnectionId>,
    detached_expires_at: Option<tokio::time::Instant>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ConnectionId(Uuid);

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone)]
pub(crate) struct SessionHandle {
    registry: Arc<SessionRegistry>,
    entry: Arc<SessionEntry>,
    connection_id: ConnectionId,
}

impl SessionRegistry {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            sessions: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) async fn attach(
        self: &Arc<Self>,
        resume_session_id: Option<String>,
        claim_session_id: Option<String>,
        notifications: RpcNotificationSender,
    ) -> Result<SessionHandle, JSONRPCErrorError> {
        let connection_id = ConnectionId(Uuid::new_v4());
        let entry = {
            let mut sessions = self.sessions.lock().await;
            if let Some(session_id) = resume_session_id {
                let entry = sessions
                    .get(&session_id)
                    .cloned()
                    .ok_or_else(|| invalid_request(format!("unknown session id {session_id}")))?;
                if entry.has_active_connection() {
                    Err(session_already_attached(format!(
                        "session {session_id} is already attached to another connection"
                    )))
                } else {
                    entry.process.set_notification_sender(Some(notifications));
                    entry.attach(connection_id);
                    Ok(entry)
                }
            } else if let Some(session_id) = claim_session_id {
                if let Some(entry) = sessions.get(&session_id).cloned() {
                    if entry.has_active_connection() {
                        Err(session_already_attached(format!(
                            "session {session_id} is already attached to another connection"
                        )))
                    } else {
                        entry.process.set_notification_sender(Some(notifications));
                        entry.attach(connection_id);
                        Ok(entry)
                    }
                } else {
                    let entry = Arc::new(SessionEntry::new(
                        session_id.clone(),
                        ProcessHandler::new(notifications),
                        connection_id,
                    ));
                    sessions.insert(session_id, Arc::clone(&entry));
                    Ok(entry)
                }
            } else {
                let session_id = Uuid::new_v4().to_string();
                let entry = Arc::new(SessionEntry::new(
                    session_id.clone(),
                    ProcessHandler::new(notifications),
                    connection_id,
                ));
                sessions.insert(session_id, Arc::clone(&entry));
                Ok(entry)
            }
        }?;

        Ok(SessionHandle {
            registry: Arc::clone(self),
            entry,
            connection_id,
        })
    }

    async fn release(
        self: &Arc<Self>,
        entry: &Arc<SessionEntry>,
        connection_id: ConnectionId,
    ) -> Result<String, JSONRPCErrorError> {
        self.release_inner(entry, connection_id).await
    }

    async fn release_inner(
        &self,
        entry: &Arc<SessionEntry>,
        connection_id: ConnectionId,
    ) -> Result<String, JSONRPCErrorError> {
        let session_id = entry.session_id.clone();
        let mut sessions = self.sessions.lock().await;
        let is_current = sessions
            .get(&session_id)
            .is_some_and(|current| Arc::ptr_eq(current, entry));
        if !is_current || !entry.is_attached_to(connection_id) {
            return Err(invalid_request(format!(
                "session {session_id} is not attached to this connection"
            )));
        }
        entry.process.shutdown(SESSION_RELEASE_TIMEOUT).await?;
        if !entry.release(connection_id) {
            return Err(invalid_request(format!(
                "session {session_id} is not attached to this connection"
            )));
        }
        if sessions.remove(&session_id).is_none() {
            return Err(invalid_request(format!(
                "session {session_id} disappeared during release"
            )));
        }
        Ok(session_id)
    }

    async fn expire_if_detached(&self, session_id: String, connection_id: ConnectionId) {
        tokio::time::sleep(DETACHED_SESSION_TTL).await;
        let entry = {
            let sessions = self.sessions.lock().await;
            let Some(entry) = sessions.get(&session_id) else {
                return;
            };
            Arc::clone(entry)
        };
        let mut attachment_changed = entry.attachment_changed.subscribe();
        loop {
            if !entry.is_detached_connection_expired(connection_id, tokio::time::Instant::now()) {
                return;
            }
            if !entry.process.has_live_processes().await {
                let removed = {
                    let mut sessions = self.sessions.lock().await;
                    if sessions.get(&session_id).is_some_and(|current| {
                        Arc::ptr_eq(current, &entry)
                            && entry.is_detached_connection_expired(
                                connection_id,
                                tokio::time::Instant::now(),
                            )
                    }) {
                        match entry.process.shutdown(SESSION_RELEASE_TIMEOUT).await {
                            Ok(()) => sessions.remove(&session_id),
                            Err(error) => {
                                tracing::warn!(
                                    session_id,
                                    detail = %error.message,
                                    "detached exec-server session could not prove process shutdown"
                                );
                                None
                            }
                        }
                    } else {
                        None
                    }
                };
                drop(removed);
                return;
            }
            tokio::select! {
                _ = entry.process.wait_until_no_live_processes() => {}
                changed = attachment_changed.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }
}

impl SessionEntry {
    fn new(session_id: String, process: ProcessHandler, connection_id: ConnectionId) -> Self {
        let (attachment_changed, _attachment_changed_rx) = watch::channel(0);
        Self {
            session_id,
            process,
            attachment: StdMutex::new(AttachmentState {
                current_connection_id: Some(connection_id),
                detached_connection_id: None,
                detached_expires_at: None,
            }),
            attachment_changed,
        }
    }

    fn attach(&self, connection_id: ConnectionId) {
        let mut attachment = self
            .attachment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        attachment.current_connection_id = Some(connection_id);
        attachment.detached_connection_id = None;
        attachment.detached_expires_at = None;
        self.attachment_changed.send_modify(|generation| {
            *generation = generation.wrapping_add(1);
        });
    }

    fn detach(&self, connection_id: ConnectionId) -> bool {
        let mut attachment = self
            .attachment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if attachment.current_connection_id != Some(connection_id) {
            return false;
        }

        self.process.set_notification_sender(None);
        attachment.current_connection_id = None;
        attachment.detached_connection_id = Some(connection_id);
        attachment.detached_expires_at = Some(tokio::time::Instant::now() + DETACHED_SESSION_TTL);
        self.attachment_changed.send_modify(|generation| {
            *generation = generation.wrapping_add(1);
        });
        true
    }

    fn release(&self, connection_id: ConnectionId) -> bool {
        let mut attachment = self
            .attachment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if attachment.current_connection_id != Some(connection_id) {
            return false;
        }

        self.process.set_notification_sender(None);
        attachment.current_connection_id = None;
        attachment.detached_connection_id = None;
        attachment.detached_expires_at = None;
        self.attachment_changed.send_modify(|generation| {
            *generation = generation.wrapping_add(1);
        });
        true
    }

    fn has_active_connection(&self) -> bool {
        self.attachment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .current_connection_id
            .is_some()
    }

    fn is_attached_to(&self, connection_id: ConnectionId) -> bool {
        self.attachment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .current_connection_id
            == Some(connection_id)
    }

    fn is_detached_connection_expired(
        &self,
        connection_id: ConnectionId,
        now: tokio::time::Instant,
    ) -> bool {
        let attachment = self
            .attachment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        attachment.current_connection_id.is_none()
            && attachment.detached_connection_id == Some(connection_id)
            && attachment
                .detached_expires_at
                .is_some_and(|deadline| now >= deadline)
    }
}

impl SessionHandle {
    pub(crate) fn session_id(&self) -> &str {
        &self.entry.session_id
    }

    pub(crate) fn connection_id(&self) -> String {
        self.connection_id.to_string()
    }

    pub(crate) fn is_session_attached(&self) -> bool {
        self.entry.is_attached_to(self.connection_id)
    }

    pub(crate) fn process(&self) -> &ProcessHandler {
        &self.entry.process
    }

    pub(crate) async fn detach(&self) {
        if !self.entry.detach(self.connection_id) {
            return;
        }

        let registry = Arc::clone(&self.registry);
        let session_id = self.entry.session_id.clone();
        let connection_id = self.connection_id;
        tokio::spawn(async move {
            registry.expire_if_detached(session_id, connection_id).await;
        });
    }

    pub(crate) async fn release(&self) -> Result<String, JSONRPCErrorError> {
        self.registry.release(&self.entry, self.connection_id).await
    }
}
