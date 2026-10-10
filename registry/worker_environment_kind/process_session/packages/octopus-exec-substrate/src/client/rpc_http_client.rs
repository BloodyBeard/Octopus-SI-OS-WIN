use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::sync::mpsc;

use super::HttpResponseBodyStream;
use super::response_body_stream::HttpBodyStreamRegistration;
use crate::HttpClient;
use crate::client::ExecServerClient;
use crate::client::ExecServerError;
use crate::protocol::HTTP_REQUEST_METHOD;
use crate::protocol::HttpRequestFailure;
use crate::protocol::HttpRequestParams;
use crate::protocol::HttpRequestResponse;

const HTTP_BODY_DELTA_CHANNEL_CAPACITY: usize = 256;

impl ExecServerClient {
    pub async fn http_request(
        &self,
        mut params: HttpRequestParams,
    ) -> Result<HttpRequestResponse, ExecServerError> {
        params.stream_response = false;
        let max_response_bytes = params.max_response_bytes.get();
        let response = self.call(HTTP_REQUEST_METHOD, &params).await?;
        enforce_response_body_ceiling(&response, max_response_bytes)?;
        Ok(response)
    }

    pub async fn http_request_stream(
        &self,
        mut params: HttpRequestParams,
    ) -> Result<(HttpRequestResponse, HttpResponseBodyStream), ExecServerError> {
        let rpc_client = self.inner.rpc_client().await?;
        params.stream_response = true;
        let request_id = self.inner.next_http_body_stream_request_id();
        params.request_id = request_id.clone();
        let (tx, rx) = mpsc::channel(HTTP_BODY_DELTA_CHANNEL_CAPACITY);
        self.inner
            .insert_http_body_stream(request_id.clone(), tx)
            .await?;
        let mut registration =
            HttpBodyStreamRegistration::new(Arc::clone(&self.inner), request_id.clone());
        let response = match self
            .call_rpc(&rpc_client, HTTP_REQUEST_METHOD, &params)
            .await
        {
            Ok(response) => response,
            Err(error) => {
                self.inner.remove_http_body_stream(&request_id).await;
                registration.disarm();
                return Err(error);
            }
        };
        let max_response_bytes = params.max_response_bytes.get();
        if let Err(error) = enforce_response_body_ceiling(&response, max_response_bytes) {
            self.inner.remove_http_body_stream(&request_id).await;
            registration.disarm();
            return Err(error);
        }
        registration.disarm();
        Ok((
            response,
            HttpResponseBodyStream::remote(
                Arc::clone(&self.inner),
                request_id,
                rx,
                max_response_bytes,
            ),
        ))
    }
}

fn enforce_response_body_ceiling(
    response: &HttpRequestResponse,
    max_response_bytes: u64,
) -> Result<(), ExecServerError> {
    let observed_response_bytes = u64::try_from(response.body.len()).unwrap_or(u64::MAX);
    if observed_response_bytes > max_response_bytes {
        return Err(ExecServerError::HttpRequestBoundary(
            HttpRequestFailure::ResponseBodyTooLarge {
                max_response_bytes,
                observed_response_bytes,
            },
        ));
    }
    Ok(())
}

impl HttpClient for ExecServerClient {
    fn http_request(
        &self,
        params: HttpRequestParams,
    ) -> BoxFuture<'_, Result<HttpRequestResponse, ExecServerError>> {
        async move { ExecServerClient::http_request(self, params).await }.boxed()
    }

    fn http_request_stream(
        &self,
        params: HttpRequestParams,
    ) -> BoxFuture<'_, Result<(HttpRequestResponse, HttpResponseBodyStream), ExecServerError>> {
        async move { ExecServerClient::http_request_stream(self, params).await }.boxed()
    }
}
