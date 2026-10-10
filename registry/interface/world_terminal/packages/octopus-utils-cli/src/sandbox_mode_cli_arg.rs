use clap::ValueEnum;
use octopus_substrate_contract::sandbox_policy::SandboxMode;

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum SandboxModeCliArg {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl From<SandboxModeCliArg> for SandboxMode {
    fn from(value: SandboxModeCliArg) -> Self {
        match value {
            SandboxModeCliArg::ReadOnly => SandboxMode::ReadOnly,
            SandboxModeCliArg::WorkspaceWrite => SandboxMode::WorkspaceWrite,
            SandboxModeCliArg::DangerFullAccess => SandboxMode::DangerFullAccess,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn maps_cli_args_to_protocol_modes() {
        assert_eq!(SandboxMode::ReadOnly, SandboxModeCliArg::ReadOnly.into());
        assert_eq!(
            SandboxMode::WorkspaceWrite,
            SandboxModeCliArg::WorkspaceWrite.into()
        );
        assert_eq!(
            SandboxMode::DangerFullAccess,
            SandboxModeCliArg::DangerFullAccess.into()
        );
    }
}
