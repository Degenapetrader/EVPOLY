pub const OFFICIAL_BUILDER_CODE: &str =
    "0xb8d6bf0c9ec3c806c30fcb0e8da931f2940a5141cf420394c4b1d82ae7c6d415";

pub fn official_builder_code() -> &'static str {
    OFFICIAL_BUILDER_CODE
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
