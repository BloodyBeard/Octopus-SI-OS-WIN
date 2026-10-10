use super::*;
use octopus_world_terminal_client::ToolCallItem;
use octopus_world_terminal_client::ToolCallStatus;
use octopus_world_terminal_client::ToolCallSurface;
use octopus_world_terminal_client::WebSearchActionItem;
use octopus_world_terminal_client::WebSearchItem;

impl ChatWidget {
    pub(super) fn on_patch_apply_begin(&mut self, changes: HashMap<PathBuf, FileChange>) {
        self.record_visible_turn_activity();
        self.add_to_history(history_cell::new_patch_event(changes, &self.config.cwd));
    }

    pub(super) fn on_view_image_tool_call(&mut self, path: AbsolutePathBuf) {
        self.record_visible_turn_activity();
        self.flush_answer_stream_with_separator();
        self.add_to_history(history_cell::new_view_image_tool_call(
            path,
            &self.config.cwd,
        ));
        self.request_redraw();
    }

    pub(super) fn on_image_generation_begin(&mut self) {
        self.record_visible_turn_activity();
        self.flush_answer_stream_with_separator();
    }

    pub(super) fn on_image_generation_end(
        &mut self,
        call_id: String,
        status: String,
        revised_prompt: Option<String>,
        saved_path: Option<AbsolutePathBuf>,
    ) {
        self.flush_answer_stream_with_separator();
        self.add_to_history(history_cell::new_image_generation_call(
            call_id,
            &status,
            revised_prompt,
            saved_path,
        ));
        self.request_redraw();
    }

    pub(super) fn on_tool_call_artifact(&mut self, artifact_label: &str, item: ToolCallItem) {
        self.record_visible_turn_activity();
        let label = artifact_label.to_string();
        let label2 = label.clone();
        let item2 = item.clone();
        self.defer_or_handle(
            |queue| queue.push_tool_activity(label, item),
            |chat| chat.handle_tool_call_artifact_now(label2, item2),
        );
    }

    pub(crate) fn handle_tool_call_artifact_now(
        &mut self,
        artifact_label: String,
        item: ToolCallItem,
    ) {
        let invocation = tool_invocation(&artifact_label, &item);
        if item.status.is_complete() {
            let result = tool_result(&artifact_label, &item);
            self.handle_mcp_tool_call_completed_now(
                item.correlation_id,
                invocation,
                item.duration_ms,
                result,
            );
        } else {
            self.handle_mcp_tool_call_started_now(item.correlation_id, invocation);
        }
    }

    pub(super) fn on_web_search_begin(&mut self, call_id: String) {
        self.record_visible_turn_activity();
        self.flush_answer_stream_with_separator();
        self.flush_active_cell();
        self.transcript.active_cell = Some(Box::new(history_cell::new_active_web_search_call(
            call_id,
            String::new(),
            self.config.terminal.animations,
        )));
        self.bump_active_cell_revision();
        self.request_redraw();
    }

    pub(super) fn on_web_search_item(&mut self, item: WebSearchItem) {
        self.on_web_search_begin(item.item_id.clone());
        self.on_web_search_end(
            item.item_id,
            item.query,
            item.action.unwrap_or(WebSearchActionItem::Other),
        );
    }

    pub(super) fn on_web_search_end(
        &mut self,
        call_id: String,
        query: String,
        action: WebSearchActionItem,
    ) {
        self.flush_answer_stream_with_separator();
        let mut handled = false;
        if let Some(cell) = self
            .transcript
            .active_cell
            .as_mut()
            .and_then(|cell| cell.as_any_mut().downcast_mut::<WebSearchCell>())
            && cell.call_id() == call_id
        {
            cell.update(action.clone(), query.clone());
            cell.complete();
            self.bump_active_cell_revision();
            self.flush_active_cell();
            handled = true;
        }

        if !handled {
            self.add_to_history(history_cell::new_web_search_call(call_id, query, action));
        }
        self.transcript.had_work_activity = true;
    }

