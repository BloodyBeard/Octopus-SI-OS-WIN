use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use octopus_descriptors::InterfaceId;
use octopus_interface_transport::{InterfaceLifeFuture, InterfaceLiftContext, InterfaceLiftFn};
use octopus_interfaces::{
    ArtifactBindingPayload, InterfaceArtifactFile, InterfaceDescriptor, InterfaceDriverConfig,
    InterfaceInvocationEnvelope, InterfaceTransportConfig,
};
use octopus_ledger::ContentHash;
use octopus_secrets::{
    LocalFileKeyringStore, SecretName, SecretScope, SecretsBackendKind, SecretsManager,
};
use octopus_world_pyecho_life_runtime::LedgerTailReader;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::multipart::{Form, Part};
use reqwest::{Client, Response, Url};
use serde::Deserialize;
use serde_json::{json, Map, Value};

const TRANSCRIPTION_RESULT_LABEL: &str = "speech.transcribe.result";
const SYNTHESIS_RESULT_LABEL: &str = "speech.synthesize.result";
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_MAX_JSON_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_MAX_AUDIO_BYTES: usize = 32 * 1024 * 1024;
const DEFAULT_MAX_TRANSCRIPT_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_TEXT_BYTES: usize = 64 * 1024;
const ABSOLUTE_MAX_BYTES: usize = 512 * 1024 * 1024;

#[derive(Clone, Copy)]
enum SpeechMode {
    Transcribe,
    Synthesize,
}

#[derive(Clone)]
struct SpeechProcess {
    transport: SpeechTransport,
    defaults: SpeechDefaults,
    max_manifest_bytes: u64,
    client: Client,
}

#[derive(Clone)]
struct SpeechTransport {
    endpoint: Url,
    headers: HeaderMap,
    auth: Option<SpeechAuth>,
    max_response_bytes: usize,
}

#[derive(Clone)]
struct SpeechAuth {
    header: HeaderName,
    value_prefix: String,
    source: SpeechAuthSource,
}

#[derive(Clone)]
enum SpeechAuthSource {
    Environment(String),
    Secret {
        octopus_home: PathBuf,
        scope: SecretScope,
        name: SecretName,
    },
}

#[derive(Clone)]
struct SpeechDefaults {
    model: String,
    voice: Option<String>,
    format: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TranscriptionArguments {
    audio_artifact_id: String,
    filename: String,
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SynthesisArguments {
    text: String,
    #[serde(default)]
    voice: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    speed: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TranscriptionOperation {
    file_field: String,
    model_field: String,
    language_field: String,
    prompt_field: String,
    response_format_field: String,
    response_format: String,
    transcript_path: String,
    language_path: String,
    duration_path: String,
    transcript_label: String,
    #[serde(default)]
    max_audio_bytes: Option<u64>,
    #[serde(default)]
    max_transcript_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SynthesisOperation {
    text_field: String,
    model_field: String,
    voice_field: String,
    format_field: String,
    speed_field: String,
    audio_label: String,
    expected_content_types: Vec<String>,
    #[serde(default)]
    max_text_bytes: Option<u64>,
    #[serde(default)]
    max_audio_bytes: Option<u64>,
}

enum SpeechInterfaceOperation {
    Transcribe {
        input_audio_label: String,
        input_request_label: String,
        operation: TranscriptionOperation,
    },
    Synthesize {
        input_request_label: String,
        operation: SynthesisOperation,
    },
}

struct TranscriptionOutput {
    text: String,
    language: Option<String>,
    duration: Option<f64>,
    model: String,
    response: Value,
    endpoint_status: u16,
}

struct SynthesisOutput {
    bytes: Vec<u8>,
    content_type: String,
    model: String,
    voice: String,
    format: String,
    endpoint_status: u16,
}

#[derive(Debug)]
struct SpeechCallFailure {
    class: &'static str,
    detail: String,
    endpoint_status: Option<u16>,
}

impl SpeechCallFailure {
    fn contract(detail: impl Into<String>) -> Self {
        Self {
            class: "contract",
            detail: detail.into(),
            endpoint_status: None,
        }
    }

    fn transport(detail: impl Into<String>) -> Self {
        Self {
            class: "transport",
            detail: detail.into(),
            endpoint_status: None,
        }
    }

    fn endpoint(status: u16, detail: impl Into<String>) -> Self {
        Self {
            class: "endpoint",
            detail: detail.into(),
            endpoint_status: Some(status),
        }
    }
}

impl SpeechAuth {
    fn header_value(&self) -> Result<HeaderValue, SpeechCallFailure> {
        let secret = match &self.source {
            SpeechAuthSource::Environment(name) => std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    SpeechCallFailure::contract(format!(
                        "speech interface auth env var '{name}' is unset or empty"
                    ))
                })?,
            SpeechAuthSource::Secret {
                octopus_home,
                scope,
                name,
            } => SecretsManager::new_with_keyring_store(
                octopus_home.clone(),
                SecretsBackendKind::Local,
                Arc::new(LocalFileKeyringStore::new(
                    octopus_home.join("secrets").join("keyring"),
                )),
            )
            .get(scope, name)
            .map_err(|error| {
                SpeechCallFailure::contract(format!(
                    "speech interface secret '{scope_name}/{name}' could not be loaded: {error}",
                    scope_name = scope.name().unwrap_or("global")
                ))
            })?
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                SpeechCallFailure::contract(format!(
                    "speech interface secret '{scope_name}/{name}' is not configured",
                    scope_name = scope.name().unwrap_or("global")
                ))
            })?,
        };
        HeaderValue::from_str(&format!("{}{secret}", self.value_prefix)).map_err(|_| {
            SpeechCallFailure::contract("speech interface secret cannot form a valid header value")
        })
    }
}

