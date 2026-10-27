use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use crate::app_command::AppCommand;
use crate::app_command::ReviewTargetSpec;
use crate::app_event::HistoryLookupResponse;
use crate::approval_events::ApplyPatchApprovalRequestEvent;
#[cfg(test)]
use crate::approval_events::ExecApprovalRequestEvent;
use crate::bottom_pane::StatusLineItem;
use crate::bottom_pane::StatusLineSetupView;
use crate::bottom_pane::StatusSurfacePreviewData;
use crate::bottom_pane::StatusSurfacePreviewItem;
use crate::bottom_pane::TerminalTitleItem;
use crate::bottom_pane::TerminalTitleSetupView;
#[cfg(test)]
use crate::diff_model::FileChange;
use crate::git_action_directives::parse_assistant_markdown;
#[cfg(test)]
use crate::guardian_events::GuardianAssessmentAction;
#[cfg(test)]
use crate::guardian_events::GuardianAssessmentEvent;
#[cfg(test)]
use crate::guardian_events::GuardianAssessmentStatus;
use crate::model_catalog::ModelCatalog;
use crate::plan_type::PlanType;
use crate::provider_config_runtime::ProviderConfigRuntime;
use crate::session_posture::ConstrainedValue;
use crate::session_posture::PostureResult;
use crate::session_posture::SessionPermissionSnapshot;
use crate::session_posture::WindowsSandboxMode;
use crate::session_state::SessionNetworkProxyRuntime;
use crate::session_state::ThreadSessionState;
use crate::session_telemetry::RuntimeMetricsSummary;
use crate::session_telemetry::SessionTelemetry;
use crate::session_vocab::config_types::ApprovalsReviewer;
use crate::session_vocab::config_types::CollaborationMode;
use crate::session_vocab::config_types::CollaborationModeMask;
use crate::session_vocab::config_types::ModeKind;
use crate::session_vocab::config_types::Personality;
use crate::session_vocab::config_types::Settings;
use crate::status::RateLimitWindowDisplay;
use crate::status::StatusAccountDisplay;
use crate::status::StatusHistoryHandle;
use crate::status::format_directory_display;
use crate::status::format_tokens_compact;
use crate::status::rate_limit_snapshot_display_for_limit;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_title::SetTerminalTitleResult;
use crate::terminal_title::clear_terminal_title;
use crate::terminal_title::set_terminal_title;
use crate::text_formatting::proper_join;
use crate::token_usage::TokenUsage;
use crate::token_usage::TokenUsageInfo;
use crate::tui_config::TuiConfig;
#[cfg(test)]
use crate::turn_items::AgentMessageContent;
#[cfg(test)]
use crate::turn_items::AgentMessageItem;
#[cfg(test)]
use crate::turn_items::MessagePhase;
use crate::version::CODEX_CLI_VERSION;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
#[cfg(test)]
use octopus_git_utils::CommitLogEntry;
use octopus_git_utils::current_branch_name;
use octopus_git_utils::get_git_repo_root;
use octopus_git_utils::local_git_branches;
use octopus_git_utils::recent_commits;
use octopus_substrate_contract::approvals_substrate::AskForApproval;
#[cfg(any(target_os = "windows", test))]
use octopus_substrate_contract::shell_env_config::WindowsSandboxLevel;
use octopus_terminal_detection::Multiplexer;
use octopus_terminal_detection::TerminalInfo;
use octopus_terminal_detection::TerminalName;
use octopus_terminal_detection::terminal_info;
use octopus_util_abspath::AbsolutePathBuf;
use octopus_util_pathuri::PathUri;
use octopus_utils_cli::resume_hint;
use octopus_world_terminal_client::ByteRange;
use octopus_world_terminal_client::ExecCommandItem;
use octopus_world_terminal_client::GoalState;
use octopus_world_terminal_client::RateLimitReachedKind;
use octopus_world_terminal_client::RateLimitSnapshotItem;
use octopus_world_terminal_client::TerminalUserInput;
use octopus_world_terminal_client::TextElement;
use octopus_world_terminal_config::Notifications;
use rand::Rng;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Text;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;
#[cfg(test)]
use tokio::sync::mpsc::UnboundedSender;
use tracing::warn;

const DEFAULT_MODEL_DISPLAY_NAME: &str = "loading";
#[cfg(test)]
const MULTI_AGENT_ENABLE_TITLE: &str = "Enable subagents?";
#[cfg(test)]
const MULTI_AGENT_ENABLE_YES: &str = "Yes, enable";
#[cfg(test)]
const MULTI_AGENT_ENABLE_NO: &str = "Not now";
#[cfg(test)]
const MULTI_AGENT_ENABLE_NOTICE: &str = "Subagents will be enabled in the next session.";
const MEMORIES_DOC_URL: &str = "https://developers.openai.com/codex/memories";
const MEMORIES_ENABLE_TITLE: &str = "Enable memories?";
const MEMORIES_ENABLE_YES: &str = "Yes, enable";
const MEMORIES_ENABLE_NO: &str = "Not now";
const MEMORIES_ENABLE_NOTICE: &str = "Memories will be enabled in the next session.";
const PLAN_MODE_REASONING_SCOPE_TITLE: &str = "Apply reasoning change";
const PLAN_MODE_REASONING_SCOPE_PLAN_ONLY: &str = "Apply to Plan mode override";
const PLAN_MODE_REASONING_SCOPE_ALL_MODES: &str = "Apply to global default and Plan mode override";
const PET_SELECTION_LOADING_VIEW_ID: &str = "pet-selection-loading";
const AMBIENT_PET_WRAP_GAP_COLUMNS: u16 = 2;
const TUI_STUB_MESSAGE: &str = "Not available in TUI yet.";

fn queued_message_edit_binding_for_terminal(terminal_info: TerminalInfo) -> KeyBinding {
    if matches!(
        terminal_info.multiplexer.as_ref(),
        Some(Multiplexer::Tmux { .. })
    ) {
        return key_hint::shift(KeyCode::Left);
    }

    match terminal_info.name {
        TerminalName::AppleTerminal | TerminalName::WarpTerminal | TerminalName::VsCode => {
            key_hint::shift(KeyCode::Left)
        }
        TerminalName::Ghostty
        | TerminalName::Iterm2
        | TerminalName::WezTerm
        | TerminalName::Kitty
        | TerminalName::Alacritty
        | TerminalName::Konsole
        | TerminalName::GnomeTerminal
        | TerminalName::Vte
        | TerminalName::WindowsTerminal
        | TerminalName::Dumb
        | TerminalName::Unknown => key_hint::alt(KeyCode::Up),
    }
}

fn queued_message_edit_hint_binding(
    bindings: &[KeyBinding],
    terminal_info: TerminalInfo,
) -> Option<KeyBinding> {
    let terminal_binding = queued_message_edit_binding_for_terminal(terminal_info);
    bindings
        .contains(&terminal_binding)
        .then_some(terminal_binding)
        .or_else(|| bindings.first().copied())
}

