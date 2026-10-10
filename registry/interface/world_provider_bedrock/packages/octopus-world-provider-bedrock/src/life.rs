use std::sync::Arc;

use octopus_descriptors::RunId;
use octopus_interface_transport::{InterfaceLifeFuture, InterfaceLiftContext, InterfaceLiftFn};
use octopus_interfaces::{InterfaceDescriptor, InterfaceInvocationEnvelope};
use octopus_provider_run_authoring::{
    ProviderEmission, ProviderEmissionRoute, ProviderHandoffSession, ProviderOutputHandoff,
    ProviderSourceRecovery, RunAuthoringResultIdentity,
};
use octopus_world_pyecho_life_runtime::LedgerTailReader;

use crate::{materialize_provider_error, BedrockConverseDriver, BedrockFailure};

pub fn interface_life() -> InterfaceLiftFn {
    Arc::new(|ctx| Box::pin(provider_life(ctx)) as InterfaceLifeFuture)
}

struct QueuedTurn {
    envelope: InterfaceInvocationEnvelope,
    seq: u64,
}

async fn provider_life(ctx: InterfaceLiftContext) -> Result<(), String> {
    let descriptor = ctx.resolved_interface_descriptor()?;
    let activation_secrets_home = ctx.root_slot("activation_secrets_home")?.to_path_buf();
    let driver = BedrockConverseDriver::from_descriptor(&descriptor, activation_secrets_home)?;
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
            drive_with_retries(
                &ctx,
                &descriptor,
                &driver,
                &handoff_session,
                &run,
                QueuedTurn {
                    envelope,
                    seq: own.movement.seq.0,
                },
            )
            .await?;
            reader.commit_through(&own)?;
        }
    }
}

async fn drive_with_retries(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    driver: &BedrockConverseDriver,
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
                ErrorEmission {
                    class: "interrupted",
                    detail: &format!(
                        "provider process restarted after non-terminal artifact '{last_artifact_label}'; the external Bedrock call was not replayed"
                    ),
                    attempts: None,
                    failure: None,
                },
            )
            .await;
        }
    }
    let max_attempts = match required_policy_u32(&turn.envelope, "max_attempts") {
        Ok(value) => value,
        Err(error) => {
            return emit_error(
                ctx,
                run,
                &turn,
                &handoff,
                ErrorEmission {
                    class: "malformed_provider_policy",
                    detail: &error,
                    attempts: None,
                    failure: None,
                },
            )
            .await;
        }
    };
    let max_in_flight = match required_policy_u32(&turn.envelope, "max_in_flight") {
        Ok(value) => value,
        Err(error) => {
            return emit_error(
                ctx,
                run,
                &turn,
                &handoff,
                ErrorEmission {
                    class: "malformed_provider_policy",
                    detail: &error,
                    attempts: None,
                    failure: None,
                },
            )
            .await;
        }
    };
    if max_attempts == 0 || max_in_flight == 0 {
        return emit_error(
            ctx,
            run,
            &turn,
            &handoff,
            ErrorEmission {
                class: "unavailable",
                detail: "formed interface policy denied Bedrock calls",
                attempts: None,
                failure: None,
            },
        )
        .await;
    }
    if let Err(failure) = driver.enforce_formed_model_policy(&turn.envelope) {
        return emit_error(
            ctx,
            run,
            &turn,
            &handoff,
            ErrorEmission {
                class: "unavailable",
                detail: &failure.detail,
                attempts: None,
                failure: Some(&failure),
            },
        )
        .await;
    }
    if max_in_flight > driver.account_max_in_flight() {
        return emit_error(
            ctx,
            run,
            &turn,
            &handoff,
            ErrorEmission {
                class: "malformed_provider_policy",
                detail: &format!(
                    "formed max_in_flight {max_in_flight} exceeds descriptor account limit {}",
                    driver.account_max_in_flight()
                ),
                attempts: None,
                failure: None,
            },
        )
        .await;
    }
    let output_root = ctx.interface_var_dir().join("emitted");
    let mut attempt = 1_u32;
    loop {
        let result = tokio::select! {
            _ = ctx.cancel.cancelled() => {
                return Err("Bedrock provider life cancelled before the movement reached an admitted outcome".to_string());
            }
            result = driver.receive_artifact_and_materialize_outputs(
                turn.envelope.clone(),
                turn.seq,
                attempt,
                &handoff,
                &output_root,
            ) => result,
        };
        match result {
            Ok(outputs) => {
                if outputs.is_empty() {
                    return emit_error(
                        ctx,
                        run,
                        &turn,
                        &handoff,
                        ErrorEmission {
                            class: "invalid_response",
                            detail: "Bedrock provider produced no output artifacts",
                            attempts: Some(attempt),
                            failure: None,
                        },
                    )
                    .await;
                }
                handoff.hand_batch(outputs).await?;
                return Ok(());
            }
            Err(failure) if failure.retryable && attempt < max_attempts => {
                let delay = driver.retry_delay(attempt);
                attempt += 1;
                tokio::select! {
                    _ = ctx.cancel.cancelled() => {
                        return Err("Bedrock provider life cancelled before the movement reached an admitted outcome".to_string());
                    }
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            Err(failure) => {
                let class = failure.class;
                let detail = failure.detail.clone();
                return emit_error(
                    ctx,
                    run,
                    &turn,
                    &handoff,
                    ErrorEmission {
                        class,
                        detail: &detail,
                        attempts: Some(attempt),
                        failure: Some(&failure),
                    },
                )
                .await;
            }
        }
    }
}

struct ErrorEmission<'a> {
    class: &'a str,
    detail: &'a str,
    attempts: Option<u32>,
    failure: Option<&'a BedrockFailure>,
}

async fn emit_error(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
    error: ErrorEmission<'_>,
) -> Result<(), String> {
    let artifact = materialize_provider_error(
        &ctx.interface_var_dir().join("emitted"),
        error.class,
        error.detail,
        error.attempts,
        error.failure,
    )
    .map_err(|error| {
        format!(
            "Bedrock provider terminal error for run '{}' movement {} failed to materialize: {error}",
            run, turn.seq
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