impl SpeechProcess {
    async fn transcribe(
        &self,
        operation: &TranscriptionOperation,
        arguments: &TranscriptionArguments,
        audio: Vec<u8>,
    ) -> Result<TranscriptionOutput, SpeechCallFailure> {
        let filename = safe_filename(&arguments.filename).map_err(SpeechCallFailure::contract)?;
        let model = self.defaults.model.as_str();
        let mut part = Part::bytes(audio).file_name(filename);
        if let Some(media_type) = optional_trimmed(arguments.media_type.as_deref(), "media_type")
            .map_err(SpeechCallFailure::contract)?
        {
            part = part.mime_str(media_type).map_err(|error| {
                SpeechCallFailure::contract(format!(
                    "media_type '{media_type}' is invalid: {error}"
                ))
            })?;
        }
        let mut form = Form::new()
            .part(operation.file_field.trim().to_string(), part)
            .text(operation.model_field.trim().to_string(), model.to_string())
            .text(
                operation.response_format_field.trim().to_string(),
                operation.response_format.trim().to_string(),
            );
        if let Some(language) = optional_trimmed(arguments.language.as_deref(), "language")
            .map_err(SpeechCallFailure::contract)?
        {
            form = form.text(
                operation.language_field.trim().to_string(),
                language.to_string(),
            );
        }
        if let Some(prompt) = optional_trimmed(arguments.prompt.as_deref(), "prompt")
            .map_err(SpeechCallFailure::contract)?
        {
            form = form.text(
                operation.prompt_field.trim().to_string(),
                prompt.to_string(),
            );
        }
        let mut request = self
            .client
            .post(self.transport.endpoint.clone())
            .headers(self.transport.headers.clone())
            .multipart(form);
        if let Some(auth) = &self.transport.auth {
            request = request.header(auth.header.clone(), auth.header_value()?);
        }
        let response = request.send().await.map_err(|error| {
            SpeechCallFailure::transport(format!("speech-to-text request failed: {error}"))
        })?;
        let status = response.status().as_u16();
        let bytes = read_bounded(
            response,
            self.transport.max_response_bytes,
            "speech-to-text response",
        )
        .await?;
        if !(200..300).contains(&status) {
            return Err(SpeechCallFailure::endpoint(
                status,
                format!(
                    "speech-to-text endpoint refused the request ({})",
                    body_evidence(&bytes)
                ),
            ));
        }
        let response: Value = serde_json::from_slice(&bytes).map_err(|error| {
            SpeechCallFailure::contract(format!(
                "speech-to-text response is not valid JSON: {error}"
            ))
        })?;
        let text = dotted_value(&response, operation.transcript_path.trim())
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                SpeechCallFailure::contract(format!(
                    "speech-to-text response has no non-empty string at '{}'",
                    operation.transcript_path.trim()
                ))
            })?;
        let max_transcript_bytes = checked_limit(
            operation.max_transcript_bytes,
            DEFAULT_MAX_TRANSCRIPT_BYTES,
            "interface private_config.max_transcript_bytes",
        )
        .map_err(SpeechCallFailure::contract)?;
        if text.len() > max_transcript_bytes {
            return Err(SpeechCallFailure::contract(format!(
                "transcript byte count {} exceeds {max_transcript_bytes}",
                text.len()
            )));
        }
        let language = optional_string_path(&response, operation.language_path.trim())?;
        let duration = optional_number_path(&response, operation.duration_path.trim())?;
        Ok(TranscriptionOutput {
            text: text.to_string(),
            language,
            duration,
            model: model.to_string(),
            response,
            endpoint_status: status,
        })
    }

    async fn synthesize(
        &self,
        operation: &SynthesisOperation,
        arguments: &SynthesisArguments,
    ) -> Result<SynthesisOutput, SpeechCallFailure> {
        let text = arguments.text.trim();
        if text.is_empty() {
            return Err(SpeechCallFailure::contract(
                "speech synthesis text must not be blank",
            ));
        }
        let max_text_bytes = checked_limit(
            operation.max_text_bytes,
            DEFAULT_MAX_TEXT_BYTES,
            "interface private_config.max_text_bytes",
        )
        .map_err(SpeechCallFailure::contract)?;
        if text.len() > max_text_bytes {
            return Err(SpeechCallFailure::contract(format!(
                "speech synthesis text byte count {} exceeds {max_text_bytes}",
                text.len()
            )));
        }
        let model = self.defaults.model.as_str();
        let voice = selected_required(
            arguments.voice.as_deref(),
            self.defaults.voice.as_deref().unwrap_or_default(),
            "voice",
        )
        .map_err(SpeechCallFailure::contract)?;
        let format = selected_required(
            arguments.format.as_deref(),
            self.defaults.format.as_deref().unwrap_or_default(),
            "format",
        )
        .map_err(SpeechCallFailure::contract)?;
        let mut body = Map::new();
        insert_field(
            &mut body,
            operation.text_field.trim(),
            Value::String(text.to_string()),
        )?;
        insert_field(
            &mut body,
            operation.model_field.trim(),
            Value::String(model.to_string()),
        )?;
        insert_field(
            &mut body,
            operation.voice_field.trim(),
            Value::String(voice.to_string()),
        )?;
        insert_field(
            &mut body,
            operation.format_field.trim(),
            Value::String(format.to_string()),
        )?;
        if let Some(speed) = arguments.speed {
            if !speed.is_finite() || speed <= 0.0 {
                return Err(SpeechCallFailure::contract(
                    "speech synthesis speed must be finite and greater than zero",
                ));
            }
            let number = serde_json::Number::from_f64(speed).ok_or_else(|| {
                SpeechCallFailure::contract("speech synthesis speed cannot encode as JSON")
            })?;
            insert_field(
                &mut body,
                operation.speed_field.trim(),
                Value::Number(number),
            )?;
        }
        let mut request = self
            .client
            .post(self.transport.endpoint.clone())
            .headers(self.transport.headers.clone())
            .json(&Value::Object(body));
        if let Some(auth) = &self.transport.auth {
            request = request.header(auth.header.clone(), auth.header_value()?);
        }
        let response = request.send().await.map_err(|error| {
            SpeechCallFailure::transport(format!("text-to-speech request failed: {error}"))
        })?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| {
                value
                    .split(';')
                    .next()
                    .unwrap_or(value)
                    .trim()
                    .to_ascii_lowercase()
            })
            .unwrap_or_default();
        let max_audio_bytes = checked_limit(
            operation.max_audio_bytes,
            DEFAULT_MAX_AUDIO_BYTES,
            "interface private_config.max_audio_bytes",
        )
        .map_err(SpeechCallFailure::contract)?;
        let bytes = read_bounded(
            response,
            max_audio_bytes.min(self.transport.max_response_bytes),
            "text-to-speech response",
        )
        .await?;
        if !(200..300).contains(&status) {
            return Err(SpeechCallFailure::endpoint(
                status,
                format!(
                    "text-to-speech endpoint refused the request ({})",
                    body_evidence(&bytes)
                ),
            ));
        }
        if bytes.is_empty() {
            return Err(SpeechCallFailure::contract(
                "text-to-speech endpoint returned zero audio bytes",
            ));
        }
        if !operation
            .expected_content_types
            .iter()
            .any(|expected| expected == &content_type)
        {
            return Err(SpeechCallFailure::contract(format!(
                "text-to-speech content type '{content_type}' is not one of {:?}",
                operation.expected_content_types
            )));
        }
        Ok(SynthesisOutput {
            bytes,
            content_type,
            model: model.to_string(),
            voice: voice.to_string(),
            format: format.to_string(),
            endpoint_status: status,
        })
    }
}