fn normalize_thread_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

use crate::app_event::AppEvent;
use crate::app_event::ExitMode;
use crate::app_event::PermissionProfileSelection;
use crate::app_event::RateLimitRefreshOrigin;
#[cfg(target_os = "windows")]
use crate::app_event::WindowsSandboxEnableMode;
use crate::app_event_sender::AppEventSender;
use crate::auto_review_denials;
use crate::auto_review_denials::RecentAutoReviewDenials;
use crate::bottom_pane::ApprovalRequest;
use crate::bottom_pane::BottomPane;
use crate::bottom_pane::BottomPaneParams;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::CollaborationModeIndicator;
use crate::bottom_pane::ColumnWidthMode;
use crate::bottom_pane::DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED;
use crate::bottom_pane::ExperimentalFeatureItem;
use crate::bottom_pane::ExperimentalFeaturesView;
use crate::bottom_pane::GoalStatusIndicator;
use crate::bottom_pane::HistoryEntry;
use crate::bottom_pane::InputResult;
use crate::bottom_pane::LocalImageAttachment;
use crate::bottom_pane::McpServerElicitationFormRequest;
use crate::bottom_pane::MemoriesSettingsView;
use crate::bottom_pane::QUIT_SHORTCUT_TIMEOUT;
use crate::bottom_pane::QueuedInputAction;
use crate::bottom_pane::SelectionAction;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::custom_prompt_view::CustomPromptView;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::clipboard_paste::paste_image_to_temp_png;
use crate::collaboration_modes;
use crate::diff_render::display_path_for;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::ExecCell;
use crate::exec_cell::new_active_exec_command;
use crate::exec_command::split_command_string;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::get_git_diff::get_git_diff;
use crate::history_cell;
use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryRenderMode;
use crate::history_cell::HookCell;
use crate::history_cell::McpInvocation;
use crate::history_cell::McpToolCallCell;
use crate::history_cell::PlainHistoryCell;
use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::ChatKeymap;
use crate::keymap::RuntimeKeymap;
use crate::render::Insets;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::FlexRenderable;
use crate::render::renderable::Renderable;
use crate::render::renderable::RenderableExt;
use crate::render::renderable::RenderableItem;
use crate::slash_command::SlashCommand;
use crate::status::RateLimitSnapshotDisplay;
use crate::status::remote_connection::RemoteConnectionStatus;
use crate::status_indicator_widget::STATUS_DETAILS_DEFAULT_MAX_LINES;
use crate::status_indicator_widget::StatusDetailsCapitalization;
use crate::text_formatting::truncate_text;
use crate::tui::FrameRequester;
mod command_lifecycle;
mod constructor;
mod detach;
mod exec_state;
mod feedback;
use self::exec_state::RunningCommand;
use self::exec_state::UnifiedExecProcessSummary;
use self::exec_state::UnifiedExecWaitState;
use self::exec_state::UnifiedExecWaitStreak;
use self::exec_state::exec_item_command_and_parsed;
use self::exec_state::exec_source_from_item;
use self::exec_state::is_standard_tool_call;
use self::exec_state::is_unified_exec_source;
use crate::exec_command::ExecCommandSource;
mod goal_status;
use self::goal_status::GoalStatusState;
mod goal_menu;
mod ide_context;
use self::ide_context::IdeContextState;
mod input_queue;
use self::input_queue::InputQueueState;
mod input_flow;
mod input_restore;
mod input_submission;
mod interrupts;
use self::interrupts::InterruptManager;
mod keymap_picker;
mod pets;
mod session_flow;
mod session_header;
use self::session_header::SessionHeader;
mod hook_lifecycle;
mod hooks;
mod interaction;
mod plan_implementation;
mod slash_dispatch;
use self::plan_implementation::PLAN_IMPLEMENTATION_TITLE;
mod model_popups;
mod notifications;
use self::notifications::Notification;
mod permission_popups;
mod permissions_menu;
mod rate_limits;
#[cfg(test)]
use self::rate_limits::RateLimitErrorKind;
use self::rate_limits::RateLimitSwitchPromptState;
use self::rate_limits::RateLimitWarningState;
pub(crate) use self::rate_limits::fallback_limit_label;
pub(crate) use self::rate_limits::limit_label_for_window;
mod reasoning_shortcuts;
mod rendering;
mod replay;
mod review;
mod review_popups;
use self::review::ReviewState;
#[cfg(test)]
pub(crate) use self::review_popups::show_review_commit_picker_with_entries;
mod service_tiers;
mod settings;
mod settings_popups;
mod side;
mod status_state;
mod windows_sandbox_prompts;
use self::status_state::StatusIndicatorState;
use self::status_state::StatusState;
use self::status_state::TerminalTitleStatusKind;
mod status_controls;
mod status_surfaces;
mod streaming;
use self::status_surfaces::CachedProjectRootName;
mod tokens;
pub(crate) use self::tokens::TokenActivityView;
mod tool_lifecycle;
mod tool_requests;
mod transcript;
mod transcript_export;
use self::transcript::TranscriptState;
mod turn_lifecycle;
mod turn_runtime;
use self::turn_lifecycle::TurnLifecycleState;
mod usage;
mod user_messages;
use self::user_messages::PendingSteer;
#[cfg(test)]
use self::user_messages::PendingSteerCompareKey;
use self::user_messages::QueueDrain;
use self::user_messages::QueuedUserMessage;
use self::user_messages::ShellEscapePolicy;
use self::user_messages::ThreadComposerState;
#[cfg(test)]
pub(crate) use self::user_messages::ThreadInputState;
pub(crate) use self::user_messages::UserMessage;
use self::user_messages::UserMessageDisplay;
#[cfg(test)]
use self::user_messages::UserMessageHistoryOverride;
use self::user_messages::UserMessageHistoryRecord;
pub(crate) use self::user_messages::create_initial_user_message;
use self::user_messages::mention_bindings_from_inputs;
#[cfg(test)]
use self::user_messages::merge_user_messages;
use self::user_messages::merge_user_messages_with_history_record;
#[cfg(test)]
use self::user_messages::remap_placeholders_for_message;
use self::user_messages::terminal_text_elements;
use self::user_messages::user_message_display_for_history;
use self::user_messages::user_message_for_restore;
use self::user_messages::user_message_preview_text;
mod warnings;
use self::warnings::WarningDisplayState;
pub(crate) use crate::branch_summary::StatusLineGitSummary;
use crate::streaming::chunking::AdaptiveChunkingPolicy;
use crate::streaming::commit_tick::CommitTickScope;
use crate::streaming::commit_tick::run_commit_tick;
use crate::streaming::controller::PlanStreamController;
use crate::streaming::controller::StreamController;
use crate::workspace_command::WorkspaceCommandRunner;

