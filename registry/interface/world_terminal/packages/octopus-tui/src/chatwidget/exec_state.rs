use octopus_substrate_contract::parse_command::ParsedCommand;
use octopus_world_terminal_client::CommandActionItem;
use octopus_world_terminal_client::ExecCommandItem;
use octopus_world_terminal_client::ExecItemSource;

use crate::exec_command::ExecCommandSource;
use crate::exec_command::split_command_string;

pub(super) fn exec_source_from_item(source: ExecItemSource) -> ExecCommandSource {
    match source {
        ExecItemSource::Agent => ExecCommandSource::Agent,
        ExecItemSource::UserShell => ExecCommandSource::UserShell,
        ExecItemSource::UnifiedExecStartup => ExecCommandSource::UnifiedExecStartup,
        ExecItemSource::UnifiedExecInteraction => ExecCommandSource::UnifiedExecInteraction,
    }
}

pub(super) fn parsed_from_action(action: &CommandActionItem) -> ParsedCommand {
    match action {
        CommandActionItem::Read { cmd, name, path } => ParsedCommand::Read {
            cmd: cmd.clone(),
            name: name.clone(),
            path: std::path::PathBuf::from(path),
        },
        CommandActionItem::ListFiles { cmd, path } => ParsedCommand::ListFiles {
            cmd: cmd.clone(),
            path: path.clone(),
        },
        CommandActionItem::Search { cmd, query, path } => ParsedCommand::Search {
            cmd: cmd.clone(),
            query: query.clone(),
            path: path.clone(),
        },
        CommandActionItem::Unknown { cmd } => ParsedCommand::Unknown { cmd: cmd.clone() },
    }
}

pub(super) fn exec_item_command_and_parsed(
    item: &ExecCommandItem,
) -> (Vec<String>, Vec<ParsedCommand>) {
    (
        split_command_string(&item.command),
        item.command_actions
            .iter()
            .map(parsed_from_action)
            .collect(),
    )
}

pub(super) struct RunningCommand {
    pub(super) command: Vec<String>,
    pub(super) parsed_cmd: Vec<ParsedCommand>,
    pub(super) source: ExecCommandSource,
}

pub(super) struct UnifiedExecProcessSummary {
    pub(super) key: String,
    pub(super) call_id: String,
    pub(super) command_display: String,
    pub(super) recent_chunks: Vec<String>,
}

pub(super) struct UnifiedExecWaitState {
    command_display: String,
}

impl UnifiedExecWaitState {
    pub(super) fn new(command_display: String) -> Self {
        Self { command_display }
    }

    pub(super) fn is_duplicate(&self, command_display: &str) -> bool {
        self.command_display == command_display
    }
}

#[derive(Clone, Debug)]
pub(super) struct UnifiedExecWaitStreak {
    pub(super) process_id: String,
    pub(super) command_display: Option<String>,
}

impl UnifiedExecWaitStreak {
    #[cfg(test)]
    pub(super) fn new(process_id: String, command_display: Option<String>) -> Self {
        Self {
            process_id,
            command_display: command_display.filter(|display| !display.is_empty()),
        }
    }

    #[cfg(test)]
    pub(super) fn update_command_display(&mut self, command_display: Option<String>) {
        if self.command_display.is_some() {
            return;
        }
        self.command_display = command_display.filter(|display| !display.is_empty());
    }
}

pub(super) fn is_unified_exec_source(source: ExecCommandSource) -> bool {
    matches!(
        source,
        ExecCommandSource::UnifiedExecStartup | ExecCommandSource::UnifiedExecInteraction
    )
}

pub(super) fn is_standard_tool_call(parsed_cmd: &[ParsedCommand]) -> bool {
    !parsed_cmd.is_empty()
        && parsed_cmd
            .iter()
            .all(|parsed| !matches!(parsed, ParsedCommand::Unknown { .. }))
}
