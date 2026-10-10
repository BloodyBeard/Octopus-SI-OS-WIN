use std::sync::Arc;
use std::time::Duration;

use octopus_descriptors::RunId;
use octopus_interface_transport::{InterfaceLifeFuture, InterfaceLiftContext, InterfaceLiftFn};
use octopus_interfaces::{InterfaceArtifactFile, InterfaceDescriptor, InterfaceInvocationEnvelope};
use octopus_provider_run_authoring::{
    ProviderEmission, ProviderEmissionRoute, ProviderHandoffSession, ProviderOutputHandoff,
    ProviderSourceRecovery, RunAuthoringResultIdentity,
};
use octopus_world_provider_responses::{
    materialize_provider_turn_error, OpenAiResponsesDriver, ResponsesFailure,
};
use octopus_world_pyecho_life_runtime::LedgerTailReader;
use serde_json::Value;

use crate::auth::{OAuthCredentialManager, OAuthFailure};
use crate::dispatch::OAuthDispatchAdmission;

pub fn interface_life() -> InterfaceLiftFn {
    Arc::new(|ctx| Box::pin(provider_life(ctx)) as InterfaceLifeFuture)
}

struct QueuedTurn {
    envelope: InterfaceInvocationEnvelope,
    seq: u64,
    label: String,
}

async fn provider_life(ctx: InterfaceLiftContext) -> Result<(), String> {
    let descriptor = ctx.resolved_interface_descriptor()?;
    let secrets_home = ctx.root_slot("activation_secrets_home")?.to_path_buf();
    let credentials = OAuthCredentialManager::from_descriptor(&descriptor, &secrets_home)
        .map_err(|error| error.to_string())?;
    let driver = OpenAiResponsesDriver::from_descriptor(&descriptor, secrets_home)?;
    let authoring_result = RunAuthoringResultIdentity::from_descriptor(&descriptor)?;
    let handoff_session = ProviderHandoffSession::new(&ctx)?;
    let mut reader = LedgerTailReader::open(&ctx)?;
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
                        "poisoned_outbound",
                        serde_json::json!({"error": error}),
                    )
                    .await?;
                    reader.commit_through(&own)?;
                    continue;
                }
            };
            let manifest = match envelope.artifact_file.read_manifest() {
                Ok(manifest) => manifest,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "malformed_artifact_manifest",
                        serde_json::json!({"error": error.to_string()}),
                    )
                    .await?;
                    reader.commit_through(&own)?;
                    continue;
                }
            };
            let metadata = match own.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "malformed_runtime_envelope",
                        serde_json::json!({"error": error}),
                    )
                    .await?;
                    reader.commit_through(&own)?;
                    continue;
                }
            };
            let Some(run) = metadata.runtime.as_ref().map(|runtime| runtime.run.clone()) else {
                ctx.hand_admitted_failure(
                    own.movement.seq.0,
                    "missing_runtime_envelope",
                    serde_json::json!({}),
                )
                .await?;
                reader.commit_through(&own)?;
                continue;
            };
            if authoring_result
                .consume_if_matches(&ctx, &own.movement, &envelope, &manifest.label)
                .await?
            {
                reader.commit_through(&own)?;
                continue;
            }
            drive_turn(
                &ctx,
                &descriptor,
                &driver,
                &credentials,
                &handoff_session,
                &run,
                QueuedTurn {
                    envelope,
                    seq: own.movement.seq.0,
                    label: manifest.label,
                },
            )
            .await?;
            reader.commit_through(&own)?;
        }
    }
}

async fn drive_turn(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    driver: &OpenAiResponsesDriver,
    credentials: &OAuthCredentialManager,
    handoff_session: &ProviderHandoffSession,
    run: &RunId,
    turn: QueuedTurn,
) -> Result<(), String> {
    let pinned_run_record = ctx.pinned_run_record_bytes(&turn.envelope)?;
    let handoff = handoff_session.from_invocation(
        ctx,
        descriptor,
        run,
        &turn.envelope,
        turn.seq,
        &pinned_run_record,
    )?;
    match handoff.recover_source().await? {
        ProviderSourceRecovery::Process => {}
        ProviderSourceRecovery::Completed => return Ok(()),
        ProviderSourceRecovery::Interrupted {
            last_artifact_label,
        } => {
            return emit_error(
                ctx,
                run,
                &turn,
                &handoff,
                ResponsesFailure {
                    class: "interrupted",
                    retryable: false,
                    status: None,
                    code: Some("provider_turn_interrupted".to_string()),
                    detail: format!(
                        "OAuth Responses process restarted after non-terminal artifact '{last_artifact_label}'; the external call was not replayed"
                    ),
                    evidence: None,
                },
            )
            .await;
        }
    }
    if OAuthCredentialManager::handles_label(&turn.label) {
        return drive_auth_artifact(ctx, credentials, &turn.label, run, &turn, &handoff).await;
    }
    drive_provider_artifact(ctx, descriptor, driver, credentials, run, &turn, &handoff).await
}