use crate::approval_presets::ApprovalPreset;
use crate::approval_presets::builtin_approval_presets;
use crate::session_vocab::openai_models::InputModality;
use crate::session_vocab::openai_models::ModelPreset;
use crate::session_vocab::openai_models::ReasoningEffort as ReasoningEffortConfig;
#[cfg(test)]
use crate::turn_items::StepStatus;
#[cfg(test)]
use crate::turn_items::UpdatePlanArgs;
use chrono::Local;
use octopus_file_search::FileMatch;
use octopus_substrate_contract::permission_profile::ActivePermissionProfile;
use octopus_substrate_contract::permission_profile::PermissionProfile;
use strum::IntoEnumIterator;
use unicode_segmentation::UnicodeSegmentation;

const USER_SHELL_COMMAND_HELP_TITLE: &str = "Prefix a command with ! to run it locally";
const USER_SHELL_COMMAND_HELP_HINT: &str = "Example: !ls";
const ASK_FOR_APPROVAL_LABEL: &str = "Ask for approval";
const APPROVE_FOR_ME_LABEL: &str = "Approve for me";
const AUTO_REVIEW_DESCRIPTION: &str = "Only ask for actions detected as potentially unsafe.";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_STATUS_LINE_ITEMS: [&str; 2] = ["model-with-reasoning", "current-dir"];
const MAX_AGENT_COPY_HISTORY: usize = 32;

