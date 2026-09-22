use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use rand::{Rng, distributions::Alphanumeric};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Admin,
    User,
}

#[derive(Clone)]
pub struct Session {
    pub user_id: String,
    pub role: Role,
}

#[derive(Clone, Default)]
pub struct SessionStore(pub Arc<Mutex<HashMap<String, Session>>>);

pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut rand::thread_rng());
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

pub fn verify_password(password: &str, encoded: &str) -> bool {
    PasswordHash::new(encoded)
        .ok()
        .map(|h| {
            Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        })
        .unwrap_or(false)
}

pub fn random_token() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(48)
        .map(char::from)
        .collect()
}

pub fn cookie_session(cookie: Option<&str>, sessions: &SessionStore) -> Option<Session> {
    let sid = cookie?
        .split(';')
        .find_map(|p| p.trim().strip_prefix("sight_session="))?;
    sessions.0.lock().ok()?.get(sid).cloned()
}
