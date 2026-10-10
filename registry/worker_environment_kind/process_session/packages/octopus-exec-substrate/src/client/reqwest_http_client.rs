use std::error::Error as StdError;
use std::time::Duration;

use crate::inlined::build_reqwest_client_with_custom_ca;
use crate::jsonrpc_lite::JSONRPCErrorError;
use bytes::Bytes;
use futures::FutureExt;
use futures::StreamExt;
use futures::future::BoxFuture;
use reqwest::Method;
use reqwest::Url;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderName;
use reqwest::header::HeaderValue;

use super::HttpResponseBodyStream;
use super::response_body_stream::send_body_delta;
use crate::HttpClient;
use crate::client::ExecServerError;
use crate::protocol::HTTP_REQUEST_FAILURE_ERROR_CODE;
use crate::protocol::HttpHeader;
use crate::protocol::HttpRequestBodyDeltaNotification;
use crate::protocol::HttpRequestFailure;
use crate::protocol::HttpRequestParams;
use crate::protocol::HttpRequestResponse;
use crate::rpc::RpcNotificationSender;
use crate::rpc::internal_error;
use crate::rpc::invalid_params;

#[derive(Clone, Default)]
pub struct ReqwestHttpClient;

pub(crate) struct PendingReqwestHttpBodyStream {
    pub(crate) request_id: String,
    pub(crate) response: reqwest::Response,
    pub(crate) max_response_bytes: u64,
}

pub(crate) struct ReqwestHttpRequestRunner {
    client: reqwest::Client,
}

impl ReqwestHttpClient {
    fn build_client(timeout_ms: Option<u64>) -> Result<reqwest::Client, ExecServerError> {
        let builder = match timeout_ms {
            None => reqwest::Client::builder(),
            Some(timeout_ms) => {
                reqwest::Client::builder().timeout(Duration::from_millis(timeout_ms))
            }
        }
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .referer(false);
        build_reqwest_client_with_custom_ca(builder)
            .map_err(|error| ExecServerError::HttpRequest(error.to_string()))
    }
}

impl HttpClient for ReqwestHttpClient {
    fn http_request(
        &self,
        params: HttpRequestParams,
    ) -> BoxFuture<'_, Result<HttpRequestResponse, ExecServerError>> {
        async move {
            let runner = ReqwestHttpRequestRunner::new(params.timeout_ms)
                .map_err(|error| ExecServerError::HttpRequest(error.message))?;
            let (response, _) = runner
                .run(HttpRequestParams {
                    stream_response: false,
                    ..params
                })
                .await
                .map_err(map_http_request_error)?;
            Ok(response)
        }
        .boxed()
    }

    fn http_request_stream(
        &self,
        params: HttpRequestParams,
    ) -> BoxFuture<'_, Result<(HttpRequestResponse, HttpResponseBodyStream), ExecServerError>> {
        async move {
            let runner = ReqwestHttpRequestRunner::new(params.timeout_ms)
                .map_err(|error| ExecServerError::HttpRequest(error.message))?;
            let (response, pending_stream) = runner
                .run(HttpRequestParams {
                    stream_response: true,
                    ..params
                })
                .await
                .map_err(map_http_request_error)?;
            let pending_stream = pending_stream.ok_or_else(|| {
                ExecServerError::Protocol(
                    "http request stream did not return a response body stream".to_string(),
                )
            })?;
            Ok((response, HttpResponseBodyStream::local(pending_stream)))
        }
        .boxed()
    }
}

impl ReqwestHttpRequestRunner {
    pub(crate) fn new(timeout_ms: Option<u64>) -> Result<Self, JSONRPCErrorError> {
        let client = ReqwestHttpClient::build_client(timeout_ms)
            .map_err(|error| internal_error(error.to_string()))?;
        Ok(Self { client })
    }

