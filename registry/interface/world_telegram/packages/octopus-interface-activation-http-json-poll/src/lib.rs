use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use octopus_descriptors::{ArtifactId, InterfaceId};
use octopus_secrets::{LocalFileKeyringStore, SecretName, SecretScope, SecretsManager};

use octopus_interface_transport::{
    ActivationStart, ActivationStartFuture, InterfaceActivationEvidence, send_arrival,
};
use octopus_interfaces::InterfaceArtifactFile;

pub const TRANSPORT_REF: &str = "http_json_poll";
const ACTIVATION_SECRETS_HOME_SLOT: &str = "activation_secrets_home";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpJsonPollConfig {
    request: HttpActivationRequest,
    items_path: JsonPath,
    item_id_path: JsonPath,
    #[serde(default)]
    text_path: Option<JsonPath>,
    #[serde(default)]
    origin_target_path: Option<JsonPath>,
    #[serde(default)]
    filters: Option<HttpJsonPollFilters>,
    #[serde(default = "default_poll_interval_ms")]
    poll_interval_ms: u64,
    #[serde(default)]
    cursor: Option<HttpJsonPollCursor>,
    arrival: HttpJsonPollArrival,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpActivationRequest {
    #[serde(default = "default_http_method")]
    method: String,
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body_json: Option<serde_json::Value>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default = "default_timeout_secs")]
    timeout_secs: u64,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpJsonPollCursor {
    request_body_path: JsonPath,
    #[serde(default = "default_cursor_advance")]
    advance: String,
}

#[derive(Clone, Debug)]
struct JsonPath(Vec<String>);