async fn drive_auth_artifact(
    ctx: &InterfaceLiftContext,
    credentials: &OAuthCredentialManager,
    label: &str,
    run: &RunId,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
) -> Result<(), String> {
    let payload = match turn.envelope.read_artifact_payload() {
        Ok(payload) => payload,
        Err(error) => {
            return emit_error(
                ctx,
                run,
                turn,
                handoff,
                ResponsesFailure {
                    class: "protocol",
                    retryable: false,
                    status: None,
                    code: Some("oauth_artifact_unreadable".to_string()),
                    detail: format!("OAuth artifact cannot be read: {error}"),
                    evidence: None,
                },
            )
            .await;
        }
    };
    let output_root = ctx.interface_var_dir().join("emitted");
    let credential_handle_fingerprint = credentials.credential_handle_fingerprint();
    let dispatch = OAuthDispatchAdmission {
        handoff,
        output_root: &output_root,
        provider_interface: credentials.interface_id(),
        source_movement_seq: turn.seq,
        attempt: 1,
        credential_handle_fingerprint: Some(&credential_handle_fingerprint),
    };
    match credentials
        .handle_artifact_with_dispatch(label, &payload, &dispatch)
        .await
    {
        Ok(output) => {
            let bytes = serde_json::to_vec(&output.payload)
                .map_err(|error| format!("OAuth auth output did not encode: {error}"))?;
            let artifact = InterfaceArtifactFile::write(&output_root, output.label, &bytes)
                .map_err(|error| format!("OAuth auth output did not materialize: {error}"))?;
            handoff
                .hand_final(ProviderEmission {
                    artifact,
                    route: ProviderEmissionRoute::ClientProjection,
                })
                .await
                .map(|_| ())
        }
        Err(error) => emit_error(ctx, run, turn, handoff, auth_failure(error)).await,
    }
}

async fn drive_provider_artifact(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    driver: &OpenAiResponsesDriver,
    credentials: &OAuthCredentialManager,
    run: &RunId,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
) -> Result<(), String> {
    let max_attempts = match required_policy_u32(&turn.envelope, "max_attempts") {
        Ok(value) => value,
        Err(error) => {
            ctx.hand_admitted_failure(
                turn.seq,
                "malformed_provider_policy",
                serde_json::json!({"field": "max_attempts", "error": error}),
            )
            .await?;
            return Ok(());
        }
    };
    let max_in_flight = match required_policy_u32(&turn.envelope, "max_in_flight") {
        Ok(value) => value,
        Err(error) => {
            ctx.hand_admitted_failure(
                turn.seq,
                "malformed_provider_policy",
                serde_json::json!({"field": "max_in_flight", "error": error}),
            )
            .await?;
            return Ok(());
        }
    };
    if max_attempts == 0 || max_in_flight == 0 {
        return emit_error(
            ctx,
            run,
            turn,
            handoff,
            local_failure(
                "unavailable",
                "provider_policy_denied",
                "formed interface policy denied OAuth provider calls",
            ),
        )
        .await;
    }
    if let Err(failure) = driver.enforce_formed_model_policy(&turn.envelope) {
        return emit_error(ctx, run, turn, handoff, failure).await;
    }
    if let Err(error) = validate_retry_budget(descriptor, max_attempts) {
        return emit_error(
            ctx,
            run,
            turn,
            handoff,
            local_failure("contract", "provider_retry_policy_invalid", &error),
        )
        .await;
    }
    if let Err(error) = credentials.ensure_access_token(false).await {
        return emit_error(ctx, run, turn, handoff, auth_failure(error)).await;
    }
    let output_root = ctx.interface_var_dir().join("emitted");
    let mut attempt = 1_u32;
    let mut authenticated_401_retried = false;
    loop {
        match driver
            .receive_artifact_and_materialize_outputs(
                turn.envelope.clone(),
                turn.seq,
                attempt,
                handoff,
                &output_root,
            )
            .await
        {
            Ok(outputs) => {
                if outputs.is_empty() {
                    return emit_error(
                        ctx,
                        run,
                        turn,
                        handoff,
                        local_failure(
                            "contract",
                            "provider_outputs_empty",
                            "Responses driver produced no output artifacts",
                        ),
                    )
                    .await;
                }
                handoff.hand_batch(outputs).await?;
                return Ok(());
            }
            Err(failure)
                if failure.status == Some(401)
                    && !authenticated_401_retried
                    && attempt < max_attempts =>
            {
                authenticated_401_retried = true;
                attempt = attempt.saturating_add(1);
                if let Err(error) = credentials.ensure_access_token(true).await {
                    return emit_error(ctx, run, turn, handoff, auth_failure(error)).await;
                }
            }
            Err(failure) if failure.retryable && attempt < max_attempts => {
                let backoff = retry_backoff(descriptor, attempt)?;
                attempt += 1;
                tokio::select! {
                    _ = ctx.cancel.cancelled() => {
                        return Err("OAuth Responses life cancelled before the movement reached an admitted outcome".to_string());
                    }
                    _ = tokio::time::sleep(backoff) => {}
                }
            }
            Err(mut failure) if failure.retryable => {
                failure.retryable = false;
                failure.class = "transport";
                failure.detail = format!(
                    "Responses turn failed after {attempt} attempts: {}",
                    failure.detail
                );
                return emit_error(ctx, run, turn, handoff, failure).await;
            }
            Err(failure) => return emit_error(ctx, run, turn, handoff, failure).await,
        }
    }
}

