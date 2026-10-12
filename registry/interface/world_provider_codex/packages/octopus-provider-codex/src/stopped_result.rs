use super::*;

#[derive(Clone)]
pub(crate) struct StoppedResultSettlement {
    pub(crate) source: CodexContinuityState,
    pub(crate) predecessor: ArtifactRef,
    pub(crate) acknowledgement: ArtifactRef,
    pub(crate) acknowledgement_seq: u64,
    pub(crate) control_source: u64,
    pub(crate) control_id: String,
    pub(crate) action: String,
    evidence: serde_json::Value,
}

impl StoppedResultSettlement {
    pub(crate) fn context_generation(&self) -> u64 {
        self.source.context_generation
    }
}

pub(crate) fn prove(
    descriptor: &InterfaceDescriptor,
    input: &InterfaceInvocationEnvelope,
    source_seq: u64,
    current: &[u8],
    session: &[SessionContextArtifact],
    outcomes: &[octopus_provider_run_authoring::ProviderHandoffOutcome],
) -> Result<Option<StoppedResultSettlement>, InterfaceDriverError> {
    let runtime = invocation_runtime_metadata(input)?;
    let record: serde_json::Value = serde_json::from_slice(current)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    let pending = &record["interface_state"][runtime.interface.as_str()]["pending_handoff"];
    if pending["source_movement_seq"] != source_seq || pending["terminal"] != true {
        return Ok(None);
    }
    let [octopus_provider_run_authoring::ProviderHandoffOutcome::Admitted { seq, artifact }] =
        outcomes
    else {
        return Ok(None);
    };
    let Some(error) = session.iter().find(|entry| {
        entry.source_artifact == *artifact && entry.source_movement_seq == Some(*seq)
    }) else {
        return Ok(None);
    };
    if error.label != TURN_ERROR_LABEL
        || error.payload["will_retry"] != false
        || error.payload["additional_details"]["source_movement_seq"] != source_seq
    {
        return Ok(None);
    }
    if !session.iter().any(|entry| {
        terminal_control_ack(entry)
            && entry
                .source_movement_seq
                .is_some_and(|ack_seq| source_seq < ack_seq && ack_seq < *seq)
    }) {
        return Ok(None);
    }
    let Some(raw) = error.payload.get(CODEX_CONTINUITY_FIELD) else {
        return Ok(None);
    };
    let state = parse_codex_continuity(raw, &runtime.interface, runtime.run.as_str())?;
    let Some(wait) = state
        .pending
        .as_ref()
        .filter(|wait| interrupted_pending_has_external_result(wait))
    else {
        return Ok(None);
    };
    let fail = |message: &str| InterfaceDriverError::FailedClosed(message.into());
    if runtime.interface != descriptor.id
        || record["id"] != runtime.run.as_str()
        || state.context_generation != read_codex_context_generation(&record, &runtime.interface)?
        || state.interrupted_pending.is_some()
        || state.native_terminal.is_some()
        || state.tool_operator_request.is_some()
    {
        return Err(fail(
            "stopped result error differs from the current checked pending state",
        ));
    }
    let mut interrupted = state.clone();
    interrupted.interrupted_pending = interrupted.pending.take();
    if !exact_interrupted_result_source(input, &interrupted)
        .map_err(InterfaceDriverError::FailedClosed)?
    {
        return Err(fail(
            "stopped result differs from its retained exact pending effect",
        ));
    }
    let labels = codex_context_output_labels(descriptor)?;
    let original = session
        .iter()
        .filter_map(|entry| {
            let raw = entry.payload.get(CODEX_CONTINUITY_FIELD)?;
            (entry.label == TOOL_CALL_LABEL
                && entry
                    .source_movement_seq
                    .is_some_and(|seq| seq < source_seq))
            .then_some((entry, raw))
        })
        .filter_map(|(entry, raw)| {
            let original =
                parse_codex_continuity(raw, &runtime.interface, runtime.run.as_str()).ok()?;
            (original.pending.as_ref() == Some(wait)).then_some((entry, original))
        })
        .last()
        .ok_or_else(|| fail("stopped result has no retained exact original effect"))?;
    let (effect, original) = original;
    let mut expected = original.clone();
    expected.context_generation = state.context_generation;
    if expected != state {
        return Err(fail(
            "stopped result error changed its native plan or private thread",
        ));
    }
    if let Some(operation) = native_pending_operation(wait)?
        && effect.payload.get("arguments") != Some(&operation.effect.arguments)
    {
        return Err(fail(
            "stopped result original effect differs from its exact native plan",
        ));
    }
    let effect_seq = effect
        .source_movement_seq
        .expect("selected effect sequence");
    let advances = session
        .iter()
        .filter(|entry| {
            labels.contains(&entry.label)
                && entry
                    .source_movement_seq
                    .is_some_and(|seq| seq > effect_seq)
        })
        .collect::<Vec<_>>();
    let [ack, final_error] = advances.as_slice() else {
        return Err(fail(
            "stopped result has unexplained context changes after its pending effect",
        ));
    };
    if final_error.source_artifact != error.source_artifact
        || !terminal_control_ack(ack)
        || original.context_generation.checked_add(2) != Some(state.context_generation)
    {
        return Err(fail(
            "stopped result lacks its exact non-dispatched Stop and error context lineage",
        ));
    }
    let action = ack.payload["action"]
        .as_str()
        .filter(|action| matches!(*action, "stop" | "interrupt"))
        .ok_or_else(|| fail("stopped result acknowledgement is not a stopping control"))?
        .to_string();
    let control_id = ack.payload["control_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| fail("stopped result lost its control id"))?
        .to_string();
    let control_source = ack.payload["source_movement_seq"]
        .as_u64()
        .ok_or_else(|| fail("stopped result lost its control source"))?;
    let ack_seq = ack
        .source_movement_seq
        .ok_or_else(|| fail("stopped result acknowledgement has no admission"))?;
    if !(effect_seq < source_seq
        && source_seq < control_source
        && control_source < ack_seq
        && ack_seq < *seq)
    {
        return Err(fail(
            "stopped result control/effect admissions are out of order",
        ));
    }
    Ok(Some(StoppedResultSettlement {
        source: state,
        predecessor: artifact.clone(),
        acknowledgement: ack.source_artifact.clone(),
        acknowledgement_seq: ack_seq,
        control_source,
        control_id,
        action,
        evidence: serde_json::json!({
            "error_artifact": artifact, "error_movement_seq": seq,
            "effect_artifact": effect.source_artifact, "effect_movement_seq": effect_seq,
            "effect_context_generation": original.context_generation,
            "result_artifact": input.artifact, "result_movement_seq": source_seq,
            "stop_artifact": ack.source_artifact, "stop_movement_seq": ack_seq,
            "stop_source_movement_seq": control_source,
            "continuation_dispatched": false,
        }),
    }))
}

fn terminal_control_ack(entry: &SessionContextArtifact) -> bool {
    entry.label == TURN_CONTROL_ACKNOWLEDGED_LABEL
        && entry.payload.get(CODEX_CONTINUITY_FIELD).is_none()
        && entry.payload["status"] == "acknowledged"
        && matches!(entry.payload["action"].as_str(), Some("stop" | "interrupt"))
        && entry.payload["detail"]["already_terminal"] == true
        && entry.payload["detail"]["external_effect_dispatched"] == false
        && entry.payload["detail"]["linearization"] == "control_reader_terminal_fence"
        && entry.payload["thread_id"].is_null()
        && entry.payload["turn_id"].is_null()
}

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn stopped_result_settlement(
        &self,
        proof: &StoppedResultSettlement,
        input: &InterfaceInvocationEnvelope,
        output_root: &Path,
        labels: &BTreeSet<String>,
    ) -> Result<ProviderEmission, InterfaceDriverError> {
        let pending = proof.source.pending.as_ref().ok_or_else(|| {
            InterfaceDriverError::FailedClosed("stopped result lost its pending effect".into())
        })?;
        let terminal = read_exact_terminal_turn(&self.client.lock().await.request_handle(), &self.next_request_id,
            &proof.source.thread_id, pending_turn_id(pending)).await.map_err(InterfaceDriverError::FailedClosed)?
            .ok_or_else(|| InterfaceDriverError::FailedClosed("stopped result recovery requires exact native terminal evidence; the retained turn is still active".into()))?;
        let events = take_native_terminal_events(
            &mut *self.client.lock().await,
            &terminal,
            &proof.source.thread_id,
            pending_turn_id(pending),
        )?;
        materialize(proof, input, terminal, events, output_root, labels)
    }
}

