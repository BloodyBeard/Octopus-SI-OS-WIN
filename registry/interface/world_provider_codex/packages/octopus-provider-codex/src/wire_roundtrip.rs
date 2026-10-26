use super::*;

use octopus_world_terminal_client::{
    ApprovalRequest, ElicitationRequest, GoalState, GoalStatusItem, HookCatalog, HookItemStatus,
    HookRunItem, HookTrustState, OutboundRenderKind, OutboundResolved, ToolCallItem,
    WebSearchActionItem, WebSearchItem, classify_outbound_artifact, emit_approval_response,
    emit_elicitation_response, outbound_resolved,
};

/// A fixture path absolute on this platform: `/work/repo` stays as is on
/// Unix and becomes `C:\work\repo` on Windows, where a path without a drive
/// is not absolute and does not deserialize as an `AbsolutePathBuf`.
fn native(unix_path: &str) -> String {
    octopus_util_abspath::test_support::test_path_buf(unix_path).display().to_string()
}

fn client_envelope_for(
    dir: &std::path::Path,
    output: &CodexTurnOutput,
) -> octopus_interfaces::InterfaceInvocationEnvelope {
    let bytes = serde_json::to_vec(&output.value).expect("provider output encodes");
    let artifact_file = InterfaceArtifactFile::write(dir, &output.label, &bytes)
        .expect("provider output artifact materializes");
    let content = octopus_ledger::ContentHash::of_bytes(&bytes);
    octopus_interfaces::InterfaceInvocationEnvelope {
        artifact: octopus_interfaces::ArtifactRef {
            id: octopus_descriptors::ArtifactId::new(format!(
                "{}:{}",
                output.label,
                content.to_hex()
            )),
            content,
        },
        artifact_file,
        artifact_binding_payloads: Vec::new(),
        metadata: None,
    }
}

fn payload_bytes(output: &CodexTurnOutput) -> Vec<u8> {
    serde_json::to_vec(&output.value).expect("provider output encodes")
}

#[test]
fn emitted_labels_match_the_operator_projection_vocabulary() {
    use octopus_app_server_interface as projection;
    assert_eq!(
        AGENT_MESSAGE_DELTA_LABEL,
        projection::AGENT_MESSAGE_DELTA_LABEL
    );
    assert_eq!(TURN_EXEC_COMMAND_LABEL, projection::TURN_EXEC_COMMAND_LABEL);
    assert_eq!(TURN_REASONING_LABEL, projection::TURN_REASONING_LABEL);
    assert_eq!(TURN_PLAN_UPDATE_LABEL, projection::TURN_PLAN_UPDATE_LABEL);
    assert_eq!(TOOL_CALL_LABEL, projection::TOOL_CALL_LABEL);
    assert_eq!(TURN_ERROR_LABEL, projection::TURN_ERROR_LABEL);
    assert_eq!(TURN_WEB_SEARCH_LABEL, projection::TURN_WEB_SEARCH_LABEL);
    assert_eq!(GOAL_STATUS_LABEL, projection::GOAL_STATUS_LABEL);
    assert_eq!(HOOK_RESULT_LABEL, projection::HOOK_RESULT_LABEL);
    assert_eq!(HOOK_CATALOG_LABEL, projection::HOOK_CATALOG_LABEL);
    assert_eq!(
        OPERATOR_APPROVAL_REQUEST_LABEL,
        projection::OPERATOR_APPROVAL_REQUEST_LABEL
    );
    assert_eq!(
        OPERATOR_ELICITATION_REQUEST_LABEL,
        projection::OPERATOR_ELICITATION_REQUEST_LABEL
    );
    assert_eq!(
        INTERFACE_ARTIFACT_OUTPUT_LABEL,
        projection::ACT_OBSERVATION_REPLY_LABEL,
        "the terminating text reply is the client's TerminalReply lane"
    );
}