impl JsonPath {
    fn as_slice(&self) -> &[String] {
        &self.0
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'de> serde::Deserialize<'de> for JsonPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = JsonPath;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a non-empty dot path string or non-empty string array")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                parse_json_path_segments(value.split('.')).map(JsonPath)
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut parts = Vec::new();
                while let Some(segment) = seq.next_element::<String>()? {
                    let trimmed = segment.trim();
                    if trimmed.is_empty() {
                        return Err(serde::de::Error::custom(
                            "JSON path array contains an empty segment",
                        ));
                    }
                    parts.push(trimmed.to_string());
                }
                if parts.is_empty() {
                    return Err(serde::de::Error::custom("JSON path array is empty"));
                }
                Ok(JsonPath(parts))
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

fn parse_json_path_segments<'a, I, E>(segments: I) -> Result<Vec<String>, E>
where
    I: IntoIterator<Item = &'a str>,
    E: serde::de::Error,
{
    let mut out = Vec::new();
    for segment in segments {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            return Err(serde::de::Error::custom(
                "JSON path string contains an empty segment",
            ));
        }
        out.push(trimmed.to_string());
    }
    if out.is_empty() {
        return Err(serde::de::Error::custom("JSON path string is empty"));
    }
    Ok(out)
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpJsonPollFilters {
    #[serde(default)]
    allow: Vec<HttpJsonPollFilterRule>,
    #[serde(default = "default_filter_mode")]
    mode: String,
    #[serde(default = "default_true")]
    deny_unmatched: bool,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpJsonPollFilterRule {
    path: JsonPath,
    equals: serde_json::Value,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpJsonPollArrival {
    payload_template: String,
}

struct HttpRequestPlan {
    method: reqwest::Method,
    url: String,
    redacted_url: String,
    headers: BTreeMap<String, String>,
    body_json: Option<serde_json::Value>,
    body: Option<String>,
    timeout_secs: u64,
    secret_values: Vec<String>,
}

fn default_http_method() -> String {
    "POST".to_string()
}

fn default_timeout_secs() -> u64 {
    30
}

fn default_poll_interval_ms() -> u64 {
    1500
}

fn default_cursor_advance() -> String {
    "increment".to_string()
}

fn default_filter_mode() -> String {
    "all".to_string()
}

fn default_true() -> bool {
    true
}

pub fn activation_start(start: ActivationStart) -> ActivationStartFuture {
    Box::pin(async move {
        let config: HttpJsonPollConfig =
            serde_json::from_value(start.config.clone()).map_err(|e| {
                format!(
                    "interface {} http_json_poll activation config is malformed: {e}",
                    start.interface_id
                )
            })?;
        validate_http_json_poll_config(&config).map_err(|e| {
            format!(
                "interface {} http_json_poll activation config is invalid: {e}",
                start.interface_id
            )
        })?;
        let secrets_home = start
            .root_slots
            .get(ACTIVATION_SECRETS_HOME_SLOT)
            .ok_or_else(|| {
                format!(
                    "interface {} http_json_poll requires root slot '{}'",
                    start.interface_id, ACTIVATION_SECRETS_HOME_SLOT
                )
            })?;
        let secrets = Arc::new(local_activation_secrets_manager(secrets_home)?);
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| format!("http_json_poll client build failed: {e}"))?;
        tracing::info!(
            interface = %start.interface_id,
            "activated interface via http_json_poll"
        );
        Ok(vec![tokio::spawn(async move {
            run_http_json_poll_loop(start, config, secrets, client).await;
        })])
    })
}

fn validate_http_json_poll_config(config: &HttpJsonPollConfig) -> Result<(), String> {
    if config.request.url.trim().is_empty() {
        return Err("request.url must be non-empty".into());
    }
    if config.items_path.is_empty() {
        return Err("items_path must be non-empty".into());
    }
    if config.item_id_path.is_empty() {
        return Err("item_id_path must be non-empty".into());
    }
    if let Some(filters) = &config.filters {
        if filters.allow.is_empty() && filters.deny_unmatched {
            return Err("filters.allow must be non-empty when deny_unmatched=true".into());
        }
        if !matches!(filters.mode.as_str(), "all" | "any") {
            return Err("filters.mode must be 'all' or 'any'".into());
        }
        for filter in &filters.allow {
            if filter.path.is_empty() {
                return Err("filter.path must be non-empty".into());
            }
        }
    }
    if config.poll_interval_ms < 250 {
        return Err("poll_interval_ms must be at least 250".into());
    }
    if config.arrival.payload_template.trim().is_empty() {
        return Err("arrival.payload_template must be non-empty".into());
    }
    if let Some(cursor) = &config.cursor {
        if cursor.request_body_path.is_empty() {
            return Err("cursor.request_body_path must be non-empty".into());
        }
        if !matches!(cursor.advance.as_str(), "copy" | "increment") {
            return Err("cursor.advance must be 'copy' or 'increment'".into());
        }
    }
    Ok(())
}

async fn run_http_json_poll_loop(
    start: ActivationStart,
    config: HttpJsonPollConfig,
    secrets: Arc<SecretsManager>,
    client: reqwest::Client,
) {
    let mut cursor_value: Option<serde_json::Value> = None;

    loop {
        if start.cancel.is_cancelled() {
            break;
        }

        let poll_result =
            poll_http_json_once(&client, &config, &secrets, cursor_value.clone()).await;
        match poll_result {
            Ok(items) => {
                for item in items {
                    if !http_json_poll_item_admitted(&item, config.filters.as_ref()) {
                        continue;
                    }
                    let Some(item_id_value) = get_json_path(&item, config.item_id_path.as_slice())
                    else {
                        tracing::warn!(
                            interface = %start.interface_id,
                            "http_json_poll item has no item_id_path; skipping"
                        );
                        continue;
                    };
                    let item_id = json_value_to_string(item_id_value);
                    if let Some(cursor) = &config.cursor {
                        cursor_value = Some(advance_cursor_value(item_id_value, cursor));
                    }

                    let substitutions =
                        item_substitutions(&start.interface_id, &config, &item, &item_id);
                    let arrival_payload =
                        match build_http_poll_arrival_payload(&config.arrival, &substitutions) {
                            Ok(payload) => payload,
                            Err(e) => {
                                tracing::warn!(
                                    interface = %start.interface_id,
                                    item_id = %item_id,
                                    error = %e,
                                    "http_json_poll could not build arrival payload"
                                );
                                continue;
                            }
                        };
                    let artifact = match InterfaceArtifactFile::write(
                        &start.artifact_root,
                        start.interface_id.as_str(),
                        &arrival_payload,
                    )
                    .and_then(|artifact| {
                        artifact.capture_bounded(
                            start.host_io_limits.max_manifest_bytes,
                            start.host_io_limits.max_payload_bytes,
                        )
                    }) {
                        Ok(artifact) => artifact,
                        Err(e) => {
                            tracing::warn!(
                                interface = %start.interface_id,
                                item_id = %item_id,
                                error = %e,
                                "http_json_poll artifact materialization failed closed"
                            );
                            continue;
                        }
                    };
                    if send_arrival(
                        &start.tx,
                        &start.interface_id,
                        artifact,
                        vec![http_json_poll_arrival_evidence(
                            &start.interface_id,
                            &item_id,
                            &item,
                        )],
                        None,
                    )
                    .await
                    .is_err()
                    {
                        return;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    interface = %start.interface_id,
                    error = %e,
                    "http_json_poll poll failed"
                );
            }
        }

        tokio::select! {
            _ = start.cancel.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_millis(config.poll_interval_ms)) => {}
        }
    }
}

async fn poll_http_json_once(
    client: &reqwest::Client,
    config: &HttpJsonPollConfig,
    secrets: &SecretsManager,
    cursor_value: Option<serde_json::Value>,
) -> Result<Vec<serde_json::Value>, String> {
    let substitutions = BTreeMap::new();
    let mut request = config.request.clone();
    if let (Some(cursor), Some(value)) = (&config.cursor, cursor_value) {
        let body = request
            .body_json
            .get_or_insert_with(|| serde_json::Value::Object(Default::default()));
        set_json_path(body, cursor.request_body_path.as_slice(), value)?;
    }
    let response = send_activation_http_request(client, &request, &substitutions, secrets).await?;
    let items = get_json_path(&response, config.items_path.as_slice())
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "poll response items_path did not resolve to an array".to_string())?;
    Ok(items.clone())
}

async fn send_activation_http_request(
    client: &reqwest::Client,
    request: &HttpActivationRequest,
    substitutions: &BTreeMap<String, String>,
    secrets: &SecretsManager,
) -> Result<serde_json::Value, String> {
    let plan = plan_activation_http_request(request, substitutions, secrets)?;
    let mut builder = client
        .request(plan.method.clone(), &plan.url)
        .timeout(Duration::from_secs(plan.timeout_secs));
    for (name, value) in &plan.headers {
        builder = builder.header(name, value);
    }
    if let Some(body_json) = &plan.body_json {
        builder = builder.json(body_json);
    } else if let Some(body) = &plan.body {
        builder = builder.body(body.clone());
    }
    let response = builder
        .send()
        .await
        .map_err(|e| format!("transport failed for {}: {e}", plan.redacted_url))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("response read failed for {}: {e}", plan.redacted_url))?;
    let redacted_body = redact_values(&body, &plan.secret_values);
    if !status.is_success() {
        return Err(format!(
            "{} returned HTTP {} body={}",
            plan.redacted_url,
            status.as_u16(),
            truncate(&redacted_body, 2048)
        ));
    }
    serde_json::from_str(&redacted_body)
        .map_err(|e| format!("{} returned non-JSON body: {e}", plan.redacted_url))
}

fn plan_activation_http_request(
    request: &HttpActivationRequest,
    substitutions: &BTreeMap<String, String>,
    secrets: &SecretsManager,
) -> Result<HttpRequestPlan, String> {
    if request.body_json.is_some() && request.body.is_some() {
        return Err("activation HTTP request accepts only one of body_json or body".into());
    }
    let mut secret_values = Vec::new();
    let rendered_url = render_template_string(&request.url, substitutions);
    let url = expand_secret_refs(&rendered_url, secrets, &mut secret_values)?;
    if !url.starts_with("https://") {
        return Err("activation HTTP request only allows https URLs".into());
    }
    let method = match request.method.trim().to_ascii_uppercase().as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "PATCH" => reqwest::Method::PATCH,
        "DELETE" => reqwest::Method::DELETE,
        other => return Err(format!("unsupported HTTP method '{other}'")),
    };
    let mut headers = BTreeMap::new();
    for (name, value) in &request.headers {
        let rendered = render_template_string(value, substitutions);
        headers.insert(
            name.clone(),
            expand_secret_refs(&rendered, secrets, &mut secret_values)?,
        );
    }
    let body_json = match &request.body_json {
        Some(value) => {
            let mut rendered = render_template_value(value, substitutions);
            expand_secret_refs_in_value(&mut rendered, secrets, &mut secret_values)?;
            Some(rendered)
        }
        None => None,
    };
    let body = match &request.body {
        Some(value) => {
            let rendered = render_template_string(value, substitutions);
            Some(expand_secret_refs(&rendered, secrets, &mut secret_values)?)
        }
        None => None,
    };
    let redacted_url = redact_values(&url, &secret_values);
    Ok(HttpRequestPlan {
        method,
        url,
        redacted_url,
        headers,
        body_json,
        body,
        timeout_secs: request.timeout_secs.clamp(1, 120),
        secret_values,
    })
}

