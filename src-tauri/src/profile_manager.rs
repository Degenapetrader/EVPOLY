#![allow(dead_code)]

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub wallet_address: String,
    pub signature_type: u8,
    pub encrypted_secrets: String,
    pub strategy_config: serde_json::Value,
    pub sizing_config: serde_json::Value,
    pub created_at: String,
    pub last_used: String,
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
        std::fs::read_to_string(&self.store_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(ProfileStore {
                profiles: Vec::new(),
                active_profile_id: None,
            })
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
        wallet_address: String,
        sig_type: u8,
    ) -> Result<Profile> {
        let now = Utc::now().to_rfc3339();
        let profile = Profile {
            id: Uuid::new_v4().to_string(),
            name,
            wallet_address,
            signature_type: sig_type,
            encrypted_secrets: String::new(),
            strategy_config: serde_json::Value::Object(serde_json::Map::new()),
            sizing_config: serde_json::Value::Object(serde_json::Map::new()),
            created_at: now.clone(),
            last_used: now,
        };
        let mut store = self.load();
        store.profiles.push(profile.clone());
        self.save(&store)?;
        Ok(profile)
    }

    pub fn get_profile(&self, id: &str) -> Option<Profile> {
        self.load().profiles.into_iter().find(|p| p.id == id)
    }

    pub fn update_profile(&self, profile: Profile) -> Result<()> {
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