    pub(crate) async fn run(
        &self,
        params: HttpRequestParams,
    ) -> Result<(HttpRequestResponse, Option<PendingReqwestHttpBodyStream>), JSONRPCErrorError>
    {
        let method = Method::from_bytes(params.method.as_bytes())
            .map_err(|error| invalid_params(format!("http/request method is invalid: {error}")))?;
        let url = Url::parse(&params.url)
            .map_err(|error| invalid_params(format!("http/request url is invalid: {error}")))?;
        match url.scheme() {
            "http" | "https" => {}
            scheme => {
                return Err(invalid_params(format!(
                    "http/request only supports http and https URLs, got {scheme}"
                )));
            }
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(invalid_params(
                "http/request url must not contain embedded credentials".to_string(),
            ));
        }

        let headers = Self::build_headers(params.headers)?;
        let mut request = self.client.request(method.clone(), url).headers(headers);
        if let Some(body) = params.body {
            request = request.body(body.into_inner());
        }

        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                let error = error.without_url();
                let error_message = error.to_string();
                log_send_error(&method, &error);
                return Err(internal_error(format!(
                    "http/request failed: {error_message}"
                )));
            }
        };
        let max_response_bytes = params.max_response_bytes.get();
        Self::reject_known_oversize(&response, max_response_bytes)?;
        let status = response.status().as_u16();
        let headers = Self::response_headers(response.headers());

        if params.stream_response {
            return Ok((
                HttpRequestResponse {
                    status,
                    headers,
                    body: Vec::new().into(),
                },
                Some(PendingReqwestHttpBodyStream {
                    request_id: params.request_id,
                    response,
                    max_response_bytes,
                }),
            ));
        }

        let body = Self::read_bounded_body(response, max_response_bytes).await?;

        Ok((
            HttpRequestResponse {
                status,
                headers,
                body: body.into(),
            },
            None,
        ))
    }

    pub(crate) async fn stream_body(
        pending_stream: PendingReqwestHttpBodyStream,
        notifications: RpcNotificationSender,
    ) {
        let PendingReqwestHttpBodyStream {
            request_id,
            response,
            max_response_bytes,
        } = pending_stream;
        let mut seq = 1_u64;
        let mut observed_response_bytes = 0_u64;
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(bytes) => {
                    observed_response_bytes = match checked_observed_response_bytes(
                        observed_response_bytes,
                        bytes.len(),
                        max_response_bytes,
                    ) {
                        Ok(observed_response_bytes) => observed_response_bytes,
                        Err(failure) => {
                            let _ =
                                send_body_failure(&notifications, request_id, seq, failure).await;
                            return;
                        }
                    };
                    let chunk = match copy_bounded_chunk(&bytes, max_response_bytes) {
                        Ok(chunk) => chunk,
                        Err(failure) => {
                            let _ =
                                send_body_failure(&notifications, request_id, seq, failure).await;
                            return;
                        }
                    };
                    let next_seq = match seq.checked_add(1) {
                        Some(next_seq) => next_seq,
                        None => {
                            let _ = send_body_failure(
                                &notifications,
                                request_id,
                                seq,
                                HttpRequestFailure::ResponseBodySequenceExhausted {
                                    last_sequence: seq,
                                },
                            )
                            .await;
                            return;
                        }
                    };
                    if !send_body_delta(
                        &notifications,
                        HttpRequestBodyDeltaNotification {
                            request_id: request_id.clone(),
                            seq,
                            delta: chunk.into(),
                            done: false,
                            failure: None,
                        },
                    )
                    .await
                    {
                        return;
                    }
                    seq = next_seq;
                }
                Err(error) => {
                    let _ = send_body_failure(
                        &notifications,
                        request_id,
                        seq,
                        response_body_read_failure(error),
                    )
                    .await;
                    return;
                }
            }
        }

        let _ = send_body_delta(
            &notifications,
            HttpRequestBodyDeltaNotification {
                request_id,
                seq,
                delta: Vec::new().into(),
                done: true,
                failure: None,
            },
        )
        .await;
    }

    fn reject_known_oversize(
        response: &reqwest::Response,
        max_response_bytes: u64,
    ) -> Result<(), JSONRPCErrorError> {
        if let Some(observed_response_bytes) = response.content_length()
            && observed_response_bytes > max_response_bytes
        {
            return Err(http_request_failure(
                HttpRequestFailure::ResponseBodyTooLarge {
                    max_response_bytes,
                    observed_response_bytes,
                },
            ));
        }
        Ok(())
    }

    async fn read_bounded_body(
        response: reqwest::Response,
        max_response_bytes: u64,
    ) -> Result<Vec<u8>, JSONRPCErrorError> {
        let mut body = Vec::new();
        let mut observed_response_bytes = 0_u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|error| http_request_failure(response_body_read_failure(error)))?;
            observed_response_bytes = checked_observed_response_bytes(
                observed_response_bytes,
                chunk.len(),
                max_response_bytes,
            )
            .map_err(http_request_failure)?;
            body.try_reserve(chunk.len()).map_err(|_| {
                http_request_failure(HttpRequestFailure::ResponseBodyBufferAllocationFailed {
                    max_response_bytes,
                    requested_response_bytes: observed_response_bytes,
                })
            })?;
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    fn build_headers(headers: Vec<HttpHeader>) -> Result<HeaderMap, JSONRPCErrorError> {
        let mut header_map = HeaderMap::new();
        for header in headers {
            let name = HeaderName::from_bytes(header.name.as_bytes()).map_err(|error| {
                invalid_params(format!("http/request header name is invalid: {error}"))
            })?;
            let value = HeaderValue::from_str(&header.value).map_err(|error| {
                invalid_params(format!(
                    "http/request header value is invalid for {}: {error}",
                    header.name
                ))
            })?;
            header_map.append(name, value);
        }
        Ok(header_map)
    }

    fn response_headers(headers: &HeaderMap) -> Vec<HttpHeader> {
        headers
            .iter()
            .filter_map(|(name, value)| {
                Some(HttpHeader {
                    name: name.as_str().to_string(),
                    value: value.to_str().ok()?.to_string(),
                })
            })
            .collect()
    }
}

