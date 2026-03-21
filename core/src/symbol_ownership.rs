use crate::strategy::{
    STRATEGY_ID_ENDGAME_SWEEP_V1, STRATEGY_ID_EVCURVE_V1, STRATEGY_ID_EVSNIPE_V1,
    STRATEGY_ID_MM_REWARDS_V1, STRATEGY_ID_PREMARKET_V1, STRATEGY_ID_SESSIONBAND_V1,
};

const SPECIAL_SYMBOLS: [&str; 3] = ["DOGE", "BNB", "HYPE"];

pub fn normalize_symbol(raw: &str) -> String {
    match raw.trim().to_ascii_uppercase().as_str() {
        "SOLANA" => "SOL".to_string(),
        "DOGECOIN" => "DOGE".to_string(),
        other => other.to_string(),
    }
}

pub fn strategy_allows_symbol(strategy_id: &str, symbol: &str) -> bool {
    let normalized = normalize_symbol(symbol);
    if !SPECIAL_SYMBOLS.contains(&normalized.as_str()) {
        return true;
    }
    matches!(
        strategy_id,
        STRATEGY_ID_ENDGAME_SWEEP_V1 | STRATEGY_ID_EVSNIPE_V1
    )
}

pub fn filter_symbols_for_strategy(strategy_id: &str, symbols: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for symbol in symbols {
        let normalized = normalize_symbol(symbol);
        if normalized.is_empty() {
            continue;
        }
        if !strategy_allows_symbol(strategy_id, normalized.as_str()) {
            continue;
        }
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    out
}

pub fn default_symbols_for_strategy(strategy_id: &str) -> Vec<String> {
    let default = vec![
        "BTC".to_string(),
        "ETH".to_string(),
        "SOL".to_string(),
        "XRP".to_string(),
    ];
    if matches!(
        strategy_id,
        STRATEGY_ID_ENDGAME_SWEEP_V1 | STRATEGY_ID_EVSNIPE_V1
    ) {
        let mut extended = default;
        extended.push("DOGE".to_string());
        extended.push("BNB".to_string());
        extended.push("HYPE".to_string());
        return extended;
    }
    default
}

pub fn parse_symbols_csv_for_strategy(strategy_id: &str, raw: &str) -> Vec<String> {
    let parsed = raw
        .split(',')
        .map(normalize_symbol)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let filtered = filter_symbols_for_strategy(strategy_id, &parsed);
    if filtered.is_empty() {
        default_symbols_for_strategy(strategy_id)
    } else {
        filtered
    }
}

pub fn strategy_uses_core_symbols_only(strategy_id: &str) -> bool {
    matches!(
        strategy_id,
        STRATEGY_ID_PREMARKET_V1
            | STRATEGY_ID_EVCURVE_V1
            | STRATEGY_ID_SESSIONBAND_V1
            | STRATEGY_ID_MM_REWARDS_V1
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_ownership_specials_are_endgame_and_evsnipe_only() {
        for special in SPECIAL_SYMBOLS {
            assert!(strategy_allows_symbol(
                STRATEGY_ID_ENDGAME_SWEEP_V1,
                special
            ));
            assert!(strategy_allows_symbol(STRATEGY_ID_EVSNIPE_V1, special));
            assert!(!strategy_allows_symbol(STRATEGY_ID_PREMARKET_V1, special));
            assert!(!strategy_allows_symbol(STRATEGY_ID_EVCURVE_V1, special));
            assert!(!strategy_allows_symbol(STRATEGY_ID_SESSIONBAND_V1, special));
            assert!(!strategy_allows_symbol(STRATEGY_ID_MM_REWARDS_V1, special));
        }
    }

    #[test]
    fn parser_filters_disallowed_symbols_by_strategy() {
        let raw = "btc,eth,sol,xrp,doge,bnb,hype";
        assert_eq!(
            parse_symbols_csv_for_strategy(STRATEGY_ID_EVCURVE_V1, raw),
            vec!["BTC", "ETH", "SOL", "XRP"]
        );
        assert_eq!(
            parse_symbols_csv_for_strategy(STRATEGY_ID_ENDGAME_SWEEP_V1, raw),
            vec!["BTC", "ETH", "SOL", "XRP", "DOGE", "BNB", "HYPE"]
        );
    }
}
