use super::*;

pub(crate) fn server_request_method(request: &ServerRequest) -> String {
    serde_json::to_value(request)
        .ok()
        .and_then(|v| {
            v.get("method")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

pub(crate) fn pending_operator_request(
    request: &ServerRequest,
    thread_id: &str,
    active_turn_id: &str,
    effect_gate: Arc<tokio::sync::Mutex<()>>,
    turn_interrupted: Arc<std::sync::atomic::AtomicBool>,
) -> Result<PendingServerRequest, InterfaceDriverError> {
    let value = serde_json::to_value(request).map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "operator server request could not be serialized: {error}"
        ))
    })?;
    let request_id =
        serde_json::from_value::<RequestId>(value.get("id").cloned().ok_or_else(|| {
            InterfaceDriverError::Protocol("operator server request has no request id".to_string())
        })?)
        .map_err(|error| {
            InterfaceDriverError::Protocol(format!(
                "operator server request id is malformed: {error}"
            ))
        })?;
    for pointer in ["/params/threadId", "/params/conversationId"] {
        let Some(request_thread) = value.pointer(pointer) else {
            continue;
        };
        let request_thread = request_thread.as_str().ok_or_else(|| {
            InterfaceDriverError::Protocol(format!(
                "operator server request carries a non-string thread identity at '{pointer}'"
            ))
        })?;
        if request_thread != thread_id {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "operator server request belongs to thread '{}', active thread is '{}'",
                request_thread, thread_id
            )));
        }
    }
    let turn_id = match value.pointer("/params/turnId") {
        None | Some(serde_json::Value::Null) => active_turn_id.to_string(),
        Some(value) => {
            let request_turn = value
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    InterfaceDriverError::Protocol(
                        "operator server request carries a malformed turnId".to_string(),
                    )
                })?;
            if request_turn != active_turn_id {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "operator server request belongs to turn '{}', active turn is '{}'",
                    request_turn, active_turn_id
                )));
            }
            request_turn.to_string()
        }
    };
    let method = server_request_method(request);
    Ok(PendingServerRequest {
        key: format!("operator:{request_id}"),
        request_id,
        thread_id: thread_id.to_string(),
        turn_id,
        kind: PendingServerRequestKind::Operator { method },
        effect_gate,
        turn_interrupted,
    })
}

pub(crate) const OPERATOR_APPROVAL_METHODS: &[&str] = &[
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
    "item/permissions/requestApproval",
    "execCommandApproval",
    "applyPatchApproval",
];

pub(crate) const OPERATOR_ELICITATION_METHODS: &[&str] = &["mcpServer/elicitation/request"];

pub(crate) fn classify_operator_request(
    request: &ServerRequest,
    continuity: &OperatorContinuityConfig,
) -> Result<Option<CodexTurnOutput>, InterfaceDriverError> {
    let value = serde_json::to_value(request).map_err(|e| {
        InterfaceDriverError::Protocol(format!(
            "failed to serialize server request for operator surfacing: {e}"
        ))
    })?;
    let method = value
        .get("method")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(
                "server request carries no method; cannot classify operator surface (fail-closed)"
                    .into(),
            )
        })?
        .to_string();

    let is_approval = OPERATOR_APPROVAL_METHODS.contains(&method.as_str());
    let is_elicitation = OPERATOR_ELICITATION_METHODS.contains(&method.as_str());
    let is_user_input = method == "item/tool/requestUserInput";
    if !is_approval && !is_elicitation && !is_user_input {
        return Ok(None);
    }

    let request_id = value
        .get("id")
        .map(|id| match id {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(format!(
                "operator server request '{method}' carries no id to correlate a response (fail-closed)"
            ))
        })?;

    let params = value
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    if is_approval {
        let kind = approval_kind_for_method(&method);
        let title = approval_title_from_params(kind, &params);
        let inputs = continuity.approval_inputs(&request_id);
        return Ok(Some(
            CodexTurnOutput::approval_request(
                &continuity.approval.request_label,
                request_id,
                kind,
                title,
                params,
                method,
            )
            .with_verified_inputs(inputs),
        ));
    }

    if is_user_input {
        let questions = params
            .get("questions")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
        let auto_resolution_ms = params
            .get("autoResolutionMs")
            .and_then(serde_json::Value::as_u64);
        let is_blocking = params
            .get("isBlocking")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let inputs = continuity.provided_input(&continuity.user_input, &request_id);
        return Ok(Some(
            CodexTurnOutput::user_input_request(
                &continuity.user_input.request_label,
                request_id,
                questions,
                auto_resolution_ms,
                is_blocking,
                method,
            )
            .with_verified_inputs(inputs),
        ));
    }

    let message = params
        .get("message")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            params
                .get("questions")
                .and_then(serde_json::Value::as_array)
                .and_then(|questions| questions.first())
                .and_then(|question| {
                    question
                        .get("question")
                        .or_else(|| question.get("header"))
                        .and_then(serde_json::Value::as_str)
                })
                .map(str::to_string)
        })
        .unwrap_or_default();
    let requested_schema = params
        .get("requestedSchema")
        .cloned()
        .unwrap_or_else(|| params.clone());
    let inputs = continuity.provided_input(&continuity.elicitation, &request_id);
    Ok(Some(
        CodexTurnOutput::elicitation_request(
            &continuity.elicitation.request_label,
            request_id,
            message,
            requested_schema,
            method,
        )
        .with_verified_inputs(inputs),
    ))
}

