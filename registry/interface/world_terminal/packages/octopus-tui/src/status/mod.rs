mod account;
mod card;
mod format;
mod helpers;
mod rate_limits;
pub(crate) mod remote_connection;

pub(crate) use account::StatusAccountDisplay;
pub(crate) use card::StatusHistoryHandle;
#[cfg(test)]
pub(crate) use card::new_status_output;
#[cfg(test)]
pub(crate) use card::new_status_output_with_rate_limits;
pub(crate) use card::new_status_output_with_rate_limits_handle;
pub(crate) use helpers::compose_agents_summary;
pub(crate) use helpers::format_directory_display;
pub(crate) use helpers::format_tokens_compact;
pub(crate) use rate_limits::RateLimitSnapshotDisplay;
pub(crate) use rate_limits::RateLimitWindowDisplay;
#[cfg(test)]
pub(crate) use rate_limits::rate_limit_snapshot_display;
pub(crate) use rate_limits::rate_limit_snapshot_display_for_limit;

#[cfg(test)]
mod tests;
