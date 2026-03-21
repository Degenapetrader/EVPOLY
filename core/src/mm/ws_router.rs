#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedState {
    Healthy,
    Stale,
    Reconnecting,
}

#[derive(Debug, Clone, Copy)]
pub struct FeedHealth {
    pub state: FeedState,
    pub market_last_ms: i64,
    pub user_last_ms: i64,
}

impl Default for FeedHealth {
    fn default() -> Self {
        Self {
            state: FeedState::Healthy,
            market_last_ms: 0,
            user_last_ms: 0,
        }
    }
}

pub fn derive_feed_state(
    now_ms: i64,
    market_last_ms: i64,
    user_last_ms: i64,
    stale_threshold_ms: i64,
) -> FeedState {
    let threshold = stale_threshold_ms.max(100);
    if now_ms.saturating_sub(market_last_ms) > threshold
        || now_ms.saturating_sub(user_last_ms) > threshold
    {
        FeedState::Stale
    } else {
        FeedState::Healthy
    }
}
