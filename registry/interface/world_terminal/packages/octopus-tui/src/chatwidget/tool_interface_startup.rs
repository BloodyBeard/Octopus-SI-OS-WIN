use std::collections::BTreeSet;

use super::ChatWidget;

const TOOL_INTERFACE_STARTUP_SINGLE_HEADER_PREFIX: &str = "Booting tool-interface endpoint:";
const TOOL_INTERFACE_STARTUP_MULTI_HEADER_PREFIX: &str = "Starting tool-interface endpoints";

#[derive(Debug, Clone)]
pub(crate) enum ToolInterfaceStartupStatus {
    Starting,
    Ready,
    Failed { error: String },
    Cancelled,
}

impl ChatWidget {
    fn update_tool_interface_startup_status(
        &mut self,
        endpoint: String,
        status: ToolInterfaceStartupStatus,
        complete_when_settled: bool,
    ) {
        let mut activated_pending_round = false;
        let startup_status = if self.tool_interface_startup_ignore_updates_until_next_start {
            if matches!(status, ToolInterfaceStartupStatus::Starting)
                && !self.tool_interface_startup_pending_next_round_saw_starting
            {
                self.tool_interface_startup_pending_next_round.clear();
                self.tool_interface_startup_allow_terminal_only_next_round = false;
            }
            self.tool_interface_startup_pending_next_round_saw_starting |=
                matches!(status, ToolInterfaceStartupStatus::Starting);
            self.tool_interface_startup_pending_next_round
                .insert(endpoint, status);
            let Some(expected_endpoints) = &self.tool_interface_startup_expected_endpoints else {
                return;
            };
            let saw_full_round = expected_endpoints.is_empty()
                || expected_endpoints.iter().all(|name| {
                    self.tool_interface_startup_pending_next_round
                        .contains_key(name)
                });
            let saw_starting = self
                .tool_interface_startup_pending_next_round
                .values()
                .any(|state| matches!(state, ToolInterfaceStartupStatus::Starting));
            if !(saw_full_round
                && (saw_starting || self.tool_interface_startup_allow_terminal_only_next_round))
            {
                return;
            }

            self.tool_interface_startup_ignore_updates_until_next_start = false;
            self.tool_interface_startup_allow_terminal_only_next_round = false;
            self.tool_interface_startup_pending_next_round_saw_starting = false;
            activated_pending_round = true;
            std::mem::take(&mut self.tool_interface_startup_pending_next_round)
        } else {
            let mut startup_status = self
                .tool_interface_startup_status
                .take()
                .unwrap_or_default();
            if let ToolInterfaceStartupStatus::Failed { error } = &status {
                let already_reported = matches!(
                    startup_status.get(&endpoint),
                    Some(ToolInterfaceStartupStatus::Failed { error: previous }) if previous == error
                );
                if !already_reported {
                    self.on_warning(error);
                }
            }
            startup_status.insert(endpoint, status);
            startup_status
        };
        if activated_pending_round {
            for state in startup_status.values() {
                if let ToolInterfaceStartupStatus::Failed { error } = state {
                    self.on_warning(error);
                }
            }
        }
        self.tool_interface_startup_status = Some(startup_status);
        self.update_task_running_state();

        if complete_when_settled
            && let Some(current) = &self.tool_interface_startup_status
            && let Some(expected_endpoints) = &self.tool_interface_startup_expected_endpoints
            && !current.is_empty()
            && expected_endpoints
                .iter()
                .all(|name| current.contains_key(name))
            && current
                .values()
                .all(|state| !matches!(state, ToolInterfaceStartupStatus::Starting))
        {
            let mut failed = Vec::new();
            let mut cancelled = Vec::new();
            for (name, state) in current {
                match state {
                    ToolInterfaceStartupStatus::Ready => {}
                    ToolInterfaceStartupStatus::Failed { .. } => failed.push(name.clone()),
                    ToolInterfaceStartupStatus::Cancelled => cancelled.push(name.clone()),
                    ToolInterfaceStartupStatus::Starting => {}
                }
            }
            failed.sort();
            cancelled.sort();
            self.finish_tool_interface_startup(failed, cancelled);
            return;
        }
        if let Some(current) = &self.tool_interface_startup_status {
            let total = current.len();
            let mut starting: Vec<_> = current
                .iter()
                .filter_map(|(name, state)| {
                    if matches!(state, ToolInterfaceStartupStatus::Starting) {
                        Some(name)
                    } else {
                        None
                    }
                })
                .collect();
            starting.sort();
            if let Some(first) = starting.first() {
                let completed = total.saturating_sub(starting.len());
                let max_to_show = 3;
                let mut to_show: Vec<String> = starting
                    .iter()
                    .take(max_to_show)
                    .map(ToString::to_string)
                    .collect();
                if starting.len() > max_to_show {
                    to_show.push("…".to_string());
                }
                let header = if total > 1 {
                    format!(
                        "{TOOL_INTERFACE_STARTUP_MULTI_HEADER_PREFIX} ({completed}/{total}): {}",
                        to_show.join(", ")
                    )
                } else {
                    format!("{TOOL_INTERFACE_STARTUP_SINGLE_HEADER_PREFIX} {first}")
                };
                self.set_status_header(header);
            }
        }
        self.request_redraw();
    }