#[test]
fn message_delta_streams_through_the_client_message_lane() {
    let dir = tempfile::tempdir().unwrap();
    let output = CodexTurnOutput::scoped_message_delta(
        0,
        "hello, wor".into(),
        "turn-1".into(),
        "item-1".into(),
        None,
    );
    let envelope = client_envelope_for(dir.path(), &output);
    let (kind, resolved) = outbound_resolved(&envelope).expect("client resolves the delta");
    assert_eq!(kind, OutboundRenderKind::MessageDelta);
    match resolved {
        OutboundResolved::Text(text) => assert_eq!(text, "hello, wor"),
        other => panic!("a delta resolves to streaming text, got {other:?}"),
    }
}

#[test]
fn terminal_reply_resolves_to_the_turn_completed_lane() {
    let dir = tempfile::tempdir().unwrap();
    let output = CodexTurnOutput::final_message(
        "thread-1", "turn-1", "final-1", "the final answer",
        Some(ContextWindowUsage {
            used_tokens: 5000,
            context_window: 272000,
        }),
    );
    assert_eq!(
        output
            .value
            .pointer("/context_window_usage/used_tokens")
            .and_then(serde_json::Value::as_u64),
        Some(5000)
    );
    let envelope = client_envelope_for(dir.path(), &output);
    let (kind, resolved) = outbound_resolved(&envelope).expect("client resolves the reply");
    assert_eq!(kind, OutboundRenderKind::TerminalReply);
    match resolved {
        OutboundResolved::Text(text) => assert_eq!(text, "the final answer"),
        other => panic!("the reply resolves to its text, got {other:?}"),
    }
}

#[test]
fn exec_command_round_trips_into_the_client_exec_item() {
    let item: ThreadItem = serde_json::from_value(serde_json::json!({
        "type": "commandExecution",
        "id": "exec-1",
        "command": "cargo check --workspace",
        "cwd": native("/work/repo"),
        "processId": "proc-42",
        "source": "agent",
        "status": "completed",
        "commandActions": [
            { "type": "read", "command": "cat src/lib.rs", "name": "lib.rs", "path": native("/work/repo/src/lib.rs") },
            { "type": "search", "command": "rg lane", "query": "lane", "path": null }
        ],
        "aggregatedOutput": "Finished dev profile",
        "exitCode": 0,
        "durationMs": 2100
    }))
    .expect("exec item deserializes from its wire form");
    let output = rich_item_output(item).expect("exec items map");
    let dir = tempfile::tempdir().unwrap();
    let envelope = client_envelope_for(dir.path(), &output);
    let (kind, resolved) = outbound_resolved(&envelope).expect("client resolves the exec item");
    assert_eq!(kind, OutboundRenderKind::ExecCommand);
    match resolved {
        OutboundResolved::Exec { item, summary } => {
            assert_eq!(item.item_id, "exec-1");
            assert_eq!(item.command, "cargo check --workspace");
            assert_eq!(item.cwd, native("/work/repo"));
            assert_eq!(
                item.status,
                octopus_world_terminal_client::ExecItemStatus::Completed
            );
            assert_eq!(item.exit_code, Some(0));
            assert_eq!(item.duration_ms, Some(2100));
            assert_eq!(
                item.aggregated_output.as_deref(),
                Some("Finished dev profile")
            );
            assert_eq!(item.process_id.as_deref(), Some("proc-42"));
            assert_eq!(
                item.command_actions,
                vec![
                    octopus_world_terminal_client::CommandActionItem::Read {
                        cmd: "cat src/lib.rs".to_string(),
                        name: "lib.rs".to_string(),
                        path: native("/work/repo/src/lib.rs").to_string(),
                    },
                    octopus_world_terminal_client::CommandActionItem::Search {
                        cmd: "rg lane".to_string(),
                        query: Some("lane".to_string()),
                        path: None,
                    },
                ]
            );
            assert!(
                summary.contains("cargo check"),
                "summary paints the command: {summary}"
            );
        }
        other => panic!("an exec item resolves structurally, got {other:?}"),
    }
}

