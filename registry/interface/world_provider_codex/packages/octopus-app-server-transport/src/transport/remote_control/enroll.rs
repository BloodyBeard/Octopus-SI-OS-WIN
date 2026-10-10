use super::auth::RemoteControlConnectionAuth;
use super::pairing_unavailable_error;
use super::protocol::EnrollRemoteServerRequest;
use super::protocol::EnrollRemoteServerResponse;
use super::protocol::RefreshRemoteServerRequest;
use super::protocol::RemoteControlPairingStatusRequest;
use super::protocol::RemoteControlPairingStatusResponse as BackendRemoteControlPairingStatusResponse;
use super::protocol::RemoteControlTarget;
use super::protocol::StartRemoteControlPairingRequest;
use super::protocol::StartRemoteControlPairingResponse;
use axum::http::HeaderMap;
use octopus_appserver_proto::RemoteControlPairingStartResponse;
use octopus_appserver_proto::RemoteControlPairingStatusResponse;
use octopus_login::default_client::build_reqwest_client;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io;
use std::io::ErrorKind;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const REMOTE_CONTROL_ENROLL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const REMOTE_CONTROL_PAIRING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const REMOTE_CONTROL_RESPONSE_BODY_MAX_BYTES: usize = 4096;
const REMOTE_CONTROL_SERVER_TOKEN_REFRESH_SKEW_SECS: i64 = 30;

const REQUEST_ID_HEADER: &str = "x-request-id";
const OAI_REQUEST_ID_HEADER: &str = "x-oai-request-id";
const CF_RAY_HEADER: &str = "cf-ray";
pub(super) const REMOTE_CONTROL_ACCOUNT_ID_HEADER: &str = "chatgpt-account-id";
pub(super) const REMOTE_CONTROL_INSTALLATION_ID_HEADER: &str = "x-codex-installation-id";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RemoteControlEnrollment {
    pub(super) remote_control_target: RemoteControlTarget,
    pub(super) account_id: String,
    pub(super) environment_id: String,
    pub(super) server_id: String,
    pub(super) server_name: String,
    pub(super) remote_control_token: Option<String>,
    pub(super) expires_at: Option<OffsetDateTime>,
}

