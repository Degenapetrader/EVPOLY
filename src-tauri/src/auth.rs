#![allow(dead_code)]

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Algorithm, Argon2, Params, Version,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use zeroize::Zeroizing;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Serialize, Deserialize)]
struct AuthData {
    password_hash: String,
}

pub struct AppAuth {
    auth_path: PathBuf,
    session_password: Option<Zeroizing<String>>,
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

    fn hash_password(password: &str) -> Result<String> {
        let salt = SaltString::generate(&mut rand::rngs::OsRng);
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::default());
        let hash = argon2
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| format!("hash password: {e}"))?
            .to_string();
        Ok(hash)
    }

    fn verify_hash(&self, password: &str) -> Result<bool> {
        let json = std::fs::read_to_string(&self.auth_path)?;
        let data: AuthData = serde_json::from_str(&json)?;
        let parsed_hash =
            PasswordHash::new(&data.password_hash).map_err(|e| format!("parse hash: {e}"))?;
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::default());
        Ok(argon2
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok())
    }

    pub fn initialize_password(&mut self, password: &str) -> Result<()> {
        if self.is_initialized() {
            return Err("password already initialized".into());
        }
        let hash = Self::hash_password(password)?;
        let data = AuthData {
            password_hash: hash,
        };
        let json = serde_json::to_string_pretty(&data)?;
        std::fs::write(&self.auth_path, json)?;
        self.session_password = Some(Zeroizing::new(password.to_string()));
        Ok(())
    }

    pub fn verify_password(&mut self, password: &str) -> Result<bool> {
        let verified = self.verify_hash(password)?;
        if verified {
            self.session_password = Some(Zeroizing::new(password.to_string()));
        }
        Ok(verified)
    }

    pub fn confirm_password(&self, password: &str) -> Result<bool> {
        self.verify_hash(password)
    }

    pub fn with_session_password<T>(&self, f: impl FnOnce(&str) -> T) -> Option<T> {
        self.session_password.as_deref().map(|password| f(password))
    }

    pub fn clear_session(&mut self) {
        self.session_password = None;
    }
}