#[test]
fn reasoning_round_trips_into_the_client_reasoning_item() {
    let output = rich_item_output(ThreadItem::Reasoning {
        id: "reason-1".into(),
        summary: vec!["Plan the change".into()],
        content: vec!["Read the file first".into()],
    })
    .expect("reasoning items map");
    let dir = tempfile::tempdir().unwrap();
    let envelope = client_envelope_for(dir.path(), &output);
    let (kind, resolved) = outbound_resolved(&envelope).expect("client resolves reasoning");
    assert_eq!(kind, OutboundRenderKind::Reasoning);
    match resolved {
        OutboundResolved::Reasoning(item) => {
            assert_eq!(item.item_id, "reason-1");
            assert_eq!(item.summary, vec!["Plan the change"]);
            assert_eq!(item.content, vec!["Read the file first"]);
        }
        other => panic!("a reasoning item resolves structurally, got {other:?}"),
    }
}

#[test]
fn plan_update_round_trips_into_the_client_plan_item() {
    use octopus_appserver_proto::TurnPlanStepStatus;
    let output = plan_update_output(&TurnPlanUpdatedNotification {
        thread_id: "t1".into(),
        turn_id: "u1".into(),
        explanation: Some("Here is the plan".into()),
        plan: vec![TurnPlanStep {
            step: "read the file".into(),
            status: TurnPlanStepStatus::InProgress,
        }],
    });
    let dir = tempfile::tempdir().unwrap();
    let envelope = client_envelope_for(dir.path(), &output);
    let (kind, resolved) = outbound_resolved(&envelope).expect("client resolves the plan");
    assert_eq!(kind, OutboundRenderKind::PlanUpdate);
    match resolved {
        OutboundResolved::Plan(item) => {
            assert_eq!(item.item_id, "u1");
            assert!(item.text.contains("[~] read the file"));
        }
        other => panic!("a plan update resolves structurally, got {other:?}"),
    }
}

#[test]
fn tool_call_round_trips_into_the_client_tool_item() {
    let output = CodexTurnOutput::native_tool_call(
        "artifact.read".into(),
        serde_json::json!({ "path": "notes.md" }),
        "call-7".into(),
    );
    assert_eq!(
        classify_outbound_artifact(&format!("{}:{}", output.label, "x")),
        OutboundRenderKind::ToolCall
    );
    let item =
        ToolCallItem::from_payload(&payload_bytes(&output)).expect("client decodes the call");
    assert_eq!(item.tool, "artifact.read");
    assert_eq!(item.arguments["path"], "notes.md");
    assert_eq!(item.correlation_id, "call-7");
}