impl RemoteControlEnrollment {
    pub(super) async fn start_pairing(
        &self,
        request: StartRemoteControlPairingRequest,
    ) -> io::Result<RemoteControlPairingStartResponse> {
        if self.should_refresh_server_token() {
            return Err(pairing_unavailable_error());
        }
        let remote_control_token = self
            .remote_control_token
            .as_deref()
            .ok_or_else(pairing_unavailable_error)?;

        let response = build_reqwest_client()
            .post(&self.remote_control_target.pair_url)
            .timeout(REMOTE_CONTROL_PAIRING_TIMEOUT)
            .bearer_auth(remote_control_token)
            .json(&request)
            .send()
            .await
            .map_err(|err| {
                io::Error::other(format!(
                    "failed to start remote control pairing at `{}`: {err}",
                    self.remote_control_target.pair_url
                ))
            })?;
        let headers = response.headers().clone();
        let status = response.status();
        let body = response.bytes().await.map_err(|err| {
            io::Error::other(format!(
                "failed to read remote control pairing response from `{}`: {err}",
                self.remote_control_target.pair_url
            ))
        })?;
        let body_preview = preview_remote_control_response_body(&body);
        if !status.is_success() {
            let error_kind = match status.as_u16() {
                401 | 403 => ErrorKind::PermissionDenied,
                404 => ErrorKind::NotFound,
                _ => ErrorKind::Other,
            };
            return Err(io::Error::new(
                error_kind,
                format!(
                    "remote control pairing failed at `{}`: HTTP {status}, {}, body: {body_preview}",
                    self.remote_control_target.pair_url,
                    format_headers(&headers)
                ),
            ));
        }

        let pairing = serde_json::from_slice::<StartRemoteControlPairingResponse>(&body).map_err(
            |err| {
                io::Error::other(format!(
                    "failed to parse remote control pairing response from `{}`: HTTP {status}, {}, body: {body_preview}, decode error: {err}",
                    self.remote_control_target.pair_url,
                    format_headers(&headers)
                ))
            },
        )?;
        let StartRemoteControlPairingResponse {
            pairing_code,
            manual_pairing_code,
            server_id,
            environment_id,
            expires_at,
        } = pairing;
        if server_id != self.server_id || environment_id != self.environment_id {
            return Err(io::Error::other(format!(
                "remote control pairing returned mismatched enrollment: expected server_id={}, environment_id={}; got server_id={}, environment_id={}",
                self.server_id, self.environment_id, server_id, environment_id
            )));
        }
        let expires_at = OffsetDateTime::parse(&expires_at, &Rfc3339)
            .map_err(|err| {
                io::Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "failed to parse remote control pairing response from `{}`: HTTP {status}, {}, body: {body_preview}, expires_at parse error: {err}",
                        self.remote_control_target.pair_url,
                        format_headers(&headers)
                    ),
                )
            })?
            .unix_timestamp();

        Ok(RemoteControlPairingStartResponse {
            pairing_code,
            manual_pairing_code,
            environment_id,
            expires_at,
        })
    }

    pub(super) async fn pairing_status(
        &self,
        request: RemoteControlPairingStatusRequest,
    ) -> io::Result<RemoteControlPairingStatusResponse> {
        if self.should_refresh_server_token() {
            return Err(pairing_unavailable_error());
        }
        let remote_control_token = self
            .remote_control_token
            .as_deref()
            .ok_or_else(pairing_unavailable_error)?;

        let response = build_reqwest_client()
            .post(&self.remote_control_target.pair_status_url)
            .timeout(REMOTE_CONTROL_PAIRING_TIMEOUT)
            .bearer_auth(remote_control_token)
            .json(&request)
            .send()
            .await
            .map_err(|err| {
                io::Error::other(format!(
                    "failed to check remote control pairing status at `{}`: {err}",
                    self.remote_control_target.pair_status_url
                ))
            })?;
        let headers = response.headers().clone();
        let status = response.status();
        let body = response.bytes().await.map_err(|err| {
            io::Error::other(format!(
                "failed to read remote control pairing status response from `{}`: {err}",
                self.remote_control_target.pair_status_url
            ))
        })?;
        let body_preview = preview_remote_control_response_body(&body);
        if !status.is_success() {
            let error_kind = match status.as_u16() {
                401 | 403 => ErrorKind::PermissionDenied,
                404 | 410 => ErrorKind::InvalidInput,
                _ => ErrorKind::Other,
            };
            return Err(io::Error::new(
                error_kind,
                format!(
                    "remote control pairing status failed at `{}`: HTTP {status}, {}, body: {body_preview}",
                    self.remote_control_target.pair_status_url,
                    format_headers(&headers)
                ),
            ));
        }

        let response = serde_json::from_slice::<BackendRemoteControlPairingStatusResponse>(&body)
            .map_err(|err| {
                io::Error::other(format!(
                    "failed to parse remote control pairing status response from `{}`: HTTP {status}, {}, body: {body_preview}, decode error: {err}",
                    self.remote_control_target.pair_status_url,
                    format_headers(&headers)
                ))
            })?;
        Ok(RemoteControlPairingStatusResponse {
            claimed: response.claimed,
        })
    }

    pub(super) fn should_refresh_server_token(&self) -> bool {
        self.remote_control_token.is_none()
            || self.expires_at.is_none_or(|expires_at| {
                expires_at.unix_timestamp()
                    <= OffsetDateTime::now_utc().unix_timestamp()
                        + REMOTE_CONTROL_SERVER_TOKEN_REFRESH_SKEW_SECS
            })
    }

    pub(super) fn clear_server_token(&mut self) {
        self.remote_control_token = None;
        self.expires_at = None;
    }
}

pub(crate) fn preview_remote_control_response_body(body: &[u8]) -> String {
    let body = String::from_utf8_lossy(body);
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "<empty>".to_string();
    }
    let redacted = redact_remote_control_response_body(trimmed);
    if redacted.len() <= REMOTE_CONTROL_RESPONSE_BODY_MAX_BYTES {
        return redacted;
    }

    let mut cut = REMOTE_CONTROL_RESPONSE_BODY_MAX_BYTES;
    while !redacted.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    let mut truncated = redacted[..cut].to_string();
    truncated.push_str("...");
    truncated
}

fn redact_remote_control_response_body(body: &str) -> String {
    let Ok(mut body_json) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.to_string();
    };
    let Some(body_object) = body_json.as_object_mut() else {
        return body.to_string();
    };
    for sensitive_field in [
        "remote_control_token",
        "pairing_code",
        "manual_pairing_code",
    ] {
        if let Some(value) = body_object.get_mut(sensitive_field) {
            *value = serde_json::Value::String("<redacted>".to_string());
        }
    }
    body_json.to_string()
}

pub(crate) fn format_headers(headers: &HeaderMap) -> String {
    let request_id_str = headers
        .get(REQUEST_ID_HEADER)
        .or_else(|| headers.get(OAI_REQUEST_ID_HEADER))
        .map(|value| value.to_str().unwrap_or("<invalid utf-8>").to_owned())
        .unwrap_or_else(|| "<none>".to_owned());
    let cf_ray_str = headers
        .get(CF_RAY_HEADER)
        .map(|value| value.to_str().unwrap_or("<invalid utf-8>").to_owned())
        .unwrap_or_else(|| "<none>".to_owned());
    format!("request-id: {request_id_str}, cf-ray: {cf_ray_str}")
}