pub(crate) struct ChatWidgetInit {
    pub(crate) config: TuiConfig,
    pub(crate) config_runtime: std::sync::Arc<dyn ProviderConfigRuntime>,
    pub(crate) frame_requester: FrameRequester,
    pub(crate) app_event_tx: AppEventSender,
    pub(crate) workspace_command_runner: Option<WorkspaceCommandRunner>,
    pub(crate) initial_user_message: Option<UserMessage>,
    pub(crate) enhanced_keys_supported: bool,
    pub(crate) has_chatgpt_account: bool,
    pub(crate) has_codex_backend_auth: bool,
    pub(crate) model_catalog: Arc<ModelCatalog>,
    pub(crate) is_first_run: bool,
    pub(crate) status_account_display: Option<StatusAccountDisplay>,
    pub(crate) runtime_model_provider_base_url: Option<String>,
    pub(crate) initial_plan_type: Option<PlanType>,
    pub(crate) model: Option<String>,
    pub(crate) startup_tooltip_override: Option<String>,
    pub(crate) status_line_invalid_items_warned: Arc<AtomicBool>,
    pub(crate) terminal_title_invalid_items_warned: Arc<AtomicBool>,
    pub(crate) session_telemetry: SessionTelemetry,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ExternalEditorState {
    #[default]
    Closed,
    Requested,
    Active,
}

pub(crate) struct ChatWidget {
    app_event_tx: AppEventSender,
    codex_op_target: CodexOpTarget,
    bottom_pane: BottomPane,
    transcript: TranscriptState,
    config: TuiConfig,
    config_runtime: std::sync::Arc<dyn ProviderConfigRuntime>,
    raw_output_mode: bool,
    effective_service_tier: Option<String>,
    current_collaboration_mode: CollaborationMode,
    active_collaboration_mask: Option<CollaborationModeMask>,
    has_chatgpt_account: bool,
    has_codex_backend_auth: bool,
    model_catalog: Arc<ModelCatalog>,
    session_telemetry: SessionTelemetry,
    session_header: SessionHeader,
    initial_user_message: Option<UserMessage>,
    status_account_display: Option<StatusAccountDisplay>,
    runtime_model_provider_base_url: Option<String>,
    pub(crate) remote_connection: Option<RemoteConnectionStatus>,
    token_info: Option<TokenUsageInfo>,
    rate_limit_snapshots_by_limit_id: BTreeMap<String, RateLimitSnapshotDisplay>,
    refreshing_status_outputs: Vec<(u64, StatusHistoryHandle)>,
    next_status_refresh_request_id: u64,
    refreshing_token_activity_output: Option<tokens::PendingTokenActivityOutput>,
    completed_token_activity_output: Option<history_cell::CompositeHistoryCell>,
    next_token_activity_request_id: u64,
    pending_rate_limit_reset_request_id: Option<u64>,
    pending_rate_limit_reset_hint_request_id: Option<u64>,
    pending_rate_limit_reset_hint: Option<PlainHistoryCell>,
    available_rate_limit_reset_credits: Option<i64>,
    next_rate_limit_reset_request_id: u64,
    plan_type: Option<PlanType>,
    codex_rate_limit_reached_type: Option<RateLimitReachedKind>,
    rate_limit_warnings: RateLimitWarningState,
    warning_display_state: WarningDisplayState,
    rate_limit_switch_prompt: RateLimitSwitchPromptState,
    #[cfg(test)]
    add_credits_nudge_email_in_flight: Option<String>,
    adaptive_chunking: AdaptiveChunkingPolicy,
    stream_controller: Option<StreamController>,
    plan_stream_controller: Option<PlanStreamController>,
    pending_stream_consolidations: usize,
    clipboard_lease: Option<crate::clipboard_copy::ClipboardLease>,
    copy_last_response_binding: Vec<KeyBinding>,
    running_commands: HashMap<String, RunningCommand>,
    suppressed_exec_calls: HashSet<String>,
    last_unified_wait: Option<UnifiedExecWaitState>,
    unified_exec_wait_streak: Option<UnifiedExecWaitStreak>,
    turn_lifecycle: TurnLifecycleState,
    task_complete_pending: bool,
    unified_exec_processes: Vec<UnifiedExecProcessSummary>,
    ide_context: IdeContextState,
    interrupts: InterruptManager,
    reasoning_buffer: String,
    full_reasoning_buffer: String,
    status_state: StatusState,
    review: ReviewState,
    active_hook_cell: Option<HookCell>,
    latest_hook_catalog: Option<octopus_world_terminal_client::HookCatalog>,
    ambient_pet: Option<crate::pets::AmbientPet>,
    pet_picker_preview_state: crate::pets::PetPickerPreviewState,
    pet_picker_preview_pet: Option<crate::pets::AmbientPet>,
    pet_picker_preview_request_id: u64,
    pet_picker_preview_image_visible: std::cell::Cell<bool>,
    pet_selection_load_request_id: u64,
    #[cfg(test)]
    pet_image_support_override: Option<crate::pets::PetImageSupport>,
    thread_id: Option<String>,
    dismissed_plan_mode_nudge_scopes: HashSet<PlanModeNudgeScope>,
    thread_name: Option<String>,
    thread_rename_block_message: Option<String>,
    active_side_conversation: bool,
    normal_placeholder_text: String,
    side_placeholder_text: String,
    forked_from: Option<String>,
    #[cfg(test)]
    interrupted_turn_notice_mode: InterruptedTurnNoticeMode,
    frame_requester: FrameRequester,
    show_welcome_banner: bool,
    startup_tooltip_override: Option<String>,
    suppress_session_configured_redraw: bool,
    suppress_initial_user_message_submit: bool,
    input_queue: InputQueueState,
    cancel_edit: CancelEditState,
    chat_keymap: ChatKeymap,
    queued_message_edit_hint_binding: Option<KeyBinding>,
    pending_notification: Option<Notification>,
    quit_shortcut_expires_at: Option<Instant>,
    quit_shortcut_key: Option<KeyBinding>,
    turn_runtime_metrics: RuntimeMetricsSummary,
    last_rendered_width: std::cell::Cell<Option<usize>>,
    current_rollout_path: Option<PathBuf>,
    current_cwd: Option<PathBuf>,
    workspace_command_runner: Option<WorkspaceCommandRunner>,
    instruction_source_paths: Vec<PathUri>,
    session_network_proxy: Option<SessionNetworkProxyRuntime>,
    status_line_invalid_items_warned: Arc<AtomicBool>,
    terminal_title_invalid_items_warned: Arc<AtomicBool>,
    pub(crate) last_terminal_title: Option<String>,
    last_terminal_title_requires_action: bool,
    terminal_title_setup_original_items: Option<Option<Vec<String>>>,
    terminal_title_animation_origin: Instant,
    status_line_project_root_name_cache: Option<CachedProjectRootName>,
    status_line_branch: Option<String>,
    status_line_branch_cwd: Option<PathBuf>,
    status_line_branch_pending: bool,
    status_line_branch_lookup_complete: bool,
    status_line_git_summary: Option<StatusLineGitSummary>,
    status_line_git_summary_cwd: Option<PathBuf>,
    status_line_git_summary_pending: bool,
    status_line_git_summary_lookup_complete: bool,
    current_goal_status_indicator: Option<GoalStatusIndicator>,
    current_goal_status: Option<GoalStatusState>,
    external_editor_state: ExternalEditorState,
    last_rendered_user_message_display: Option<UserMessageDisplay>,
}

enum CodexOpTarget {
    #[cfg(test)]
    Direct(UnboundedSender<AppCommand>),
    AppEvent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ActiveCellTranscriptKey {
    pub(crate) revision: u64,
    pub(crate) is_stream_continuation: bool,
    pub(crate) animation_tick: Option<u64>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum InterruptedTurnNoticeMode {
    #[default]
    Default,
    Suppress,
}

#[derive(Debug, Default)]
struct CancelEditState {
    prompt: Option<UserMessage>,
    eligible: bool,
    armed: bool,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReplayKind {
    ThreadSnapshot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionConfiguredDisplay {
    Normal,
    Quiet,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum PlanModeNudgeScope {
    NewThread,
    Thread(String),
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum TurnAbortReason {
    Interrupted,
    BudgetLimited,
}

fn contains_plan_keyword(text: &str) -> bool {
    text.split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .any(|word| word.eq_ignore_ascii_case("plan"))
}

fn token_usage_info_from_item(
    item: &octopus_world_terminal_client::TokenUsageItem,
) -> TokenUsageInfo {
    let breakdown = |b: &octopus_world_terminal_client::TokenBreakdown| TokenUsage {
        total_tokens: b.total_tokens,
        input_tokens: b.input_tokens,
        cached_input_tokens: b.cached_input_tokens,
        output_tokens: b.output_tokens,
        reasoning_output_tokens: b.reasoning_output_tokens,
    };
    TokenUsageInfo {
        total_token_usage: breakdown(&item.total),
        last_token_usage: breakdown(&item.last),
        model_context_window: item.model_context_window,
    }
}

impl ChatWidget {
    fn record_agent_markdown(&mut self, message: &str) {
        if !message.is_empty() {
            self.transcript.record_agent_markdown(message.to_string());
        }
    }

    fn record_visible_user_turn_for_copy(&mut self) {
        self.transcript.record_visible_user_turn();
    }

    #[cfg(test)]
    pub(crate) fn open_multi_agent_enable_prompt(&mut self) {
        let items = vec![
            SelectionItem {
                name: MULTI_AGENT_ENABLE_YES.to_string(),
                description: Some(
                    "Save the setting now. You will need a new session to use it.".to_string(),
                ),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::UpdateFeatureFlags {
                        updates: vec![("multi_agent".to_string(), true)],
                    });
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_warning_event(MULTI_AGENT_ENABLE_NOTICE.to_string()),
                    )));
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: MULTI_AGENT_ENABLE_NO.to_string(),
                description: Some("Keep subagents disabled.".to_string()),
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(MULTI_AGENT_ENABLE_TITLE.to_string()),
            subtitle: Some("Subagents are currently disabled in your config.".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn open_memories_popup(&mut self) {
        if !self.config.features.enabled_key("memories") {
            self.open_memories_enable_prompt();
            return;
        }

        let view = MemoriesSettingsView::new(
            self.config.memories.use_memories,
            self.config.memories.generate_memories,
            self.app_event_tx.clone(),
            self.bottom_pane.list_keymap(),
        );
        self.bottom_pane.show_view(Box::new(view));
    }

    pub(crate) fn open_memories_enable_prompt(&mut self) {
        let items = vec![
            SelectionItem {
                name: MEMORIES_ENABLE_YES.to_string(),
                description: Some(
                    "Save the setting now. You will need a new session to use it.".to_string(),
                ),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::UpdateFeatureFlags {
                        updates: vec![("memories".to_string(), true)],
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: MEMORIES_ENABLE_NO.to_string(),
                description: Some("Keep memories disabled.".to_string()),
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(MEMORIES_ENABLE_TITLE.to_string()),
            subtitle: Some("Memories are currently disabled in your config.".to_string()),
            footer_note: Some(Line::from(vec![
                "Learn more: ".dim(),
                MEMORIES_DOC_URL.cyan().underlined(),
            ])),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn set_memory_settings(&mut self, use_memories: bool, generate_memories: bool) {
        self.config.memories.use_memories = use_memories;
        self.config.memories.generate_memories = generate_memories;
    }

    pub(crate) fn set_token_info(&mut self, info: Option<TokenUsageInfo>) {
        match info {
            Some(info) => self.apply_token_info(info),
            None => {
                self.bottom_pane.set_context_window(None, None);
                self.token_info = None;
            }
        }
    }

    fn apply_token_info(&mut self, info: TokenUsageInfo) {
        let percent = self.context_remaining_percent(&info);
        let used_tokens = self.context_used_tokens(&info, percent.is_some());
        self.bottom_pane.set_context_window(percent, used_tokens);
        self.token_info = Some(info);
    }

    fn context_remaining_percent(&self, info: &TokenUsageInfo) -> Option<i64> {
        info.model_context_window.map(|window| {
            info.last_token_usage
                .percent_of_context_window_remaining(window)
        })
    }

    fn context_used_tokens(&self, info: &TokenUsageInfo, percent_known: bool) -> Option<i64> {
        if percent_known {
            return None;
        }

        Some(info.total_token_usage.tokens_in_context_window())
    }

    #[cfg(test)]
    fn restore_pre_review_token_info(&mut self) {
        if let Some(saved) = self.review.pre_review_token_info.take() {
            match saved {
                Some(info) => self.apply_token_info(info),
                None => {
                    self.bottom_pane.set_context_window(None, None);
                    self.token_info = None;
                }
            }
        }
    }

    pub(crate) fn handle_history_entry_response(&mut self, event: HistoryLookupResponse) {
        let HistoryLookupResponse {
            offset,
            log_id,
            entry,
        } = event;
        self.bottom_pane
            .on_history_entry_response(log_id, offset, entry);
    }

    pub(crate) fn pre_draw_tick(&mut self) {
        self.update_due_hook_visibility();
        self.schedule_hook_timer_if_needed();
        self.bottom_pane.pre_draw_tick();
        if let Some(pet) = self.ambient_pet.as_ref() {
            pet.schedule_next_frame();
        }
        self.refresh_plan_mode_nudge();
        self.refresh_goal_status_indicator_for_time_tick();
        if self.terminal_title_shows_action_required() != self.last_terminal_title_requires_action {
            self.refresh_terminal_title();
        }
        if self.should_animate_terminal_title_spinner()
            || self.should_animate_terminal_title_action_required()
        {
            self.refresh_terminal_title();
        }
    }

    fn flush_active_cell(&mut self) {
        if let Some(active) = self.transcript.active_cell.take() {
            self.transcript.needs_final_message_separator = true;
            self.app_event_tx.send(AppEvent::InsertHistoryCell(active));
            self.request_pending_usage_output_insertion();
        }
    }

    pub(crate) fn add_to_history(&mut self, cell: impl HistoryCell + 'static) {
        self.add_boxed_history(Box::new(cell));
    }

    fn add_boxed_history(&mut self, cell: Box<dyn HistoryCell>) {
        if self.turn_lifecycle.agent_turn_running && !cell.display_lines(u16::MAX).is_empty() {
            self.record_visible_turn_activity();
        }
        let keep_placeholder_header_active = !self.is_session_configured()
            && self
                .transcript
                .active_cell
                .as_ref()
                .is_some_and(|c| c.as_any().is::<history_cell::SessionHeaderHistoryCell>());

        if !keep_placeholder_header_active && !cell.display_lines(u16::MAX).is_empty() {
            if !self.has_active_stream_tail() {
                self.flush_active_cell();
            }
            self.transcript.needs_final_message_separator = true;
        }
        self.app_event_tx.send(AppEvent::InsertHistoryCell(cell));
    }

    #[cfg(test)]
    fn enter_review_mode_with_hint(&mut self, hint: String, from_replay: bool) {
        if self.review.pre_review_token_info.is_none() {
            self.review.pre_review_token_info = Some(self.token_info.clone());
        }
        if !from_replay && !self.bottom_pane.is_task_running() {
            self.bottom_pane.set_task_running(true);
        }
        self.review.is_review_mode = true;
        let banner = format!(">> Code review started: {hint} <<");
        self.add_to_history(history_cell::new_review_status_line(banner));
        self.request_redraw();
    }

    #[cfg(test)]
    fn exit_review_mode_after_item(&mut self) {
        self.flush_answer_stream_with_separator();
        self.flush_interrupt_queue();
        self.flush_active_cell();
        self.review.is_review_mode = false;
        self.restore_pre_review_token_info();
        self.add_to_history(history_cell::new_review_status_line(
            "<< Code review finished >>".to_string(),
        ));
        self.request_redraw();
    }

    pub(crate) fn on_committed_user_message(
        &mut self,
        items: &[TerminalUserInput],
        from_replay: bool,
    ) {
        let display = Self::user_message_display_from_inputs(items);
        let mention_bindings = mention_bindings_from_inputs(items, &display.message);
        if from_replay {
            if self.review.is_review_mode {
                return;
            }
            self.bottom_pane
                .record_replayed_user_message_history(HistoryEntry {
                    text: display.message.clone(),
                    text_elements: display.text_elements.clone(),
                    local_image_paths: display.local_images.clone(),
                    remote_image_urls: display.remote_image_urls.clone(),
                    mention_bindings,
                    pending_pastes: Vec::new(),
                });
            self.on_user_message_display(display);
            return;
        }

        let compare_key = Self::pending_steer_compare_key_from_items(items);
        if self
            .input_queue
            .pending_steers
            .front()
            .is_some_and(|pending| pending.compare_key == compare_key)
        {
            if let Some(pending) = self.input_queue.pending_steers.pop_front() {
                self.refresh_pending_input_preview();
                let pending_display =
                    user_message_display_for_history(pending.user_message, &pending.history_record);
                self.on_user_message_display(pending_display);
            } else if self.last_rendered_user_message_display.as_ref() != Some(&display) {
                tracing::warn!(
                    "pending steer matched compare key but queue was empty when rendering committed user message"
                );
                self.on_user_message_display(display);
            }
        } else if !self.review.is_review_mode
            && self.last_rendered_user_message_display.as_ref() != Some(&display)
        {
            self.on_user_message_display(display);
        }
    }

    fn on_user_message_display(&mut self, display: UserMessageDisplay) {
        self.last_rendered_user_message_display = Some(display.clone());
        if !display.message.trim().is_empty()
            || !display.text_elements.is_empty()
            || !display.local_images.is_empty()
            || !display.remote_image_urls.is_empty()
        {
            self.record_visible_user_turn_for_copy();
            self.add_to_history(history_cell::new_user_prompt(
                display.message,
                display.text_elements,
                display.local_images,
                display.remote_image_urls,
            ));
        }

        self.transcript.needs_final_message_separator = false;
    }

    fn request_quit_without_confirmation(&self) {
        self.app_event_tx
            .send(AppEvent::Exit(ExitMode::ShutdownFirst));
    }

    pub(crate) fn show_shutdown_in_progress(&mut self) {
        self.bottom_pane.show_shutdown_in_progress();
    }

    fn request_redraw(&mut self) {
        self.frame_requester.schedule_frame();
    }

    fn bump_active_cell_revision(&mut self) {
        self.transcript.bump_active_cell_revision();
    }

    #[cfg(test)]
    fn finalize_active_cell_as_failed(&mut self) {
        if let Some(mut cell) = self.transcript.active_cell.take() {
            if let Some(exec) = cell.as_any_mut().downcast_mut::<ExecCell>() {
                exec.mark_failed();
            } else if let Some(tool) = cell.as_any_mut().downcast_mut::<McpToolCallCell>() {
                tool.mark_failed();
            }
            self.add_boxed_history(cell);
            self.request_pending_usage_output_insertion();
        }
    }

    #[cfg(test)]
    pub(crate) fn set_thread_rename_block_message(&mut self, message: impl Into<String>) {
        self.thread_rename_block_message = Some(message.into());
    }

    #[cfg(test)]
    pub(crate) fn set_interrupted_turn_notice_mode(&mut self, mode: InterruptedTurnNoticeMode) {
        self.interrupted_turn_notice_mode = mode;
    }

    pub(crate) fn add_diff_in_progress(&mut self) {
        self.request_redraw();
    }

    pub(crate) fn on_diff_complete(&mut self) {
        self.request_redraw();
    }

    pub(crate) fn add_debug_config_output(&mut self) {
        self.add_to_history(crate::debug_config::new_debug_config_output(
            self.config_runtime.as_ref(),
            self.session_network_proxy.as_ref(),
        ));
    }

    pub(crate) fn add_ps_output(&mut self) {
        let processes = self
            .unified_exec_processes
            .iter()
            .map(|process| history_cell::UnifiedExecProcessDetails {
                command_display: process.command_display.clone(),
                recent_chunks: process.recent_chunks.clone(),
            })
            .collect();
        self.add_to_history(history_cell::new_unified_exec_processes_output(processes));
    }

    fn clean_background_terminals(&mut self) {
        self.submit_op(AppCommand::clean_background_terminals());
        self.unified_exec_processes.clear();
        self.sync_unified_exec_footer();
        self.add_info_message("Stopping all background terminals.".to_string(), None);
    }

    fn placeholder_session_header_cell(config: &TuiConfig) -> Box<dyn HistoryCell> {
        let placeholder_style = Style::default().add_modifier(Modifier::DIM | Modifier::ITALIC);
        Box::new(
            history_cell::SessionHeaderHistoryCell::new_with_style(
                DEFAULT_MODEL_DISPLAY_NAME.to_string(),
                placeholder_style,
                None,
                false,
                config.cwd.to_path_buf(),
                CODEX_CLI_VERSION,
            )
            .with_yolo_mode(history_cell::is_yolo_mode(config)),
        )
    }

    fn apply_session_info_cell(&mut self, cell: history_cell::SessionInfoCell) {
        let mut session_info_cell = Some(Box::new(cell) as Box<dyn HistoryCell>);
        let merged_header = if let Some(active) = self.transcript.active_cell.take() {
            if active
                .as_any()
                .is::<history_cell::SessionHeaderHistoryCell>()
            {
                if let Some(cell) = session_info_cell.take() {
                    self.transcript.active_cell = Some(cell);
                }
                true
            } else {
                self.transcript.active_cell = Some(active);
                false
            }
        } else {
            false
        };

        self.flush_active_cell();

        if !merged_header && let Some(cell) = session_info_cell {
            self.add_boxed_history(cell);
        }
    }

    pub(crate) fn add_info_message(&mut self, message: String, hint: Option<String>) {
        self.add_to_history(history_cell::new_info_event(message, hint));
        self.request_redraw();
    }

    pub(crate) fn add_memories_enable_notice(&mut self) {
        self.add_to_history(history_cell::new_warning_event(
            MEMORIES_ENABLE_NOTICE.to_string(),
        ));
        self.request_redraw();
    }

    pub(crate) fn add_plain_history_lines(&mut self, lines: Vec<Line<'static>>) {
        self.add_boxed_history(Box::new(PlainHistoryCell::new(lines)));
        self.request_redraw();
    }

    pub(crate) fn add_warning_message(&mut self, message: String) {
        self.add_to_history(history_cell::new_warning_event(message));
        self.request_redraw();
    }

    pub(crate) fn add_error_message(&mut self, message: String) {
        self.add_to_history(history_cell::new_error_event(message));
        self.request_redraw();
    }

    pub(crate) fn apply_embedded_reply_lane(
        &mut self,
        lane: &crate::embedded_spine::ReplyRenderLane,
        delta_text: Option<&str>,
    ) {
        use crate::embedded_spine::ReplyRenderLane;
        match lane {
            ReplyRenderLane::TurnStarted => {
                self.on_task_started();
            }
            ReplyRenderLane::MessageDelta { .. } => {
                if let Some(text) = delta_text {
                    self.on_agent_message_delta(text.to_string());
                }
            }
            ReplyRenderLane::ExecOutputDelta { index, .. } => {
                if let Some(text) = delta_text {
                    let call_id = format!("embedded-exec-{index}");
                    self.on_exec_command_output_delta(&call_id, text);
                }
            }
            ReplyRenderLane::MarkOutcomeItem { mark } => {
                self.add_info_message(format!("✓ {mark}"), None);
            }
            ReplyRenderLane::Approval { request, .. } => {
                self.on_operator_approval_request(request.clone());
            }
            ReplyRenderLane::Elicitation { request, .. } => {
                self.on_elicitation_request(request.clone());
            }
            ReplyRenderLane::UserInputRequest { request, .. } => {
                self.on_request_user_input(request.clone());
            }
            ReplyRenderLane::OperatorRequestResolved { request_id, .. } => {
                self.on_operator_request_resolved(request_id);
            }
            ReplyRenderLane::HookResult { item, .. } => {
                if item.status == octopus_world_terminal_client::HookItemStatus::Running {
                    self.on_hook_started(item.clone());
                } else {
                    self.on_hook_completed(item.clone());
                }
            }
            ReplyRenderLane::ToolCall {
                artifact_label,
                item,
            } => {
                self.on_tool_call_artifact(artifact_label, item.clone());
            }
            ReplyRenderLane::HookCatalog { catalog, .. } => {
                self.on_hook_catalog(catalog.clone());
            }
            ReplyRenderLane::TurnCompleted { .. } => {
                if let Some(text) = delta_text {
                    let parsed = parse_assistant_markdown(text, self.config.cwd.as_path());
                    self.record_agent_markdown(&parsed.visible_markdown);
                }
                if self.stream_controller.is_none()
                    && let Some(text) = delta_text
                    && !text.trim().is_empty()
                {
                    self.handle_streaming_delta(text.to_string());
                }
                self.flush_answer_stream_with_separator();
                self.handle_stream_finished();
                self.request_redraw();
            }
            ReplyRenderLane::Reasoning { item } => {
                let mut buffer = item.summary.join("\n\n");
                if !item.content.is_empty() {
                    if !buffer.is_empty() {
                        buffer.push_str("\n\n");
                    }
                    buffer.push_str(&item.content.join("\n\n"));
                }
                if !buffer.trim().is_empty() {
                    let cell = history_cell::new_reasoning_summary_block(buffer, &self.config.cwd);
                    self.add_boxed_history(cell);
                    self.request_redraw();
                }
            }
            ReplyRenderLane::PlanUpdate { item } => {
                if !item.text.trim().is_empty() {
                    self.add_to_history(history_cell::new_proposed_plan(
                        item.text.clone(),
                        &self.config.cwd,
                    ));
                    self.request_redraw();
                }
            }
            ReplyRenderLane::RichExec { item, .. } => match item.status {
                octopus_world_terminal_client::ExecItemStatus::InProgress => {
                    self.on_command_execution_started(item.clone());
                }
                _ => self.on_command_execution_completed(item.clone()),
            },
            ReplyRenderLane::TokenUsage { item } => {
                self.set_token_info(Some(token_usage_info_from_item(item)));
            }
            ReplyRenderLane::UserMessage { item } => {
                self.on_committed_user_message(&item.items, false);
            }
            ReplyRenderLane::ProviderError { message } => {
                self.add_error_message(message.clone());
            }
            ReplyRenderLane::RecordedArtifact { label, text } => {
                if !text.trim().is_empty() {
                    self.add_info_message(text.clone(), Some(label.clone()));
                }
            }
            ReplyRenderLane::WorkerStarted { role, run } => {
                self.add_info_message(format!("▶ {role} working"), Some(format!("run {run}")));
            }
            ReplyRenderLane::WorkerToolArtifact { role, run, count } => {
                self.add_info_message(
                    format!("⚙ {role} crossed the gate ({count} tool artifact(s))"),
                    Some(format!("run {run}")),
                );
            }
            ReplyRenderLane::WorkerMark { role, run, mark } => {
                self.add_info_message(format!("✓ {role}: {mark}"), Some(format!("run {run}")));
            }
            ReplyRenderLane::WorkerCompleted { role, run, reply } => {
                self.add_info_message(
                    format!("■ {role} done — {reply}"),
                    Some(format!("run {run}")),
                );
            }
        }
    }

    fn add_app_server_stub_message(&mut self, feature: &str) {
        warn!(feature, "stubbed unsupported TUI feature");
        self.add_error_message(format!("{feature}: {TUI_STUB_MESSAGE}"));
    }

    fn rename_confirmation_cell(name: &str, thread_id: Option<String>) -> PlainHistoryCell {
        let mut line = vec![
            "• ".into(),
            "Chat renamed to ".into(),
            name.to_string().cyan(),
        ];
        if let Some(hint) = resume_hint(Some(name), thread_id.as_deref()) {
            line.extend([". To resume this chat run ".into(), hint.cyan()]);
        }
        PlainHistoryCell::new(vec![line.into()])
    }

    pub(crate) fn apply_file_search_result(&mut self, query: String, matches: Vec<FileMatch>) {
        self.bottom_pane.on_file_search_result(query, matches);
    }

    fn current_stream_width(&self, reserved_cols: usize) -> Option<usize> {
        self.last_rendered_width.get().and_then(|width| {
            if width == 0 {
                None
            } else {
                let width = u16::try_from(width).unwrap_or(u16::MAX);
                let width = usize::from(self.history_wrap_width(width));
                Some(crate::width::usable_content_width(width, reserved_cols).unwrap_or(1))
            }
        })
    }

    pub(crate) fn raw_output_mode(&self) -> bool {
        self.raw_output_mode
    }

    pub(crate) fn history_render_mode(&self) -> HistoryRenderMode {
        if self.raw_output_mode {
            HistoryRenderMode::Raw
        } else {
            HistoryRenderMode::Rich
        }
    }

    pub(crate) fn set_raw_output_mode(&mut self, enabled: bool) {
        self.raw_output_mode = enabled;
        self.config.terminal.raw_output_mode = enabled;
        let render_mode = self.history_render_mode();
        if let Some(controller) = self.stream_controller.as_mut() {
            controller.set_render_mode(render_mode);
        }
        if let Some(controller) = self.plan_stream_controller.as_mut() {
            controller.set_render_mode(render_mode);
        }
        self.refresh_status_surfaces();
    }

    pub(crate) fn raw_output_mode_notice(enabled: bool) -> &'static str {
        if enabled {
            "Raw output mode on: transcript text is shown for clean terminal selection."
        } else {
            "Raw output mode off: rich transcript rendering restored."
        }
    }

    pub(crate) fn set_raw_output_mode_and_notify(&mut self, enabled: bool) {
        self.set_raw_output_mode(enabled);
        self.add_info_message(Self::raw_output_mode_notice(enabled).to_string(), None);
    }

    pub(crate) fn toggle_raw_output_mode_and_notify(&mut self) -> bool {
        let enabled = !self.raw_output_mode;
        self.set_raw_output_mode_and_notify(enabled);
        enabled
    }

    pub(crate) fn on_terminal_resize(&mut self, width: u16) {
        let had_rendered_width = self.last_rendered_width.get().is_some();
        self.last_rendered_width.set(Some(width as usize));
        let stream_width = self.current_stream_width(2);
        let plan_stream_width = self.current_stream_width(4);
        if let Some(controller) = self.stream_controller.as_mut() {
            controller.set_width(stream_width);
        }
        if let Some(controller) = self.plan_stream_controller.as_mut() {
            controller.set_width(plan_stream_width);
        }
        self.sync_active_stream_tail();
        if !had_rendered_width {
            self.request_redraw();
        }
    }

    pub(crate) fn has_active_agent_stream(&self) -> bool {
        self.stream_controller.is_some()
    }

    pub(crate) fn has_active_plan_stream(&self) -> bool {
        self.plan_stream_controller.is_some()
    }

    fn is_plan_streaming_in_tui(&self) -> bool {
        self.plan_stream_controller.is_some()
    }

    pub(crate) fn composer_is_empty(&self) -> bool {
        self.bottom_pane.composer_is_empty()
    }

    pub(crate) fn is_task_running(&self) -> bool {
        self.bottom_pane.is_task_running()
    }

    pub(crate) fn toggle_vim_mode_and_notify(&mut self) {
        let enabled = self.bottom_pane.toggle_vim_enabled();
        let message = if enabled {
            "Vim mode enabled."
        } else {
            "Vim mode disabled."
        };
        self.add_info_message(message.to_string(), None);
    }

    pub(crate) fn is_normal_backtrack_mode(&self) -> bool {
        self.bottom_pane.is_normal_backtrack_mode()
    }

    pub(crate) fn should_handle_vim_insert_escape(&self, key_event: KeyEvent) -> bool {
        self.bottom_pane
            .composer_should_handle_vim_insert_escape(key_event)
    }

    pub(crate) fn insert_str(&mut self, text: &str) {
        self.bottom_pane.insert_str(text);
    }

    pub(crate) fn set_task_mentions(
        &mut self,
        task_mentions: Vec<octopus_world_terminal_client::TaskMentionCandidate>,
    ) {
        self.bottom_pane.set_task_mentions(task_mentions);
    }

    pub(crate) fn set_composer_text(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
    ) {
        self.bottom_pane
            .set_composer_text(text, text_elements, local_image_paths);
        self.refresh_plan_mode_nudge();
    }

    pub(crate) fn set_remote_image_urls(&mut self, remote_image_urls: Vec<String>) {
        self.bottom_pane.set_remote_image_urls(remote_image_urls);
    }

    fn take_remote_image_urls(&mut self) -> Vec<String> {
        self.bottom_pane.take_remote_image_urls()
    }

    #[cfg(test)]
    pub(crate) fn remote_image_urls(&self) -> Vec<String> {
        self.bottom_pane.remote_image_urls()
    }

    pub(crate) fn show_esc_backtrack_hint(&mut self) {
        self.bottom_pane.show_esc_backtrack_hint();
    }

    pub(crate) fn clear_esc_backtrack_hint(&mut self) {
        self.bottom_pane.clear_esc_backtrack_hint();
    }

    pub(crate) fn submit_op<T>(&mut self, op: T) -> bool
    where
        T: Into<AppCommand>,
    {
        let op: AppCommand = op.into();
        self.prepare_local_op_submission(&op);
        if op.is_review() && !self.bottom_pane.is_task_running() {
            self.bottom_pane.set_task_running(true);
        }
        match &self.codex_op_target {
            #[cfg(test)]
            CodexOpTarget::Direct(codex_op_tx) => {
                crate::session_log::log_outbound_op(&op);
                if let Err(e) = codex_op_tx.send(op) {
                    tracing::error!("failed to submit op: {e}");
                    return false;
                }
            }
            CodexOpTarget::AppEvent => {
                self.app_event_tx.send(AppEvent::CodexOp(op));
            }
        }
        true
    }

    pub(crate) fn set_prompt_history_metadata(&mut self, log_id: u64, entry_count: usize) {
        self.bottom_pane.set_history_metadata(log_id, entry_count);
    }

    pub(crate) fn prepare_local_op_submission(&mut self, op: &AppCommand) {
        if let AppCommand::Interrupt { behavior } = op
            && self.turn_lifecycle.agent_turn_running
        {
            if *behavior == crate::app_command::InterruptBehavior::RestorePromptIfNoOutput {
                self.arm_cancel_edit();
            }
            if let Some(controller) = self.stream_controller.as_mut() {
                controller.clear_queue();
            }
            if let Some(controller) = self.plan_stream_controller.as_mut() {
                controller.clear_queue();
            }
            self.clear_active_stream_tail();
            self.request_redraw();
        }
    }

    pub(crate) fn sync_runtime_projection_config(&mut self, config: &TuiConfig) {
        self.config.features = config.features.clone();
        self.config.effective_config = config.effective_config.clone();
        self.config.project_layers = config.project_layers.clone();
        self.config.memories = config.memories.clone();
        self.config.terminal.resize_reflow = config.terminal.resize_reflow;
        self.sync_mentions_v2_enabled();
    }

    pub(crate) fn token_usage(&self) -> TokenUsage {
        self.token_info
            .as_ref()
            .map(|ti| ti.total_token_usage.clone())
            .unwrap_or_default()
    }

    pub(crate) fn thread_id(&self) -> Option<String> {
        self.thread_id.clone()
    }

    pub(crate) fn thread_name(&self) -> Option<String> {
        self.thread_name.clone()
    }

    pub(crate) fn rollout_path(&self) -> Option<PathBuf> {
        self.current_rollout_path.clone()
    }

    pub(crate) fn active_cell_transcript_key(&self) -> Option<ActiveCellTranscriptKey> {
        let cell = self.transcript.active_cell.as_ref();
        let hook_cell = self.active_hook_cell.as_ref();
        let token_activity_cell = self.pending_token_activity_output();
        let rate_limit_reset_hint = self.pending_rate_limit_reset_hint();
        if cell.is_none()
            && hook_cell.is_none()
            && token_activity_cell.is_none()
            && rate_limit_reset_hint.is_none()
        {
            return None;
        }
        Some(ActiveCellTranscriptKey {
            revision: self.transcript.active_cell_revision,
            is_stream_continuation: cell
                .map(|cell| cell.is_stream_continuation())
                .unwrap_or(false),
            animation_tick: cell
                .and_then(|cell| cell.transcript_animation_tick())
                .or_else(|| {
                    hook_cell.and_then(super::history_cell::HistoryCell::transcript_animation_tick)
                }),
        })
    }

    pub(crate) fn active_cell_transcript_hyperlink_lines(
        &self,
        width: u16,
    ) -> Option<Vec<HyperlinkLine>> {
        let mut lines = Vec::new();
        if let Some(cell) = self.transcript.active_cell.as_ref() {
            lines.extend(cell.transcript_hyperlink_lines(width));
        }
        if let Some(hook_cell) = self.active_hook_cell.as_ref() {
            let hook_lines = hook_cell.transcript_hyperlink_lines(width);
            if !hook_lines.is_empty() && !lines.is_empty() {
                lines.push(HyperlinkLine::from(""));
            }
            lines.extend(hook_lines);
        }
        if let Some(token_activity_cell) = self.pending_token_activity_output() {
            let token_activity_lines = token_activity_cell.transcript_hyperlink_lines(width);
            if !token_activity_lines.is_empty() && !lines.is_empty() {
                lines.push(HyperlinkLine::from(""));
            }
            lines.extend(token_activity_lines);
        }
        if let Some(rate_limit_reset_hint) = self.pending_rate_limit_reset_hint() {
            let hint_lines = rate_limit_reset_hint.transcript_hyperlink_lines(width);
            if !hint_lines.is_empty() && !lines.is_empty() {
                lines.push(HyperlinkLine::from(""));
            }
            lines.extend(hint_lines);
        }
        (!lines.is_empty()).then_some(lines)
    }

    #[cfg(test)]
    pub(crate) fn active_cell_transcript_lines(&self, width: u16) -> Option<Vec<Line<'static>>> {
        self.active_cell_transcript_hyperlink_lines(width)
            .map(crate::terminal_hyperlinks::visible_lines)
    }

    pub(crate) fn config_ref(&self) -> &TuiConfig {
        &self.config
    }

    #[cfg(test)]
    pub(crate) fn status_line_text(&self) -> Option<String> {
        self.bottom_pane.status_line_text()
    }

    pub(crate) fn clear_token_usage(&mut self) {
        self.token_info = None;
    }
}

fn has_websocket_timing_metrics(summary: RuntimeMetricsSummary) -> bool {
    summary.responses_api_overhead_ms > 0
        || summary.responses_api_inference_time_ms > 0
        || summary.responses_api_engine_iapi_ttft_ms > 0
        || summary.responses_api_engine_service_ttft_ms > 0
        || summary.responses_api_engine_iapi_tbt_ms > 0
        || summary.responses_api_engine_service_tbt_ms > 0
}

impl Drop for ChatWidget {
    fn drop(&mut self) {
        self.stop_rate_limit_poller();
    }
}

const PLACEHOLDERS: [&str; 8] = [
    "Explain this codebase",
    "Summarize recent commits",
    "Implement {feature}",
    "Find and fix a bug in @filename",
    "Write tests for @filename",
    "Improve documentation in @filename",
    "Run /review on my current changes",
    "Inspect available interfaces",
];

const SIDE_PLACEHOLDERS: [&str; 3] = [
    "Check recently modified functions for compatibility",
    "How many files have been modified?",
    "Will this algorithm scale well?",
];

fn extract_first_bold(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'*' {
            let start = i + 2;
            let mut j = start;
            while j + 1 < bytes.len() {
                if bytes[j] == b'*' && bytes[j + 1] == b'*' {
                    let inner = &s[start..j];
                    let trimmed = inner.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    } else {
                        return None;
                    }
                }
                j += 1;
            }
            return None;
        }
        i += 1;
    }
    None
}

#[cfg(test)]
pub(crate) mod tests;
