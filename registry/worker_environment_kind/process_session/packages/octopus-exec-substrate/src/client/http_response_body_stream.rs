use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use bytes::Bytes;
use futures::StreamExt;
use serde_json::Value;
use serde_json::from_value;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tracing::debug;

use crate::client::ExecServerError;
use crate::client::Inner;
use crate::client::http_client::PendingReqwestHttpBodyStream;
use crate::protocol::HTTP_REQUEST_BODY_DELTA_METHOD;
use crate::protocol::HttpRequestBodyDeltaNotification;
use crate::protocol::HttpRequestFailure;
use crate::rpc::RpcNotificationSender;

pub(super) struct HttpBodyStreamRegistration {
    inner: Arc<Inner>,
    request_id: String,
    active: bool,
}

enum HttpResponseBodyStreamInner {
    Local {
        body: Option<Pin<Box<dyn futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send>>>,
        max_response_bytes: u64,
        observed_response_bytes: u64,
        closed: bool,
    },
    Remote {
        inner: Arc<Inner>,
        request_id: String,
        next_seq: u64,
        rx: mpsc::Receiver<HttpRequestBodyDeltaNotification>,
        max_response_bytes: u64,
        observed_response_bytes: u64,
        pending_eof: bool,
        closed: bool,
    },
}

pub struct HttpResponseBodyStream {
    inner: HttpResponseBodyStreamInner,
}

impl HttpResponseBodyStream {
    pub(super) fn local(pending: PendingReqwestHttpBodyStream) -> Self {
        let PendingReqwestHttpBodyStream {
            response,
            max_response_bytes,
            ..
        } = pending;
        Self {
            inner: HttpResponseBodyStreamInner::Local {
                body: Some(Box::pin(response.bytes_stream())),
                max_response_bytes,
                observed_response_bytes: 0,
                closed: false,
            },
        }
    }

    pub(super) fn remote(
        inner: Arc<Inner>,
        request_id: String,
        rx: mpsc::Receiver<HttpRequestBodyDeltaNotification>,
        max_response_bytes: u64,
    ) -> Self {
        Self {
            inner: HttpResponseBodyStreamInner::Remote {
                inner,
                request_id,
                next_seq: 1,
                rx,
                max_response_bytes,
                observed_response_bytes: 0,
                pending_eof: false,
                closed: false,
            },
        }
    }

    pub async fn recv(&mut self) -> Result<Option<Vec<u8>>, ExecServerError> {
        match &mut self.inner {
            HttpResponseBodyStreamInner::Local {
                body,
                max_response_bytes,
                observed_response_bytes,
                closed,
            } => {
                if *closed {
                    return Ok(None);
                }
                let next = match body.as_mut() {
                    Some(body) => body.next().await,
                    None => return Ok(None),
                };
                match next {
                    Some(Ok(bytes)) => {
                        *observed_response_bytes = checked_stream_total(
                            *observed_response_bytes,
                            bytes.len(),
                            *max_response_bytes,
                        )
                        .map_err(|failure| {
                            *closed = true;
                            *body = None;
                            ExecServerError::HttpRequestBoundary(failure)
                        })?;
                        let mut chunk = Vec::new();
                        chunk.try_reserve_exact(bytes.len()).map_err(|_| {
                            *closed = true;
                            *body = None;
                            ExecServerError::HttpRequestBoundary(
                                HttpRequestFailure::ResponseBodyBufferAllocationFailed {
                                    max_response_bytes: *max_response_bytes,
                                    requested_response_bytes: u64::try_from(bytes.len())
                                        .unwrap_or(u64::MAX),
                                },
                            )
                        })?;
                        chunk.extend_from_slice(&bytes);
                        Ok(Some(chunk))
                    }
                    Some(Err(error)) => {
                        *closed = true;
                        *body = None;
                        Err(ExecServerError::HttpRequestBoundary(
                            HttpRequestFailure::ResponseBodyReadFailed {
                                message: error.without_url().to_string(),
                            },
                        ))
                    }
                    None => {
                        *closed = true;
                        *body = None;
                        Ok(None)
                    }
                }
            }
            HttpResponseBodyStreamInner::Remote {
                inner,
                request_id,
                next_seq,
                rx,
                max_response_bytes,
                observed_response_bytes,
                pending_eof,
                closed,
            } => {
                if *pending_eof {
                    *pending_eof = false;
                    finish_remote_stream(inner, request_id, closed).await;
                    return Ok(None);
                }

                let Some(delta) = rx.recv().await else {
                    finish_remote_stream(inner, request_id, closed).await;
                    if let Some(error) = inner.take_http_body_stream_failure(request_id).await {
                        return Err(ExecServerError::HttpRequestBoundary(
                            HttpRequestFailure::ResponseBodyTransportFailed { message: error },
                        ));
                    }
                    return Ok(None);
                };
                if delta.seq != *next_seq {
                    finish_remote_stream(inner, request_id, closed).await;
                    return Err(ExecServerError::Protocol(format!(
                        "http response stream `{request_id}` received seq {}, expected {}",
                        delta.seq, *next_seq
                    )));
                }
                let chunk = delta.delta.into_inner();

                if let Some(failure) = delta.failure {
                    finish_remote_stream(inner, request_id, closed).await;
                    if !delta.done || !chunk.is_empty() {
                        return Err(ExecServerError::Protocol(format!(
                            "http response stream `{request_id}` sent malformed failure terminal"
                        )));
                    }
                    return Err(ExecServerError::HttpRequestBoundary(failure));
                }
                *observed_response_bytes = match checked_stream_total(
                    *observed_response_bytes,
                    chunk.len(),
                    *max_response_bytes,
                ) {
                    Ok(observed_response_bytes) => observed_response_bytes,
                    Err(failure) => {
                        finish_remote_stream(inner, request_id, closed).await;
                        return Err(ExecServerError::HttpRequestBoundary(failure));
                    }
                };
                if !delta.done {
                    *next_seq = match next_seq.checked_add(1) {
                        Some(next_seq) => next_seq,
                        None => {
                            let last_sequence = *next_seq;
                            finish_remote_stream(inner, request_id, closed).await;
                            return Err(ExecServerError::HttpRequestBoundary(
                                HttpRequestFailure::ResponseBodySequenceExhausted { last_sequence },
                            ));
                        }
                    };
                }
                if delta.done {
                    finish_remote_stream(inner, request_id, closed).await;
                    if chunk.is_empty() {
                        return Ok(None);
                    }
                    *pending_eof = true;
                }
                Ok(Some(chunk))
            }
        }
    }
}

