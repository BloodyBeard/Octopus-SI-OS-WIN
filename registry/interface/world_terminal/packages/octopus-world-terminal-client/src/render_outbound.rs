use octopus_interfaces::InterfaceInvocationEnvelope;
use octopus_world_terminal_render::render_markdown_text;

use octopus_app_server_interface::{
    ACT_OBSERVATION_REPLY_LABEL, AGENT_MESSAGE_DELTA_LABEL, COMMAND_EXECUTION_COMPLETED_LABEL,
    COMMAND_EXECUTION_STARTED_LABEL, HOOK_CATALOG_LABEL, HOOK_RESULT_LABEL,
    OPERATOR_APPROVAL_REQUEST_LABEL, OPERATOR_ELICITATION_REQUEST_LABEL,
    OPERATOR_USER_INPUT_REQUEST_LABEL, TOOL_CALL_LABEL, TURN_ERROR_LABEL, TURN_EXEC_COMMAND_LABEL,
    TURN_PLAN_UPDATE_LABEL, TURN_REASONING_LABEL, TURN_TOKEN_USAGE_LABEL, TURN_USER_MESSAGE_LABEL,
};

use crate::hook_items::{HookCatalog, HookRunItem};
use crate::operator_forms::UserInputRequest;
use crate::rich_items::{
    ExecCommandItem, PlanUpdateItem, ReasoningItem, TokenUsageItem, ToolCallItem, UserMessageItem,
};
use crate::rich_requests::{ApprovalRequest, ElicitationRequest, OperatorRequestResolved};

const TURN_DYNAMIC_TOOL_CALL_LABEL: &str = "turn.dynamic_tool_call";
const TURN_MCP_TOOL_CALL_LABEL: &str = "turn.mcp.tool_call";
const TURN_OPERATOR_REQUEST_RESOLVED_LABEL: &str = "turn.operator_request.resolved";
const TURN_AUTH_RECOVERY_STARTED_LABEL: &str = "turn.auth_recovery.started";
const TURN_AUTH_RECOVERY_COMPLETED_LABEL: &str = "turn.auth_recovery.completed";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboundRenderKind {
    MessageDelta,
    CommandExecution,
    TerminalReply,
    ApprovalRequest,
    ElicitationRequest,
    UserInputRequest,
    OperatorRequestResolved,
    HookResult,
    ToolCall,
    DynamicToolCall,
    McpToolCall,
    ExecCommand,
    Reasoning,
    TokenUsage,
    PlanUpdate,
    UserMessage,
    ProviderError,
    AuthRecoveryStarted,
    AuthRecoveryCompleted,
    HookCatalog,
    Other,
}

#[derive(Clone, Debug)]
pub struct RenderedOutbound {
    pub kind: OutboundRenderKind,
    pub text: ratatui::text::Text<'static>,
}

pub fn classify_outbound_artifact(artifact_id: &str) -> OutboundRenderKind {
    let label = artifact_id.split_once(':').map_or(artifact_id, |(l, _)| l);
    match label {
        AGENT_MESSAGE_DELTA_LABEL => OutboundRenderKind::MessageDelta,
        COMMAND_EXECUTION_STARTED_LABEL | COMMAND_EXECUTION_COMPLETED_LABEL => {
            OutboundRenderKind::CommandExecution
        }
        ACT_OBSERVATION_REPLY_LABEL => OutboundRenderKind::TerminalReply,
        OPERATOR_APPROVAL_REQUEST_LABEL => OutboundRenderKind::ApprovalRequest,
        OPERATOR_ELICITATION_REQUEST_LABEL => OutboundRenderKind::ElicitationRequest,
        OPERATOR_USER_INPUT_REQUEST_LABEL => OutboundRenderKind::UserInputRequest,
        TURN_OPERATOR_REQUEST_RESOLVED_LABEL => OutboundRenderKind::OperatorRequestResolved,
        HOOK_RESULT_LABEL => OutboundRenderKind::HookResult,
        TOOL_CALL_LABEL => OutboundRenderKind::ToolCall,
        TURN_DYNAMIC_TOOL_CALL_LABEL => OutboundRenderKind::DynamicToolCall,
        TURN_MCP_TOOL_CALL_LABEL => OutboundRenderKind::McpToolCall,
        TURN_EXEC_COMMAND_LABEL => OutboundRenderKind::ExecCommand,
        TURN_REASONING_LABEL => OutboundRenderKind::Reasoning,
        TURN_TOKEN_USAGE_LABEL => OutboundRenderKind::TokenUsage,
        TURN_PLAN_UPDATE_LABEL => OutboundRenderKind::PlanUpdate,
        TURN_USER_MESSAGE_LABEL => OutboundRenderKind::UserMessage,
        TURN_ERROR_LABEL => OutboundRenderKind::ProviderError,
        TURN_AUTH_RECOVERY_STARTED_LABEL => OutboundRenderKind::AuthRecoveryStarted,
        TURN_AUTH_RECOVERY_COMPLETED_LABEL => OutboundRenderKind::AuthRecoveryCompleted,
        HOOK_CATALOG_LABEL => OutboundRenderKind::HookCatalog,
        _ => OutboundRenderKind::Other,
    }
}

