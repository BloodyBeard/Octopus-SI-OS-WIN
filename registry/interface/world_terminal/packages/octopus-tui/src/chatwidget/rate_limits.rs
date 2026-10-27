use super::*;
use octopus_world_terminal_client::CreditsItem;

pub(super) const NUDGE_MODEL_SLUG: &str = "gpt-5.4-mini";
pub(super) const RATE_LIMIT_SWITCH_PROMPT_THRESHOLD: f64 = 90.0;

const RATE_LIMIT_WARNING_THRESHOLDS: [f64; 3] = [75.0, 90.0, 95.0];
const PRIMARY_LIMIT_FALLBACK_LABEL: &str = "usage";
const SECONDARY_LIMIT_FALLBACK_LABEL: &str = "secondary usage";

#[derive(Default)]
pub(super) struct RateLimitWarningState {
    pub(super) secondary_index: usize,
    pub(super) primary_index: usize,
}

impl RateLimitWarningState {
    pub(super) fn take_warnings(
        &mut self,
        secondary_used_percent: Option<f64>,
        secondary_window_minutes: Option<i64>,
        primary_used_percent: Option<f64>,
        primary_window_minutes: Option<i64>,
    ) -> Vec<String> {
        let reached_secondary_cap =
            matches!(secondary_used_percent, Some(percent) if percent == 100.0);
        let reached_primary_cap = matches!(primary_used_percent, Some(percent) if percent == 100.0);
        if reached_secondary_cap || reached_primary_cap {
            return Vec::new();
        }

        let mut warnings = Vec::new();

        if let Some(secondary_used_percent) = secondary_used_percent {
            let mut highest_secondary: Option<f64> = None;
            while self.secondary_index < RATE_LIMIT_WARNING_THRESHOLDS.len()
                && secondary_used_percent >= RATE_LIMIT_WARNING_THRESHOLDS[self.secondary_index]
            {
                highest_secondary = Some(RATE_LIMIT_WARNING_THRESHOLDS[self.secondary_index]);
                self.secondary_index += 1;
            }
            if let Some(threshold) = highest_secondary {
                let limit_label = limit_label_for_window(secondary_window_minutes, true);
                let remaining_percent = 100.0 - threshold;
                warnings.push(format!(
                    "Heads up, you have less than {remaining_percent:.0}% of your {limit_label} limit left. Run /status for a breakdown."
                ));
            }
        }

        if let Some(primary_used_percent) = primary_used_percent {
            let mut highest_primary: Option<f64> = None;
            while self.primary_index < RATE_LIMIT_WARNING_THRESHOLDS.len()
                && primary_used_percent >= RATE_LIMIT_WARNING_THRESHOLDS[self.primary_index]
            {
                highest_primary = Some(RATE_LIMIT_WARNING_THRESHOLDS[self.primary_index]);
                self.primary_index += 1;
            }
            if let Some(threshold) = highest_primary {
                let limit_label = limit_label_for_window(primary_window_minutes, false);
                let remaining_percent = 100.0 - threshold;
                warnings.push(format!(
                    "Heads up, you have less than {remaining_percent:.0}% of your {limit_label} limit left. Run /status for a breakdown."
                ));
            }
        }

        warnings
    }
}

pub(crate) fn limit_label_for_window(window_minutes: Option<i64>, is_secondary: bool) -> String {
    window_minutes
        .and_then(get_limits_duration)
        .unwrap_or_else(|| fallback_limit_label(is_secondary).to_string())
}

pub(crate) fn get_limits_duration(windows_minutes: i64) -> Option<String> {
    const MINUTES_PER_HOUR: i64 = 60;
    const MINUTES_PER_5_HOURS: i64 = 5 * MINUTES_PER_HOUR;
    const MINUTES_PER_DAY: i64 = 24 * MINUTES_PER_HOUR;
    const MINUTES_PER_WEEK: i64 = 7 * MINUTES_PER_DAY;
    const MINUTES_PER_MONTH: i64 = 30 * MINUTES_PER_DAY;
    const MINUTES_PER_YEAR: i64 = 365 * MINUTES_PER_DAY;

    let windows_minutes = windows_minutes.max(0);

    if is_approximate_window(windows_minutes, MINUTES_PER_5_HOURS) {
        Some("5h".to_string())
    } else if is_approximate_window(windows_minutes, MINUTES_PER_DAY) {
        Some("daily".to_string())
    } else if is_approximate_window(windows_minutes, MINUTES_PER_WEEK) {
        Some("weekly".to_string())
    } else if is_approximate_window(windows_minutes, MINUTES_PER_MONTH) {
        Some("monthly".to_string())
    } else if is_approximate_window(windows_minutes, MINUTES_PER_YEAR) {
        Some("annual".to_string())
    } else {
        None
    }
}

pub(crate) fn fallback_limit_label(is_secondary: bool) -> &'static str {
    if is_secondary {
        SECONDARY_LIMIT_FALLBACK_LABEL
    } else {
        PRIMARY_LIMIT_FALLBACK_LABEL
    }
}