pub(super) async fn enroll_remote_control_server(
    remote_control_target: &RemoteControlTarget,
    auth: &RemoteControlConnectionAuth,
    installation_id: &str,
    server_name: &str,
) -> io::Result<RemoteControlEnrollment> {
    let enroll_url = &remote_control_target.enroll_url;
    let request = EnrollRemoteServerRequest {
        name: server_name.to_string(),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        app_server_version: env!("CARGO_PKG_VERSION"),
        installation_id: installation_id.to_string(),
    };
    let enrollment_response = send_remote_control_server_request::<_, EnrollRemoteServerResponse>(
        enroll_url,
        auth,
        installation_id,
        &request,
        "enroll",
        "server enrollment",
    )
    .await?;
    let mut enrollment = RemoteControlEnrollment {
        remote_control_target: remote_control_target.clone(),
        account_id: auth.account_id.clone(),
        environment_id: enrollment_response.environment_id,
        server_id: enrollment_response.server_id,
        server_name: server_name.to_string(),
        remote_control_token: None,
        expires_at: None,
    };
    update_remote_control_server_token(
        &mut enrollment,
        enroll_url,
        enrollment_response.remote_control_token,
        enrollment_response.expires_at,
    )?;
    Ok(enrollment)
}

pub(super) async fn refresh_remote_control_server(
    auth: &RemoteControlConnectionAuth,
    installation_id: &str,
    enrollment: &mut RemoteControlEnrollment,
) -> io::Result<()> {
    let refresh_url = enrollment.remote_control_target.refresh_url.clone();
    let request = RefreshRemoteServerRequest {
        server_id: enrollment.server_id.clone(),
        installation_id: installation_id.to_string(),
    };
    let refreshed = send_remote_control_server_request::<_, EnrollRemoteServerResponse>(
        &refresh_url,
        auth,
        installation_id,
        &request,
        "refresh",
        "server refresh",
    )
    .await?;
    if refreshed.server_id != enrollment.server_id
        || refreshed.environment_id != enrollment.environment_id
    {
        return Err(io::Error::other(format!(
            "remote control server refresh returned mismatched enrollment: expected server_id={}, environment_id={}; got server_id={}, environment_id={}",
            enrollment.server_id,
            enrollment.environment_id,
            refreshed.server_id,
            refreshed.environment_id
        )));
    }

    update_remote_control_server_token(
        enrollment,
        &refresh_url,
        refreshed.remote_control_token,
        refreshed.expires_at,
    )
}

async fn send_remote_control_server_request<Request, Response>(
    url: &str,
    auth: &RemoteControlConnectionAuth,
    installation_id: &str,
    request: &Request,
    action: &str,
    response_kind: &str,
) -> io::Result<Response>
where
    Request: Serialize,
    Response: DeserializeOwned,
{
    let client = build_reqwest_client();
    let mut auth_headers = HeaderMap::new();
    auth.auth_provider.add_auth_headers(&mut auth_headers);
    let response = client
        .post(url)
        .timeout(REMOTE_CONTROL_ENROLL_TIMEOUT)
        .headers(auth_headers)
        .header(REMOTE_CONTROL_ACCOUNT_ID_HEADER, &auth.account_id)
        .header(REMOTE_CONTROL_INSTALLATION_ID_HEADER, installation_id)
        .json(request)
        .send()
        .await
        .map_err(|err| {
            io::Error::other(format!(
                "failed to {action} remote control server at `{url}`: {err}"
            ))
        })?;
    let headers = response.headers().clone();
    let status = response.status();
    let body = response.bytes().await.map_err(|err| {
        io::Error::other(format!(
            "failed to read remote control {response_kind} response from `{url}`: {err}"
        ))
    })?;
    let body_preview = preview_remote_control_response_body(&body);
    if !status.is_success() {
        let headers_str = format_headers(&headers);
        let error_kind = match status.as_u16() {
            401 | 403 => ErrorKind::PermissionDenied,
            404 => ErrorKind::NotFound,
            _ => ErrorKind::Other,
        };
        return Err(io::Error::new(
            error_kind,
            format!(
                "remote control {response_kind} failed at `{url}`: HTTP {status}, {headers_str}, body: {body_preview}"
            ),
        ));
    }

    serde_json::from_slice::<Response>(&body).map_err(|err| {
        let headers_str = format_headers(&headers);
        io::Error::other(format!(
            "failed to parse remote control {response_kind} response from `{url}`: HTTP {status}, {headers_str}, body: {body_preview}, decode error: {err}"
        ))
    })
}

fn update_remote_control_server_token(
    enrollment: &mut RemoteControlEnrollment,
    url: &str,
    token: String,
    expires_at: String,
) -> io::Result<()> {
    let expires_at = OffsetDateTime::parse(&expires_at, &Rfc3339).map_err(|err| {
        io::Error::other(format!(
            "failed to parse remote control server token expiry from `{url}`: {err}"
        ))
    })?;
    enrollment.remote_control_token = Some(token);
    enrollment.expires_at = Some(expires_at);
    Ok(())
}