pub fn interface_life() -> InterfaceLiftFn {
    transcription_interface_life()
}

pub fn transcription_interface_life() -> InterfaceLiftFn {
    speech_interface_life(SpeechMode::Transcribe)
}

pub fn synthesis_interface_life() -> InterfaceLiftFn {
    speech_interface_life(SpeechMode::Synthesize)
}

fn speech_interface_life(mode: SpeechMode) -> InterfaceLiftFn {
    Arc::new(move |ctx| Box::pin(run_speech_interface_life(ctx, mode)) as InterfaceLifeFuture)
}

async fn run_speech_interface_life(
    ctx: InterfaceLiftContext,
    mode: SpeechMode,
) -> Result<(), String> {
    let descriptor = ctx.resolved_interface_descriptor()?;
    let carrier_limits = ctx.required_host_io_limits()?;
    let max_manifest_bytes = carrier_limits.max_manifest_bytes;
    let config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
        format!(
            "speech interface '{}' resolves no driver config",
            ctx.interface
        )
    })?;
    let operation = match mode {
        SpeechMode::Transcribe => SpeechInterfaceOperation::Transcribe {
            input_audio_label: metadata_string(config, &ctx.interface, "input_audio_label")?,
            input_request_label: metadata_string(config, &ctx.interface, "input_request_label")?,
            operation: parse_transcription_operation(config, &ctx.interface)?,
        },
        SpeechMode::Synthesize => SpeechInterfaceOperation::Synthesize {
            input_request_label: metadata_string(config, &ctx.interface, "input_request_label")?,
            operation: parse_synthesis_operation(config, &ctx.interface)?,
        },
    };
    let octopus_home = match mode {
        SpeechMode::Transcribe => Some(ctx.root_slot("activation_secrets_home")?.to_path_buf()),
        SpeechMode::Synthesize => None,
    };
    let expected_wire = match mode {
        SpeechMode::Transcribe => "speech_multipart_http",
        SpeechMode::Synthesize => "speech_json_http",
    };
    let (transport, defaults, timeout) = load_interface_config(
        descriptor,
        &ctx.interface,
        mode,
        expected_wire,
        octopus_home,
    )?;
    let client = Client::builder()
        .no_proxy()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("speech interface HTTP client build failed: {error}"))?;
    let process = SpeechProcess {
        transport,
        defaults,
        max_manifest_bytes,
        client,
    };
    let reader_name = match mode {
        SpeechMode::Transcribe => "speech-transcription",
        SpeechMode::Synthesize => "speech-synthesis",
    };
    let mut reader = LedgerTailReader::open_named(&ctx, reader_name)?;
    ctx.signal_ready()?;
    loop {
        if ctx.cancel.is_cancelled() {
            return Ok(());
        }
        let batch = reader.read_new()?;
        if batch.is_empty() {
            reader.wait_for_change(&ctx.cancel).await?;
            continue;
        }
        for own in batch {
            let envelope = match reader.envelope(&own) {
                Ok(envelope) => envelope,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "poisoned_speech_input",
                        json!({ "error": error }),
                    )
                    .await?;
                    reader.commit_through(&own)?;
                    continue;
                }
            };
            let manifest = match envelope
                .artifact_file
                .read_manifest_bounded(max_manifest_bytes)
            {
                Ok(manifest) => manifest,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "poisoned_speech_manifest",
                        json!({ "error": error.to_string() }),
                    )
                    .await?;
                    reader.commit_through(&own)?;
                    continue;
                }
            };
            let label = manifest.manifest.label.as_str();
            let accepted = match &operation {
                SpeechInterfaceOperation::Transcribe {
                    input_audio_label,
                    input_request_label,
                    ..
                } => label == input_audio_label || label == input_request_label,
                SpeechInterfaceOperation::Synthesize {
                    input_request_label,
                    ..
                } => label == input_request_label,
            };
            if !accepted {
                reader.commit_through(&own)?;
                continue;
            }
            if let Err(error) = process_speech_artifact(
                &ctx,
                &process,
                &operation,
                &envelope,
                own.movement.seq.0,
                carrier_limits.max_control_frame_bytes,
            )
            .await
            {
                ctx.hand_admitted_failure(
                    own.movement.seq.0,
                    "poisoned_speech_input",
                    json!({ "error": error }),
                )
                .await?;
            }
            reader.commit_through(&own)?;
        }
    }
}

