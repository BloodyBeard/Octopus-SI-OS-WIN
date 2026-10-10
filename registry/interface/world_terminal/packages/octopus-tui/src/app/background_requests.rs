use super::*;
use crate::app_event::RateLimitsReport;
use crate::config_update::format_config_error;
use octopus_world_terminal_client::AccountUsageReport;
use octopus_world_terminal_client::CreditsItem;

use crate::hooks_surface::fetch_hooks_catalog;
use crate::hooks_surface::write_hook_trust;
use crate::hooks_surface::write_hook_trusts;

const TOKEN_ACTIVITY_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const RATE_LIMIT_RESET_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

impl App {
    pub(super) fn refresh_rate_limits(
        &mut self,
        _app_server: &AppServerSession,
        origin: RateLimitRefreshOrigin,
    ) {
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let request = fetch_account_rate_limits();
            let result = match origin {
                RateLimitRefreshOrigin::ResetConsume { .. } => {
                    tokio::time::timeout(RATE_LIMIT_RESET_REQUEST_TIMEOUT, request)
                        .await
                        .map_err(|_| "account/rateLimits/read timed out in TUI".to_string())
                        .and_then(|result| result.map_err(|err| err.to_string()))
                }
                RateLimitRefreshOrigin::StartupPrefetch { .. }
                | RateLimitRefreshOrigin::StatusCommand { .. } => {
                    request.await.map_err(|err| err.to_string())
                }
            };
            app_event_tx.send(AppEvent::RateLimitsLoaded { origin, result });
        });
    }

    pub(super) fn refresh_token_activity(
        &mut self,
        _app_server: &AppServerSession,
        request_id: u64,
    ) {
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result =
                tokio::time::timeout(TOKEN_ACTIVITY_FETCH_TIMEOUT, fetch_account_token_activity())
                    .await
                    .map_err(|_| "account/usage/read timed out in TUI".to_string())
                    .and_then(|result| result.map_err(|err| err.to_string()));
            app_event_tx.send(AppEvent::TokenActivityLoaded { request_id, result });
        });
    }

    pub(super) fn refresh_rate_limit_reset_credits(
        &mut self,
        _app_server: &AppServerSession,
        request_id: u64,
    ) {
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = tokio::time::timeout(
                RATE_LIMIT_RESET_REQUEST_TIMEOUT,
                fetch_rate_limit_reset_credits(),
            )
            .await
            .map_err(|_| "account/rateLimits/read timed out in TUI".to_string())
            .and_then(|result| result.map_err(|err| err.to_string()));
            app_event_tx.send(AppEvent::RateLimitResetCreditsLoaded { request_id, result });
        });
    }

    pub(super) fn consume_rate_limit_reset_credit(
        &mut self,
        _app_server: &AppServerSession,
        request_id: u64,
        idempotency_key: String,
    ) {
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = tokio::time::timeout(
                RATE_LIMIT_RESET_REQUEST_TIMEOUT,
                consume_rate_limit_reset_credit_request(idempotency_key.clone()),
            )
            .await
            .map_err(|_| "account/rateLimitResetCredit/consume timed out in TUI".to_string())
            .and_then(|result| result.map_err(|err| err.to_string()));
            app_event_tx.send(AppEvent::RateLimitResetCreditConsumed {
                request_id,
                idempotency_key,
                result,
            });
        });
    }

    pub(super) fn send_add_credits_nudge_email(
        &mut self,
        _app_server: &AppServerSession,
        credit_type: String,
    ) {
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = send_add_credits_nudge_email(credit_type)
                .await
                .map_err(|err| err.to_string());
            app_event_tx.send(AppEvent::AddCreditsNudgeEmailFinished { result });
        });
    }

    pub(super) fn fetch_hooks_list(&mut self, _app_server: &AppServerSession, cwd: PathBuf) {
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = fetch_hooks_catalog(cwd.clone())
                .await
                .map_err(|err| err.to_string());
            app_event_tx.send(AppEvent::HooksLoaded { cwd, result });
        });
    }

    pub(super) fn set_hook_enabled(
        &mut self,
        _app_server: &AppServerSession,
        key: String,
        enabled: bool,
    ) {
        if let Some(queued_enabled) = self.pending_hook_enabled_writes.get_mut(&key) {
            *queued_enabled = Some(enabled);
            return;
        }

        self.pending_hook_enabled_writes.insert(key.clone(), None);
        self.spawn_hook_enabled_write(_app_server, key, enabled);
    }

    pub(super) fn spawn_hook_enabled_write(
        &mut self,
        _app_server: &AppServerSession,
        key: String,
        enabled: bool,
    ) {
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let key_for_event = key.clone();
            let result = write_hook_enabled(key, enabled)
                .await
                .map(|_| ())
                .map_err(|err| {
                    format!(
                        "Failed to update hook config: {}",
                        format_config_error(&err)
                    )
                });
            app_event_tx.send(AppEvent::HookEnabledSet {
                key: key_for_event,
                enabled,
                result,
            });
        });
    }

    pub(super) fn trust_hook(
        &mut self,
        _app_server: &AppServerSession,
        key: String,
        current_hash: String,
    ) {
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = write_hook_trust(key, current_hash)
                .await
                .map(|_| ())
                .map_err(|err| format!("Failed to trust hook: {}", format_config_error(&err)));
            app_event_tx.send(AppEvent::HookTrusted { result });
        });
    }

    pub(super) fn trust_hooks(
        &mut self,
        _app_server: &AppServerSession,
        updates: Vec<HookTrustUpdate>,
    ) {
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = write_hook_trusts(updates)
                .await
                .map(|_| ())
                .map_err(|err| format!("Failed to trust hooks: {}", format_config_error(&err)));
            app_event_tx.send(AppEvent::HookTrusted { result });
        });
    }
}

pub(super) async fn fetch_account_rate_limits() -> Result<RateLimitsReport> {
    crate::unavailable_interface_action::fail("account rate-limit read")
}

pub(super) async fn fetch_account_token_activity() -> Result<AccountUsageReport> {
    crate::unavailable_interface_action::fail("account usage read")
}

pub(super) async fn fetch_rate_limit_reset_credits() -> Result<CreditsItem> {
    crate::unavailable_interface_action::fail("account rate-limit read")
}

pub(super) async fn consume_rate_limit_reset_credit_request(
    idempotency_key: String,
) -> Result<CreditsItem> {
    let _ = idempotency_key;
    crate::unavailable_interface_action::fail("rate-limit reset credit consumption")
}

pub(super) async fn send_add_credits_nudge_email(credit_type: String) -> Result<String> {
    let _ = credit_type;
    crate::unavailable_interface_action::fail("account credit notification")
}

pub(super) async fn write_hook_enabled(key: String, enabled: bool) -> Result<()> {
    let _ = (key, enabled);
    crate::unavailable_interface_action::fail("remote terminal config write")
}