pub fn render_outbound_artifact(
    envelope: &InterfaceInvocationEnvelope,
) -> Result<RenderedOutbound, String> {
    let kind = classify_outbound_artifact(envelope.artifact.id.as_str());
    let bytes = envelope
        .read_artifact_payload()
        .map_err(|e| format!("outbound artifact payload unreadable: {e}"))?;
    let display = display_text_for_kind(kind, &bytes);
    Ok(RenderedOutbound {
        kind,
        text: render_markdown_text(&display),
    })
}

pub fn outbound_display(
    envelope: &InterfaceInvocationEnvelope,
) -> Result<(OutboundRenderKind, String), String> {
    let kind = classify_outbound_artifact(envelope.artifact.id.as_str());
    let bytes = envelope
        .read_artifact_payload()
        .map_err(|e| format!("outbound artifact payload unreadable: {e}"))?;
    Ok((kind, display_text_for_kind(kind, &bytes)))
}

#[derive(Clone, Debug)]
pub enum OutboundResolved {
    Text(String),
    Invalid(String),
    Approval(ApprovalRequest),
    Elicitation(ElicitationRequest),
    UserInput(UserInputRequest),
    OperatorRequestResolved(OperatorRequestResolved),
    Hook(HookRunItem),
    Tool(ToolCallItem),
    HookCatalog(HookCatalog),
    Exec {
        item: ExecCommandItem,
        summary: String,
    },
    Reasoning(ReasoningItem),
    Plan(PlanUpdateItem),
    Tokens(TokenUsageItem),
    UserMessage(UserMessageItem),
}

pub fn outbound_resolved(
    envelope: &InterfaceInvocationEnvelope,
) -> Result<(OutboundRenderKind, OutboundResolved), String> {
    let bytes = envelope
        .read_artifact_payload()
        .map_err(|e| format!("outbound artifact payload unreadable: {e}"))?;
    Ok(resolve_outbound_payload(
        envelope.artifact.id.as_str(),
        &bytes,
    ))
}