async fn process_speech_artifact(
    ctx: &InterfaceLiftContext,
    process: &SpeechProcess,
    interface_operation: &SpeechInterfaceOperation,
    envelope: &InterfaceInvocationEnvelope,
    source_movement_seq: u64,
    max_request_bytes: u64,
) -> Result<(), String> {
    match interface_operation {
        SpeechInterfaceOperation::Transcribe {
            input_audio_label,
            input_request_label,
            operation,
        } => {
            let manifest = envelope
                .read_artifact_manifest_bounded(process.max_manifest_bytes)
                .map_err(|error| error.to_string())?;
            let (arguments, audio) = if manifest.manifest.label == *input_audio_label {
                let max_audio_bytes = checked_limit(
                    operation.max_audio_bytes,
                    DEFAULT_MAX_AUDIO_BYTES,
                    "interface max_audio_bytes",
                )?;
                let artifact = envelope
                    .read_artifact_bounded(
                        process.max_manifest_bytes,
                        u64::try_from(max_audio_bytes)
                            .map_err(|_| "interface max_audio_bytes exceeds u64".to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                if artifact.payload_bytes.is_empty() {
                    return Err("speech transcription audio must not be empty".to_string());
                }
                let filename = artifact
                    .manifest
                    .payload_path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or("dictation.webm")
                    .to_string();
                (
                    TranscriptionArguments {
                        audio_artifact_id: envelope.artifact.id.as_str().to_string(),
                        filename: filename.clone(),
                        media_type: media_type_from_filename(&filename).map(str::to_string),
                        language: None,
                        prompt: None,
                    },
                    artifact.payload_bytes,
                )
            } else if manifest.manifest.label == *input_request_label {
                let payload = envelope
                    .read_artifact_payload_bounded(max_request_bytes)
                    .map_err(|error| error.to_string())?;
                let arguments: TranscriptionArguments =
                    serde_json::from_slice(&payload).map_err(|error| {
                        format!("speech transcription request is malformed: {error}")
                    })?;
                let bound = bound_artifact(envelope, arguments.audio_artifact_id.trim())?;
                let max_audio_bytes = checked_limit(
                    operation.max_audio_bytes,
                    DEFAULT_MAX_AUDIO_BYTES,
                    "interface max_audio_bytes",
                )?;
                let audio = bound
                    .read_bounded(
                        u64::try_from(max_audio_bytes)
                            .map_err(|_| "interface max_audio_bytes exceeds u64".to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                if audio.is_empty() || audio.len() > max_audio_bytes {
                    return Err(format!(
                        "bound audio artifact '{}' byte count {} is outside 1..={max_audio_bytes}",
                        arguments.audio_artifact_id,
                        audio.len()
                    ));
                }
                (arguments, audio.into_owned())
            } else {
                return Ok(());
            };
            let source_audio_artifact_id = arguments.audio_artifact_id.clone();
            match process.transcribe(operation, &arguments, audio).await {
                Ok(transcription) => {
                    let transcript_payload = json!({
                        "text": transcription.text,
                        "language": transcription.language,
                        "duration": transcription.duration,
                        "model": transcription.model,
                        "source_interface": ctx.interface.as_str(),
                        "source_audio_artifact_id": source_audio_artifact_id,
                        "source_movement_seq": source_movement_seq,
                    });
                    let transcript = materialize_json(
                        &ctx.interface_var_dir().join("emitted"),
                        operation.transcript_label.trim(),
                        &transcript_payload,
                    )?;
                    let transcript_ref =
                        artifact_identity(&transcript, process.max_manifest_bytes)?;
                    let result = materialize_json(
                        &ctx.interface_var_dir().join("emitted"),
                        TRANSCRIPTION_RESULT_LABEL,
                        &json!({
                            "interface": ctx.interface.as_str(),
                            "ok": true,
                            "source_audio_artifact_id": source_audio_artifact_id,
                            "source_movement_seq": source_movement_seq,
                            "endpoint_status": transcription.endpoint_status,
                            "model": transcription.model,
                            "transcript_bytes": transcript_payload["text"].as_str().map(str::len),
                            "artifact": transcript_ref,
                            "provider_response": transcription.response,
                        }),
                    )?;
                    emit_speech_artifacts(ctx, vec![transcript, result]).await
                }
                Err(failure) => {
                    let result = materialize_json(
                        &ctx.interface_var_dir().join("emitted"),
                        TRANSCRIPTION_RESULT_LABEL,
                        &json!({
                            "interface": ctx.interface.as_str(),
                            "ok": false,
                            "source_audio_artifact_id": source_audio_artifact_id,
                            "source_movement_seq": source_movement_seq,
                            "error": {
                                "class": failure.class,
                                "detail": failure.detail,
                                "endpoint_status": failure.endpoint_status,
                            }
                        }),
                    )?;
                    emit_speech_artifacts(ctx, vec![result]).await
                }
            }
        }
        SpeechInterfaceOperation::Synthesize {
            input_request_label,
            operation,
        } => {
            let artifact = envelope
                .read_artifact_bounded(process.max_manifest_bytes, max_request_bytes)
                .map_err(|error| error.to_string())?;
            if artifact.manifest.label != *input_request_label {
                return Ok(());
            }
            let arguments: SynthesisArguments = serde_json::from_slice(&artifact.payload_bytes)
                .map_err(|error| format!("speech synthesis request is malformed: {error}"))?;
            match process.synthesize(operation, &arguments).await {
                Ok(synthesis) => {
                    let audio = InterfaceArtifactFile::write(
                        ctx.interface_var_dir().join("emitted"),
                        operation.audio_label.trim(),
                        &synthesis.bytes,
                    )
                    .map_err(|error| {
                        format!("speech interface could not materialize audio artifact: {error}")
                    })?;
                    let audio_ref = artifact_identity(&audio, process.max_manifest_bytes)?;
                    let result = materialize_json(
                        &ctx.interface_var_dir().join("emitted"),
                        SYNTHESIS_RESULT_LABEL,
                        &json!({
                            "interface": ctx.interface.as_str(),
                            "ok": true,
                            "source_movement_seq": source_movement_seq,
                            "endpoint_status": synthesis.endpoint_status,
                            "byte_count": synthesis.bytes.len(),
                            "content_type": synthesis.content_type,
                            "model": synthesis.model,
                            "format": synthesis.format,
                            "voice": synthesis.voice,
                            "artifact": audio_ref,
                        }),
                    )?;
                    emit_speech_artifacts(ctx, vec![audio, result]).await
                }
                Err(failure) => {
                    let result = materialize_json(
                        &ctx.interface_var_dir().join("emitted"),
                        SYNTHESIS_RESULT_LABEL,
                        &json!({
                            "interface": ctx.interface.as_str(),
                            "ok": false,
                            "source_movement_seq": source_movement_seq,
                            "error": {
                                "class": failure.class,
                                "detail": failure.detail,
                                "endpoint_status": failure.endpoint_status,
                            }
                        }),
                    )?;
                    emit_speech_artifacts(ctx, vec![result]).await
                }
            }
        }
    }
}

async fn emit_speech_artifacts(
    ctx: &InterfaceLiftContext,
    artifacts: Vec<InterfaceArtifactFile>,
) -> Result<(), String> {
    for artifact in artifacts {
        ctx.hand_admitted_arrival(artifact, Vec::new()).await?;
    }
    Ok(())
}

fn media_type_from_filename(filename: &str) -> Option<&'static str> {
    match Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("webm") => Some("audio/webm"),
        Some("wav") => Some("audio/wav"),
        Some("mp3") => Some("audio/mpeg"),
        Some("m4a") => Some("audio/mp4"),
        Some("ogg") | Some("oga") => Some("audio/ogg"),
        Some("flac") => Some("audio/flac"),
        _ => None,
    }
}

fn bound_artifact<'a>(
    envelope: &'a InterfaceInvocationEnvelope,
    id: &str,
) -> Result<&'a ArtifactBindingPayload, String> {
    require_trimmed("audio_artifact_id", id)?;
    let mut matches = envelope
        .artifact_binding_payloads
        .iter()
        .filter(|binding| binding.artifact.id.as_str() == id);
    let artifact = matches
        .next()
        .ok_or_else(|| format!("audio artifact '{id}' is not bound to this run"))?;
    if matches.next().is_some() {
        return Err(format!("audio artifact id '{id}' is bound more than once"));
    }
    Ok(artifact)
}