async fn emit_error(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
    failure: ResponsesFailure,
) -> Result<(), String> {
    let artifact =
        materialize_provider_turn_error(&ctx.interface_var_dir().join("emitted"), &failure)
            .map_err(|error| {
                format!(
            "OAuth Responses terminal error for run '{}' movement {} failed to materialize: {}",
            run, turn.seq, error.detail
        )
            })?;
    handoff
        .hand_final(ProviderEmission {
            artifact,
            route: ProviderEmissionRoute::ClientProjection,
        })
        .await
        .map(|_| ())
}

fn auth_failure(error: OAuthFailure) -> ResponsesFailure {
    ResponsesFailure {
        class: error.class,
        retryable: error.retryable,
        status: error.status,
        code: Some(error.code.to_string()),
        detail: error.detail,
        evidence: error.evidence,
    }
}

fn local_failure(class: &'static str, code: &str, detail: &str) -> ResponsesFailure {
    ResponsesFailure {
        class,
        retryable: false,
        status: None,
        code: Some(code.to_string()),
        detail: detail.to_string(),
        evidence: None,
    }
}

fn validate_retry_budget(
    descriptor: &InterfaceDescriptor,
    max_attempts: u32,
) -> Result<(), String> {
    let _ = retry_config(descriptor)?;
    if max_attempts == 0 {
        return Err("Responses max_attempts must be positive".to_string());
    }
    Ok(())
}

fn retry_backoff(
    descriptor: &InterfaceDescriptor,
    failed_attempt: u32,
) -> Result<Duration, String> {
    let (base, maximum) = retry_config(descriptor)?;
    let milliseconds = base
        .saturating_mul(u64::from(failed_attempt.max(1)))
        .min(maximum);
    Ok(Duration::from_millis(milliseconds))
}

fn retry_config(descriptor: &InterfaceDescriptor) -> Result<(u64, u64), String> {
    let retry = descriptor
        .interface_driver_config
        .as_ref()
        .and_then(|config| config.private_config.get("responses_api"))
        .and_then(|value| value.get("retry"))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            "OAuth Responses descriptor has no responses_api.retry object".to_string()
        })?;
    let base = retry
        .get("backoff_ms")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| "OAuth Responses retry backoff_ms must be positive".to_string())?;
    let maximum = retry
        .get("max_backoff_ms")
        .and_then(Value::as_u64)
        .filter(|value| *value >= base)
        .ok_or_else(|| {
            "OAuth Responses retry max_backoff_ms must be at least backoff_ms".to_string()
        })?;
    Ok((base, maximum))
}

fn required_policy_u32(envelope: &InterfaceInvocationEnvelope, name: &str) -> Result<u32, String> {
    let values = envelope
        .interface_policy()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            "OAuth Responses invocation carries no formed interface policy".to_string()
        })?;
    let value = values.get(name).and_then(Value::as_u64).ok_or_else(|| {
        format!("OAuth Responses formed interface policy carries no unsigned '{name}'")
    })?;
    u32::try_from(value).map_err(|_| format!("OAuth Responses policy '{name}' exceeds u32"))
}