fn build_http_poll_arrival_payload(
    arrival: &HttpJsonPollArrival,
    substitutions: &BTreeMap<String, String>,
) -> Result<Vec<u8>, String> {
    Ok(render_template_string(&arrival.payload_template, substitutions).into_bytes())
}

fn http_json_poll_arrival_evidence(
    interface_id: &InterfaceId,
    item_id: &str,
    item: &serde_json::Value,
) -> InterfaceActivationEvidence {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "label": "interface.arrival.item",
        "interface_id": interface_id.as_str(),
        "item_id": item_id,
        "item": item,
    }))
    .unwrap_or_else(|_| b"{\"label\":\"interface.arrival.item\"}".to_vec());
    InterfaceActivationEvidence {
        interface_id: interface_id.clone(),
        artifact_id: ArtifactId::new(format!(
            "interface.arrival:{}:{}",
            sanitize_artifact_id_segment(interface_id.as_str()),
            sanitize_artifact_id_segment(item_id)
        )),
        label: "interface.arrival.item".into(),
        bytes,
    }
}

fn sanitize_artifact_id_segment(input: &str) -> String {
    let mut out = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    out.truncate(96);
    if out.is_empty() {
        "item".to_string()
    } else {
        out
    }
}

fn item_substitutions(
    interface_id: &InterfaceId,
    config: &HttpJsonPollConfig,
    item: &serde_json::Value,
    item_id: &str,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    out.insert(
        "interface_id".to_string(),
        interface_id.as_str().to_string(),
    );
    out.insert("item_id".to_string(), item_id.to_string());
    out.insert("item_json".to_string(), item.to_string());
    out.insert(
        "text".to_string(),
        config
            .text_path
            .as_ref()
            .and_then(|path| get_json_path(item, path.as_slice()))
            .map(json_value_to_string)
            .unwrap_or_default(),
    );
    out.insert(
        "origin_target".to_string(),
        config
            .origin_target_path
            .as_ref()
            .and_then(|path| get_json_path(item, path.as_slice()))
            .map(json_value_to_string)
            .unwrap_or_default(),
    );
    out
}