fn plan_type_from_word(word: &str) -> Option<PlanType> {
    serde_json::from_value(serde_json::Value::String(word.to_ascii_lowercase())).ok()
}

fn is_approximate_window(minutes: i64, expected_minutes: i64) -> bool {
    let minutes = minutes as f64;
    let expected_minutes = expected_minutes as f64;
    minutes >= expected_minutes * 0.95 && minutes <= expected_minutes * 1.05
}

#[derive(Default)]
pub(super) enum RateLimitSwitchPromptState {
    #[default]
    Idle,
    Pending,
    Shown,
}

#[cfg(test)]
#[derive(Debug)]
pub(super) enum RateLimitErrorKind {
    UsageLimit,
    Generic,
}

impl ChatWidget {
    pub(crate) fn on_rate_limit_snapshot(&mut self, snapshot: Option<RateLimitSnapshotItem>) {
        if let Some(mut snapshot) = snapshot {
            let limit_id = snapshot
                .limit_id
                .clone()
                .unwrap_or_else(|| "codex".to_string());
            let limit_label = snapshot
                .limit_name
                .clone()
                .unwrap_or_else(|| limit_id.clone());
            if snapshot.credits.is_none() {
                snapshot.credits = self
                    .rate_limit_snapshots_by_limit_id
                    .get(&limit_id)
                    .and_then(|display| display.credits.as_ref())
                    .map(|credits| CreditsItem {
                        has_credits: credits.has_credits,
                        unlimited: credits.unlimited,
                        balance: credits.balance.clone(),
                    });
            }
            self.plan_type = snapshot
                .plan_type
                .as_deref()
                .and_then(plan_type_from_word)
                .or(self.plan_type);

            let is_codex_limit = limit_id.eq_ignore_ascii_case("codex");
            if is_codex_limit && let Some(rate_limit_reached) = snapshot.rate_limit_reached {
                self.codex_rate_limit_reached_type = Some(rate_limit_reached);
            }
            let warnings = if is_codex_limit {
                self.rate_limit_warnings.take_warnings(
                    snapshot
                        .secondary
                        .as_ref()
                        .map(|window| window.used_percent),
                    snapshot
                        .secondary
                        .as_ref()
                        .and_then(|window| window.window_minutes),
                    snapshot.primary.as_ref().map(|window| window.used_percent),
                    snapshot
                        .primary
                        .as_ref()
                        .and_then(|window| window.window_minutes),
                )
            } else {
                vec![]
            };

            let high_usage = is_codex_limit
                && (snapshot
                    .secondary
                    .as_ref()
                    .map(|w| w.used_percent >= RATE_LIMIT_SWITCH_PROMPT_THRESHOLD)
                    .unwrap_or(false)
                    || snapshot
                        .primary
                        .as_ref()
                        .map(|w| w.used_percent >= RATE_LIMIT_SWITCH_PROMPT_THRESHOLD)
                        .unwrap_or(false));

            let has_workspace_credits = snapshot
                .credits
                .as_ref()
                .map(|credits| credits.has_credits)
                .unwrap_or(false);

            if high_usage
                && !has_workspace_credits
                && !self.rate_limit_switch_prompt_hidden()
                && self.current_model() != NUDGE_MODEL_SLUG
                && !matches!(
                    self.rate_limit_switch_prompt,
                    RateLimitSwitchPromptState::Shown
                )
            {
                self.rate_limit_switch_prompt = RateLimitSwitchPromptState::Pending;
            }

            let display =
                rate_limit_snapshot_display_for_limit(&snapshot, limit_label, Local::now());
            self.rate_limit_snapshots_by_limit_id
                .insert(limit_id, display);

            if !warnings.is_empty() {
                for warning in warnings {
                    self.add_to_history(history_cell::new_warning_event(warning));
                }
                self.request_redraw();
            }
        } else {
            self.rate_limit_snapshots_by_limit_id.clear();
            self.codex_rate_limit_reached_type = None;
        }
        self.refresh_status_line();
    }

    pub(super) fn stop_rate_limit_poller(&mut self) {}

    pub(super) fn prefetch_rate_limits(&mut self) {
        self.stop_rate_limit_poller();
    }

    pub(super) fn should_prefetch_rate_limits(&self) -> bool {
        self.config.model_provider.requires_openai_auth && self.has_chatgpt_account
    }

    fn lower_cost_preset(&self) -> Option<ModelPreset> {
        let models = self.model_catalog.try_list_models().ok()?;
        models
            .iter()
            .find(|preset| preset.show_in_picker && preset.model == NUDGE_MODEL_SLUG)
            .cloned()
    }

    fn rate_limit_switch_prompt_hidden(&self) -> bool {
        self.config
            .notices
            .hide_rate_limit_model_nudge
            .unwrap_or(false)
    }