fn map_http_request_error(error: JSONRPCErrorError) -> ExecServerError {
    if error.code == HTTP_REQUEST_FAILURE_ERROR_CODE {
        let Some(data) = error.data else {
            return ExecServerError::Protocol(
                "http/request returned typed failure code without failure data".to_string(),
            );
        };
        return match serde_json::from_value::<HttpRequestFailure>(data) {
            Ok(failure) => ExecServerError::HttpRequestBoundary(failure),
            Err(parse_error) => ExecServerError::Protocol(format!(
                "http/request returned malformed typed failure data: {parse_error}"
            )),
        };
    }
    ExecServerError::HttpRequest(error.message)
}

fn http_request_failure(failure: HttpRequestFailure) -> JSONRPCErrorError {
    let message = failure.to_string();
    match serde_json::to_value(&failure) {
        Ok(data) => JSONRPCErrorError {
            code: HTTP_REQUEST_FAILURE_ERROR_CODE,
            data: Some(data),
            message,
        },
        Err(error) => internal_error(format!(
            "failed to encode typed http/request failure: {error}"
        )),
    }
}

fn response_body_read_failure(error: reqwest::Error) -> HttpRequestFailure {
    HttpRequestFailure::ResponseBodyReadFailed {
        message: error.without_url().to_string(),
    }
}

