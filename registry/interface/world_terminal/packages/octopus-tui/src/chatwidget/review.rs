use crate::auto_review_denials::RecentAutoReviewDenials;
#[cfg(test)]
use crate::token_usage::TokenUsageInfo;

#[derive(Debug, Default)]
pub(super) struct ReviewState {
    pub(super) recent_auto_review_denials: RecentAutoReviewDenials,
    pub(super) is_review_mode: bool,
    #[cfg(test)]
    pub(super) pre_review_token_info: Option<Option<TokenUsageInfo>>,
}