    pub(super) fn maybe_show_pending_rate_limit_prompt(&mut self) {
        if self.rate_limit_switch_prompt_hidden() {
            self.rate_limit_switch_prompt = RateLimitSwitchPromptState::Idle;
            return;
        }
        if !matches!(
            self.rate_limit_switch_prompt,
            RateLimitSwitchPromptState::Pending
        ) {
            return;
        }
        if let Some(preset) = self.lower_cost_preset() {
            self.open_rate_limit_switch_prompt(preset);
            self.rate_limit_switch_prompt = RateLimitSwitchPromptState::Shown;
        } else {
            self.rate_limit_switch_prompt = RateLimitSwitchPromptState::Idle;
        }
    }

    fn open_rate_limit_switch_prompt(&mut self, preset: ModelPreset) {
        let switch_model = preset.model;
        let switch_model_for_events = switch_model.clone();
        let default_effort: ReasoningEffortConfig = preset.default_reasoning_effort;

        let switch_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            tx.send(AppEvent::CodexOp(AppCommand::override_turn_context(
                None,
                None,
                None,
                None,
                None,
                None,
                Some(switch_model_for_events.clone()),
                Some(Some(default_effort.clone())),
                None,
                None,
                None,
                None,
            )));
            tx.send(AppEvent::UpdateModel(switch_model_for_events.clone()));
            tx.send(AppEvent::UpdateReasoningEffort(Some(
                default_effort.clone(),
            )));
        })];

        let keep_actions: Vec<SelectionAction> = Vec::new();
        let never_actions: Vec<SelectionAction> = vec![Box::new(|tx| {
            tx.send(AppEvent::UpdateRateLimitSwitchPromptHidden(true));
            tx.send(AppEvent::PersistRateLimitSwitchPromptHidden);
        })];
        let description = if preset.description.is_empty() {
            Some("Uses fewer credits for upcoming turns.".to_string())
        } else {
            Some(preset.description)
        };

        let items = vec![
            SelectionItem {
                name: format!("Switch to {switch_model}"),
                description,
                selected_description: None,
                is_current: false,
                actions: switch_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Keep current model".to_string(),
                description: None,
                selected_description: None,
                is_current: false,
                actions: keep_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Keep current model (never show again)".to_string(),
                description: Some(
                    "Hide future rate limit reminders about switching models.".to_string(),
                ),
                selected_description: None,
                is_current: false,
                actions: never_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Approaching rate limits".to_string()),
            subtitle: Some(format!("Switch to {switch_model} for lower credit usage?")),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    #[cfg(test)]
    pub(super) fn open_workspace_owner_nudge_prompt(&mut self, credit_type: &str) {
        if self.add_credits_nudge_email_in_flight.is_some() {
            return;
        }

        let (title, prompt) = if credit_type == "usage_limit" {
            (
                "Usage limit reached",
                "Request a limit increase from your owner to continue using codex. Request increase?",
            )
        } else {
            (
                "You've reached your workspace credit limit",
                "Your workspace is out of credits. Ask your workspace owner to add more. Notify owner?",
            )
        };
        let credit_type = credit_type.to_string();
        let send_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            tx.send(AppEvent::SendAddCreditsNudgeEmail {
                credit_type: credit_type.clone(),
            });
        })];
        let items = vec![
            SelectionItem {
                name: "Yes".to_string(),
                display_shortcut: Some(key_hint::plain(KeyCode::Char('y'))),
                actions: send_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "No".to_string(),
                display_shortcut: Some(key_hint::plain(KeyCode::Char('n'))),
                is_default: true,
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(title.to_string()),
            subtitle: Some(prompt.to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx: Some(1),
            ..Default::default()
        });
    }

    #[cfg(test)]
    pub(crate) fn start_add_credits_nudge_email_request(&mut self, credit_type: String) -> bool {
        self.add_credits_nudge_email_in_flight = Some(credit_type);
        true
    }

    #[cfg(test)]
    pub(crate) fn finish_add_credits_nudge_email_request(
        &mut self,
        result: Result<String, String>,
    ) {
        let credit_type = self
            .add_credits_nudge_email_in_flight
            .take()
            .unwrap_or_else(|| "credits".to_string());
        let usage_limit = credit_type == "usage_limit";
        let message = match (usage_limit, result) {
            (false, Ok(status)) if status == "cooldown_active" => {
                "Workspace owner was already notified recently."
            }
            (false, Ok(_)) => "Workspace owner notified.",
            (false, Err(_)) => "Could not notify your workspace owner. Please try again.",
            (true, Ok(status)) if status == "cooldown_active" => {
                "A limit increase was already requested recently."
            }
            (true, Ok(_)) => "Limit increase requested.",
            (true, Err(_)) => "Could not request a limit increase. Please try again.",
        };
        self.add_to_history(history_cell::new_info_event(message.to_string(), None));
        self.request_redraw();
    }

    pub(crate) fn set_rate_limit_switch_prompt_hidden(&mut self, hidden: bool) {
        self.config.notices.hide_rate_limit_model_nudge = Some(hidden);
        if hidden {
            self.rate_limit_switch_prompt = RateLimitSwitchPromptState::Idle;
        }
    }
}