fn parse_transcription_operation(
    config: &InterfaceDriverConfig,
    interface: &InterfaceId,
) -> Result<TranscriptionOperation, String> {
    let operation = TranscriptionOperation {
        file_field: metadata_string(config, interface, "file_field")?,
        model_field: metadata_string(config, interface, "model_field")?,
        language_field: metadata_string(config, interface, "language_field")?,
        prompt_field: metadata_string(config, interface, "prompt_field")?,
        response_format_field: metadata_string(config, interface, "response_format_field")?,
        response_format: metadata_string(config, interface, "response_format")?,
        transcript_path: metadata_string(config, interface, "transcript_path")?,
        language_path: metadata_string(config, interface, "language_path")?,
        duration_path: metadata_string(config, interface, "duration_path")?,
        transcript_label: metadata_string(config, interface, "transcript_label")?,
        max_audio_bytes: Some(metadata_u64(config, interface, "max_audio_bytes")?),
        max_transcript_bytes: Some(metadata_u64(config, interface, "max_transcript_bytes")?),
    };
    for (name, value) in [
        ("file_field", operation.file_field.as_str()),
        ("model_field", operation.model_field.as_str()),
        ("language_field", operation.language_field.as_str()),
        ("prompt_field", operation.prompt_field.as_str()),
        (
            "response_format_field",
            operation.response_format_field.as_str(),
        ),
        ("response_format", operation.response_format.as_str()),
        ("transcript_path", operation.transcript_path.as_str()),
        ("transcript_label", operation.transcript_label.as_str()),
    ] {
        require_trimmed(name, value)?;
    }
    require_optional_path("language_path", &operation.language_path)?;
    require_optional_path("duration_path", &operation.duration_path)?;
    unique_fields([
        operation.file_field.as_str(),
        operation.model_field.as_str(),
        operation.language_field.as_str(),
        operation.prompt_field.as_str(),
        operation.response_format_field.as_str(),
    ])?;
    checked_limit(
        operation.max_audio_bytes,
        DEFAULT_MAX_AUDIO_BYTES,
        "interface private_config.max_audio_bytes",
    )?;
    checked_limit(
        operation.max_transcript_bytes,
        DEFAULT_MAX_TRANSCRIPT_BYTES,
        "interface private_config.max_transcript_bytes",
    )?;
    Ok(operation)
}

