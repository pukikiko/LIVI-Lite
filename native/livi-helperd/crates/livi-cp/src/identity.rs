//! Phones stay paired as long as this file stays.

use std::fs;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::crypto::{ed25519_public, random_bytes};

#[derive(Debug, Clone, PartialEq)]
pub struct Identity {
    pub secret: [u8; 32],
    pub public: [u8; 32],
    /// The `pi` the phone knows the accessory by.
    pub pairing_id: String,
}

impl Identity {
    /// The `pk` advertised next to `pi`.
    pub fn pk_hex(&self) -> String {
        hex(&self.public)
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    #[serde(rename = "priv")]
    secret: String,
    #[serde(rename = "pub")]
    public: String,
    pi: String,
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn read(path: &Path) -> Option<Identity> {
    let stored: Stored = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    Some(Identity {
        secret: unhex(&stored.secret)?.try_into().ok()?,
        public: unhex(&stored.public)?.try_into().ok()?,
        pairing_id: stored.pi,
    })
}

pub(crate) fn write_private(path: &Path, data: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let _ = fs::remove_file(&tmp);
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    io::Write::write_all(&mut file, data.as_bytes())?;
    fs::rename(&tmp, path)
}

/// A file that cannot be read starts a new identity, and every phone pairs anew.
pub fn load_or_create(path: &Path) -> Identity {
    if let Some(id) = read(path) {
        return id;
    }
    let secret = random_bytes::<32>();
    let id = Identity {
        secret,
        public: ed25519_public(&secret),
        pairing_id: uuid::Uuid::new_v4().to_string(),
    };
    let stored =
        Stored { secret: hex(&id.secret), public: hex(&id.public), pi: id.pairing_id.clone() };
    let json = serde_json::to_string(&stored).expect("strings always serialize");
    if let Err(e) = write_private(path, &json) {
        eprintln!("[cp] could not persist the identity: {e}");
    }
    id
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::pairings::tests::TempDir;

    #[test]
    fn created_once_then_read_back() {
        let dir = TempDir::new();
        let path = dir.0.join("cp/identity.json");
        let first = load_or_create(&path);
        assert_eq!(first.public, ed25519_public(&first.secret));
        assert_eq!(first.pk_hex().len(), 64);
        assert_eq!(load_or_create(&path), first);
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn reads_the_file_the_electron_app_wrote() {
        let dir = TempDir::new();
        let path = dir.0.join("identity.json");
        let secret = [7u8; 32];
        let public = ed25519_public(&secret);
        let json = format!(r#"{{"priv":"{}","pub":"{}","pi":"abc"}}"#, hex(&secret), hex(&public));
        fs::write(&path, json).unwrap();
        assert_eq!(load_or_create(&path), Identity { secret, public, pairing_id: "abc".into() });
    }

    #[test]
    fn a_broken_file_starts_over() {
        let dir = TempDir::new();
        let path = dir.0.join("identity.json");
        fs::write(&path, r#"{"priv":"zz","pub":"00","pi":"x"}"#).unwrap();
        assert_ne!(load_or_create(&path).pairing_id, "x");
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(unhex(&hex(&[0, 0xab, 0xff])), Some(vec![0, 0xab, 0xff]));
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex("zz"), None);
    }
}
