//! Pairing credentials; only token hashes reach disk.

use std::path::PathBuf;
use std::time::Duration;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::time::Instant;

const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
pub const PAIR_TTL: Duration = Duration::from_secs(600);

pub struct Pairing {
    pub code: String,
    pub expires: Instant,
    failures: u8,
}

impl Pairing {
    pub fn new() -> Result<Self, String> {
        let mut code = String::with_capacity(8);
        while code.len() < 8 {
            let mut byte = [0];
            getrandom::getrandom(&mut byte).map_err(|error| error.to_string())?;
            // Rejection sampling keeps every alphabet character equally likely.
            if usize::from(byte[0]) < 256 / ALPHABET.len() * ALPHABET.len() {
                code.push(char::from(ALPHABET[usize::from(byte[0]) % ALPHABET.len()]));
            }
        }
        Ok(Self {
            code,
            expires: Instant::now() + PAIR_TTL,
            failures: 0,
        })
    }

    pub fn consume(&mut self, code: &str) -> bool {
        if Instant::now() >= self.expires || self.failures >= 5 {
            return false;
        }
        if self.code != code {
            self.failures += 1;
            return false;
        }
        self.failures = 5;
        true
    }

    pub fn active(&self) -> bool {
        self.failures < 5 && Instant::now() < self.expires
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    pub token_sha256: String,
    pub paired_at: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Devices {
    devices: Vec<Device>,
}

pub struct Auth {
    file: PathBuf,
    devices: Devices,
    pub pairing: Pairing,
}

impl Auth {
    pub fn load(file: PathBuf) -> Result<Self, String> {
        let devices = match std::fs::read_to_string(&file) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| error.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Devices::default(),
            Err(error) => return Err(error.to_string()),
        };
        Ok(Self {
            file,
            devices,
            pairing: Pairing::new()?,
        })
    }

    pub fn pair(&mut self, code: &str, name: String) -> Result<(Device, String), String> {
        if !self.pairing.consume(code) {
            return Err("unauthorized".into());
        }
        let mut bytes = [0; 32];
        getrandom::getrandom(&mut bytes).map_err(|error| error.to_string())?;
        let token = URL_SAFE_NO_PAD.encode(bytes);
        let device = Device {
            name,
            token_sha256: hash(&token),
            paired_at: jiff::Timestamp::now().to_string(),
        };
        self.devices.devices.push(device.clone());
        let text = serde_json::to_string(&self.devices).expect("device serialization");
        if let Err(error) = crate::config::write_atomic(&self.file, &text, 0o600) {
            self.devices.devices.pop();
            return Err(error.to_string());
        }
        Ok((device, token))
    }

    pub fn verify(&self, token: &str) -> Option<Device> {
        let hash = hash(token);
        self.devices
            .devices
            .iter()
            .find(|device| device.token_sha256 == hash)
            .cloned()
    }
}

fn hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn pairing_is_single_use_expires_and_locks_after_five_failures() {
        let mut pair = Pairing::new().unwrap();
        let code = pair.code.clone();
        assert!(pair.consume(&code));
        assert!(!pair.consume(&code));
        let mut pair = Pairing::new().unwrap();
        tokio::time::advance(PAIR_TTL).await;
        assert!(!pair.consume(&pair.code.clone()));
        let mut pair = Pairing::new().unwrap();
        for _ in 0..5 {
            assert!(!pair.consume("wrong"));
        }
        assert!(!pair.consume(&pair.code.clone()));
    }

    #[tokio::test(start_paused = true)]
    async fn persisted_hashes_authenticate_without_storing_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("remote-devices.json");
        let mut auth = Auth::load(file.clone()).unwrap();
        let (device, token) = auth
            .pair(&auth.pairing.code.clone(), "phone".into())
            .unwrap();
        assert_eq!(URL_SAFE_NO_PAD.decode(&token).unwrap().len(), 32);
        assert!(auth.verify("wrong").is_none());
        assert_eq!(
            auth.verify(&token).unwrap().token_sha256,
            device.token_sha256
        );
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains(&token));
        assert_eq!(
            Auth::load(file.clone())
                .unwrap()
                .verify(&token)
                .unwrap()
                .name,
            "phone"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