fn parse_synthesis_operation(
    config: &InterfaceDriverConfig,
    interface: &InterfaceId,
) -> Result<SynthesisOperation, String> {
    let mut operation = SynthesisOperation {
        text_field: metadata_string(config, interface, "text_field")?,
        model_field: metadata_string(config, interface, "model_field")?,
        voice_field: metadata_string(config, interface, "voice_field")?,
        format_field: metadata_string(config, interface, "format_field")?,
        speed_field: metadata_string(config, interface, "speed_field")?,
        audio_label: metadata_string(config, interface, "audio_label")?,
        expected_content_types: metadata_string_array(config, interface, "expected_content_types")?,
        max_text_bytes: Some(metadata_u64(config, interface, "max_text_bytes")?),
        max_audio_bytes: Some(metadata_u64(config, interface, "max_audio_bytes")?),
    };
    for (name, value) in [
        ("text_field", operation.text_field.as_str()),
        ("model_field", operation.model_field.as_str()),
        ("voice_field", operation.voice_field.as_str()),
        ("format_field", operation.format_field.as_str()),
        ("speed_field", operation.speed_field.as_str()),
        ("audio_label", operation.audio_label.as_str()),
    ] {
        require_trimmed(name, value)?;
    }
    unique_fields([
        operation.text_field.as_str(),
        operation.model_field.as_str(),
        operation.voice_field.as_str(),
        operation.format_field.as_str(),
        operation.speed_field.as_str(),
    ])?;
    if operation.expected_content_types.is_empty() {
        return Err(
            "interface private_config.expected_content_types must not be empty".to_string(),
        );
    }
    let mut content_types = BTreeSet::new();
    for content_type in std::mem::take(&mut operation.expected_content_types) {
        require_trimmed(
            "interface private_config.expected_content_types[]",
            &content_type,
        )?;
        let normalized = content_type.to_ascii_lowercase();
        if normalized.contains(';') || !normalized.contains('/') {
            return Err(format!(
                "interface private_config.expected_content_types contains invalid media type '{content_type}'"
            ));
        }
        if !content_types.insert(normalized.clone()) {
            return Err(format!(
                "interface private_config.expected_content_types repeats '{normalized}'"
            ));
        }
    }
    operation.expected_content_types = content_types.into_iter().collect();
    checked_limit(
        operation.max_text_bytes,
        DEFAULT_MAX_TEXT_BYTES,
        "interface private_config.max_text_bytes",
    )?;
    checked_limit(
        operation.max_audio_bytes,
        DEFAULT_MAX_AUDIO_BYTES,
        "interface private_config.max_audio_bytes",
    )?;
    Ok(operation)
}

fn load_interface_config(
    descriptor: InterfaceDescriptor,
    interface: &InterfaceId,
    mode: SpeechMode,
    expected_wire: &str,
    octopus_home: Option<PathBuf>,
) -> Result<(SpeechTransport, SpeechDefaults, Duration), String> {
    let config = resolved_driver_config(descriptor, interface)?;
    let defaults = load_defaults(&config, mode, interface)?;
    let transport = config.transport.ok_or_else(|| {
        format!(
            "speech interface '{}' resolves no interface_driver_config.transport",
            interface
        )
    })?;
    if transport.wire.as_deref() != Some(expected_wire) {
        return Err(format!(
            "speech interface '{}' transport.wire must be '{expected_wire}'",
            interface
        ));
    }
    let endpoint = resolve_endpoint(&transport)?;
    let max_response_bytes = checked_limit(
        transport.max_response_bytes,
        DEFAULT_MAX_JSON_RESPONSE_BYTES,
        "interface transport.max_response_bytes",
    )?;
    let timeout_ms = transport
        .request_timeout_ms
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
    if timeout_ms == 0 || timeout_ms > 30 * 60 * 1000 {
        return Err(format!(
            "speech interface '{}' request_timeout_ms must be within 1..=1800000",
            interface
        ));
    }
    let mut headers = HeaderMap::new();
    for (name, value) in &transport.headers {
        let name = parse_header_name(name, "interface transport header")?;
        reject_secret_header(&name)?;
        let value = HeaderValue::from_str(value)
            .map_err(|error| format!("interface transport header value is invalid: {error}"))?;
        headers.insert(name, value);
    }
    let auth = resolve_auth(&transport, octopus_home)?;
    Ok((
        SpeechTransport {
            endpoint,
            headers,
            auth,
            max_response_bytes,
        },
        defaults,
        Duration::from_millis(timeout_ms),
    ))
}

