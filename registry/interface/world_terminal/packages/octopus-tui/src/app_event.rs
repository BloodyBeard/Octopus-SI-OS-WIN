use std::path::PathBuf;

use crate::approval_presets::ApprovalPreset;
use crate::session_vocab::openai_models::ModelPreset;
use octopus_file_search::FileMatch;
use octopus_substrate_contract::approvals_substrate::AskForApproval;
use octopus_world_terminal_client::AccountUsageReport;
use octopus_world_terminal_client::CreditsItem;
use octopus_world_terminal_client::ExecCommandItem;
use octopus_world_terminal_client::GoalState;
use octopus_world_terminal_client::RateLimitSnapshotItem;

use crate::app_command::AppCommand;
use crate::bottom_pane::ApprovalRequest;
use crate::bottom_pane::StatusLineItem;
use crate::bottom_pane::TerminalTitleItem;
use crate::chatwidget::UserMessage;
use crate::goal_files::GoalDraft;
use crate::session_vocab::config_types::ApprovalsReviewer;
use crate::session_vocab::config_types::CollaborationModeMask;
use crate::session_vocab::config_types::Personality;
use crate::session_vocab::openai_models::ReasoningEffort;
use octopus_substrate_contract::permission_profile::ActivePermissionProfile;

use crate::history_cell::HistoryCell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThreadGoalSetMode {
    ConfirmIfExists,
    ReplaceExisting,
    UpdateExisting {
        status: GoalState,
        token_budget: Option<i64>,
    },
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RateLimitsReport {
    pub(crate) snapshots: Vec<RateLimitSnapshotItem>,
    pub(crate) reset_credits: Option<CreditsItem>,
}

#[derive(Debug, Clone)]
pub(crate) struct HistoryLookupResponse {
    pub(crate) offset: usize,
    pub(crate) log_id: u64,
    pub(crate) entry: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConsolidationScrollbackReflow {
    IfResizeReflowRan,
    Required,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TranscriptExportDestination {
    Clipboard,
    File(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) enum WindowsSandboxEnableMode {
    Elevated,
    Legacy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RateLimitRefreshOrigin {
    StartupPrefetch { reset_hint_request_id: u64 },
    StatusCommand { request_id: u64 },
    ResetConsume { request_id: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeymapEditIntent {
    ReplaceAll,
    AddAlternate,
    ReplaceOne { old_key: String },
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum AppEvent {
    OpenAgentPicker,
    SelectAgentThread(String),

    StartSide {
        parent_thread_id: String,
        user_message: Option<UserMessage>,
    },

    PromptHistoryEntryResponse {
        event: HistoryLookupResponse,
    },

    SyncThreadGitBranch {
        thread_id: String,
        branch: String,
    },

    LookupMessageHistoryEntry {
        offset: usize,
        log_id: u64,
    },

    NewSession,

    ClearUi,

    RawOutputModeChanged {
        enabled: bool,
    },

    ClearUiAndSubmitUserMessage {
        text: String,
    },

    OpenRegistriesTab {
        focus_kind: Option<String>,
    },

    OpenDashboardTab,

    OpenMetricsTab,

    OpenResumePicker,

    OpenTaskManager,

    ShowWorkingDirectory,

    ChangeWorkingDirectory {
        path: String,
    },

    ResumeSessionByIdOrName(String),

    ArchiveCurrentThread,

    DeleteCurrentThread,

    ForkCurrentSession,

    OpenTranscriptExportFilePrompt,

    ExportTranscript {
        destination: TranscriptExportDestination,
    },

    OpenFeedbackPrompt {
        category: octopus_world_terminal_support::FeedbackCategory,
    },

    OpenFeedbackConsent {
        category: octopus_world_terminal_support::FeedbackCategory,
        note: String,
    },

    SubmitFeedback {
        category: octopus_world_terminal_support::FeedbackCategory,
        note: String,
        consent: octopus_world_terminal_support::AttachmentConsent,
    },

    FeedbackBundleFinished {
        result: Result<octopus_world_terminal_support::SupportBundleReceipt, String>,
    },

    ShowUpdateDiagnostics,

    Exit(ExitMode),

    Logout,

    #[allow(dead_code)]
    FatalExitRequest(String),

    CodexOp(AppCommand),

    ShellCommandCompleted {
        item: ExecCommandItem,
    },

    RestoreCancelledTurn(UserMessage),

    ApproveRecentAutoReviewDenial {
        thread_id: String,
        id: String,
    },

    StartFileSearch(String),

    FileSearchResult {
        query: String,
        matches: Vec<FileMatch>,
    },

    RefreshRateLimits {
        origin: RateLimitRefreshOrigin,
    },

    OpenThreadGoalMenu {
        thread_id: String,
    },

    OpenThreadGoalEditor {
        thread_id: Option<String>,
    },

    SetThreadGoalDraft {
        thread_id: String,
        draft: GoalDraft,
        mode: ThreadGoalSetMode,
    },

    SetThreadGoalStatus {
        thread_id: String,
        status: GoalState,
    },

    ClearThreadGoal {
        thread_id: String,
    },

    RateLimitsLoaded {
        origin: RateLimitRefreshOrigin,
        result: Result<RateLimitsReport, String>,
    },

    OpenTokenActivity,

    OpenRateLimitResetCredits,

    RateLimitResetCreditsLoaded {
        request_id: u64,
        result: Result<CreditsItem, String>,
    },

    ConsumeRateLimitResetCredit {
        idempotency_key: String,
    },

    RateLimitResetCreditConsumed {
        request_id: u64,
        idempotency_key: String,
        result: Result<CreditsItem, String>,
    },

    RefreshTokenActivity {
        request_id: u64,
    },

    TokenActivityLoaded {
        request_id: u64,
        result: Result<AccountUsageReport, String>,
    },

    CommitPendingUsageOutput,

    CommitPendingUsageOutputAfterStreamShutdown,

    SendAddCreditsNudgeEmail {
        credit_type: String,
    },

    AddCreditsNudgeEmailFinished {
        result: Result<String, String>,
    },

    DiffResult(String),

    OpenUrlInBrowser {
        url: String,
    },

    OpenDesktopThread {
        thread_id: String,
    },

    PetSelected {
        pet_id: String,
    },

    PetDisabled,

    PetPreviewRequested {
        pet_id: String,
    },

    PetPreviewLoaded {
        request_id: u64,
        result: Result<crate::pets::AmbientPet, String>,
    },

    PetSelectionLoaded {
        request_id: u64,
        pet_id: String,
        result: Result<Option<crate::pets::AmbientPet>, String>,
    },

    ConfiguredPetLoaded {
        pet_id: String,
        result: Result<Option<crate::pets::AmbientPet>, String>,
    },

    FetchHooksList {
        cwd: PathBuf,
    },

    HooksLoaded {
        cwd: PathBuf,
        result: Result<octopus_world_terminal_client::HookCatalog, String>,
    },

    BeginInitialHistoryReplayBuffer,

    BeginThreadSwitchHistoryReplayBuffer,

    InsertHistoryCell(Box<dyn HistoryCell>),

    EndInitialHistoryReplayBuffer,

    ConsolidateAgentMessage {
        source: String,
        cwd: PathBuf,
        scrollback_reflow: ConsolidationScrollbackReflow,
        deferred_history_cell: Option<Box<dyn HistoryCell>>,
    },

    ConsolidateProposedPlan(String),

    ApplyThreadRollback {
        num_turns: u32,
    },

    StartCommitAnimation,
    StopCommitAnimation,
    CommitTick,

    UpdateReasoningEffort(Option<ReasoningEffort>),

    UpdateModel(String),

    UpdatePersonality(Personality),

    SettingsSelectionClosed,
    SettingsSelectionSettled,

    PersistModelSelection {
        model: String,
        effort: Option<ReasoningEffort>,
    },

    PersistPersonalitySelection {
        personality: Personality,
    },

    PersistServiceTierSelection {
        service_tier: Option<String>,
    },

    OpenReasoningPopup {
        model: ModelPreset,
    },

    OpenPlanReasoningScopePrompt {
        model: String,
        effort: Option<ReasoningEffort>,
    },

    OpenAllModelsPopup {
        models: Vec<ModelPreset>,
    },

    OpenFullAccessConfirmation {
        preset: ApprovalPreset,
        return_to_permissions: bool,
        profile_selection: Option<PermissionProfileSelection>,
    },

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    OpenWorldWritableWarningConfirmation {
        preset: Option<ApprovalPreset>,
        profile_selection: Option<PermissionProfileSelection>,
        sample_paths: Vec<String>,
        extra_count: usize,
        failed_scan: bool,
    },

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    OpenWindowsSandboxEnablePrompt {
        preset: ApprovalPreset,
        profile_selection: Option<PermissionProfileSelection>,
    },

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    OpenWindowsSandboxFallbackPrompt {
        preset: ApprovalPreset,
        profile_selection: Option<PermissionProfileSelection>,
    },

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    BeginWindowsSandboxElevatedSetup {
        preset: ApprovalPreset,
        profile_selection: Option<PermissionProfileSelection>,
    },

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    BeginWindowsSandboxLegacySetup {
        preset: ApprovalPreset,
        profile_selection: Option<PermissionProfileSelection>,
    },

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    BeginWindowsSandboxGrantReadRoot {
        path: String,
    },

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    WindowsSandboxGrantReadRootCompleted {
        path: PathBuf,
        error: Option<String>,
    },

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    EnableWindowsSandboxForAgentMode {
        preset: ApprovalPreset,
        mode: WindowsSandboxEnableMode,
        profile_selection: Option<PermissionProfileSelection>,
    },

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    UpdateAskForApprovalPolicy(AskForApproval),

    UpdateActivePermissionProfile(ActivePermissionProfile),

    SelectPermissionProfile(PermissionProfileSelection),

    UpdateApprovalsReviewer(ApprovalsReviewer),

    UpdateFeatureFlags {
        updates: Vec<(String, bool)>,
    },

    UpdateMemorySettings {
        use_memories: bool,
        generate_memories: bool,
    },

    ResetMemories,

    UpdateFullAccessWarningAcknowledged(bool),

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    UpdateWorldWritableWarningAcknowledged(bool),

    UpdateRateLimitSwitchPromptHidden(bool),

    UpdatePlanModeReasoningEffort(Option<ReasoningEffort>),

    PersistFullAccessWarningAcknowledged,

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    PersistWorldWritableWarningAcknowledged,

    PersistRateLimitSwitchPromptHidden,

    PersistPlanModeReasoningEffort(Option<ReasoningEffort>),

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    SkipNextWorldWritableScan,

    OpenApprovalsPopup,

    SetHookEnabled {
        key: String,
        enabled: bool,
    },

    TrustHook {
        key: String,
        current_hash: String,
    },

    TrustHooks {
        updates: Vec<crate::hooks_surface::HookTrustUpdate>,
    },

    HookEnabledSet {
        key: String,
        enabled: bool,
        result: Result<(), String>,
    },

    HookTrusted {
        result: Result<(), String>,
    },

    OpenPermissionsPopup,

    OpenReviewBranchPicker(PathBuf),

    OpenReviewCommitPicker(PathBuf),

    OpenReviewCustomPrompt,

    SubmitUserMessageWithMode {
        text: String,
        collaboration_mode: CollaborationModeMask,
    },

    FullScreenApprovalRequest(ApprovalRequest),

    LaunchExternalEditor,

    StatusLineBranchUpdated {
        cwd: PathBuf,
        branch: Option<String>,
    },
    StatusLineGitSummaryUpdated {
        cwd: PathBuf,
        summary: crate::chatwidget::StatusLineGitSummary,
    },
    StatusLineSetup {
        items: Vec<StatusLineItem>,
        use_theme_colors: bool,
    },
    StatusLineSetupCancelled,

    TerminalTitleSetup {
        items: Vec<TerminalTitleItem>,
    },
    TerminalTitleSetupPreview {
        items: Vec<TerminalTitleItem>,
    },
    TerminalTitleSetupCancelled,

    SyntaxThemeSelected {
        name: String,
    },

    SyntaxThemePreviewed,

    OpenKeymapActionMenu {
        context: String,
        action: String,
    },

    OpenKeymapReplaceBindingMenu {
        context: String,
        action: String,
    },

    OpenKeymapCapture {
        context: String,
        action: String,
        intent: KeymapEditIntent,
    },

    OpenKeymapDebug,

    KeymapCaptured {
        context: String,
        action: String,
        key: String,
        intent: KeymapEditIntent,
    },

    KeymapCleared {
        context: String,
        action: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct PermissionProfileSelection {
    pub profile_id: String,
    pub approval_policy: Option<AskForApproval>,
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    pub display_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExitMode {
    ShutdownFirst,
    Immediate,
    Detach { run_id: String },
}