fn http_json_poll_item_admitted(
    item: &serde_json::Value,
    filters: Option<&HttpJsonPollFilters>,
) -> bool {
    let Some(filters) = filters else {
        return true;
    };
    if filters.allow.is_empty() {
        return !filters.deny_unmatched;
    }

    let matched = if filters.mode == "any" {
        filters.allow.iter().any(|filter| filter.matches(item))
    } else {
        filters.allow.iter().all(|filter| filter.matches(item))
    };
    matched || !filters.deny_unmatched
}

impl HttpJsonPollFilterRule {
    fn matches(&self, item: &serde_json::Value) -> bool {
        get_json_path(item, self.path.as_slice())
            .map(|value| json_values_equivalent(value, &self.equals))
            .unwrap_or(false)
    }
}

fn json_values_equivalent(actual: &serde_json::Value, expected: &serde_json::Value) -> bool {
    if actual == expected {
        return true;
    }
    json_value_to_string(actual) == json_value_to_string(expected)
}

fn advance_cursor_value(
    item_id_value: &serde_json::Value,
    cursor: &HttpJsonPollCursor,
) -> serde_json::Value {
    if cursor.advance == "copy" {
        return item_id_value.clone();
    }
    if let Some(value) = item_id_value.as_i64() {
        return serde_json::json!(value.saturating_add(1));
    }
    if let Some(value) = item_id_value.as_u64() {
        return serde_json::json!(value.saturating_add(1));
    }
    if let Some(text) = item_id_value.as_str() {
        if let Ok(value) = text.parse::<i64>() {
            return serde_json::json!(value.saturating_add(1));
        }
    }
    item_id_value.clone()
}