fn resolved_driver_config(
    resolved: InterfaceDescriptor,
    interface: &InterfaceId,
) -> Result<InterfaceDriverConfig, String> {
    if resolved.id != *interface {
        return Err(format!(
            "resolved speech interface '{}' does not match process interface '{}'",
            resolved.id, interface
        ));
    }
    resolved.interface_driver_config.ok_or_else(|| {
        format!(
            "speech interface '{}' resolves no interface_driver_config",
            interface
        )
    })
}

fn load_defaults(
    config: &InterfaceDriverConfig,
    mode: SpeechMode,
    interface: &InterfaceId,
) -> Result<SpeechDefaults, String> {
    let model = metadata_string(config, interface, "model")?;
    let (voice, format) = match mode {
        SpeechMode::Transcribe => (None, None),
        SpeechMode::Synthesize => (
            Some(metadata_string(config, interface, "voice")?),
            Some(metadata_string(config, interface, "format")?),
        ),
    };
    Ok(SpeechDefaults {
        model,
        voice,
        format,
    })
}

fn metadata_string(
    config: &InterfaceDriverConfig,
    interface: &InterfaceId,
    key: &str,
) -> Result<String, String> {
    let value = config
        .private_config
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!(
                "speech interface '{}' private_config.{key} must be a string",
                interface
            )
        })?;
    require_trimmed(&format!("interface private_config.{key}"), value)?;
    Ok(value.to_string())
}

fn metadata_u64(
    config: &InterfaceDriverConfig,
    interface: &InterfaceId,
    key: &str,
) -> Result<u64, String> {
    config
        .private_config
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "speech interface '{}' private_config.{key} must be an unsigned integer",
                interface
            )
        })
}

fn metadata_string_array(
    config: &InterfaceDriverConfig,
    interface: &InterfaceId,
    key: &str,
) -> Result<Vec<String>, String> {
    let values = config
        .private_config
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "speech interface '{}' private_config.{key} must be an array",
                interface
            )
        })?;
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let value = value.as_str().ok_or_else(|| {
                format!(
                    "speech interface '{}' private_config.{key}[{index}] must be a string",
                    interface
                )
            })?;
            require_trimmed(&format!("interface private_config.{key}[{index}]"), value)?;
            Ok(value.to_string())
        })
        .collect()
}

fn resolve_endpoint(transport: &InterfaceTransportConfig) -> Result<Url, String> {
    let configured = transport.base_url.trim();
    let endpoint = if !configured.is_empty() {
        configured.to_string()
    } else {
        let env_key = transport
            .base_url_env
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                "speech interface transport has neither base_url nor base_url_env".to_string()
            })?;
        std::env::var(env_key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                format!("speech interface endpoint env var '{env_key}' is unset or empty")
            })?
    };
    parse_http_url(&endpoint, "speech interface endpoint")
}

fn resolve_auth(
    transport: &InterfaceTransportConfig,
    octopus_home: Option<PathBuf>,
) -> Result<Option<SpeechAuth>, String> {
    let header = parse_header_name(
        transport.auth_header.as_deref().unwrap_or("Authorization"),
        "speech interface auth header",
    )?;
    let value_prefix = transport
        .auth_value_prefix
        .as_deref()
        .unwrap_or("Bearer ")
        .to_string();
    if let (Some(reference), Some(octopus_home)) = (
        transport
            .auth_secret_ref
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        octopus_home,
    ) {
        let (scope, name) = parse_secret_ref(reference)?;
        return Ok(Some(SpeechAuth {
            header,
            value_prefix,
            source: SpeechAuthSource::Secret {
                octopus_home,
                scope,
                name,
            },
        }));
    }
    if let Some(env_key) = transport
        .auth_env_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(Some(SpeechAuth {
            header,
            value_prefix,
            source: SpeechAuthSource::Environment(env_key.to_string()),
        }));
    }
    if transport
        .auth_secret_ref
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Err(
            "speech interface secret authentication requires an Octopus Home handle".to_string(),
        );
    }
    Ok(None)
}

fn parse_secret_ref(reference: &str) -> Result<(SecretScope, SecretName), String> {
    let path = reference
        .strip_prefix("secret://")
        .ok_or_else(|| "speech interface auth_secret_ref must start with secret://".to_string())?;
    let (scope, name) = path.split_once('/').ok_or_else(|| {
        "speech interface auth_secret_ref must be secret://<scope>/<name>".to_string()
    })?;
    if name.contains('/') {
        return Err("speech interface auth_secret_ref must name one scoped secret".to_string());
    }
    Ok((
        SecretScope::named(scope.to_string()).map_err(|error| error.to_string())?,
        SecretName::new(name).map_err(|error| error.to_string())?,
    ))
}

