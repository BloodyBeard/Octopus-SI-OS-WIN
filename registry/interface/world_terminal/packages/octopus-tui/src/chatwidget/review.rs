use crate::auto_review_denials::RecentAutoReviewDenials;
use crate::token_usage::TokenUsageInfo;

#[derive(Debug, Default)]
pub(super) struct ReviewState {
    pub(super) recent_auto_review_denials: RecentAutoReviewDenials,
    pub(super) is_review_mode: bool,
    pub(super) pre_review_token_info: Option<Option<TokenUsageInfo>>,
}