#[test]
fn turn_error_payload_fields_are_pinned() {
    let error: octopus_appserver_proto::ErrorNotification =
        serde_json::from_value(serde_json::json!({
            "error": {
                "message": "Your access token could not be refreshed because your refresh token was revoked.",
                "codexErrorInfo": "unauthorized",
                "additionalDetails": null
            },
            "willRetry": false,
            "threadId": "t1",
            "turnId": "u1"
        }))
        .expect("error notification deserializes from its wire form");
    let output = turn_error_output(&error);
    assert_eq!(output.label, TURN_ERROR_LABEL);
    let payload: serde_json::Value = serde_json::from_slice(&payload_bytes(&output)).unwrap();
    assert_eq!(
        payload["message"],
        "Your access token could not be refreshed because your refresh token was revoked."
    );
    assert_eq!(payload["will_retry"], false);
    assert_eq!(
        payload["failure_code"],
        PROVIDER_AUTHENTICATION_FAILURE_CODE
    );

    let dir = tempfile::tempdir().unwrap();
    let terminal = materialize_terminal_turn_error_payload(&dir.path().join("emitted"), "Codex turn failed: Your access token could not be refreshed because your refresh token was revoked.", None, serde_json::Value::Null)
    .expect("terminal turn.error materializes");
    let manifest = terminal.read_manifest().expect("manifest reads");
    assert_eq!(manifest.label, TURN_ERROR_LABEL);
    let payload: serde_json::Value =
        serde_json::from_slice(&terminal.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(
        payload["failure_code"],
        PROVIDER_AUTHENTICATION_FAILURE_CODE
    );
    assert_eq!(payload["will_retry"], false);
}

#[test]
fn incompatible_continuity_materializes_a_terminal_task_diagnostic() {
    let error = parse_codex_continuity(
        &serde_json::json!({
            "schema_version": 1,
            "provider_interface": "world_provider_test",
            "run": "run-test",
            "thread_id": "thread-test",
            "thread_shape_fingerprint": "a".repeat(64),
            "context_generation": 1,
            "static_context_fingerprint": "b".repeat(64),
            "pending": null,
            "interrupted_pending": null,
            "run_boundary": {"kind": "retired"}
        }),
        &InterfaceId::new("world_provider_test"),
        "run-test",
    )
    .expect_err("retired continuity shape fails closed");
    let detail = codex_continuity_incompatibility(&error)
        .expect("malformed continuity has a typed incompatibility diagnostic");
    assert!(detail.contains("unknown field `run_boundary`"));

    let dir = tempfile::tempdir().unwrap();
    let terminal = materialize_incompatible_continuity_turn_error(
        &dir.path().join("emitted"),
        46234,
        detail,
        "original terminal failure",
    )
    .expect("incompatible continuity turn.error materializes without migration");
    let payload: serde_json::Value =
        serde_json::from_slice(&terminal.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(payload["label"], TURN_ERROR_LABEL);
    assert_eq!(payload["will_retry"], false);
    assert!(
        payload["message"]
            .as_str()
            .unwrap()
            .contains("Create a new task")
    );
    assert_eq!(
        payload["additional_details"]["code"],
        PROVIDER_CONTINUITY_INCOMPATIBLE_CODE
    );
    assert_eq!(payload["additional_details"]["source_movement_seq"], 46234);
    assert_eq!(payload.get(CODEX_CONTINUITY_FIELD), None);
}

#[test]
fn web_search_round_trips_into_the_client_web_search_item() {
    let output = web_search_output(
        "ws-1".into(),
        "octopus".into(),
        Some(WebSearchAction::FindInPage {
            url: Some("https://example.com".into()),
            pattern: Some("ledger".into()),
        }),
        None,
    );
    let item = WebSearchItem::from_payload(&payload_bytes(&output)).expect("client decodes");
    assert_eq!(item.item_id, "ws-1");
    assert_eq!(item.query, "octopus");
    assert_eq!(
        item.action,
        Some(WebSearchActionItem::FindInPage {
            url: Some("https://example.com".into()),
            pattern: Some("ledger".into()),
        })
    );
}

#[test]
fn goal_status_round_trips_into_the_client_goal_item() {
    let goal: octopus_appserver_proto::ThreadGoal = serde_json::from_value(serde_json::json!({
        "threadId": "t1",
        "objective": "ship the streaming front",
        "status": "active",
        "tokenBudget": 50000,
        "tokensUsed": 12000,
        "timeUsedSeconds": 90,
        "createdAt": 0,
        "updatedAt": 0
    }))
    .expect("thread goal deserializes from its wire form");
    let output = goal_status_output(&goal);
    let item = GoalStatusItem::from_payload(&payload_bytes(&output)).expect("client decodes");
    assert_eq!(item.objective, "ship the streaming front");
    assert_eq!(item.status, GoalState::Active);
    assert_eq!(item.token_budget, Some(50000));
    assert_eq!(item.tokens_used, 12000);
}

#[test]
fn hook_result_round_trips_into_the_client_hook_run_item() {
    let run: octopus_appserver_proto::HookRunSummary = serde_json::from_value(serde_json::json!({
        "id": "hook-1",
        "eventName": "preToolUse",
        "handlerType": "command",
        "executionMode": "sync",
        "scope": "turn",
        "sourcePath": native("/home/u/.octopus/hooks.toml"),
        "source": "user",
        "displayOrder": 1,
        "status": "blocked",
        "statusMessage": "denied by guard",
        "startedAt": 0,
        "completedAt": null,
        "durationMs": null,
        "entries": [{ "kind": "stop", "text": "not allowed" }]
    }))
    .expect("hook run summary deserializes from its wire form");
    let output = hook_result_output(&run, "completed", Some("turn-1"));
    let bytes = payload_bytes(&output);
    let wire: serde_json::Value = serde_json::from_slice(&bytes).expect("projection is JSON");
    assert_eq!(wire["notification"], "completed");
    assert_eq!(wire["turn_id"], "turn-1");
    assert_eq!(wire["handler_type"], "command");
    assert_eq!(wire["execution_mode"], "sync");
    assert_eq!(wire["scope"], "turn");
    assert_eq!(wire["source"], "user");
    assert_eq!(wire["display_order"], 1);
    let item = HookRunItem::from_payload(&bytes).expect("client decodes");
    assert_eq!(item.id, "hook-1");
    assert_eq!(item.event, "PreToolUse");
    assert_eq!(item.status, HookItemStatus::Blocked);
    assert_eq!(item.status_message.as_deref(), Some("denied by guard"));
    assert_eq!(item.outputs.len(), 1);
    assert_eq!(item.outputs[0].text, "not allowed");
}

#[test]
fn hook_catalog_round_trips_into_the_client_catalog() {
    let entries: Vec<HooksListEntry> = serde_json::from_value(serde_json::json!([{
        "cwd": native("/work/repo"),
        "hooks": [{
            "key": "user:PreToolUse:0",
            "eventName": "preToolUse",
            "handlerType": "command",
            "matcher": "coder.exec",
            "command": "./guard.sh",
            "async": true,
            "timeoutSec": 30,
            "statusMessage": null,
            "additionalContextLimit": 2500,
            "sourcePath": native("/home/u/.octopus/hooks.toml"),
            "source": "user",
            "pluginId": null,
            "displayOrder": 1,
            "enabled": true,
            "isManaged": false,
            "currentHash": "abc123",
            "trustStatus": "trusted"
        }, {
            "key": "project:SessionEnd:1",
            "eventName": "sessionEnd",
            "handlerType": "mcpTool",
            "server": "audit",
            "tool": "record",
            "matcher": null,
            "timeoutSec": 1,
            "statusMessage": "recording",
            "additionalContextLimit": null,
            "sourcePath": native("/work/repo/.codex/hooks.json"),
            "source": "project",
            "pluginId": null,
            "displayOrder": 2,
            "enabled": false,
            "isManaged": false,
            "currentHash": "def456",
            "trustStatus": "untrusted"
        }],
        "warnings": ["one warning"],
        "errors": [{ "path": native("/bad/file.toml"), "message": "parse failed" }]
    }]))
    .expect("hooks/list entries deserialize");
    let output = hook_catalog_output(&entries);
    assert_eq!(output.label, HOOK_CATALOG_LABEL);
    let bytes = payload_bytes(&output);
    let wire: serde_json::Value = serde_json::from_slice(&bytes).expect("projection is JSON");
    assert_eq!(wire["cwds"], serde_json::json!([native("/work/repo")]));
    assert_eq!(wire["hooks"][0]["handler_type"], "command");
    assert_eq!(wire["hooks"][0]["execution_mode"], "async");
    assert_eq!(wire["hooks"][0]["additional_context_limit"], 2500);
    assert_eq!(wire["hooks"][1]["event"], "SessionEnd");
    assert_eq!(wire["hooks"][1]["handler_type"], "mcp_tool");
    assert_eq!(wire["hooks"][1]["handler"]["server"], "audit");
    assert_eq!(wire["hooks"][1]["handler"]["tool"], "record");
    assert_eq!(wire["warning_entries"][0]["cwd"], native("/work/repo"));
    assert_eq!(wire["errors"][0]["cwd"], native("/work/repo"));
    let catalog = HookCatalog::from_payload(&bytes).expect("client decodes");
    assert_eq!(catalog.hooks.len(), 2);
    let hook = &catalog.hooks[0];
    assert_eq!(hook.key, "user:PreToolUse:0");
    assert_eq!(hook.event, "PreToolUse");
    assert_eq!(hook.source, "user");
    assert_eq!(hook.timeout_sec, 30);
    assert_eq!(hook.trust, HookTrustState::Trusted);
    assert!(
        hook.is_active(),
        "an enabled+trusted hook is active on the client side"
    );
    assert_eq!(catalog.warnings, vec!["one warning"]);
    assert_eq!(catalog.errors.len(), 1);
    assert_eq!(catalog.errors[0].message, "parse failed");
    let error = hook_catalog_error_output("method unavailable");
    assert_eq!(error.label, HOOK_CATALOG_ERROR_LABEL);
    let error: serde_json::Value =
        serde_json::from_slice(&payload_bytes(&error)).expect("error projection is JSON");
    assert_eq!(error["operation"], "hooks/list");
    assert_eq!(error["error"]["message"], "method unavailable");
}

#[test]
fn approval_request_and_response_round_trip() {
    let exec_approval: ServerRequest = serde_json::from_value(serde_json::json!({
        "method": "item/commandExecution/requestApproval",
        "id": 42,
        "params": {
            "threadId": "t1",
            "turnId": "u1",
            "itemId": "i1",
            "startedAtMs": 0,
            "command": "rm -rf /tmp/x",
            "reason": "cleanup"
        }
    }))
    .expect("approval request deserializes");
    let output = classify_operator_request(&exec_approval, &test_operator_continuity_config())
        .expect("classify does not error")
        .expect("approvals have an operator home");
    assert_eq!(
        classify_outbound_artifact(&format!("{}:{}", output.label, "x")),
        OutboundRenderKind::ApprovalRequest
    );
    let request = ApprovalRequest::from_payload(&payload_bytes(&output)).expect("client decodes");
    assert_eq!(request.request_id, "42");
    assert_eq!(request.kind, "exec");
    assert_eq!(request.title, "rm -rf /tmp/x");
    assert_eq!(request.detail["reason"], "cleanup");

    let dir = tempfile::tempdir().unwrap();
    let built =
        emit_approval_response(dir.path(), &request.request_id, serde_json::json!("accept"))
            .expect("response emits");
    let manifest = built.artifact_file.read_manifest().unwrap();
    assert_eq!(
        manifest.label,
        octopus_app_server_interface::OPERATOR_APPROVAL_RESPONSE_LABEL
    );
    let body: serde_json::Value =
        serde_json::from_slice(&built.artifact_file.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(body["operator_approval_response"]["request_id"], "42");
    assert_eq!(body["operator_approval_response"]["decision"], "accept");
}

#[test]
fn elicitation_request_and_response_round_trip() {
    let elicitation: ServerRequest = serde_json::from_value(serde_json::json!({
        "method": "mcpServer/elicitation/request",
        "id": "elic-7",
        "params": {
            "threadId": "t1",
            "serverName": "docs",
            "mode": "openai/form",
            "message": "Which environment?",
            "requestedSchema": { "type": "object", "properties": { "env": { "type": "string" } } }
        }
    }))
    .expect("elicitation request deserializes");
    let output = classify_operator_request(&elicitation, &test_operator_continuity_config())
        .expect("classify does not error")
        .expect("elicitations have an operator home");
    assert_eq!(
        classify_outbound_artifact(&format!("{}:{}", output.label, "x")),
        OutboundRenderKind::ElicitationRequest
    );
    let request =
        ElicitationRequest::from_payload(&payload_bytes(&output)).expect("client decodes");
    assert_eq!(request.request_id, "elic-7");
    assert_eq!(request.message, "Which environment?");
    assert_eq!(request.requested_schema["type"], "object");

    let dir = tempfile::tempdir().unwrap();
    let built = emit_elicitation_response(
        dir.path(),
        &request.request_id,
        serde_json::json!({ "accept": true, "content": { "env": "dev" } }),
    )
    .expect("answer emits");
    let manifest = built.artifact_file.read_manifest().unwrap();
    assert_eq!(
        manifest.label,
        octopus_app_server_interface::OPERATOR_ELICITATION_RESPONSE_LABEL
    );
    let body: serde_json::Value =
        serde_json::from_slice(&built.artifact_file.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(
        body["operator_elicitation_response"]["request_id"],
        "elic-7"
    );
    assert_eq!(
        body["operator_elicitation_response"]["answer"]["content"]["env"],
        "dev"
    );
}
