//! The connection password and remembered phones; phones keep a token and only
//! its hash reaches disk.

use std::path::PathBuf;
use std::time::Duration;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::time::Instant;

const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
/// Wrong passwords in a row before sign-in pauses; each further run doubles the pause.
const ATTEMPTS: u32 = 5;
const FIRST_LOCK: Duration = Duration::from_secs(30);
const MAX_LOCK: Duration = Duration::from_secs(15 * 60);
/// A password set by hand must be at least this long, spaces and dashes aside.
pub const MIN_PASSWORD: usize = 8;
/// Remembered phones; signing in another drops the oldest.
const MAX_DEVICES: usize = 20;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    pub token_sha256: String,
    pub paired_at: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    password: String,
    #[serde(default)]
    devices: Vec<Device>,
}

/// Why a password sign-in was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Wrong,
    /// Sign-in is paused for this long.
    Locked(Duration),
}

pub struct Auth {
    file: PathBuf,
    stored: Stored,
    failures: u32,
    locked_until: Option<Instant>,
}

impl Auth {
    /// Reads `remote.json`, creating a password the first time.
    pub fn load(file: PathBuf) -> Result<Self, String> {
        let stored = match std::fs::read_to_string(&file) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| error.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Stored::default(),
            Err(error) => return Err(error.to_string()),
        };
        let mut auth = Self {
            file,
            stored,
            failures: 0,
            locked_until: None,
        };
        if normalize(&auth.stored.password).is_empty() {
            auth.stored.password = generate()?;
            auth.save()?;
        }
        Ok(auth)
    }

    pub fn password(&self) -> &str {
        &self.stored.password
    }

    /// Replaces the password, a generated one for `None`, and forgets every phone.
    pub fn set_password(&mut self, password: Option<&str>) -> Result<(), String> {
        let password = match password {
            Some(password) if normalize(password).chars().count() < MIN_PASSWORD => {
                return Err(format!(
                    "A remote password needs at least {MIN_PASSWORD} characters"
                ));
            }
            Some(password) => password.trim().to_owned(),
            None => generate()?,
        };
        let old = std::mem::replace(
            &mut self.stored,
            Stored {
                password,
                devices: Vec::new(),
            },
        );
        if let Err(error) = self.save() {
            self.stored = old;
            return Err(error);
        }
        self.failures = 0;
        self.locked_until = None;
        Ok(())
    }

    /// Checks `password` and makes a token for the phone; nothing is stored
    /// until [`Auth::remember`].
    pub fn prepare_login(
        &mut self,
        password: &str,
        name: String,
    ) -> Result<(Device, String), Refusal> {
        let now = Instant::now();
        if let Some(until) = self.locked_until.filter(|until| now < *until) {
            return Err(Refusal::Locked(until - now));
        }
        if hash(&normalize(password)) != hash(&normalize(&self.stored.password)) {
            self.failures += 1;
            if self.failures.is_multiple_of(ATTEMPTS) {
                let runs = (self.failures / ATTEMPTS - 1).min(5);
                let lock = (FIRST_LOCK * 2u32.pow(runs)).min(MAX_LOCK);
                self.locked_until = Some(now + lock);
                return Err(Refusal::Locked(lock));
            }
            return Err(Refusal::Wrong);
        }
        self.failures = 0;
        let mut bytes = [0; 32];
        getrandom::getrandom(&mut bytes).expect("the OS random source");
        let token = URL_SAFE_NO_PAD.encode(bytes);
        let device = Device {
            name,
            token_sha256: hash(&token),
            paired_at: jiff::Timestamp::now().to_string(),
        };
        Ok((device, token))
    }

    /// Stores a phone the TUI admitted.
    pub fn remember(&mut self, device: Device) -> Result<(), String> {
        let before = self.stored.devices.clone();
        self.stored.devices.push(device);
        let excess = self.stored.devices.len().saturating_sub(MAX_DEVICES);
        self.stored.devices.drain(..excess);
        if let Err(error) = self.save() {
            self.stored.devices = before;
            return Err(error);
        }
        Ok(())
    }

    pub fn verify(&self, token: &str) -> Option<Device> {
        let hash = hash(token);
        self.stored
            .devices
            .iter()
            .find(|device| device.token_sha256 == hash)
            .cloned()
    }

    fn save(&self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&self.stored).expect("remote file serialization");
        crate::config::write_atomic(&self.file, &text, 0o600).map_err(|error| error.to_string())
    }
}

/// Spaces and dashes only group a password for reading; they never count.
fn normalize(password: &str) -> String {
    password
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect()
}

