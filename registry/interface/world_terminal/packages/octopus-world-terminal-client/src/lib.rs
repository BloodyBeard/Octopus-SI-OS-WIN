mod account_items;
mod capability_formation;
mod chat_topology;
mod daemon_arrival;
mod emit_input;
mod goal_items;
mod history_projection;
mod hook_items;
mod host_path;
mod input_elements;
mod ledger_transcript;
mod mention_sigils;
mod operator_forms;
mod prompt_history;
mod read_outbound;
mod render_outbound;
mod rich_items;
mod rich_requests;
mod run_index;
mod task_projection;
mod task_reference;
mod task_state;
#[cfg(test)]
mod test_ledger;

pub use octopus_interface_transport::InterfaceHostIoLimits;

pub use account_items::{
    AccountUsageBucket, AccountUsageReport, AccountUsageSummary, CreditsItem, RateLimitReachedKind,
    RateLimitSnapshotItem, RateLimitWindowItem, SpendLimitItem, rate_limit_snapshots_from_payload,
};
pub use capability_formation::{
    CapabilityFormationError, CapabilityFormationRequest, CapabilityRegistry, CapabilitySidecarRequirement,
    FormedCapabilityTopology, FormedToolCapability, LiveEePolicyLayer, ToolCapabilityBinding,
    WorkerRelayCapabilityBinding, form_capability_topology, resolve_live_ee_policy_chain,
    resolve_live_ee_template_chain,
};
pub use chat_topology::{
    AuthoredPrivateRun, PrivateRunRequest, PrivateRunTopology, PrivateShellExecution,
    ProviderModelPreset, ProviderReasoningEffortPreset, TerminalClientDescriptor,
    TerminalHostRoots, TopologyError, author_private_run, await_private_run_interfaces,
    await_private_terminal_reply, configure_private_model_interface, default_private_run_request,
    execute_private_shell, fork_private_run_request, load_interface_host_io_limits,
    load_private_provider_model_catalog, load_private_run_topology,
};
pub use daemon_arrival::{
    DaemonArrivalError, DaemonEndpoint, TERMINAL_LISTEN_ENV, TerminalDaemonEmitter,
    daemon_control_socket_path, interface_control_socket_path, terminal_hand_in_root_path,
};
pub use emit_input::{
    TERMINAL_INPUT_ARTIFACT_LABEL, TerminalInputArtifact, TerminalInputError, TerminalUserInput,
    materialize_input_artifact, terminal_input_manifest, terminal_input_manifest_from_items,
    write_terminal_input_reference,
};
pub use goal_items::{GoalState, GoalStatusItem, MAX_THREAD_GOAL_OBJECTIVE_CHARS};
pub use history_projection::{
    HistoryCursor, HistoryPage, HistoryProjectionError, MAX_HISTORY_PAGE_ENTRIES,
    MAX_HISTORY_SCAN_ROWS, ReverseHistoryPager, TranscriptArtifactEntry,
};
pub use hook_items::{
    HookCatalog, HookCatalogIssue, HookCatalogItem, HookItemStatus, HookOutputItem, HookOutputKind,
    HookRunItem, HookTrustState, KNOWN_HOOK_EVENTS, hook_event_description, hook_event_names,
    hook_source_label,
};
pub use host_path::HostPath;
pub use input_elements::{
    ByteRange, MAX_USER_INPUT_TEXT_CHARS, TextElement, local_image_label_text,
};
pub use mention_sigils::{PLUGIN_TEXT_MENTION_SIGIL, TOOL_MENTION_SIGIL};
pub use operator_forms::{
    APPROVAL_META_KIND_KEY, APPROVAL_META_KIND_MCP_TOOL_CALL, APPROVAL_META_PERSIST_ALWAYS,
    APPROVAL_META_PERSIST_KEY, APPROVAL_META_PERSIST_SESSION,
    APPROVAL_META_TOOL_PARAMS_DISPLAY_KEY, APPROVAL_META_TOOL_PARAMS_KEY, ApprovalDetail,
    ApprovalOptionItem, ElicitationFieldInput, ElicitationFormField, ElicitationOption,
    NetworkApprovalScope, OperatorResponder, PermissionScope, UserInputOption, UserInputQuestion,
    UserInputRequest, approval_options_from_detail, approval_options_from_payload,
    emit_user_input_response, parse_elicitation_fields,
};
pub use read_outbound::{TerminalOutboundError, TerminalOutboundReader};
pub use render_outbound::{
    OutboundRenderKind, OutboundResolved, RenderedOutbound, classify_outbound_artifact,
    outbound_display, outbound_resolved, render_outbound_artifact, resolve_outbound_payload,
};
pub use rich_items::{
    CommandActionItem, ExecCommandItem, ExecItemSource, ExecItemStatus, PlanUpdateItem,
    ReasoningItem, RichItemError, TokenBreakdown, TokenUsageItem, ToolCallItem, ToolCallStatus,
    ToolCallSurface, UserMessageItem, WebSearchActionItem, WebSearchItem,
};
pub use rich_requests::{
    ApprovalRequest, ElicitationRequest, OperatorRequestResolved, RichRequestError,
    emit_approval_response, emit_elicitation_response,
};

pub use ledger_transcript::{
    LedgerPageCursor, LedgerScan, LedgerScanError, LedgerTranscriptHydrator, LedgerTranscriptPage,
    RunActivity, TranscriptEntry, TranscriptSide, render_markdown_transcript,
};
pub use prompt_history::PromptHistory;
pub use run_index::{RunEntry, RunIndexError, TerminalRunIndex};
pub use task_projection::{TaskEntry, TaskProjectionError, TaskStatus, TerminalTaskProjection};
pub use task_reference::{
    TaskArtifactReference, TaskMentionCandidate, TaskReferenceError, bind_task_references,
    task_mention_candidates,
};
pub use task_state::{TaskPresentation, TaskStateError, TaskStateStore};
