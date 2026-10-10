#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentMetadata {
    pub(crate) agent_nickname: Option<String>,
    pub(crate) agent_role: Option<String>,
}
