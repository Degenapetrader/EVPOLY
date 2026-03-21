#[derive(Debug, Clone, Copy, Default)]
pub struct TopOfBook {
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub struct TwoSidedQuotePlan {
    pub up_price: f64,
    pub down_price: f64,
    pub size_shares: f64,
}

pub fn maker_buy_price(
    book: TopOfBook,
    min_price: f64,
    max_price: f64,
    tick_size: f64,
) -> Option<f64> {
    let tick = tick_size.max(0.001);
    let mut candidate = if let Some(bid) = book.best_bid {
        (bid - tick).max(min_price)
    } else if let Some(ask) = book.best_ask {
        (ask - (2.0 * tick)).max(min_price)
    } else {
        return None;
    };
    if let Some(ask) = book.best_ask {
        candidate = candidate.min((ask - (2.0 * tick)).max(min_price));
    }
    Some((candidate / tick).floor() * tick)
        .map(|price| price.clamp(min_price, max_price))
        .filter(|price| price.is_finite() && *price >= min_price && *price <= max_price)
}

pub fn build_two_sided_plan(
    up_book: TopOfBook,
    down_book: TopOfBook,
    side_shares: f64,
    min_price: f64,
    max_price: f64,
    tick_size: f64,
) -> Option<TwoSidedQuotePlan> {
    let up_price = maker_buy_price(up_book, min_price, max_price, tick_size)?;
    let down_price = maker_buy_price(down_book, min_price, max_price, tick_size)?;
    if !(up_price.is_finite() && down_price.is_finite()) {
        return None;
    }
    Some(TwoSidedQuotePlan {
        up_price,
        down_price,
        size_shares: side_shares.max(1.0),
    })
}
