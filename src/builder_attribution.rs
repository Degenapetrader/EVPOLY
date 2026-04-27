pub const OFFICIAL_BUILDER_CODE: &str =
    "0xb8d6bf0c9ec3c806c30fcb0e8da931f2940a5141cf420394c4b1d82ae7c6d415";
pub const OFFICIAL_BUILDER_MAKER_FEE_BPS: u16 = 10;
pub const OFFICIAL_BUILDER_TAKER_FEE_BPS: u16 = 10;
pub const MAX_BUILDER_MAKER_FEE_BPS: u16 = 50;
pub const MAX_BUILDER_TAKER_FEE_BPS: u16 = 100;

pub fn official_builder_code() -> &'static str {
    OFFICIAL_BUILDER_CODE
}

pub fn official_builder_fee_bps() -> (u16, u16) {
    (
        OFFICIAL_BUILDER_MAKER_FEE_BPS,
        OFFICIAL_BUILDER_TAKER_FEE_BPS,
    )
}

pub fn configured_builder_code() -> String {
    std::env::var("POLY_BUILDER_CODE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OFFICIAL_BUILDER_CODE.to_string())
}

pub fn configured_builder_code_is_official() -> bool {
    configured_builder_code().eq_ignore_ascii_case(OFFICIAL_BUILDER_CODE)
}

pub fn validate_builder_fee_bps(maker_bps: u16, taker_bps: u16) -> Result<(), String> {
    if maker_bps > MAX_BUILDER_MAKER_FEE_BPS {
        return Err(format!(
            "maker fee {} bps exceeds Polymarket builder max {} bps",
            maker_bps, MAX_BUILDER_MAKER_FEE_BPS
        ));
    }
    if taker_bps > MAX_BUILDER_TAKER_FEE_BPS {
        return Err(format!(
            "taker fee {} bps exceeds Polymarket builder max {} bps",
            taker_bps, MAX_BUILDER_TAKER_FEE_BPS
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_builder_fee_defaults_are_within_polymarket_limits() {
        let (maker_bps, taker_bps) = official_builder_fee_bps();

        assert!(validate_builder_fee_bps(maker_bps, taker_bps).is_ok());
    }

    #[test]
    fn builder_fee_validation_rejects_values_above_limits() {
        assert!(validate_builder_fee_bps(MAX_BUILDER_MAKER_FEE_BPS + 1, 0).is_err());
        assert!(validate_builder_fee_bps(0, MAX_BUILDER_TAKER_FEE_BPS + 1).is_err());
    }
}
