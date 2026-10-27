use std::path::PathBuf;

use crate::session_vocab::config_types::CollaborationMode;
use crate::session_vocab::config_types::Personality;
use octopus_substrate_contract::approvals_substrate::AskForApproval;
use octopus_substrate_contract::permission_profile::ActivePermissionProfile;
use octopus_substrate_contract::permission_profile::PermissionProfile;
use octopus_util_abspath::AbsolutePathBuf;
use octopus_util_pathuri::PathUri;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionNetworkProxyRuntime {
    pub(crate) http_addr: String,
    pub(crate) socks_addr: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ThreadSessionState {
    pub(crate) thread_id: String,
    pub(crate) forked_from_id: Option<String>,
    pub(crate) fork_parent_title: Option<String>,
    pub(crate) thread_name: Option<String>,
    pub(crate) model: String,
    pub(crate) model_provider_id: String,
    pub(crate) service_tier: Option<String>,
    pub(crate) approval_policy: AskForApproval,
    pub(crate) approvals_reviewer: crate::session_vocab::config_types::ApprovalsReviewer,
    pub(crate) permission_profile: PermissionProfile,
    pub(crate) active_permission_profile: Option<ActivePermissionProfile>,
    pub(crate) cwd: AbsolutePathBuf,
    pub(crate) runtime_workspace_roots: Vec<AbsolutePathBuf>,
    pub(crate) instruction_source_paths: Vec<PathUri>,
    pub(crate) reasoning_effort: Option<crate::session_vocab::openai_models::ReasoningEffort>,
    pub(crate) collaboration_mode: Option<Box<CollaborationMode>>,
    pub(crate) personality: Option<Personality>,
    pub(crate) network_proxy: Option<SessionNetworkProxyRuntime>,
    pub(crate) rollout_path: Option<PathBuf>,
}