pub fn resolve_outbound_payload(
    artifact_id: &str,
    bytes: &[u8],
) -> (OutboundRenderKind, OutboundResolved) {
    let kind = classify_outbound_artifact(artifact_id);
    let resolved = match kind {
        OutboundRenderKind::ApprovalRequest => match ApprovalRequest::from_payload(bytes) {
            Ok(item) => OutboundResolved::Approval(item),
            Err(error) => invalid_typed_payload(artifact_id, error),
        },
        OutboundRenderKind::ElicitationRequest => match ElicitationRequest::from_payload(bytes) {
            Ok(item) => OutboundResolved::Elicitation(item),
            Err(error) => invalid_typed_payload(artifact_id, error),
        },
        OutboundRenderKind::UserInputRequest => match UserInputRequest::from_payload(bytes) {
            Ok(item) => OutboundResolved::UserInput(item),
            Err(error) => invalid_typed_payload(artifact_id, error),
        },
        OutboundRenderKind::OperatorRequestResolved => {
            match OperatorRequestResolved::from_payload(bytes) {
                Ok(item) => OutboundResolved::OperatorRequestResolved(item),
                Err(error) => invalid_typed_payload(artifact_id, error),
            }
        }
        OutboundRenderKind::HookResult => match HookRunItem::from_payload(bytes) {
            Ok(item) => OutboundResolved::Hook(item),
            Err(error) => invalid_typed_payload(artifact_id, error),
        },
        OutboundRenderKind::ToolCall => match ToolCallItem::from_payload(bytes) {
            Ok(item) => OutboundResolved::Tool(item),
            Err(error) => invalid_typed_payload(artifact_id, error),
        },
        OutboundRenderKind::DynamicToolCall => match ToolCallItem::from_dynamic_payload(bytes) {
            Ok(item) => OutboundResolved::Tool(item),
            Err(error) => invalid_typed_payload(artifact_id, error),
        },
        OutboundRenderKind::McpToolCall => match ToolCallItem::from_mcp_payload(bytes) {
            Ok(item) => OutboundResolved::Tool(item),
            Err(error) => invalid_typed_payload(artifact_id, error),
        },
        OutboundRenderKind::HookCatalog => match HookCatalog::from_payload(bytes) {
            Ok(item) => OutboundResolved::HookCatalog(item),
            Err(error) => invalid_typed_payload(artifact_id, error),
        },
        OutboundRenderKind::ExecCommand => match ExecCommandItem::from_payload(&bytes) {
            Ok(item) => OutboundResolved::Exec {
                summary: exec_command_summary(&bytes),
                item,
            },
            Err(_) => OutboundResolved::Text(display_text_for_kind(kind, &bytes)),
        },
        OutboundRenderKind::Reasoning => match ReasoningItem::from_payload(&bytes) {
            Ok(item) => OutboundResolved::Reasoning(item),
            Err(_) => OutboundResolved::Text(display_text_for_kind(kind, &bytes)),
        },
        OutboundRenderKind::PlanUpdate => match PlanUpdateItem::from_payload(&bytes) {
            Ok(item) => OutboundResolved::Plan(item),
            Err(_) => OutboundResolved::Text(display_text_for_kind(kind, &bytes)),
        },
        OutboundRenderKind::TokenUsage => match TokenUsageItem::from_payload(&bytes) {
            Ok(item) => OutboundResolved::Tokens(item),
            Err(_) => OutboundResolved::Text(display_text_for_kind(kind, &bytes)),
        },
        OutboundRenderKind::UserMessage => match UserMessageItem::from_payload(&bytes) {
            Ok(item) => OutboundResolved::UserMessage(item),
            Err(_) => OutboundResolved::Text(display_text_for_kind(kind, &bytes)),
        },
        _ => OutboundResolved::Text(display_text_for_kind(kind, &bytes)),
    };
    (kind, resolved)
}

fn invalid_typed_payload(artifact_id: &str, error: impl std::fmt::Display) -> OutboundResolved {
    OutboundResolved::Invalid(format!(
        "terminal rejected typed artifact '{artifact_id}': {error}"
    ))
}

fn display_text_for_kind(kind: OutboundRenderKind, bytes: &[u8]) -> String {
    match kind {
        OutboundRenderKind::ApprovalRequest => approval_request_summary(bytes),
        OutboundRenderKind::ElicitationRequest => elicitation_request_summary(bytes),
        OutboundRenderKind::UserInputRequest => user_input_request_summary(bytes),
        OutboundRenderKind::OperatorRequestResolved => operator_request_resolved_summary(bytes),
        OutboundRenderKind::HookResult => hook_result_summary(bytes),
        OutboundRenderKind::ToolCall
        | OutboundRenderKind::DynamicToolCall
        | OutboundRenderKind::McpToolCall => tool_call_summary(kind, bytes),
        OutboundRenderKind::ExecCommand => exec_command_summary(bytes),
        OutboundRenderKind::Reasoning => reasoning_summary(bytes),
        OutboundRenderKind::TokenUsage => token_usage_summary(bytes),
        OutboundRenderKind::PlanUpdate => plan_update_summary(bytes),
        OutboundRenderKind::UserMessage => user_message_summary(bytes),
        OutboundRenderKind::AuthRecoveryStarted | OutboundRenderKind::AuthRecoveryCompleted => {
            auth_recovery_summary(bytes)
        }
        OutboundRenderKind::MessageDelta
        | OutboundRenderKind::CommandExecution
        | OutboundRenderKind::TerminalReply
        | OutboundRenderKind::ProviderError
        | OutboundRenderKind::Other => display_text_from_payload(bytes),
        OutboundRenderKind::HookCatalog => hook_catalog_summary(bytes),
    }
}

fn auth_recovery_summary(bytes: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| display_text_from_payload(bytes))
}

