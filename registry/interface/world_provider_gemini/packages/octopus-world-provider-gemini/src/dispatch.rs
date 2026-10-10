use std::path::Path;

use octopus_descriptors::InterfaceId;
use octopus_interfaces::InterfaceArtifactFile;
use octopus_provider_run_authoring::{ProviderHandoffOutcome, ProviderOutputHandoff};
use reqwest::Url;
use serde::Serialize;

const TRANSPORT_DISPATCH_LABEL: &str = "provider.transport.dispatch";

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct TransportDispatch<'a> {
    label: &'static str,
    provider_interface: &'a InterfaceId,
    source_movement_seq: u64,
    attempt: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    credential_handle_fingerprint: Option<&'a str>,
    method: &'a str,
    transport: &'a str,
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

pub(crate) struct DispatchAdmission<'a> {
    pub handoff: &'a ProviderOutputHandoff,
    pub output_root: &'a Path,
    pub provider_interface: &'a InterfaceId,
    pub source_movement_seq: u64,
    pub attempt: u32,
    pub credential_handle_fingerprint: Option<&'a str>,
}

impl DispatchAdmission<'_> {
    pub async fn admit(
        &self,
        method: &str,
        transport: &str,
        endpoint: &Url,
        request_bytes: &[u8],
    ) -> Result<u64, String> {
        if self.attempt == 0 {
            return Err("Gemini transport dispatch attempt must be positive".to_string());
        }
        let payload = TransportDispatch {
            label: TRANSPORT_DISPATCH_LABEL,
            provider_interface: self.provider_interface,
            source_movement_seq: self.source_movement_seq,
            attempt: self.attempt,
            credential_handle_fingerprint: self.credential_handle_fingerprint,
            method,
            transport,
            endpoint: endpoint_coordinate(endpoint)?,
            request: RequestCoordinate {
                bytes: u64::try_from(request_bytes.len()).map_err(|_| {
                    "Gemini transport dispatch request byte count exceeds u64".to_string()
                })?,
                blake3: blake3::hash(request_bytes).to_hex().to_string(),
            },
        };
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("Gemini transport dispatch did not encode: {error}"))?;
        let artifact =
            InterfaceArtifactFile::write(self.output_root, TRANSPORT_DISPATCH_LABEL, &bytes)
                .map_err(|error| {
                    format!("Gemini transport dispatch did not materialize: {error}")
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

fn endpoint_coordinate(endpoint: &Url) -> Result<EndpointCoordinate, String> {
    let host = endpoint
        .host_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Gemini transport dispatch endpoint has no host".to_string())?;
    Ok(EndpointCoordinate {
        scheme: endpoint.scheme().to_string(),
        host: host.to_string(),
        port: endpoint.port(),
        path: endpoint.path().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::endpoint_coordinate;
    use reqwest::Url;

    #[test]
    fn dispatch_endpoint_is_a_secret_free_coordinate() {
        let endpoint =
            Url::parse("https://user:secret@example.com/v1/models/x?key=secret").unwrap();
        let encoded = serde_json::to_string(&endpoint_coordinate(&endpoint).unwrap()).unwrap();
        assert!(encoded.contains("/v1/models/x"));
        assert!(!encoded.contains("secret"));
        assert!(!encoded.contains("key"));
    }
}