fn materialize(
    proof: &StoppedResultSettlement,
    input: &InterfaceInvocationEnvelope,
    terminal: NativeTerminalEvidence,
    events: Vec<serde_json::Value>,
    output_root: &Path,
    labels: &BTreeSet<String>,
) -> Result<ProviderEmission, InterfaceDriverError> {
    let source = &proof.source;
    let pending = source.pending.as_ref().ok_or_else(|| {
        InterfaceDriverError::FailedClosed("stopped result lost its pending effect".into())
    })?;
    let turn = terminal.exact_turn(&source.thread_id, Some(pending_turn_id(pending)))?;
    let retained = terminal.outputs(&events)?;
    if turn.status != TurnStatus::Interrupted
        || retained.iter().any(|output| labels.contains(&output.label))
    {
        return Err(InterfaceDriverError::FailedClosed(
            "stopped result cannot discard a completed native answer".into(),
        ));
    }
    let result: serde_json::Value = serde_json::from_slice(&input.read_artifact_payload()?)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    let ok = result["ok"].as_bool().ok_or_else(|| {
        InterfaceDriverError::FailedClosed("stopped result lost its actual verdict".into())
    })?;
    let (kind, correlation) = match pending {
        CodexPendingContinuity::DynamicTool { call_id, .. } => ("tool_result", call_id),
        CodexPendingContinuity::InterfaceOperation { call_id, .. } => {
            ("interface_operation_result", call_id)
        }
        _ => {
            return Err(InterfaceDriverError::FailedClosed(
                "stopped result is not an external effect".into(),
            ));
        }
    };
    let mut closed = source.clone();
    closed.pending = None;
    closed.native_terminal = Some(terminal.clone());
    let mut outputs = Vec::new();
    for mut output in retained {
        output.value["label"] = serde_json::json!(output.label);
        output.value[CODEX_CONTINUITY_FIELD] = serde_json::to_value(&closed)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        outputs.push(output.value);
    }
    closed.context_generation = source
        .context_generation
        .checked_add(u64::from(labels.contains(TURN_INTERRUPTED_RESULT_LABEL)))
        .ok_or_else(|| {
            InterfaceDriverError::FailedClosed("stopped result generation overflow".into())
        })?;
    let mut output = CodexTurnOutput::interrupted_result_closed(
        kind,
        correlation.clone(),
        source.thread_id.clone(),
        pending_turn_id(pending).to_string(),
        ok,
        &input.artifact,
    )
    .with_native_terminal_events(events);
    terminal.apply_disposition(&mut output)?;
    output.value["label"] = serde_json::json!(TURN_INTERRUPTED_RESULT_LABEL);
    output.value[CODEX_CONTINUITY_FIELD] = serde_json::to_value(closed)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    output.value["native_terminal_source_continuity"] = serde_json::to_value(source)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    output.value["result_origin"] = serde_json::json!("native_turn_completion");
    output.value["terminal_evidence_source"] = serde_json::json!("thread/read");
    output.value["native_terminal_input_context"] = serde_json::json!(false);
    output.value["retained_native_outputs"] = serde_json::json!(outputs);
    output.value["completed_source_settlement"] = proof.evidence.clone();
    let bytes = serde_json::to_vec(&output.value)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
    interrupted_result_closure_payload(
        TURN_INTERRUPTED_RESULT_LABEL,
        &bytes,
        source,
        &input.artifact,
        labels,
    )
    .map_err(InterfaceDriverError::FailedClosed)?
    .ok_or_else(|| {
        InterfaceDriverError::FailedClosed("stopped result closure did not validate".into())
    })?;
    Ok(ProviderEmission {
        artifact: materialize_codex_emitted_artifact(
            output_root,
            TURN_INTERRUPTED_RESULT_LABEL,
            &bytes,
        )?,
        route: ProviderEmissionRoute::ClientProjection,
    })
}

#[cfg(test)]
pub(crate) fn materialize_for_test(
    proof: &StoppedResultSettlement,
    input: &InterfaceInvocationEnvelope,
    turn: serde_json::Value,
    output_root: &Path,
    labels: &BTreeSet<String>,
) -> Result<ProviderEmission, InterfaceDriverError> {
    materialize(
        proof,
        input,
        NativeTerminalEvidence {
            thread_id: proof.source.thread_id.clone(),
            turn,
        },
        Vec::new(),
        output_root,
        labels,
    )
}