fn approval_request_summary(bytes: &[u8]) -> String {
    let Some(object) = payload_object(bytes) else {
        return display_text_from_payload(bytes);
    };
    let title = str_field(&object, "title");
    let kind = str_field(&object, "kind");
    let header = match (kind.is_empty(), title.is_empty()) {
        (false, false) => format!("Approval requested ({kind}): {title}"),
        (false, true) => format!("Approval requested ({kind})"),
        (true, false) => format!("Approval requested: {title}"),
        (true, true) => "Approval requested".to_string(),
    };
    append_detail(header, object.get("detail"))
}

fn elicitation_request_summary(bytes: &[u8]) -> String {
    let Some(object) = payload_object(bytes) else {
        return display_text_from_payload(bytes);
    };
    let message = str_field(&object, "message");
    let header = if message.is_empty() {
        "Input requested".to_string()
    } else {
        format!("Input requested: {message}")
    };
    append_detail(header, object.get("requested_schema"))
}

fn user_input_request_summary(bytes: &[u8]) -> String {
    let Ok(item) = UserInputRequest::from_payload(bytes) else {
        return display_text_from_payload(bytes);
    };
    let count = item.questions.len();
    let first = item
        .questions
        .first()
        .map(|question| question.question.as_str())
        .filter(|question| !question.is_empty());
    match (count, first) {
        (0, _) => "Input requested".to_string(),
        (1, Some(question)) => format!("Input requested: {question}"),
        (1, None) => "Input requested: 1 question".to_string(),
        (count, _) => format!("Input requested: {count} questions"),
    }
}

fn operator_request_resolved_summary(bytes: &[u8]) -> String {
    match OperatorRequestResolved::from_payload(bytes) {
        Ok(item) => format!("Operator request {} resolved", item.request_id),
        Err(_) => display_text_from_payload(bytes),
    }
}

fn hook_result_summary(bytes: &[u8]) -> String {
    let Some(object) = payload_object(bytes) else {
        return display_text_from_payload(bytes);
    };
    let event = str_field(&object, "event");
    let status = str_field(&object, "status");
    let message = str_field(&object, "message");
    let mut header = String::from("Hook");
    if !event.is_empty() {
        header.push_str(&format!(" {event}"));
    }
    if !status.is_empty() {
        header.push_str(&format!(" [{status}]"));
    }
    if !message.is_empty() {
        header.push_str(&format!(": {message}"));
    }
    append_detail(header, object.get("output"))
}

fn tool_call_summary(kind: OutboundRenderKind, bytes: &[u8]) -> String {
    let parsed = match kind {
        OutboundRenderKind::DynamicToolCall => ToolCallItem::from_dynamic_payload(bytes),
        OutboundRenderKind::McpToolCall => ToolCallItem::from_mcp_payload(bytes),
        _ => ToolCallItem::from_payload(bytes),
    };
    let Ok(item) = parsed else {
        return display_text_from_payload(bytes);
    };
    let header = if item.tool.is_empty() {
        "Tool call".to_string()
    } else {
        format!("Tool call: {}", item.tool)
    };
    let mut details = vec![item.arguments];
    if let Some(result) = item.result {
        details.push(result);
    }
    if let Some(error) = item.error {
        details.push(serde_json::Value::String(error));
    }
    append_detail(header, Some(&serde_json::Value::Array(details)))
}

fn hook_catalog_summary(bytes: &[u8]) -> String {
    match HookCatalog::from_payload(bytes) {
        Ok(catalog) => format!("Hook catalog: {} hook(s)", catalog.hooks.len()),
        Err(_) => display_text_from_payload(bytes),
    }
}

fn exec_command_summary(bytes: &[u8]) -> String {
    let Ok(item) = ExecCommandItem::from_payload(bytes) else {
        return display_text_from_payload(bytes);
    };
    let command = if item.command.is_empty() {
        String::new()
    } else {
        let parts =
            octopus_world_terminal_render::exec_command::split_command_string(&item.command);
        octopus_world_terminal_render::exec_command::strip_bash_lc_and_escape(&parts)
    };

    let mut header = String::from("$");
    if !command.is_empty() {
        header.push(' ');
        header.push_str(&command);
    }
    let mut status_bits: Vec<String> = vec![item.status.label().to_string()];
    if let Some(code) = item.exit_code {
        status_bits.push(format!("exit {code}"));
    }
    if let Some(ms) = item.duration_ms {
        status_bits.push(format_duration_ms(ms));
    }
    header.push_str(&format!(" [{}]", status_bits.join(" · ")));
    if !item.cwd.is_empty() {
        header.push_str(&format!("\nin {}", item.cwd));
    }

    match item.aggregated_output {
        Some(output) if !output.is_empty() => format!("{header}\n\n```\n{output}\n```"),
        _ => header,
    }
}

