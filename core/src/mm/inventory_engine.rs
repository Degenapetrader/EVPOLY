use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct InventoryState {
    // key format: "<condition_id>:<token_id>"
    filled_notional_by_side: HashMap<String, f64>,
}

impl InventoryState {
    pub fn record_fill(&mut self, condition_id: &str, token_id: &str, notional_usd: f64) {
        let key = format!("{}:{}", condition_id, token_id);
        self.filled_notional_by_side
            .entry(key)
            .and_modify(|value| *value += notional_usd.max(0.0))
            .or_insert_with(|| notional_usd.max(0.0));
    }

    pub fn filled_notional_for_side(&self, condition_id: &str, token_id: &str) -> f64 {
        let key = format!("{}:{}", condition_id, token_id);
        self.filled_notional_by_side
            .get(key.as_str())
            .copied()
            .unwrap_or(0.0)
    }
}