pub(crate) fn operator_response_value(
    method: &str,
    response: &OperatorResponseArtifact,
) -> Result<serde_json::Value, InterfaceDriverError> {
    if matches!(
        method,
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "execCommandApproval"
            | "applyPatchApproval"
    ) {
        let OperatorResponseArtifact::Approval { decision, .. } = response else {
            return Err(InterfaceDriverError::Protocol(format!(
                "operator response shape does not answer approval method '{method}'"
            )));
        };
        return Ok(serde_json::json!({ "decision": decision }));
    }
    if method == "item/permissions/requestApproval" {
        let OperatorResponseArtifact::Approval { decision, .. } = response else {
            return Err(InterfaceDriverError::Protocol(
                "permissions approval requires an operator approval response".to_string(),
            ));
        };
        if !decision.is_object() || decision.get("permissions").is_none() {
            return Err(InterfaceDriverError::Protocol(
                "permissions approval decision must carry the complete app-server permissions response object"
                    .to_string(),
            ));
        }
        return Ok(decision.clone());
    }
    if method == "mcpServer/elicitation/request" {
        let OperatorResponseArtifact::Elicitation { answer, .. } = response else {
            return Err(InterfaceDriverError::Protocol(
                "MCP elicitation requires an operator elicitation response".to_string(),
            ));
        };
        if answer.get("action").is_some() {
            return Ok(answer.clone());
        }
        return Ok(serde_json::json!({
            "action": "accept",
            "content": answer,
            "_meta": null
        }));
    }
    if method == "item/tool/requestUserInput" {
        let OperatorResponseArtifact::UserInput { answers, .. } = response else {
            return Err(InterfaceDriverError::Protocol(
                "tool user-input request requires an operator user-input response".to_string(),
            ));
        };
        let answers = answers.as_object().ok_or_else(|| {
            InterfaceDriverError::Protocol(
                "operator user-input answers must be a JSON object".to_string(),
            )
        })?;
        let mut encoded = serde_json::Map::new();
        for (question, values) in answers {
            let values = values.as_array().ok_or_else(|| {
                InterfaceDriverError::Protocol(format!(
                    "operator user-input answer '{question}' must be an array"
                ))
            })?;
            if !values.iter().all(serde_json::Value::is_string) {
                return Err(InterfaceDriverError::Protocol(format!(
                    "operator user-input answer '{question}' contains a non-string value"
                )));
            }
            encoded.insert(question.clone(), serde_json::json!({ "answers": values }));
        }
        return Ok(serde_json::json!({ "answers": encoded }));
    }
    Err(InterfaceDriverError::FailedClosed(format!(
        "operator response targets unsupported app-server method '{method}'"
    )))
}

pub(crate) fn validate_operator_response_evidence(
    continuity: &OperatorContinuityConfig,
    manifest_label: &str,
    response: &OperatorResponseArtifact,
    metadata: &InvocationMetadata,
) -> Result<(), InterfaceDriverError> {
    let request_id = response.request_id()?;
    let (expected_label, allowed) = match response {
        OperatorResponseArtifact::Approval { .. } => (
            continuity.approval.response_label.as_str(),
            vec![
                continuity
                    .approval
                    .approved
                    .required(&continuity.approval.response_label, &request_id),
                continuity
                    .approval
                    .denied
                    .required(&continuity.approval.response_label, &request_id),
            ],
        ),
        OperatorResponseArtifact::Elicitation { .. } => (
            continuity.elicitation.response_label.as_str(),
            vec![
                continuity
                    .elicitation
                    .provided
                    .required(&continuity.elicitation.response_label, &request_id),
            ],
        ),
        OperatorResponseArtifact::UserInput { .. } => (
            continuity.user_input.response_label.as_str(),
            vec![
                continuity
                    .user_input
                    .provided
                    .required(&continuity.user_input.response_label, &request_id),
            ],
        ),
    };
    if manifest_label != expected_label {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "operator response body requires manifest label '{expected_label}', found '{manifest_label}'"
        )));
    }
    let [required] = metadata.required_verified_artifacts.as_slice() else {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "operator response '{request_id}' must arrive through exactly one authored verified-artifact requirement"
        )));
    };
    if !allowed.contains(required) {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "operator response '{request_id}' runtime requirement does not match its exact authored continuity edge"
        )));
    }
    if !metadata
        .grant
        .mark_outcomes
        .iter()
        .any(|mark| mark == required.mark.as_str())
    {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "operator response '{request_id}' required mark '{}' is absent from the formed grant",
            required.mark.as_str()
        )));
    }
    let admission = metadata.checkpoint_admission.as_ref().ok_or_else(|| {
        InterfaceDriverError::FailedClosed(format!(
            "operator response '{request_id}' carries no checkpoint-admission evidence"
        ))
    })?;
    if !admission.complete
        || admission.artifact_label != expected_label
        || admission.required_artifact_labels.as_slice() != [expected_label]
        || metadata.verification_failure.is_some()
    {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "operator response '{request_id}' did not arrive as one complete admitted verified artifact"
        )));
    }
    Ok(())
}

pub(crate) fn approval_kind_for_method(method: &str) -> &'static str {
    if method.contains("commandExecution") || method == "execCommandApproval" {
        "exec"
    } else if method.contains("fileChange") || method == "applyPatchApproval" {
        "patch"
    } else {
        "permission"
    }
}

pub(crate) fn approval_title_from_params(kind: &'static str, params: &serde_json::Value) -> String {
    if kind == "exec" {
        if let Some(command) = params.get("command") {
            match command {
                serde_json::Value::String(command) if !command.trim().is_empty() => {
                    return command.clone();
                }
                serde_json::Value::Array(argv) => {
                    let joined = argv
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ");
                    if !joined.trim().is_empty() {
                        return joined;
                    }
                }
                _ => {}
            }
        }
        return "Run command".to_string();
    }
    if kind == "patch" {
        return "Apply patch".to_string();
    }
    "Grant permissions".to_string()
}
