//! Wallet / keystore operations.
//!
//! Keystores are JSON files holding hex-encoded Ed25519 material. When a
//! passphrase is supplied the 32-byte secret seed is obfuscated with a
//! passphrase-derived XOR keystream — see [`encrypt`] for the security caveat.

use std::fs;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use kvnc_types::{Address, Hash, PublicKey, SigningKey};

/// Current on-disk keystore schema version.
pub const KEYSTORE_VERSION: u32 = 1;

/// On-disk keystore (JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keystore {
    /// Schema version.
    pub version: u32,
    /// Account address (32-byte hex, no `0x`).
    pub address: String,
    /// Ed25519 public key (32-byte hex, no `0x`).
    pub public_key: String,
    /// Ed25519 secret seed (32-byte hex), possibly passphrase-obfuscated.
    pub secret_key: String,
    /// Whether `secret_key` is obfuscated with a passphrase.
    #[serde(default)]
    pub encrypted: bool,
}

/// Generate a fresh keypair and return its keystore (not yet persisted).
pub fn keygen(passphrase: Option<&str>) -> Result<Keystore> {
    let (signing_key, public_key) = kvnc_crypto::generate_keypair();
    let address = Address::from_public_key(&public_key);
    let seed = signing_key.to_bytes();

    let (secret_key, encrypted) = match passphrase {
        Some(passphrase) => (encrypt(&seed, passphrase), true),
        None => (hex::encode(seed), false),
    };

    Ok(Keystore {
        version: KEYSTORE_VERSION,
        address: address.to_hex(),
        public_key: hex::encode(public_key.as_bytes()),
        secret_key,
        encrypted,
    })
}

/// Persist a keystore as pretty JSON.
pub fn save(path: &Path, keystore: &Keystore) -> Result<()> {
    let json = serde_json::to_string_pretty(keystore)?;
    fs::write(path, json).with_context(|| format!("writing keystore {}", path.display()))?;
    Ok(())
}

/// Load and parse a keystore from disk.
pub fn load(path: &Path) -> Result<Keystore> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("reading keystore {}", path.display()))?;
    let keystore: Keystore = serde_json::from_str(&raw)
        .with_context(|| format!("parsing keystore {}", path.display()))?;
    Ok(keystore)
}

/// Recover the 32-byte Ed25519 secret seed, decrypting when necessary.
pub fn secret_seed(keystore: &Keystore, passphrase: Option<&str>) -> Result<[u8; 32]> {
    let stored =
        hex::decode(&keystore.secret_key).context("keystore secret_key is not valid hex")?;

    let plain = if keystore.encrypted {
        let passphrase =
            passphrase.ok_or_else(|| anyhow!("keystore is encrypted; provide --passphrase"))?;
        xor_keystream(&stored, passphrase)
    } else {
        stored
    };

    if plain.len() != 32 {
        bail!("expected a 32-byte secret seed, got {} bytes", plain.len());
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&plain);
    Ok(seed)
}

/// Rebuild the [`SigningKey`] from a keystore.
pub fn signing_key(keystore: &Keystore, passphrase: Option<&str>) -> Result<SigningKey> {
    Ok(SigningKey::from_bytes(&secret_seed(keystore, passphrase)?))
}

/// Parse the stored public key.
pub fn public_key(keystore: &Keystore) -> Result<PublicKey> {
    let bytes =
        hex::decode(&keystore.public_key).context("keystore public_key is not valid hex")?;
    if bytes.len() != 32 {
        bail!("expected a 32-byte public key, got {} bytes", bytes.len());
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(PublicKey::from_bytes(arr))
}

/// Parse the stored address.
pub fn address(keystore: &Keystore) -> Result<Address> {
    parse_address(&keystore.address, "keystore address")
}

/// Parse a 32-byte address from hex (optional `0x` prefix).
pub fn parse_address(hex_str: &str, what: &str) -> Result<Address> {
    let raw = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    let bytes = hex::decode(raw).with_context(|| format!("{what}: invalid hex"))?;
    if bytes.len() != 32 {
        bail!(
            "{what}: expected 32 bytes (64 hex chars), got {} bytes",
            bytes.len()
        );
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(Address(arr))
}

/// Obfuscate a 32-byte seed with a passphrase-derived keystream, hex-encoded.
///
/// TODO: this is **not** secure encryption — it is a deterministic XOR against
/// `BLAKE3(passphrase)` and must be replaced with a real AEAD (Argon2id-derived
/// key + ChaCha20-Poly1305 or AES-GCM) before any production use. It exists so
/// the keystore format can be exercised end-to-end.
pub fn encrypt(seed: &[u8; 32], passphrase: &str) -> String {
    hex::encode(xor_keystream(seed, passphrase))
}

/// XOR `data` with a repeating keystream derived from `passphrase`.
fn xor_keystream(data: &[u8], passphrase: &str) -> Vec<u8> {
    let key = Hash::new(passphrase.as_bytes());
    data.iter()
        .enumerate()
        .map(|(i, byte)| byte ^ key.0[i % key.0.len()])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keygen_roundtrips_unencrypted() {
        let keystore = keygen(None).unwrap();
        assert!(!keystore.encrypted);
        let sk = signing_key(&keystore, None).unwrap();
        let pk = public_key(&keystore).unwrap();
        assert_eq!(sk.verifying_key().to_bytes(), *pk.as_bytes());
        assert_eq!(Address::from_public_key(&pk).to_hex(), keystore.address);
    }

    #[test]
    fn keygen_roundtrips_encrypted() {
        let keystore = keygen(Some("hunter2")).unwrap();
        assert!(keystore.encrypted);
        assert!(signing_key(&keystore, Some("hunter2")).is_ok());
        assert!(signing_key(&keystore, Some("wrong")).is_ok()); // XOR cannot detect wrong passphrases
        assert!(signing_key(&keystore, None).is_err());
    }

    #[test]
    fn parses_address_hex() {
        let hex = "ab".repeat(32);
        assert_eq!(parse_address(&hex, "addr").unwrap().0, [0xab; 32]);
        assert!(parse_address("0x1234", "addr").is_err());
    }
}