fn get_json_path<'a>(
    value: &'a serde_json::Value,
    path: &[String],
) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in path {
        if let Ok(index) = segment.parse::<usize>() {
            current = current.get(index)?;
        } else {
            current = current.get(segment)?;
        }
    }
    Some(current)
}

fn set_json_path(
    value: &mut serde_json::Value,
    path: &[String],
    new_value: serde_json::Value,
) -> Result<(), String> {
    let Some((last, parents)) = path.split_last() else {
        return Err("cannot set an empty JSON path".into());
    };
    let mut current = value;
    for segment in parents {
        if !current.is_object() {
            *current = serde_json::Value::Object(Default::default());
        }
        let object = current
            .as_object_mut()
            .ok_or_else(|| "cursor path parent is not an object".to_string())?;
        current = object
            .entry(segment.clone())
            .or_insert_with(|| serde_json::Value::Object(Default::default()));
    }
    let object = current
        .as_object_mut()
        .ok_or_else(|| "cursor path target parent is not an object".to_string())?;
    object.insert(last.clone(), new_value);
    Ok(())
}

fn render_template_value(
    value: &serde_json::Value,
    substitutions: &BTreeMap<String, String>,
) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => {
            serde_json::Value::String(render_template_string(text, substitutions))
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .map(|item| render_template_value(item, substitutions))
                .collect(),
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), render_template_value(value, substitutions)))
                .collect(),
        ),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            value.clone()
        }
    }
}

fn render_template_string(input: &str, substitutions: &BTreeMap<String, String>) -> String {
    let mut out = input.to_string();
    for (key, value) in substitutions {
        out = out.replace(&format!("{{{key}}}"), value);
    }
    out
}

fn json_value_to_string(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn local_activation_secrets_manager(octopus_home: &Path) -> Result<SecretsManager, String> {
    Ok(SecretsManager::new_with_keyring_store(
        octopus_home.to_path_buf(),
        Default::default(),
        Arc::new(LocalFileKeyringStore::new(
            octopus_home.join("secrets").join("keyring"),
        )),
    ))
}

fn expand_secret_refs(
    input: &str,
    secrets: &SecretsManager,
    secret_values: &mut Vec<String>,
) -> Result<String, String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find("secret://") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + "secret://".len()..];
        let name_len = after
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
            .map(char::len_utf8)
            .sum::<usize>();
        if name_len == 0 {
            return Err("secret ref has empty name".to_string());
        }
        let raw_name = &after[..name_len];
        let normalized = raw_name
            .chars()
            .map(|ch| {
                if ch == '-' {
                    '_'
                } else {
                    ch.to_ascii_uppercase()
                }
            })
            .collect::<String>();
        let name = SecretName::new(&normalized)
            .map_err(|e| format!("invalid secret ref '{raw_name}': {e}"))?;
        let value = secrets
            .get(&SecretScope::global(), &name)
            .map_err(|e| format!("secret lookup failed for {name}: {e}"))?
            .ok_or_else(|| format!("missing secret://{name}"))?;
        secret_values.push(value.clone());
        out.push_str(&value);
        rest = &after[name_len..];
    }
    out.push_str(rest);
    Ok(out)
}

fn expand_secret_refs_in_value(
    value: &mut serde_json::Value,
    secrets: &SecretsManager,
    secret_values: &mut Vec<String>,
) -> Result<(), String> {
    match value {
        serde_json::Value::String(text) => {
            *text = expand_secret_refs(text, secrets, secret_values)?;
        }
        serde_json::Value::Array(items) => {
            for item in items {
                expand_secret_refs_in_value(item, secrets, secret_values)?;
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values_mut() {
                expand_secret_refs_in_value(item, secrets, secret_values)?;
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
    Ok(())
}

fn redact_values(input: &str, values: &[String]) -> String {
    values.iter().fold(input.to_string(), |acc, value| {
        if value.is_empty() {
            acc
        } else {
            acc.replace(value, "<secret>")
        }
    })
}

fn truncate(input: &str, max: usize) -> String {
    if input.len() <= max {
        return input.to_string();
    }
    let end = input
        .char_indices()
        .map(|(idx, _)| idx)
        .take_while(|idx| *idx <= max)
        .last()
        .unwrap_or(max);
    format!("{}...<truncated>", &input[..end])
}
