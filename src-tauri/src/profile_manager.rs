#![allow(dead_code)]

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub eoa_wallet_address: String,
    #[serde(default)]
    pub proxy_wallet_address: String,
    #[serde(default)]
    pub wallet_address: String,
    pub signature_type: u8,
    pub encrypted_secrets: String,
    pub strategy_config: serde_json::Value,
    pub sizing_config: serde_json::Value,
    pub created_at: String,
    pub last_used: String,
}

impl Profile {
    pub fn normalize_wallet_fields(&mut self) {
        let legacy = self.wallet_address.trim().to_string();

        if self.eoa_wallet_address.trim().is_empty()
            && self.proxy_wallet_address.trim().is_empty()
            && !legacy.is_empty()
        {
            if self.signature_type == 0 {
                self.eoa_wallet_address = legacy.clone();
            } else {
                self.proxy_wallet_address = legacy.clone();
            }
        }

        if self.signature_type == 0 {
            if self.eoa_wallet_address.trim().is_empty() && !legacy.is_empty() {
                self.eoa_wallet_address = legacy;
            }
            self.wallet_address = self.eoa_wallet_address.trim().to_string();
            self.proxy_wallet_address = self.proxy_wallet_address.trim().to_string();
        } else {
            if self.proxy_wallet_address.trim().is_empty() {
                if !legacy.is_empty() {
                    self.proxy_wallet_address = legacy;
                } else if !self.eoa_wallet_address.trim().is_empty() {
                    self.proxy_wallet_address = self.eoa_wallet_address.trim().to_string();
                }
            }
            self.proxy_wallet_address = self.proxy_wallet_address.trim().to_string();
            self.eoa_wallet_address = self.eoa_wallet_address.trim().to_string();
            self.wallet_address = self.proxy_wallet_address.clone();
        }
    }

    pub fn primary_wallet_address(&self) -> String {
        if self.signature_type == 0 {
            return self.eoa_wallet_address.trim().to_string();
        }
        self.proxy_wallet_address.trim().to_string()
    }
}

#[derive(Serialize, Deserialize)]
struct ProfileStore {
    profiles: Vec<Profile>,
    active_profile_id: Option<String>,
}

pub struct ProfileManager {
    store_path: PathBuf,
}

impl ProfileManager {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            store_path: data_dir.join("profiles.json"),
        }
    }

    fn load(&self) -> ProfileStore {
        let mut store = match std::fs::read_to_string(&self.store_path) {
            Ok(content) => match serde_json::from_str::<ProfileStore>(&content) {
                Ok(store) => store,
                Err(_) => {
                    if !content.trim().is_empty() {
                        let backup = self.store_path.with_extension(format!(
                            "json.corrupt.{}",
                            Utc::now().format("%Y%m%d%H%M%S")
                        ));
                        let _ = std::fs::write(&backup, content);
                    }
                    ProfileStore {
                        profiles: Vec::new(),
                        active_profile_id: None,
                    }
                }
            },
            Err(_) => ProfileStore {
                profiles: Vec::new(),
                active_profile_id: None,
            },
        };

        let mut migrated = false;
        for profile in &mut store.profiles {
            let before = profile.clone();
            profile.normalize_wallet_fields();
            if *profile != before {
                migrated = true;
            }
        }

        if let Some(active_id) = store.active_profile_id.clone() {
            if !store.profiles.iter().any(|p| p.id == active_id) {
                store.active_profile_id = None;
                migrated = true;
            }
        }

        if migrated {
            let _ = self.save(&store);
        }

        store
    }

    fn save(&self, store: &ProfileStore) -> Result<()> {
        let json = serde_json::to_string_pretty(store)?;
        std::fs::write(&self.store_path, json)?;
        Ok(())
    }

    pub fn list_profiles(&self) -> Vec<Profile> {
        self.load().profiles
    }

    pub fn create_profile(
        &self,
        name: String,
        eoa_wallet_address: String,
        proxy_wallet_address: String,
        sig_type: u8,
    ) -> Result<Profile> {
        let now = Utc::now().to_rfc3339();
        let mut profile = Profile {
            id: Uuid::new_v4().to_string(),
            name,
            eoa_wallet_address,
            proxy_wallet_address,
            wallet_address: String::new(),
            signature_type: sig_type,
            encrypted_secrets: String::new(),
            strategy_config: serde_json::Value::Object(serde_json::Map::new()),
            sizing_config: serde_json::Value::Object(serde_json::Map::new()),
            created_at: now.clone(),
            last_used: now,
        };
        profile.normalize_wallet_fields();
        let mut store = self.load();
        store.profiles.push(profile.clone());
        self.save(&store)?;
        Ok(profile)
    }

    pub fn get_profile(&self, id: &str) -> Option<Profile> {
        self.load().profiles.into_iter().find(|p| p.id == id)
    }

    pub fn update_profile(&self, mut profile: Profile) -> Result<()> {
        profile.normalize_wallet_fields();
        let mut store = self.load();
        if let Some(existing) = store.profiles.iter_mut().find(|p| p.id == profile.id) {
            *existing = profile;
        } else {
            return Err(format!("profile {} not found", profile.id).into());
        }
        self.save(&store)
    }

    pub fn delete_profile(&self, id: &str) -> Result<()> {
        let mut store = self.load();
        let before = store.profiles.len();
        store.profiles.retain(|p| p.id != id);
        if store.profiles.len() == before {
            return Err(format!("profile {id} not found").into());
        }
        if store.active_profile_id.as_deref() == Some(id) {
            store.active_profile_id = None;
        }
        self.save(&store)
    }

    pub fn get_active_profile_id(&self) -> Option<String> {
        self.load().active_profile_id
    }

    pub fn set_active_profile(&self, id: &str) -> Result<()> {
        let mut store = self.load();
        if !store.profiles.iter().any(|p| p.id == id) {
            return Err(format!("profile {id} not found").into());
        }
        store.active_profile_id = Some(id.to_string());
        self.save(&store)
    }
}

#[cfg(test)]
mod tests {
    use super::Profile;

    fn base_profile(signature_type: u8) -> Profile {
        Profile {
            id: "p1".to_string(),
            name: "test".to_string(),
            eoa_wallet_address: String::new(),
            proxy_wallet_address: String::new(),
            wallet_address: "0xabc".to_string(),
            signature_type,
            encrypted_secrets: String::new(),
            strategy_config: serde_json::json!({}),
            sizing_config: serde_json::json!({}),
            created_at: "now".to_string(),
            last_used: "now".to_string(),
        }
    }

    #[test]
    fn legacy_wallet_maps_to_eoa_for_sig_type_zero() {
        let mut profile = base_profile(0);
        profile.normalize_wallet_fields();
        assert_eq!(profile.eoa_wallet_address, "0xabc");
        assert_eq!(profile.proxy_wallet_address, "");
        assert_eq!(profile.wallet_address, "0xabc");
        assert_eq!(profile.primary_wallet_address(), "0xabc");
    }

    #[test]
    fn legacy_wallet_maps_to_proxy_for_sig_type_one() {
        let mut profile = base_profile(1);
        profile.normalize_wallet_fields();
        assert_eq!(profile.eoa_wallet_address, "");
        assert_eq!(profile.proxy_wallet_address, "0xabc");
        assert_eq!(profile.wallet_address, "0xabc");
        assert_eq!(profile.primary_wallet_address(), "0xabc");
    }
}