impl Drop for HttpResponseBodyStream {
    fn drop(&mut self) {
        if let HttpResponseBodyStreamInner::Remote {
            inner,
            request_id,
            closed,
            ..
        } = &mut self.inner
        {
            if *closed {
                return;
            }
            *closed = true;
            spawn_remove_http_body_stream(Arc::clone(inner), request_id.clone());
        }
    }
}

impl HttpBodyStreamRegistration {
    pub(super) fn new(inner: Arc<Inner>, request_id: String) -> Self {
        Self {
            inner,
            request_id,
            active: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for HttpBodyStreamRegistration {
    fn drop(&mut self) {
        if self.active {
            spawn_remove_http_body_stream(Arc::clone(&self.inner), self.request_id.clone());
        }
    }
}

async fn finish_remote_stream(inner: &Arc<Inner>, request_id: &str, closed: &mut bool) {
    if *closed {
        return;
    }
    *closed = true;
    inner.remove_http_body_stream(request_id).await;
}

fn spawn_remove_http_body_stream(inner: Arc<Inner>, request_id: String) {
    if let Ok(handle) = Handle::try_current() {
        handle.spawn(async move {
            inner.remove_http_body_stream(&request_id).await;
        });
    }
}

fn checked_stream_total(
    observed_response_bytes: u64,
    chunk_bytes: usize,
    max_response_bytes: u64,
) -> Result<u64, HttpRequestFailure> {
    let chunk_bytes =
        u64::try_from(chunk_bytes).map_err(|_| HttpRequestFailure::ResponseBodyTooLarge {
            max_response_bytes,
            observed_response_bytes: u64::MAX,
        })?;
    let observed_response_bytes = observed_response_bytes.checked_add(chunk_bytes).ok_or(
        HttpRequestFailure::ResponseBodyTooLarge {
            max_response_bytes,
            observed_response_bytes: u64::MAX,
        },
    )?;
    if observed_response_bytes > max_response_bytes {
        return Err(HttpRequestFailure::ResponseBodyTooLarge {
            max_response_bytes,
            observed_response_bytes,
        });
    }
    Ok(observed_response_bytes)
}

pub(super) async fn send_body_delta(
    notifications: &RpcNotificationSender,
    delta: HttpRequestBodyDeltaNotification,
) -> bool {
    notifications
        .notify(HTTP_REQUEST_BODY_DELTA_METHOD, &delta)
        .await
        .is_ok()
}

impl Inner {
    pub(crate) async fn handle_http_body_delta_notification(
        &self,
        params: Option<Value>,
    ) -> Result<(), ExecServerError> {
        let params: HttpRequestBodyDeltaNotification = from_value(params.unwrap_or(Value::Null))?;
        if let Some(tx) = self
            .http_body_streams
            .load()
            .get(&params.request_id)
            .cloned()
        {
            let request_id = params.request_id.clone();
            let terminal_delta = params.done || params.failure.is_some();
            match tx.try_send(params) {
                Ok(()) => {
                    if terminal_delta {
                        self.remove_http_body_stream(&request_id).await;
                    }
                }
                Err(TrySendError::Closed(_)) => {
                    self.remove_http_body_stream(&request_id).await;
                    debug!("http response stream receiver dropped before body delta delivery");
                }
                Err(TrySendError::Full(_)) => {
                    self.record_http_body_stream_failure(
                        &request_id,
                        "body delta channel filled before delivery".to_string(),
                    )
                    .await;
                    self.remove_http_body_stream(&request_id).await;
                    debug!(
                        "closing http response stream `{request_id}` after body delta backpressure"
                    );
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn fail_all_http_body_streams(&self, message: String) {
        let _streams_write_guard = self.http_body_streams_write_lock.lock().await;
        let streams = self.http_body_streams.load();
        let streams = streams.as_ref().clone();
        self.http_body_streams.store(Arc::new(HashMap::new()));
        for (request_id, tx) in streams {
            if tx
                .try_send(HttpRequestBodyDeltaNotification {
                    request_id: request_id.clone(),
                    seq: 1,
                    delta: Vec::new().into(),
                    done: true,
                    failure: Some(HttpRequestFailure::ResponseBodyTransportFailed {
                        message: message.clone(),
                    }),
                })
                .is_err()
            {
                let mut next_failures = self.http_body_stream_failures.load().as_ref().clone();
                next_failures.insert(request_id, message.clone());
                self.http_body_stream_failures
                    .store(Arc::new(next_failures));
            }
        }
    }

    pub(super) fn next_http_body_stream_request_id(&self) -> String {
        let id = self
            .http_body_stream_next_id
            .fetch_add(1, Ordering::Relaxed);
        format!("http-{id}")
    }

    pub(super) async fn insert_http_body_stream(
        &self,
        request_id: String,
        tx: mpsc::Sender<HttpRequestBodyDeltaNotification>,
    ) -> Result<(), ExecServerError> {
        let _streams_write_guard = self.http_body_streams_write_lock.lock().await;
        let streams = self.http_body_streams.load();
        if streams.contains_key(&request_id) {
            return Err(ExecServerError::Protocol(format!(
                "http response stream already registered for request {request_id}"
            )));
        }
        let mut next_streams = streams.as_ref().clone();
        next_streams.insert(request_id.clone(), tx);
        self.http_body_streams.store(Arc::new(next_streams));
        let failures = self.http_body_stream_failures.load();
        if failures.contains_key(&request_id) {
            let mut next_failures = failures.as_ref().clone();
            next_failures.remove(&request_id);
            self.http_body_stream_failures
                .store(Arc::new(next_failures));
        }
        Ok(())
    }

    pub(super) async fn remove_http_body_stream(
        &self,
        request_id: &str,
    ) -> Option<mpsc::Sender<HttpRequestBodyDeltaNotification>> {
        let _streams_write_guard = self.http_body_streams_write_lock.lock().await;
        let streams = self.http_body_streams.load();
        let stream = streams.get(request_id).cloned();
        stream.as_ref()?;
        let mut next_streams = streams.as_ref().clone();
        next_streams.remove(request_id);
        self.http_body_streams.store(Arc::new(next_streams));
        stream
    }

    async fn record_http_body_stream_failure(&self, request_id: &str, message: String) {
        let _streams_write_guard = self.http_body_streams_write_lock.lock().await;
        let failures = self.http_body_stream_failures.load();
        let mut next_failures = failures.as_ref().clone();
        next_failures.insert(request_id.to_string(), message);
        self.http_body_stream_failures
            .store(Arc::new(next_failures));
    }

    async fn take_http_body_stream_failure(&self, request_id: &str) -> Option<String> {
        let _streams_write_guard = self.http_body_streams_write_lock.lock().await;
        let failures = self.http_body_stream_failures.load();
        let error = failures.get(request_id).cloned();
        error.as_ref()?;
        let mut next_failures = failures.as_ref().clone();
        next_failures.remove(request_id);
        self.http_body_stream_failures
            .store(Arc::new(next_failures));
        error
    }
}
