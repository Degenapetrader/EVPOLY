use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const GEO_LOOKUP_URL: &str = "https://ipwho.is/";
const BLOCKED_COUNTRY_CODES: [&str; 10] =
    ["US", "IR", "KP", "CU", "RU", "BY", "VE", "SG", "TH", "VN"];
const BLOCKED_REGIONS: [&str; 4] = ["ontario", "crimea", "donetsk", "luhansk"];

#[derive(Clone, Serialize, Default)]
pub struct GeoAccessStatus {
    pub status: String,
    pub country_code: Option<String>,
    pub country_name: Option<String>,
    pub region_name: Option<String>,
    pub reason: String,
    pub checked_at: String,
}

#[derive(Deserialize)]
struct GeoLookupResponse {
    success: Option<bool>,
    country_code: Option<String>,
    country: Option<String>,
    region: Option<String>,
    message: Option<String>,
}

fn checked_now() -> String {
    Utc::now().to_rfc3339()
}

fn blocked_country(country_code: Option<&str>) -> bool {
    country_code
        .map(|code| {
            let code = code.trim().to_ascii_uppercase();
            BLOCKED_COUNTRY_CODES.iter().any(|blocked| *blocked == code)
        })
        .unwrap_or(false)
}

fn blocked_region(region_name: Option<&str>) -> bool {
    region_name
        .map(|region| {
            let lower = region.trim().to_ascii_lowercase();
            BLOCKED_REGIONS
                .iter()
                .any(|blocked| lower.contains(blocked))
        })
        .unwrap_or(false)
}

fn build_block_reason(country_name: Option<&str>, region_name: Option<&str>) -> String {
    if let Some(region) = region_name.filter(|value| !value.trim().is_empty()) {
        return format!(
            "Access is unavailable in {region} due to regulatory, sanctions, or platform restrictions."
        );
    }
    if let Some(country) = country_name.filter(|value| !value.trim().is_empty()) {
        return format!(
            "Access is unavailable in {country} due to regulatory, sanctions, or platform restrictions."
        );
    }
    "Access is unavailable due to regulatory, sanctions, or platform restrictions.".to_string()
}

fn unknown_status(reason: String) -> GeoAccessStatus {
    GeoAccessStatus {
        status: "unknown".to_string(),
        country_code: None,
        country_name: None,
        region_name: None,
        reason,
        checked_at: checked_now(),
    }
}

pub fn current_geo_access_status() -> GeoAccessStatus {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(4))
        .user_agent("EVPoly Desktop")
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            return unknown_status(format!(
                "We could not verify your location right now ({err})."
            ));
        }
    };

    let response = match client.get(GEO_LOOKUP_URL).send() {
        Ok(response) => response,
        Err(err) => {
            return unknown_status(format!(
                "We could not verify your location right now ({err})."
            ));
        }
    };

    let payload = match response.json::<GeoLookupResponse>() {
        Ok(payload) => payload,
        Err(err) => {
            return unknown_status(format!(
                "We could not verify your location right now ({err})."
            ));
        }
    };

    if payload.success == Some(false) {
        return unknown_status(
            payload
                .message
                .unwrap_or_else(|| "We could not verify your location right now.".to_string()),
        );
    }

    let country_code = payload
        .country_code
        .as_ref()
        .map(|code| code.trim().to_ascii_uppercase());
    let country_name = payload.country.filter(|value| !value.trim().is_empty());
    let region_name = payload.region.filter(|value| !value.trim().is_empty());

    let status =
        if blocked_country(country_code.as_deref()) || blocked_region(region_name.as_deref()) {
            "blocked"
        } else {
            "allowed"
        };

    GeoAccessStatus {
        status: status.to_string(),
        country_code,
        country_name: country_name.clone(),
        region_name: region_name.clone(),
        reason: if status == "blocked" {
            build_block_reason(country_name.as_deref(), region_name.as_deref())
        } else {
            "Location verified.".to_string()
        },
        checked_at: checked_now(),
    }
}

pub fn ensure_geo_start_allowed() -> Result<(), String> {
    let status = current_geo_access_status();
    if status.status == "blocked" {
        return Err(status.reason);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{blocked_country, blocked_region, build_block_reason};

    #[test]
    fn blocks_fixed_country_list() {
        assert!(blocked_country(Some("US")));
        assert!(blocked_country(Some("vn")));
        assert!(!blocked_country(Some("NL")));
    }

    #[test]
    fn blocks_fixed_regions() {
        assert!(blocked_region(Some("Ontario")));
        assert!(blocked_region(Some("Autonomous Republic of Crimea")));
        assert!(!blocked_region(Some("California")));
    }

    #[test]
    fn block_reason_prefers_region() {
        let reason = build_block_reason(Some("Canada"), Some("Ontario"));
        assert!(reason.contains("Ontario"));
    }
}