fn reasoning_summary(bytes: &[u8]) -> String {
    let Ok(item) = ReasoningItem::from_payload(bytes) else {
        return display_text_from_payload(bytes);
    };
    let mut out = String::from("Reasoning");
    for line in &item.summary {
        if !line.is_empty() {
            out.push_str(&format!("\n\n_{line}_"));
        }
    }
    for block in &item.content {
        if !block.is_empty() {
            out.push_str(&format!("\n\n{block}"));
        }
    }
    out
}

fn token_usage_summary(bytes: &[u8]) -> String {
    let Ok(item) = TokenUsageItem::from_payload(bytes) else {
        return display_text_from_payload(bytes);
    };
    let mut out = format!(
        "Tokens: {} total ({} in · {} out)",
        item.total.total_tokens, item.total.input_tokens, item.total.output_tokens
    );
    if item.total.cached_input_tokens > 0 {
        out.push_str(&format!(" · {} cached", item.total.cached_input_tokens));
    }
    if item.last.total_tokens > 0 {
        out.push_str(&format!(" · last turn {}", item.last.total_tokens));
    }
    if let Some(window) = item.model_context_window {
        if window > 0 {
            let pct =
                (item.total.total_tokens.max(0) as f64 / window as f64 * 100.0).round() as i64;
            out.push_str(&format!(" · {pct}% of {window} window"));
        }
    }
    out
}

fn plan_update_summary(bytes: &[u8]) -> String {
    let Ok(item) = PlanUpdateItem::from_payload(bytes) else {
        return display_text_from_payload(bytes);
    };
    if item.text.is_empty() {
        "Plan updated".to_string()
    } else {
        format!("Plan\n\n{}", item.text)
    }
}

fn user_message_summary(bytes: &[u8]) -> String {
    let Ok(item) = UserMessageItem::from_payload(bytes) else {
        return display_text_from_payload(bytes);
    };
    let mut text = String::new();
    let mut image_count = 0usize;
    for input in &item.items {
        match input {
            crate::TerminalUserInput::Text { text: chunk, .. } => text.push_str(chunk),
            crate::TerminalUserInput::Image { .. }
            | crate::TerminalUserInput::LocalImage { .. } => {
                image_count += 1;
            }
            crate::TerminalUserInput::Skill { .. } | crate::TerminalUserInput::Mention { .. } => {}
        }
    }
    let mut out = String::from("User");
    if !text.is_empty() {
        out.push_str(&format!(": {text}"));
    }
    if image_count > 0 {
        out.push_str(&format!(" [{image_count} image(s)]"));
    }
    out
}

fn format_duration_ms(ms: i64) -> String {
    if ms < 0 {
        return format!("{ms}ms");
    }
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let secs = ms as f64 / 1000.0;
    if secs < 60.0 {
        return format!("{secs:.1}s");
    }
    let whole = ms / 1000;
    format!("{}m{:02}s", whole / 60, whole % 60)
}

fn payload_object(bytes: &[u8]) -> Option<serde_json::Map<String, serde_json::Value>> {
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(serde_json::Value::Object(map)) => Some(map),
        _ => None,
    }
}