    pub(crate) fn on_patch_apply_completed(&mut self, success: bool) {
        if !success {
            self.add_to_history(history_cell::new_patch_apply_failure(String::new()));
        }
        self.transcript.had_work_activity = true;
    }

    pub(crate) fn handle_mcp_tool_call_started_now(
        &mut self,
        id: String,
        invocation: McpInvocation,
    ) {
        self.record_visible_turn_activity();
        let McpInvocation {
            server,
            tool,
            arguments,
        } = invocation;
        let arguments = arguments.unwrap_or(serde_json::Value::Null);
        self.flush_answer_stream_with_separator();
        self.flush_active_cell();
        self.transcript.active_cell = Some(Box::new(history_cell::new_active_mcp_tool_call(
            id,
            McpInvocation {
                server,
                tool,
                arguments: Some(arguments),
            },
            self.config.terminal.animations,
        )));
        self.bump_active_cell_revision();
        self.request_redraw();
    }

    pub(crate) fn handle_mcp_tool_call_completed_now(
        &mut self,
        id: String,
        invocation: McpInvocation,
        duration_ms: Option<i64>,
        result: Result<crate::turn_items::CallToolResult, String>,
    ) {
        self.flush_answer_stream_with_separator();
        let duration = Duration::from_millis(duration_ms.unwrap_or_default().max(0) as u64);

        let extra_cell = match self
            .transcript
            .active_cell
            .as_mut()
            .and_then(|cell| cell.as_any_mut().downcast_mut::<McpToolCallCell>())
        {
            Some(cell) if cell.call_id() == id => cell.complete(duration, result),
            _ => {
                self.flush_active_cell();
                let mut cell = history_cell::new_active_mcp_tool_call(
                    id,
                    invocation,
                    self.config.terminal.animations,
                );
                let extra_cell = cell.complete(duration, result);
                self.transcript.active_cell = Some(Box::new(cell));
                extra_cell
            }
        };

        self.flush_active_cell();
        if let Some(extra) = extra_cell {
            self.add_boxed_history(extra);
        }
        self.transcript.had_work_activity = true;
    }
}

fn tool_invocation(artifact_label: &str, item: &ToolCallItem) -> McpInvocation {
    let (derived_namespace, tool) = match item.tool.split_once('.') {
        Some((namespace, tool)) => (Some(namespace.to_string()), tool.to_string()),
        None => (None, item.tool.clone()),
    };
    McpInvocation {
        server: item
            .namespace
            .clone()
            .or(derived_namespace)
            .unwrap_or_else(|| artifact_label.to_string()),
        tool,
        arguments: Some(item.arguments.clone()),
    }
}

fn tool_result(
    artifact_label: &str,
    item: &ToolCallItem,
) -> Result<crate::turn_items::CallToolResult, String> {
    if item.status == ToolCallStatus::Failed {
        return Err(item
            .error
            .clone()
            .unwrap_or_else(|| format!("{} reported a failed tool call", artifact_label)));
    }
    let value = item.result.clone().unwrap_or(serde_json::Value::Null);
    if let Ok(result) = serde_json::from_value::<crate::turn_items::CallToolResult>(value.clone()) {
        return Ok(result);
    }
    let text = match &value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => "Tool call completed".to_string(),
        other => other.to_string(),
    };
    Ok(crate::turn_items::CallToolResult {
        content: vec![serde_json::json!({ "type": "text", "text": text })],
        structured_content: (!value.is_null()).then_some(value),
        is_error: Some(false),
        meta: Some(serde_json::json!({
            "artifact_label": artifact_label,
            "surface": match item.surface {
                ToolCallSurface::WorkerEnvironment => "worker_environment",
                ToolCallSurface::Dynamic => "dynamic",
                ToolCallSurface::Mcp => "mcp",
            }
        })),
    })
}
