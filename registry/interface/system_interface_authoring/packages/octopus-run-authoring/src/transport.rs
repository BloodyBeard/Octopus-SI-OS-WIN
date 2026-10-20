use super::*;

pub(crate) fn validate_transport_dispatch_payload(
    payload: &[u8],
    provider: &InterfaceId,
    source_movement_seq: u64,
) -> Result<ProviderTransportDispatch, String> {
    let dispatch: ProviderTransportDispatch = serde_json::from_slice(payload)
        .map_err(|error| format!("provider transport dispatch payload is malformed: {error}"))?;
    if dispatch.label != PROVIDER_TRANSPORT_DISPATCH_LABEL
        || dispatch.provider_interface != *provider
        || dispatch.source_movement_seq != source_movement_seq
    {
        return Err(
            "provider transport dispatch identity differs from the active handoff".to_string(),
        );
    }
    if dispatch.attempt == 0 {
        return Err("provider transport dispatch attempt must be positive".to_string());
    }
    for (name, value) in [
        ("method", dispatch.method.as_str()),
        ("transport", dispatch.transport.as_str()),
        ("endpoint.scheme", dispatch.endpoint.scheme.as_str()),
        ("endpoint.path", dispatch.endpoint.path.as_str()),
    ] {
        if value.is_empty() || value.trim() != value {
            return Err(format!(
                "provider transport dispatch {name} must be non-empty and trimmed"
            ));
        }
    }
    match dispatch.transport.as_str() {
        "http" if !canonical_http_method(&dispatch.method) => {
            return Err(
                "provider HTTP dispatch method must be a bounded canonical uppercase token"
                    .to_string(),
            );
        }
        "websocket" if !canonical_websocket_method(&dispatch.method) => {
            return Err(
                "provider WebSocket dispatch method must be a bounded canonical lowercase event token"
                    .to_string(),
            );
        }
        "process" | "local_ipc" if !canonical_local_method(&dispatch.method) => {
            return Err(
                "provider local dispatch method must be a bounded canonical method token"
                    .to_string(),
            );
        }
        _ => {}
    }
    if !dispatch
        .endpoint
        .scheme
        .bytes()
        .enumerate()
        .all(|(index, byte)| {
            if index == 0 {
                byte.is_ascii_lowercase()
            } else {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'+' | b'-' | b'.')
            }
        })
    {
        return Err("provider transport dispatch endpoint.scheme is not canonical".to_string());
    }
    if !dispatch.endpoint.path.starts_with('/') {
        return Err("provider transport dispatch endpoint.path must be absolute".to_string());
    }
    match dispatch.transport.as_str() {
        "http" | "websocket" => {
            let compatible = match dispatch.transport.as_str() {
                "http" => matches!(dispatch.endpoint.scheme.as_str(), "http" | "https"),
                "websocket" => matches!(dispatch.endpoint.scheme.as_str(), "ws" | "wss"),
                _ => false,
            };
            if !compatible {
                return Err(format!(
                    "provider transport dispatch '{}' transport is incompatible with '{}' scheme",
                    dispatch.transport, dispatch.endpoint.scheme
                ));
            }
            validate_network_endpoint(&dispatch.endpoint)?;
        }
        "process" => {
            if dispatch.endpoint.scheme != "process" {
                return Err(format!(
                    "provider transport dispatch '{}' transport is incompatible with '{}' scheme",
                    dispatch.transport, dispatch.endpoint.scheme
                ));
            }
            if dispatch.endpoint.host.is_empty()
                || dispatch.endpoint.port.is_some()
                || malformed_endpoint_host(&dispatch.endpoint.host)
            {
                return Err(
                    "provider process dispatch endpoint requires one canonical host identity and no port"
                        .to_string(),
                );
            }
        }
        "local_ipc" => {
            if dispatch.endpoint.scheme != "unix" {
                return Err(format!(
                    "provider transport dispatch '{}' transport is incompatible with '{}' scheme",
                    dispatch.transport, dispatch.endpoint.scheme
                ));
            }
            if !dispatch.endpoint.host.is_empty() || dispatch.endpoint.port.is_some() {
                return Err(
                    "provider local IPC dispatch endpoint requires an empty host and no port"
                        .to_string(),
                );
            }
        }
        _ => {
            return Err(format!(
                "provider transport dispatch transport '{}' is unsupported",
                dispatch.transport
            ));
        }
    }
    if !canonical_lower_hex_64(&dispatch.request.blake3) {
        return Err(
            "provider transport dispatch request.blake3 must be canonical lowercase hex"
                .to_string(),
        );
    }
    if dispatch.transport == "http" && dispatch.method == "GET" {
        let empty_hash = blake3::hash(&[]).to_hex().to_string();
        if dispatch.request.bytes != 0 || dispatch.request.blake3 != empty_hash {
            return Err(
                "provider transport dispatch GET must prove the canonical empty request body"
                    .to_string(),
            );
        }
    } else if dispatch.request.bytes == 0 {
        return Err(format!(
            "provider transport dispatch {} request.bytes must be positive",
            dispatch.method
        ));
    }
    if let Some(fingerprint) = &dispatch.credential_handle_fingerprint {
        if !canonical_lower_hex_64(fingerprint) {
            return Err(
                "provider transport dispatch credential_handle_fingerprint must be canonical lowercase hex"
                    .to_string(),
            );
        }
    }
    Ok(dispatch)
}

pub(crate) fn canonical_http_method(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().any(|byte| byte.is_ascii_uppercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-')
}

pub(crate) fn canonical_websocket_method(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.split('.').all(|segment| {
            !segment.is_empty()
                && segment.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-')
                })
        })
}

pub(crate) fn canonical_local_method(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.split(['.', '/', ':']).all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'$'))
        })
}

pub(crate) fn malformed_endpoint_host(value: &str) -> bool {
    value
        .bytes()
        .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'@' | b'?' | b'#'))
}

pub(crate) fn validate_network_endpoint(endpoint: &ProviderTransportEndpoint) -> Result<(), String> {
    if endpoint.host.is_empty() || malformed_endpoint_host(&endpoint.host) {
        return Err("provider transport dispatch endpoint.host is malformed".to_string());
    }
    if endpoint.port == Some(0) {
        return Err("provider transport dispatch endpoint.port must be positive".to_string());
    }
    Ok(())
}

pub(crate) fn canonical_lower_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn validate_handoff_completion_payload(
    payload: &[u8],
    provider: &InterfaceId,
    source_movement_seq: u64,
) -> Result<ProviderHandoffCompletion, String> {
    let completion: ProviderHandoffCompletion = serde_json::from_slice(payload)
        .map_err(|error| format!("provider handoff completion payload is malformed: {error}"))?;
    if completion.label != PROVIDER_HANDOFF_COMPLETE_LABEL
        || completion.provider_interface != *provider
        || completion.source_movement_seq != source_movement_seq
    {
        return Err(
            "provider handoff completion identity differs from the active handoff".to_string(),
        );
    }
    if completion.artifacts.is_empty() {
        return Err("provider handoff completion contains no terminal artifacts".to_string());
    }
    let mut artifacts = HashSet::new();
    for artifact in &completion.artifacts {
        let key = artifact_ref_key(artifact);
        if !artifacts.insert(key.clone()) {
            return Err(format!(
                "provider handoff completion repeats terminal artifact '{key}'"
            ));
        }
    }
    Ok(completion)
}