async fn read_bounded(
    mut response: Response,
    max_bytes: usize,
    surface: &str,
) -> Result<Vec<u8>, SpeechCallFailure> {
    if let Some(length) = response.content_length() {
        if length > max_bytes as u64 {
            return Err(SpeechCallFailure::contract(format!(
                "{surface} content-length {length} exceeds {max_bytes} bytes"
            )));
        }
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        SpeechCallFailure::transport(format!("{surface} body read failed: {error}"))
    })? {
        let next = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| SpeechCallFailure::contract(format!("{surface} size overflowed")))?;
        if next > max_bytes {
            return Err(SpeechCallFailure::contract(format!(
                "{surface} exceeds {max_bytes} bytes"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn insert_field(
    body: &mut Map<String, Value>,
    name: &str,
    value: Value,
) -> Result<(), SpeechCallFailure> {
    if name.is_empty() {
        return Err(SpeechCallFailure::contract(
            "speech request field name must not be empty",
        ));
    }
    if body.insert(name.to_string(), value).is_some() {
        return Err(SpeechCallFailure::contract(format!(
            "speech request field '{name}' is declared more than once"
        )));
    }
    Ok(())
}

fn unique_fields<'a>(fields: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    let mut unique = BTreeSet::new();
    for field in fields {
        if !unique.insert(field) {
            return Err(format!(
                "speech request field '{field}' is declared more than once"
            ));
        }
    }
    Ok(())
}

fn optional_string_path(response: &Value, path: &str) -> Result<Option<String>, SpeechCallFailure> {
    if path.is_empty() {
        return Ok(None);
    }
    let Some(value) = dotted_value(response, path) else {
        return Ok(None);
    };
    let value = value.as_str().ok_or_else(|| {
        SpeechCallFailure::contract(format!("speech response field '{path}' is not a string"))
    })?;
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    Ok(Some(value.to_string()))
}

fn optional_number_path(response: &Value, path: &str) -> Result<Option<f64>, SpeechCallFailure> {
    if path.is_empty() {
        return Ok(None);
    }
    let Some(value) = dotted_value(response, path) else {
        return Ok(None);
    };
    let value = value.as_f64().ok_or_else(|| {
        SpeechCallFailure::contract(format!("speech response field '{path}' is not a number"))
    })?;
    if !value.is_finite() || value < 0.0 {
        return Err(SpeechCallFailure::contract(format!(
            "speech response field '{path}' must be finite and non-negative"
        )));
    }
    Ok(Some(value))
}

fn dotted_value<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return None;
    }
    let mut current = value;
    for part in path.split('.') {
        if part.is_empty() {
            return None;
        }
        current = current.get(part)?;
    }
    Some(current)
}

fn selected_required<'a>(
    override_value: Option<&'a str>,
    default: &'a str,
    surface: &str,
) -> Result<&'a str, String> {
    let value = override_value.unwrap_or(default);
    require_trimmed(surface, value)?;
    Ok(value)
}

fn optional_trimmed<'a>(value: Option<&'a str>, surface: &str) -> Result<Option<&'a str>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    require_trimmed(surface, value)?;
    Ok(Some(value))
}

fn safe_filename(value: &str) -> Result<String, String> {
    require_trimmed("filename", value)?;
    if value.len() > 255 || value.contains('/') || value.contains('\\') {
        return Err("filename must be a single path-free component up to 255 bytes".to_string());
    }
    let path = Path::new(value);
    if path.file_name().and_then(|name| name.to_str()) != Some(value)
        || value == "."
        || value == ".."
    {
        return Err("filename must be a single safe UTF-8 component".to_string());
    }
    Ok(value.to_string())
}

fn checked_limit(value: Option<u64>, default: usize, surface: &str) -> Result<usize, String> {
    let value = value.unwrap_or(default as u64);
    let value = usize::try_from(value).map_err(|_| format!("{surface} exceeds usize"))?;
    if value == 0 || value > ABSOLUTE_MAX_BYTES {
        return Err(format!("{surface} must be within 1..={ABSOLUTE_MAX_BYTES}"));
    }
    Ok(value)
}

fn require_trimmed(surface: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.trim() != value {
        return Err(format!("{surface} must be non-empty and trimmed"));
    }
    Ok(())
}

fn require_optional_path(surface: &str, value: &str) -> Result<(), String> {
    if value.trim() != value || value.split('.').any(str::is_empty) {
        return Err(format!("{surface} must be empty or a trimmed dotted path"));
    }
    Ok(())
}

fn parse_header_name(value: &str, surface: &str) -> Result<HeaderName, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{surface} must not be empty"));
    }
    HeaderName::from_bytes(value.as_bytes())
        .map_err(|error| format!("{surface} '{value}' is invalid: {error}"))
}

fn reject_secret_header(name: &HeaderName) -> Result<(), String> {
    if matches!(
        name.as_str(),
        "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
    ) {
        return Err(format!(
            "interface transport header '{}' must use a secret handle instead of tracked JSON",
            name.as_str()
        ));
    }
    Ok(())
}

fn parse_http_url(value: &str, surface: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|error| format!("{surface} is invalid: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("{surface} must use http or https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("{surface} must not carry credentials"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| format!("{surface} has no host"))?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if url.scheme() == "http" && !loopback {
        return Err(format!("{surface} requires https outside loopback"));
    }
    Ok(url)
}

fn materialize_json(
    root: &Path,
    label: &str,
    value: &Value,
) -> Result<InterfaceArtifactFile, String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("speech result artifact did not encode: {error}"))?;
    InterfaceArtifactFile::write(root, label, &bytes)
        .map_err(|error| format!("speech result artifact could not be written: {error}"))
}

fn artifact_identity(
    artifact: &InterfaceArtifactFile,
    max_manifest_bytes: u64,
) -> Result<Value, String> {
    let bounded = artifact
        .read_manifest_bounded(max_manifest_bytes)
        .map_err(|error| format!("speech artifact manifest is invalid: {error}"))?;
    let content = ContentHash::of_bytes(&bounded.manifest_bytes).to_hex();
    Ok(json!({
        "id": format!("{}:{content}", bounded.manifest.label),
        "content": content,
        "payload_content": bounded.manifest.payload_content_hash,
    }))
}

fn body_evidence(value: &[u8]) -> String {
    format!(
        "bytes={} blake3={}",
        value.len(),
        blake3::hash(value).to_hex()
    )
}

#[cfg(test)]
mod tests;
