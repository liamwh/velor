//! Cryptographic operations for vault encryption.
//!
//! Uses XChaCha20-Poly1305 AEAD for encryption with a 256-bit master key.

use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use rand::RngCore;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{Result, VaultError};

/// Vault format marker for corruption detection.
pub const VAULT_FORMAT: &str = "velor-vault";

/// Current vault format version.
pub const VERSION: u8 = 1;

/// Nonce length for XChaCha20 (24 bytes).
pub const NONCE_LEN: usize = 24;

/// Key length for XChaCha20-Poly1305 (32 bytes).
pub const KEY_LEN: usize = 32;

/// A master key for vault encryption.
///
/// This type wraps a 256-bit key and ensures it is zeroized when dropped.
#[derive(ZeroizeOnDrop)]
pub struct MasterKey([u8; KEY_LEN]);

impl MasterKey {
    /// Generate a new random master key.
    ///
    /// # Errors
    ///
    /// Returns an error if the random number generator fails.
    pub fn generate() -> Result<Self> {
        let mut key = [0u8; KEY_LEN];
        rand::thread_rng()
            .try_fill_bytes(&mut key)
            .map_err(|e| VaultError::Crypto(format!("Failed to generate key: {e}")))?;
        Ok(Self(key))
    }

    /// Create a master key from raw bytes.
    ///
    /// # Panics
    ///
    /// Panics if the slice is not exactly 32 bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// Get the raw key bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl Zeroize for MasterKey {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

/// A nonce for XChaCha20-Poly1305 encryption.
#[derive(Clone, Copy, Zeroize)]
pub struct Nonce([u8; NONCE_LEN]);

impl Nonce {
    /// Generate a new random nonce.
    ///
    /// # Errors
    ///
    /// Returns an error if the random number generator fails.
    pub fn generate() -> Result<Self> {
        let mut nonce = [0u8; NONCE_LEN];
        rand::thread_rng()
            .try_fill_bytes(&mut nonce)
            .map_err(|e| VaultError::Crypto(format!("Failed to generate nonce: {e}")))?;
        Ok(Self(nonce))
    }

    /// Create a nonce from raw bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; NONCE_LEN]) -> Self {
        Self(bytes)
    }

    /// Get the raw nonce bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; NONCE_LEN] {
        &self.0
    }
}

/// Encrypt plaintext using XChaCha20-Poly1305.
///
/// # Arguments
///
/// * `plaintext` - The data to encrypt.
/// * `master_key` - The 256-bit encryption key.
///
/// # Returns
///
/// A tuple of (ciphertext, nonce) on success.
///
/// # Errors
///
/// Returns an error if encryption fails.
pub fn encrypt(plaintext: &[u8], master_key: &MasterKey) -> Result<(Vec<u8>, Nonce)> {
    let nonce = Nonce::generate()?;
    let cipher = XChaCha20Poly1305::new_from_slice(master_key.as_bytes())
        .map_err(|e| VaultError::Crypto(format!("Failed to initialize cipher: {e}")))?;

    let xnonce = XNonce::from_slice(nonce.as_bytes());
    let ciphertext = cipher
        .encrypt(xnonce, plaintext)
        .map_err(|e| VaultError::Crypto(format!("Encryption failed: {e}")))?;

    Ok((ciphertext, nonce))
}

/// Decrypt ciphertext using XChaCha20-Poly1305.
///
/// # Arguments
///
/// * `ciphertext` - The encrypted data.
/// * `nonce` - The 24-byte nonce used during encryption.
/// * `master_key` - The 256-bit decryption key.
///
/// # Returns
///
/// The decrypted plaintext on success.
///
/// # Errors
///
/// Returns an error if decryption fails (wrong key or corrupted data).
pub fn decrypt(ciphertext: &[u8], nonce: &Nonce, master_key: &MasterKey) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new_from_slice(master_key.as_bytes())
        .map_err(|e| VaultError::Crypto(format!("Failed to initialize cipher: {e}")))?;

    let xnonce = XNonce::from_slice(nonce.as_bytes());
    let plaintext = cipher
        .decrypt(xnonce, ciphertext)
        .map_err(|_| VaultError::VaultDecryptFailed)?;

    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = MasterKey::generate().expect("Failed to generate key");
        let plaintext = b"Hello, World! This is a secret message.";

        let (ciphertext, nonce) = encrypt(plaintext, &key).expect("Encryption failed");
        let decrypted = decrypt(&ciphertext, &nonce, &key).expect("Decryption failed");

        assert_eq!(
            plaintext.as_slice(),
            decrypted.as_slice(),
            "Decrypted text should match original"
        );
    }

    #[test]
    fn test_decrypt_with_wrong_key_fails() {
        let key1 = MasterKey::generate().expect("Failed to generate key 1");
        let key2 = MasterKey::generate().expect("Failed to generate key 2");
        let plaintext = b"Secret data";

        let (ciphertext, nonce) = encrypt(plaintext, &key1).expect("Encryption failed");
        let result = decrypt(&ciphertext, &nonce, &key2);

        assert!(result.is_err(), "Decryption with wrong key should fail");
    }

    #[test]
    fn test_nonce_uniqueness() {
        let nonce1 = Nonce::generate().expect("Failed to generate nonce 1");
        let nonce2 = Nonce::generate().expect("Failed to generate nonce 2");

        assert_ne!(
            nonce1.as_bytes(),
            nonce2.as_bytes(),
            "Nonces should be unique"
        );
    }

    #[test]
    fn test_key_uniqueness() {
        let key1 = MasterKey::generate().expect("Failed to generate key 1");
        let key2 = MasterKey::generate().expect("Failed to generate key 2");

        assert_ne!(key1.as_bytes(), key2.as_bytes(), "Keys should be unique");
    }

    #[test]
    fn test_master_key_zeroize() {
        let mut key = MasterKey::generate().expect("Failed to generate key");
        let original_bytes = *key.as_bytes();
        key.zeroize();
        assert_ne!(key.as_bytes(), &original_bytes, "Key should be zeroized");
    }
}
