use std::sync::Arc;

use octopus_descriptors::RunId;
use octopus_interface_transport::{InterfaceLifeFuture, InterfaceLiftContext, InterfaceLiftFn};
use octopus_interfaces::{InterfaceArtifactFile, InterfaceDescriptor, InterfaceInvocationEnvelope};
use octopus_provider_run_authoring::{
    ProviderEmission, ProviderEmissionRoute, ProviderHandoffSession, ProviderOutputHandoff,
    ProviderSourceRecovery, RunAuthoringResultIdentity,
};
use octopus_world_provider_openai_compatible::{
    materialize_provider_turn_error, ProviderErrorEvidence, WorldProviderInterfaceDriver,
};
use octopus_world_pyecho_life_runtime::LedgerTailReader;

use crate::auth::{NousOAuthCredentialManager, NousOAuthFailure};

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
    let credentials = NousOAuthCredentialManager::from_descriptor(&descriptor, &secrets_home)
        .map_err(|error| error.to_string())?;
    let driver = WorldProviderInterfaceDriver::from_descriptor_with_secret_home(
        &descriptor,
        Some(&secrets_home),
    )?;
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
                        serde_json::json!({ "error": error }),
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
                        serde_json::json!({ "error": error.to_string() }),
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
                        serde_json::json!({ "error": error }),
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
    driver: &WorldProviderInterfaceDriver,
    credentials: &NousOAuthCredentialManager,
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
        ProviderSourceRecovery::Bridged { .. } => return Ok(()),
        ProviderSourceRecovery::Completed => return Ok(()),
        ProviderSourceRecovery::Interrupted {
            last_artifact_label,
        } => {
            let evidence = ProviderErrorEvidence::new(
                "failed_closed",
                "provider_turn_interrupted",
                format!(
                    "Nous provider process restarted after non-terminal artifact '{last_artifact_label}'; the interrupted external turn was closed without replay"
                ),
            );
            return emit_turn_error(ctx, &turn, &handoff, 1, &evidence).await;
        }
    }
    if NousOAuthCredentialManager::handles_label(&turn.label) {
        return drive_auth_artifact(ctx, credentials, &turn.label, &turn, &handoff).await;
    }
    drive_provider_artifact(ctx, driver, credentials, &turn, &handoff).await
}

async fn drive_auth_artifact(
    ctx: &InterfaceLiftContext,
    credentials: &NousOAuthCredentialManager,
    label: &str,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
) -> Result<(), String> {
    let payload = match turn.envelope.read_artifact_payload() {
        Ok(payload) => payload,
        Err(error) => {
            let evidence = ProviderErrorEvidence::new(
                "protocol",
                "nous_oauth_artifact_unreadable",
                format!("Nous OAuth artifact cannot be read: {error}"),
            );
            return emit_turn_error(ctx, turn, handoff, 0, &evidence).await;
        }
    };
    match credentials.handle_artifact(label, &payload).await {
        Ok(output) => {
            let bytes = serde_json::to_vec(&output.payload)
                .map_err(|error| format!("Nous auth output did not encode: {error}"))?;
            let artifact = InterfaceArtifactFile::write(
                ctx.interface_var_dir().join("emitted"),
                output.label,
                &bytes,
            )
            .map_err(|error| format!("Nous auth output did not materialize: {error}"))?;
            handoff
                .hand_final(ProviderEmission {
                    artifact,
                    route: ProviderEmissionRoute::ClientProjection,
                })
                .await
                .map(|_| ())
        }
        Err(error) => {
            let evidence = auth_evidence(&error);
            emit_turn_error(ctx, turn, handoff, 0, &evidence).await
        }
    }
}