fn checked_observed_response_bytes(
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

fn copy_bounded_chunk(
    bytes: &Bytes,
    max_response_bytes: u64,
) -> Result<Vec<u8>, HttpRequestFailure> {
    let bytes_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if bytes_len > max_response_bytes {
        return Err(HttpRequestFailure::ResponseBodyTooLarge {
            max_response_bytes,
            observed_response_bytes: bytes_len,
        });
    }
    let mut chunk = Vec::new();
    chunk.try_reserve_exact(bytes.len()).map_err(|_| {
        HttpRequestFailure::ResponseBodyBufferAllocationFailed {
            max_response_bytes,
            requested_response_bytes: bytes_len,
        }
    })?;
    chunk.extend_from_slice(bytes);
    Ok(chunk)
}

async fn send_body_failure(
    notifications: &RpcNotificationSender,
    request_id: String,
    seq: u64,
    failure: HttpRequestFailure,
) -> bool {
    send_body_delta(
        notifications,
        HttpRequestBodyDeltaNotification {
            request_id,
            seq,
            delta: Vec::new().into(),
            done: true,
            failure: Some(failure),
        },
    )
    .await
}

fn log_send_error(method: &Method, error: &reqwest::Error) {
    let source_chain = error_source_chain(error);
    tracing::warn!(
        http_method = method.as_str(),
        error_is_timeout = error.is_timeout(),
        error_is_connect = error.is_connect(),
        error = %error,
        error_sources = ?source_chain,
        "http/request send failed"
    );
}

fn error_source_chain(error: &reqwest::Error) -> Option<String> {
    let mut sources = Vec::new();
    let mut source = error.source();
    while let Some(error) = source {
        sources.push(error.to_string());
        source = error.source();
    }
    (!sources.is_empty()).then(|| sources.join(": "))
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;
    use tokio::time::Duration;
    use tokio::time::sleep;

    use super::*;

    fn params(url: String, max_response_bytes: u64, stream_response: bool) -> HttpRequestParams {
        HttpRequestParams {
            method: "GET".to_string(),
            url,
            headers: Vec::new(),
            body: None,
            timeout_ms: Some(2_000),
            request_id: "http-boundary-test".to_string(),
            stream_response,
            max_response_bytes: NonZeroU64::new(max_response_bytes)
                .expect("test response ceiling must be positive"),
        }
    }

    async fn serve(chunks: Vec<Vec<u8>>) -> (String, JoinHandle<std::io::Result<()>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test server should bind");
        let address = listener.local_addr().expect("test server address");
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await?;
            for chunk in chunks {
                socket.write_all(&chunk).await?;
                socket.flush().await?;
                sleep(Duration::from_millis(20)).await;
            }
            Ok(())
        });
        (format!("http://{address}/resource"), task)
    }

    #[tokio::test]
    async fn non_stream_response_ceiling_refuses_known_oversize_before_body_read() {
        let (url, server) = serve(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\n123456789".to_vec(),
        ])
        .await;
        let error = ReqwestHttpClient
            .http_request(params(url, 8, false))
            .await
            .expect_err("oversized response should fail closed");

        assert!(matches!(
            error,
            ExecServerError::HttpRequestBoundary(HttpRequestFailure::ResponseBodyTooLarge {
                max_response_bytes: 8,
                observed_response_bytes: 9,
            })
        ));
        server
            .await
            .expect("test server task should join")
            .expect("test server should finish");
    }

    #[tokio::test]
    async fn streaming_response_ceiling_counts_across_chunks() {
        let (url, server) = serve(vec![
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\n12345\r\n"
                .to_vec(),
            b"5\r\n67890\r\n0\r\n\r\n".to_vec(),
        ])
        .await;
        let (_, mut stream) = ReqwestHttpClient
            .http_request_stream(params(url, 8, true))
            .await
            .expect("stream headers should fit");
        let first = stream
            .recv()
            .await
            .expect("first bounded chunk should arrive")
            .expect("first chunk should exist");
        let error = stream
            .recv()
            .await
            .expect_err("cumulative response limit should fail closed");

        assert_eq!(first, b"12345");
        assert!(matches!(
            error,
            ExecServerError::HttpRequestBoundary(HttpRequestFailure::ResponseBodyTooLarge {
                max_response_bytes: 8,
                observed_response_bytes: 10,
            })
        ));
        server
            .await
            .expect("test server task should join")
            .expect("test server should finish");
    }

    #[tokio::test]
    async fn client_does_not_follow_redirects_or_accept_url_credentials() {
        let (url, server) = serve(vec![
            b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/target\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_vec(),
        ])
        .await;
        let response = ReqwestHttpClient
            .http_request(params(url, 8, false))
            .await
            .expect("redirect response should be returned without following it");
        let credential_error = ReqwestHttpClient
            .http_request(params(
                "http://user:secret@127.0.0.1:9/resource".to_string(),
                8,
                false,
            ))
            .await
            .expect_err("embedded URL credentials should fail closed");

        assert_eq!(response.status, 302);
        assert!(
            credential_error
                .to_string()
                .contains("embedded credentials")
        );
        server
            .await
            .expect("test server task should join")
            .expect("test server should finish");
    }
}