    pub(crate) fn set_tool_interface_startup_expected_endpoints<I>(&mut self, endpoint_names: I)
    where
        I: IntoIterator<Item = String>,
    {
        self.tool_interface_startup_expected_endpoints = Some(endpoint_names.into_iter().collect());
    }

    pub(super) fn finish_tool_interface_startup(
        &mut self,
        failed: Vec<String>,
        cancelled: Vec<String>,
    ) {
        if !cancelled.is_empty() {
            self.on_warning(format!(
                "tool-interface startup interrupted. The following endpoints were not initialized: {}",
                cancelled.join(", ")
            ));
        }
        let mut parts = Vec::new();
        if !failed.is_empty() {
            parts.push(format!("failed: {}", failed.join(", ")));
        }
        if !parts.is_empty() {
            self.on_warning(format!(
                "tool-interface startup incomplete ({})",
                parts.join("; ")
            ));
        }

        let tool_interface_startup_owned_status =
            self.status_header_is_tool_interface_startup_owned();
        self.tool_interface_startup_status = None;
        self.tool_interface_startup_ignore_updates_until_next_start = true;
        self.tool_interface_startup_allow_terminal_only_next_round = false;
        self.tool_interface_startup_pending_next_round.clear();
        self.tool_interface_startup_pending_next_round_saw_starting = false;
        self.update_task_running_state();
        if self.bottom_pane.is_task_running() && tool_interface_startup_owned_status {
            self.restore_reasoning_status_header();
        }
        self.maybe_send_next_queued_input();
        self.request_redraw();
    }

    pub(crate) fn finish_tool_interface_startup_after_lag(&mut self) {
        if self.tool_interface_startup_ignore_updates_until_next_start {
            if self.tool_interface_startup_pending_next_round.is_empty() {
                self.tool_interface_startup_pending_next_round_saw_starting = false;
            }
            self.tool_interface_startup_allow_terminal_only_next_round = true;
        }

        let Some(current) = &self.tool_interface_startup_status else {
            return;
        };

        let mut failed = Vec::new();
        let mut cancelled = Vec::new();

        let mut endpoint_names: BTreeSet<String> = current.keys().cloned().collect();
        if let Some(expected_endpoints) = &self.tool_interface_startup_expected_endpoints {
            endpoint_names.extend(expected_endpoints.iter().cloned());
        }

        for name in endpoint_names {
            match current.get(&name) {
                Some(ToolInterfaceStartupStatus::Ready) => {}
                Some(ToolInterfaceStartupStatus::Failed { .. }) => failed.push(name),
                Some(
                    ToolInterfaceStartupStatus::Cancelled | ToolInterfaceStartupStatus::Starting,
                )
                | None => {
                    cancelled.push(name);
                }
            }
        }

        failed.sort();
        failed.dedup();
        cancelled.sort();
        cancelled.dedup();
        self.finish_tool_interface_startup(failed, cancelled);
    }

    pub(super) fn status_header_is_tool_interface_startup_owned(&self) -> bool {
        self.status_state
            .current_status
            .header
            .starts_with(TOOL_INTERFACE_STARTUP_SINGLE_HEADER_PREFIX)
            || self
                .status_state
                .current_status
                .header
                .starts_with(TOOL_INTERFACE_STARTUP_MULTI_HEADER_PREFIX)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn on_tool_interface_startup_update(
        &mut self,
        endpoint: String,
        status: ToolInterfaceStartupStatus,
    ) {
        self.update_tool_interface_startup_status(endpoint, status, true);
    }
}