async fn drive_provider_artifact(
    ctx: &InterfaceLiftContext,
    driver: &WorldProviderInterfaceDriver,
    credentials: &NousOAuthCredentialManager,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
) -> Result<(), String> {
    let max_attempts = match required_policy_u32(&turn.envelope, "max_attempts") {
        Ok(value) => value,
        Err(error) => {
            ctx.hand_admitted_failure(
                turn.seq,
                "malformed_provider_policy",
                serde_json::json!({ "field": "max_attempts", "error": error }),
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
                serde_json::json!({ "field": "max_in_flight", "error": error }),
            )
            .await?;
            return Ok(());
        }
    };
    if max_attempts == 0 || max_in_flight == 0 {
        let evidence = ProviderErrorEvidence::new(
            "failed_closed",
            "provider_policy_denied",
            "formed interface policy denied Nous provider calls",
        );
        return emit_turn_error(ctx, turn, handoff, 0, &evidence).await;
    }
    if let Err(error) = driver.enforce_formed_model_policy(&turn.envelope) {
        return emit_turn_error(ctx, turn, handoff, 0, error.evidence()).await;
    }
    if let Err(error) = driver.validate_retry_budget(max_attempts) {
        let evidence =
            ProviderErrorEvidence::new("failed_closed", "provider_retry_policy_invalid", error);
        return emit_turn_error(ctx, turn, handoff, 0, &evidence).await;
    }
    if let Err(error) = credentials.ensure_invoke_token(false).await {
        let evidence = auth_evidence(&error);
        return emit_turn_error(ctx, turn, handoff, 0, &evidence).await;
    }
    let output_root = ctx.interface_var_dir().join("emitted");
    let mut attempt = 1_u32;
    let mut authenticated_401_retried = false;
    loop {
        let result = driver
            .receive_artifact_and_materialize_outputs(
                turn.envelope.clone(),
                turn.seq,
                attempt,
                None,
                handoff,
                &output_root,
            )
            .await;
        match result {
            Ok(outputs) => {
                handoff.hand_batch(outputs).await?;
                return Ok(());
            }
            Err(error)
                if !authenticated_401_retried
                    && attempt < max_attempts
                    && error.evidence().http.as_ref().map(|http| http.status) == Some(401) =>
            {
                authenticated_401_retried = true;
                attempt += 1;
                if let Err(refresh_error) = credentials.ensure_invoke_token(true).await {
                    let evidence = auth_evidence(&refresh_error);
                    return emit_turn_error(ctx, turn, handoff, attempt, &evidence).await;
                }
            }
            Err(error) if error.retryable() && attempt < max_attempts => {
                let backoff = driver.retry_backoff(attempt)?;
                attempt += 1;
                tokio::select! {
                    _ = ctx.cancel.cancelled() => {
                        return Err("Nous provider life cancelled before the movement reached an admitted outcome".to_string());
                    }
                    _ = tokio::time::sleep(backoff) => {}
                }
            }
            Err(error) => {
                return emit_turn_error(ctx, turn, handoff, attempt, error.evidence()).await;
            }
        }
    }
}

fn auth_evidence(error: &NousOAuthFailure) -> ProviderErrorEvidence {
    let mut evidence = ProviderErrorEvidence::new(error.class, error.code, error.detail.clone());
    evidence.http.clone_from(&error.http);
    evidence.body.clone_from(&error.body);
    evidence
}

async fn emit_turn_error(
    ctx: &InterfaceLiftContext,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
    attempts: u32,
    evidence: &ProviderErrorEvidence,
) -> Result<(), String> {
    let artifact = materialize_provider_turn_error(
        &ctx.interface_var_dir().join("emitted"),
        evidence,
        attempts,
        turn.seq,
    )
    .map_err(|error| format!("Nous provider terminal error failed to materialize: {error}"))?;
    handoff
        .hand_final(ProviderEmission {
            artifact,
            route: ProviderEmissionRoute::ClientProjection,
        })
        .await
        .map(|_| ())
}

fn required_policy_u32(envelope: &InterfaceInvocationEnvelope, name: &str) -> Result<u32, String> {
    let values = envelope
        .interface_policy()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "provider invocation carries no formed interface policy".to_string())?;
    let value = values
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("provider formed interface policy carries no unsigned '{name}'"))?;
    u32::try_from(value).map_err(|_| format!("provider policy '{name}' exceeds u32"))
}