/// Twelve unambiguous lowercase characters in groups of four, easy to type on a phone.
fn generate() -> Result<String, String> {
    let mut password = String::with_capacity(14);
    let mut chosen = 0;
    while chosen < 12 {
        let mut byte = [0];
        getrandom::getrandom(&mut byte).map_err(|error| error.to_string())?;
        // Rejection sampling keeps every alphabet character equally likely.
        if usize::from(byte[0]) < 256 / ALPHABET.len() * ALPHABET.len() {
            if chosen > 0 && chosen % 4 == 0 {
                password.push('-');
            }
            password.push(char::from(ALPHABET[usize::from(byte[0]) % ALPHABET.len()]));
            chosen += 1;
        }
    }
    Ok(password)
}

fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_load_creates_and_keeps_a_grouped_password() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("remote.json");
        let password = Auth::load(file.clone()).unwrap().password().to_owned();
        assert_eq!(password.len(), 14, "{password}");
        assert_eq!(password.matches('-').count(), 2, "{password}");
        assert!(password.bytes().all(|b| b == b'-' || ALPHABET.contains(&b)));
        assert_eq!(Auth::load(file.clone()).unwrap().password(), password);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_password_ignores_grouping_and_tokens_persist_only_as_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("remote.json");
        let mut auth = Auth::load(file.clone()).unwrap();
        let typed = auth.password().replace('-', " ");
        let (device, token) = auth.prepare_login(&typed, "phone".into()).unwrap();
        assert!(
            auth.verify(&token).is_none(),
            "not remembered before admission"
        );
        auth.remember(device).unwrap();
        assert_eq!(URL_SAFE_NO_PAD.decode(&token).unwrap().len(), 32);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains(&token));
        assert_eq!(
            Auth::load(file).unwrap().verify(&token).unwrap().name,
            "phone"
        );
        assert_eq!(
            auth.prepare_login(&auth.password().to_uppercase(), "x".into())
                .unwrap_err(),
            Refusal::Wrong,
            "case counts"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn wrong_passwords_pause_sign_in_for_longer_each_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut auth = Auth::load(dir.path().join("remote.json")).unwrap();
        let right = auth.password().to_owned();
        for _ in 0..4 {
            assert_eq!(
                auth.prepare_login("wrong", "x".into()).unwrap_err(),
                Refusal::Wrong
            );
        }
        assert_eq!(
            auth.prepare_login("wrong", "x".into()).unwrap_err(),
            Refusal::Locked(FIRST_LOCK)
        );
        assert!(matches!(
            auth.prepare_login(&right, "x".into()),
            Err(Refusal::Locked(_))
        ));
        tokio::time::advance(FIRST_LOCK).await;
        for _ in 0..4 {
            auth.prepare_login("wrong", "x".into()).unwrap_err();
        }
        assert_eq!(
            auth.prepare_login("wrong", "x".into()).unwrap_err(),
            Refusal::Locked(FIRST_LOCK * 2)
        );
        tokio::time::advance(FIRST_LOCK * 2).await;
        assert!(auth.prepare_login(&right, "x".into()).is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_password_signs_out_every_phone_and_short_ones_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut auth = Auth::load(dir.path().join("remote.json")).unwrap();
        let password = auth.password().to_owned();
        let (device, token) = auth.prepare_login(&password, "p".into()).unwrap();
        auth.remember(device).unwrap();
        assert!(auth.set_password(Some("short-1")).is_err());
        assert!(
            auth.verify(&token).is_some(),
            "a refused change keeps phones"
        );
        auth.set_password(Some("  correct horse  ")).unwrap();
        assert_eq!(auth.password(), "correct horse");
        assert!(auth.verify(&token).is_none());
        assert!(auth.prepare_login("correcthorse", "p".into()).is_ok());
        auth.set_password(None).unwrap();
        assert_ne!(auth.password(), "correct horse");
    }

    #[tokio::test(start_paused = true)]
    async fn only_the_newest_phones_are_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let mut auth = Auth::load(dir.path().join("remote.json")).unwrap();
        let password = auth.password().to_owned();
        let mut tokens = Vec::new();
        for n in 0..=MAX_DEVICES {
            let (device, token) = auth.prepare_login(&password, format!("p{n}")).unwrap();
            auth.remember(device).unwrap();
            tokens.push(token);
        }
        assert!(auth.verify(&tokens[0]).is_none());
        assert!(auth.verify(&tokens[MAX_DEVICES]).is_some());
    }
}
