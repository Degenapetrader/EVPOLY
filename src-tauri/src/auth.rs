#![allow(dead_code)]

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Algorithm, Argon2, Params, Version,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Serialize, Deserialize)]
struct AuthData {
    password_hash: String,
}

pub struct AppAuth {
    auth_path: PathBuf,
    session_password: Option<String>,
}

impl AppAuth {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            auth_path: data_dir.join("auth.json"),
            session_password: None,
        }
    }

    pub fn is_initialized(&self) -> bool {
        self.auth_path.exists()
    }

    pub fn set_password(&mut self, password: &str) -> Result<()> {
        let salt = SaltString::generate(&mut rand::rngs::OsRng);
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::default());
        let hash = argon2
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| format!("hash password: {e}"))?
            .to_string();

        let data = AuthData {
            password_hash: hash,
        };
        let json = serde_json::to_string_pretty(&data)?;
        std::fs::write(&self.auth_path, json)?;
        self.session_password = Some(password.to_string());
        Ok(())
    }

    pub fn verify_password(&mut self, password: &str) -> Result<bool> {
        let json = std::fs::read_to_string(&self.auth_path)?;
        let data: AuthData = serde_json::from_str(&json)?;
        let parsed_hash =
            PasswordHash::new(&data.password_hash).map_err(|e| format!("parse hash: {e}"))?;
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::default());
        let verified = argon2
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok();
        if verified {
            self.session_password = Some(password.to_string());
        }
        Ok(verified)
    }

    pub fn session_password(&self) -> Option<String> {
        self.session_password.clone()
    }

    pub fn clear_session(&mut self) {
        self.session_password = None;
    }
}
