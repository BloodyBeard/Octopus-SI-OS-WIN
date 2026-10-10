use std::path::Path;

use octopus_interfaces::InterfaceArtifactFile;
use octopus_provider_run_authoring::{ProviderHandoffOutcome, ProviderOutputHandoff};
use reqwest::Request;
use serde::Serialize;

const TRANSPORT_DISPATCH_LABEL: &str = "provider.transport.dispatch";

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct TransportDispatch<'a> {
    label: &'static str,
    provider_interface: &'a str,
    source_movement_seq: u64,
    attempt: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    credential_handle_fingerprint: Option<&'a str>,
    method: &'a str,
    transport: &'static str,
    endpoint: EndpointCoordinate,
    request: RequestCoordinate,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct EndpointCoordinate {
    scheme: String,
    host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    path: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct RequestCoordinate {
    bytes: u64,
    blake3: String,
}

pub(crate) struct OAuthDispatchAdmission<'a> {
    pub handoff: &'a ProviderOutputHandoff,
    pub output_root: &'a Path,
    pub provider_interface: &'a str,
    pub source_movement_seq: u64,
    pub attempt: u32,
    pub credential_handle_fingerprint: Option<&'a str>,
}

impl OAuthDispatchAdmission<'_> {
    pub async fn admit_request(&self, request: &Request) -> Result<u64, String> {
        if self.attempt == 0 {
            return Err("OAuth transport dispatch attempt must be positive".to_string());
        }
        let request_bytes = request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .ok_or_else(|| {
                "OAuth transport dispatch requires an exact in-memory request body".to_string()
            })?;
        let endpoint = request.url();
        let host = endpoint
            .host_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "OAuth transport dispatch endpoint has no host".to_string())?;
        let payload = TransportDispatch {
            label: TRANSPORT_DISPATCH_LABEL,
            provider_interface: self.provider_interface,
            source_movement_seq: self.source_movement_seq,
            attempt: self.attempt,
            credential_handle_fingerprint: self.credential_handle_fingerprint,
            method: request.method().as_str(),
            transport: "http",
            endpoint: EndpointCoordinate {
                scheme: endpoint.scheme().to_string(),
                host: host.to_string(),
                port: endpoint.port(),
                path: endpoint.path().to_string(),
            },
            request: RequestCoordinate {
                bytes: u64::try_from(request_bytes.len()).map_err(|_| {
                    "OAuth transport dispatch request byte count exceeds u64".to_string()
                })?,
                blake3: blake3::hash(request_bytes).to_hex().to_string(),
            },
        };
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("OAuth transport dispatch did not encode: {error}"))?;
        let artifact =
            InterfaceArtifactFile::write(self.output_root, TRANSPORT_DISPATCH_LABEL, &bytes)
                .map_err(|error| {
                    format!("OAuth transport dispatch did not materialize: {error}")
                })?;
        match self.handoff.hand_transport_dispatch(artifact).await? {
            ProviderHandoffOutcome::Admitted { seq, .. } => Ok(seq),
            ProviderHandoffOutcome::TerminalFailure {
                failure_seq,
                failure_class,
                ..
            } => Err(format!(
                "provider transport dispatch stopped by terminal run-authoring failure '{failure_class}' at movement {failure_seq}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_coordinate_drops_oauth_query_and_credentials() {
        let request = reqwest::Client::new()
            .post("https://user:secret@auth.example/token?code=secret")
            .body("grant_type=authorization_code")
            .build()
            .unwrap();
        let endpoint = request.url();
        let coordinate = EndpointCoordinate {
            scheme: endpoint.scheme().to_string(),
            host: endpoint.host_str().unwrap().to_string(),
            port: endpoint.port(),
            path: endpoint.path().to_string(),
        };
        let encoded = serde_json::to_string(&coordinate).unwrap();
        assert!(encoded.contains("auth.example"));
        assert!(encoded.contains("/token"));
        assert!(!encoded.contains("secret"));
        assert!(!encoded.contains("code"));
    }
}