fn str_field(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    object
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn append_detail(header: String, detail: Option<&serde_json::Value>) -> String {
    match detail {
        None | Some(serde_json::Value::Null) => header,
        Some(serde_json::Value::String(text)) if !text.is_empty() => format!("{header}\n\n{text}"),
        Some(value) => {
            let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
            format!("{header}\n\n```\n{pretty}\n```")
        }
    }
}

pub(crate) fn display_text_from_payload(bytes: &[u8]) -> String {
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(value) => {
            if let Some(text) = value
                .get("text")
                .or_else(|| value.get("prompt"))
                .and_then(serde_json::Value::as_str)
            {
                return text.to_string();
            }
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        }
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_labels_into_lanes() {
        assert_eq!(
            classify_outbound_artifact("agent.message.delta:abc"),
            OutboundRenderKind::MessageDelta
        );
        assert_eq!(
            classify_outbound_artifact("command_execution.started:abc"),
            OutboundRenderKind::CommandExecution
        );
        assert_eq!(
            classify_outbound_artifact("command_execution.completed:abc"),
            OutboundRenderKind::CommandExecution
        );
        assert_eq!(
            classify_outbound_artifact("interface.artifact.output:abc"),
            OutboundRenderKind::TerminalReply
        );
        assert_eq!(
            classify_outbound_artifact("turn.auth_recovery.started:abc"),
            OutboundRenderKind::AuthRecoveryStarted
        );
        assert_eq!(
            classify_outbound_artifact("turn.auth_recovery.completed:abc"),
            OutboundRenderKind::AuthRecoveryCompleted
        );
        assert_eq!(
            classify_outbound_artifact("operator.approval.request:abc"),
            OutboundRenderKind::ApprovalRequest
        );
        assert_eq!(
            classify_outbound_artifact("operator.elicitation.request:abc"),
            OutboundRenderKind::ElicitationRequest
        );
        assert_eq!(
            classify_outbound_artifact("operator.user_input.request:abc"),
            OutboundRenderKind::UserInputRequest
        );
        assert_eq!(
            classify_outbound_artifact("turn.operator_request.resolved:abc"),
            OutboundRenderKind::OperatorRequestResolved
        );
        assert_eq!(
            classify_outbound_artifact("hook.result:abc"),
            OutboundRenderKind::HookResult
        );
        assert_eq!(
            classify_outbound_artifact("tool.call:abc"),
            OutboundRenderKind::ToolCall
        );
        assert_eq!(
            classify_outbound_artifact("turn.dynamic_tool_call:abc"),
            OutboundRenderKind::DynamicToolCall
        );
        assert_eq!(
            classify_outbound_artifact("turn.mcp.tool_call:abc"),
            OutboundRenderKind::McpToolCall
        );
        assert_eq!(
            classify_outbound_artifact("turn.exec.command:abc"),
            OutboundRenderKind::ExecCommand
        );
        assert_eq!(
            classify_outbound_artifact("turn.reasoning:abc"),
            OutboundRenderKind::Reasoning
        );
        assert_eq!(
            classify_outbound_artifact("turn.token_usage:abc"),
            OutboundRenderKind::TokenUsage
        );
        assert_eq!(
            classify_outbound_artifact("turn.plan.update:abc"),
            OutboundRenderKind::PlanUpdate
        );
        assert_eq!(
            classify_outbound_artifact("turn.user_message:abc"),
            OutboundRenderKind::UserMessage
        );
        assert_eq!(
            classify_outbound_artifact("turn.error:abc"),
            OutboundRenderKind::ProviderError
        );
        assert_eq!(
            classify_outbound_artifact("hook.catalog:abc"),
            OutboundRenderKind::HookCatalog
        );
        assert_eq!(
            classify_outbound_artifact("some.future.label:abc"),
            OutboundRenderKind::Other
        );
        assert_eq!(
            classify_outbound_artifact("agent.message.delta"),
            OutboundRenderKind::MessageDelta
        );
    }

    #[test]
    fn decodes_text_prompt_json_and_raw_payloads() {
        assert_eq!(display_text_from_payload(br#"{"text":"hello"}"#), "hello");
        assert_eq!(
            display_text_from_payload(br#"{"prompt":"do the thing"}"#),
            "do the thing"
        );
        let pretty = display_text_from_payload(br#"{"other":1}"#);
        assert!(pretty.contains("\"other\""));
        assert_eq!(
            display_text_from_payload(b"plain terminal line"),
            "plain terminal line"
        );
    }

    #[test]
    fn paints_rich_lanes_from_their_own_payload_vocabulary() {
        let approval = display_text_for_kind(
            OutboundRenderKind::ApprovalRequest,
            br#"{"request_id":"c1","kind":"exec","title":"rm -rf /x","detail":{"cwd":"/w"}}"#,
        );
        assert!(approval.contains("Approval requested (exec): rm -rf /x"));
        assert!(approval.contains("cwd"));

        let elic = display_text_for_kind(
            OutboundRenderKind::ElicitationRequest,
            br#"{"request_id":"e1","message":"Which env?","requested_schema":{"type":"string"}}"#,
        );
        assert!(elic.contains("Input requested: Which env?"));

        let hook = display_text_for_kind(
            OutboundRenderKind::HookResult,
            br#"{"event":"PreToolUse","status":"blocked","message":"denied"}"#,
        );
        assert!(hook.contains("Hook PreToolUse [blocked]: denied"));

        let tool = display_text_for_kind(
            OutboundRenderKind::ToolCall,
            br#"{"tool":"coder.exec","arguments":{"cmd":"ls"},"correlation_id":"x"}"#,
        );
        assert!(tool.contains("Tool call: coder.exec"));
        assert!(tool.contains("cmd"));

        let input = display_text_for_kind(
            OutboundRenderKind::UserInputRequest,
            br#"{"request_id":"u1","questions":[{"id":"q1","question":"Which root?"}]}"#,
        );
        assert!(input.contains("Which root?"));

        let odd = display_text_for_kind(OutboundRenderKind::ApprovalRequest, b"not json");
        assert_eq!(odd, "not json");
    }

    #[test]
    fn resolves_operator_hook_and_tool_artifacts_as_typed_items() {
        assert!(matches!(
            resolve_outbound_payload(
                "operator.approval.request:x",
                br#"{"request_id":"a1","kind":"exec","title":"pwd","detail":{},"method":"item/commandExecution/requestApproval"}"#,
            ),
            (
                OutboundRenderKind::ApprovalRequest,
                OutboundResolved::Approval(_)
            )
        ));
        assert!(matches!(
            resolve_outbound_payload(
                "hook.result:x",
                br#"{"id":"h1","event":"PreToolUse","status":"completed","outputs":[]}"#,
            ),
            (OutboundRenderKind::HookResult, OutboundResolved::Hook(_))
        ));
        assert!(matches!(
            resolve_outbound_payload(
                "turn.mcp.tool_call:x",
                br#"{"id":"m1","server":"calendar","tool":"list","status":"inProgress","arguments":{}}"#,
            ),
            (OutboundRenderKind::McpToolCall, OutboundResolved::Tool(_))
        ));
        assert!(matches!(
            resolve_outbound_payload("operator.approval.request:x", br#"{"kind":"exec"}"#),
            (
                OutboundRenderKind::ApprovalRequest,
                OutboundResolved::Invalid(_)
            )
        ));
    }

    #[test]
    fn paints_rich_turn_items_from_their_own_payload_vocabulary() {
        let exec = display_text_for_kind(
            OutboundRenderKind::ExecCommand,
            br#"{"item_id":"e1","command":"bash -lc \"cargo test\"","cwd":"/w","source":"agent","status":"completed","exit_code":0,"duration_ms":1500,"aggregated_output":"ok"}"#,
        );
        assert!(exec.contains("$ cargo test"), "exec header: {exec}");
        assert!(exec.contains("[ok · exit 0 · 1.5s]"), "exec status: {exec}");
        assert!(exec.contains("in /w"));
        assert!(exec.contains("ok"));

        let running = display_text_for_kind(
            OutboundRenderKind::ExecCommand,
            br#"{"command":"sleep 5","status":"inProgress"}"#,
        );
        assert!(running.contains("$ sleep 5"));
        assert!(running.contains("[running]"), "running status: {running}");

        let reasoning = display_text_for_kind(
            OutboundRenderKind::Reasoning,
            br#"{"summary":["Plan it"],"content":["Detail here"]}"#,
        );
        assert!(reasoning.contains("Reasoning"));
        assert!(reasoning.contains("_Plan it_"));
        assert!(reasoning.contains("Detail here"));

        let tokens = display_text_for_kind(
            OutboundRenderKind::TokenUsage,
            br#"{"total":{"total_tokens":2720,"input_tokens":2000,"output_tokens":700,"cached_input_tokens":500},"last":{"total_tokens":300},"model_context_window":272000}"#,
        );
        assert!(
            tokens.contains("2720 total (2000 in · 700 out)"),
            "tokens: {tokens}"
        );
        assert!(tokens.contains("500 cached"));
        assert!(tokens.contains("last turn 300"));
        assert!(tokens.contains("% of 272000 window"));

        let plan = display_text_for_kind(
            OutboundRenderKind::PlanUpdate,
            br#"{"item_id":"p1","text":"1. read\n2. edit"}"#,
        );
        assert!(plan.contains("Plan"));
        assert!(plan.contains("2. edit"));

        let odd = display_text_for_kind(OutboundRenderKind::ExecCommand, b"[1,2,3]");
        assert!(
            odd.contains('1') && odd.contains('3'),
            "non-object exec falls back to plain: {odd}"
        );
        let raw = display_text_for_kind(OutboundRenderKind::Reasoning, b"not json");
        assert_eq!(raw, "not json");
    }
}
