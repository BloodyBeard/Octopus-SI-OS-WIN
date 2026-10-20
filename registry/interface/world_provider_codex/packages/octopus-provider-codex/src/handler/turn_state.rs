use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContextWindowUsage {
    pub(crate) used_tokens: u64,
    pub(crate) context_window: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct TokenUsageBreakdownEvidence {
    pub(crate) total_tokens: u64,
    pub(crate) input_tokens: u64,
    pub(crate) cached_input_tokens: u64,
    pub(crate) uncached_input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) reasoning_output_tokens: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ProviderTokenUsageEvidence {
    pub(crate) cumulative: TokenUsageBreakdownEvidence,
    pub(crate) current_context: TokenUsageBreakdownEvidence,
    pub(crate) context_window: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelResultProjection {
    pub(crate) max_tokens: usize,
}

impl ModelResultProjection {
    pub(crate) fn from_private_config(
        config: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, InterfaceDriverError> {
        let value = config
            .get("model_result_projection")
            .cloned()
            .ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "codex_app_server descriptor declares no model_result_projection".into(),
                )
            })?;
        serde_json::from_value::<Self>(value).map_err(|error| {
            InterfaceDriverError::Protocol(format!("interface_driver_config.private_config.model_result_projection is malformed: {error}"))
        })?.validate()
    }

    pub(crate) fn validate(self) -> Result<Self, InterfaceDriverError> {
        if self.max_tokens < 512 {
            return Err(InterfaceDriverError::FailedClosed(
                "interface_driver_config.private_config.model_result_projection.max_tokens must be at least 512"
                    .to_string(),
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TransportStateScope {
    Run(String),
}

impl TransportStateScope {
    pub(crate) fn from_run(run: &octopus_interfaces::RunId) -> TransportStateScope {
        TransportStateScope::Run(run.as_str().to_string())
    }
}

#[derive(Default)]
pub(crate) struct TransportState {
    pub(crate) context_generation: u64,
    pub(crate) hooks_catalog_emitted: bool,
    pub(crate) thread_id: Option<String>,
    pub(crate) thread_shape_fingerprint: Option<String>,
    pub(crate) static_context_fingerprint: Option<String>,
    pub(crate) rollout_path: Option<PathBuf>,
}

pub(crate) const CODEX_CONTINUITY_FIELD: &str = "codex_continuity";
pub(crate) const LIFECYCLE_PROVIDER_CONTINUITY_FIELD: &str = "provider_continuity";

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodexContinuityState {
    pub(crate) schema_version: u32,
    pub(crate) provider_interface: InterfaceId,
    pub(crate) run: String,
    pub(crate) thread_id: String,
    pub(crate) thread_shape_fingerprint: String,
    pub(crate) context_generation: u64,
    pub(crate) static_context_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pending: Option<CodexPendingContinuity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) interrupted_pending: Option<CodexPendingContinuity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tool_operator_request: Option<ToolOperatorContinuity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) native_terminal: Option<NativeTerminalEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) context_refresh: Option<ContextRefresh>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) native_history_retirement: Option<NativeHistoryRetirement>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeTerminalEvidence {
    pub(crate) thread_id: String,
    pub(crate) turn: serde_json::Value,
}

impl NativeTerminalEvidence {
    pub(crate) fn exact_turn(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
    ) -> Result<octopus_appserver_proto::Turn, InterfaceDriverError> {
        let turn: octopus_appserver_proto::Turn = serde_json::from_value(self.turn.clone())
            .map_err(|error| InterfaceDriverError::Protocol(format!("native terminal turn is malformed: {error}")))?;
        if self.thread_id != thread_id
            || turn.id.trim().is_empty()
            || turn_id.is_some_and(|expected| turn.id != expected)
            || turn.status == TurnStatus::InProgress
            || turn.items_view != TurnItemsView::Full
        {
            return Err(InterfaceDriverError::FailedClosed(
                "native terminal evidence does not prove the exact closed turn with loaded items".into(),
            ));
        }
        Ok(turn)
    }

    pub(crate) fn outputs(&self, events: &[serde_json::Value]) -> Result<Vec<CodexTurnOutput>, InterfaceDriverError> {
        let turn = self.exact_turn(&self.thread_id, None)?;
        let status = serde_json::to_value(&turn.status).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        if events.iter().any(|frame| frame["method"] == "turn/completed" && frame.pointer("/params/turn/status") != Some(&status)) {
            return Err(InterfaceDriverError::FailedClosed("native terminal notification conflicts with the exact thread/read evidence".into()));
        }
        let mut outputs = Vec::new();
        let finals = turn.items.iter().filter_map(|item| match item {
            ThreadItem::AgentMessage { id, text, phase: Some(MessagePhase::FinalAnswer) } => Some((id, text)),
            _ => None,
        }).collect::<Vec<_>>();
        if !finals.is_empty() || turn.status == TurnStatus::Completed {
            let (id, text) = completed_turn_final_message(&turn)?;
            let mut output = CodexTurnOutput::final_message(&self.thread_id, &turn.id, id, text, None);
            output.value["native_terminal_status"] = serde_json::to_value(turn.status)
                .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
            output.value["native_completed_at"] = serde_json::to_value(turn.completed_at)
                .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
            outputs.push(output);
        }
        outputs.push(CodexTurnOutput::client(TURN_COMPLETED_LABEL, serde_json::json!({
            "threadId": self.thread_id,
            "turn": self.turn,
            "retained_native_events": events,
        })));
        Ok(outputs)
    }

    pub(crate) fn apply_disposition(&self, output: &mut CodexTurnOutput) -> Result<(), InterfaceDriverError> {
        let turn = self.exact_turn(&self.thread_id, output.value.get("turn_id").and_then(serde_json::Value::as_str))?;
        output.value["turn_interrupted"] = serde_json::Value::Bool(turn.status == TurnStatus::Interrupted);
        output.value["native_terminal_status"] = serde_json::to_value(turn.status)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        output.value["result_disposition"] = serde_json::json!("durably_admitted_after_native_terminal");
        Ok(())
    }
}

pub(crate) async fn read_exact_terminal_turn(
    handle: &AppServerRequestHandle,
    request_ids: &std::sync::atomic::AtomicI64,
    thread_id: &str,
    turn_id: &str,
) -> Result<Option<NativeTerminalEvidence>, String> {
    let response: ThreadReadResponse = handle.request_typed(ClientRequest::ThreadRead {
        request_id: RequestId::Integer(request_ids.fetch_add(1, std::sync::atomic::Ordering::SeqCst)),
        params: ThreadReadParams { thread_id: thread_id.into(), include_turns: false },
    }).await.map_err(|error| format!("exact terminal thread/read failed: {error}"))?;
    if response.thread.id != thread_id {
        return Err("terminal thread/read returned a different thread".into());
    }
    let mut turn = read_native_turn(handle, request_ids, thread_id, turn_id)
        .await.map_err(|error| error.to_string())?
        .ok_or("terminal thread/turns/list did not contain the exact turn")?;
    if turn.status == TurnStatus::InProgress {
        return Ok(None);
    }
    turn.items = read_native_turn_items(handle, request_ids, thread_id, turn_id)
        .await.map_err(|error| error.to_string())?;
    turn.items_view = TurnItemsView::Full;
    let evidence = NativeTerminalEvidence {
        thread_id: thread_id.into(),
        turn: serde_json::to_value(turn).map_err(|error| error.to_string())?,
    };
    evidence.exact_turn(thread_id, Some(turn_id)).map_err(|error| error.to_string())?;
    Ok(Some(evidence))
}

pub(crate) fn take_native_terminal_events(
    client: &mut AppServerClient,
    terminal: &NativeTerminalEvidence,
    thread_id: &str,
    turn_id: &str,
) -> Result<Vec<serde_json::Value>, InterfaceDriverError> {
    let turn = terminal.exact_turn(thread_id, Some(turn_id))?;
    let mut deferred = Vec::new();
    let mut events = Vec::new();
    let mut terminal_conflict = false;
    for event in client.take_ready_events() {
        match event {
            AppServerEvent::ServerNotification(notification) => {
                let frame = serde_json::to_value(&notification).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
                if !native_frame_matches_turn(&frame, thread_id, turn_id) {
                    deferred.push(AppServerEvent::ServerNotification(notification));
                    continue;
                }
                if let ServerNotification::TurnCompleted(completed) = &notification {
                    if completed.turn.status != turn.status {
                        terminal_conflict = true;
                        deferred.push(AppServerEvent::ServerNotification(notification));
                        continue;
                    }
                }
                events.push(frame);
            }
            AppServerEvent::ServerRequest(request) => {
                let frame = serde_json::to_value(&request).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
                if native_frame_matches_turn(&frame, thread_id, turn_id) {
                    events.push(serde_json::json!({"quarantined_server_request": frame}));
                } else {
                    deferred.push(AppServerEvent::ServerRequest(request));
                }
            }
            event => deferred.push(event),
        }
    }
    client.restore_ready_events(deferred);
    if terminal_conflict {
        return Err(InterfaceDriverError::FailedClosed("native terminal notification conflicts with the exact thread/read evidence".into()));
    }
    Ok(events)
}
