//! Vault management for encrypted secrets storage.
//!
//! This module provides the main Vault struct for creating, loading, and managing
//! encrypted secrets with OS-backed key storage.

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::crypto::{self, MasterKey, Nonce, VAULT_FORMAT, VERSION};
use crate::error::{Result, VaultError};
use crate::keyring::{BackendKind, KeyringBackend, VaultScope};
use crate::passphrase::KdfMetadata;

/// Vault file format with metadata.
#[derive(Debug, Serialize, Deserialize)]
pub struct VaultFile {
    /// Format marker for corruption detection.
    pub format: String,
    /// Format version.
    pub version: u8,
    /// How to unlock this vault.
    pub backend_kind: BackendKind,
    /// XChaCha20 nonce (24 bytes).
    #[serde(with = "serde_nonce")]
    pub nonce: [u8; 24],
    /// Encrypted secrets JSON.
    #[serde(with = "serde_base64")]
    pub ciphertext: Vec<u8>,
    /// KDF parameters for passphrase backend.
    pub kdf: Option<KdfMetadata>,
}

/// Serde helper for nonce encoding/decoding.
mod serde_nonce {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(nonce: &[u8; 24], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        BASE64.encode(nonce).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 24], D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let bytes = BASE64
            .decode(&s)
            .map_err(|e| D::Error::custom(format!("Invalid base64: {e}")))?;
        bytes
            .try_into()
            .map_err(|_| D::Error::custom("Invalid nonce length"))
    }
}

/// Serde helper for base64 encoding/decoding.
mod serde_base64 {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        BASE64.encode(bytes).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        BASE64
            .decode(&s)
            .map_err(|e| D::Error::custom(format!("Invalid base64: {e}")))
    }
}

/// Internal secrets storage (for serialization).
#[derive(Debug, Serialize, Deserialize, Default)]
struct SecretsMap {
    secrets: HashMap<String, String>,
}

/// Vault for encrypted secrets storage.
pub struct Vault {
    /// Vault file path.
    path: PathBuf,
    /// Scope for key lookup.
    scope: VaultScope,
    /// Decrypted secrets.
    entries: HashMap<String, SecretString>,
    /// Backend for key storage.
    backend_kind: BackendKind,
    /// Keyring backend (if using keyring mode).
    keyring_backend: Option<Box<dyn KeyringBackend>>,
    /// KDF metadata (if using passphrase mode).
    kdf: Option<KdfMetadata>,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("path", &self.path)
            .field("scope", &self.scope)
            .field("entries", &format!("<{} keys>", self.entries.len()))
            .field("backend_kind", &self.backend_kind)
            .finish()
    }
}

impl Vault {
    /// Create a new vault with a fresh master key stored in the keyring.
    ///
    /// # Arguments
    ///
    /// * `path` - Path where the vault file will be stored.
    /// * `scope` - Vault scope for key lookup.
    /// * `backend` - Keyring backend for master key storage.
    ///
    /// # Errors
    ///
    /// Returns an error if the vault cannot be created or the key cannot be stored.
    pub async fn create_keyring(
        path: &Path,
        scope: VaultScope,
        backend: Box<dyn KeyringBackend>,
    ) -> Result<Self> {
        // Generate master key
        let master_key = MasterKey::generate()?;

        // Store in keyring
        backend.store_key(
            &scope,
            &secrecy::SecretVec::new(master_key.as_bytes().to_vec()),
        )?;

        // Create empty vault
        let vault = Self {
            path: path.to_path_buf(),
            scope,
            entries: HashMap::new(),
            backend_kind: BackendKind::Keyring,
            keyring_backend: Some(backend),
            kdf: None,
        };

        // Save empty vault
        vault.save_with_key(&master_key).await?;

        tracing::info!(
            path = %vault.path.display(),
            backend = %vault.backend_kind,
            "Created new vault"
        );

        Ok(vault)
    }

    /// Create a new vault with passphrase-derived key.
    ///
    /// # Arguments
    ///
    /// * `path` - Path where the vault file will be stored.
    /// * `scope` - Vault scope for key lookup.
    /// * `passphrase` - Passphrase to derive the master key from.
    ///
    /// # Errors
    ///
    /// Returns an error if the vault cannot be created or key derivation fails.
    pub async fn create_passphrase(
        path: &Path,
        scope: VaultScope,
        passphrase: &SecretString,
    ) -> Result<Self> {
        // Generate KDF metadata
        let kdf = KdfMetadata::new()?;

        // Derive master key
        let master_key = kdf.derive_key(passphrase)?;

        // Create vault
        let vault = Self {
            path: path.to_path_buf(),
            scope,
            entries: HashMap::new(),
            backend_kind: BackendKind::Passphrase,
            keyring_backend: None,
            kdf: Some(kdf),
        };

        // Save empty vault
        vault.save_with_key(&master_key).await?;

        tracing::info!(
            path = %vault.path.display(),
            backend = %vault.backend_kind,
            "Created new passphrase vault"
        );

        Ok(vault)
    }

    /// Load and decrypt an existing vault from the keyring.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the vault file.
    /// * `scope` - Vault scope for key lookup.
    /// * `backend` - Keyring backend to load the master key from.
    ///
    /// # Errors
    ///
    /// Returns an error if the vault cannot be loaded or decrypted.
    pub async fn load_keyring(
        path: &Path,
        scope: VaultScope,
        backend: Box<dyn KeyringBackend>,
    ) -> Result<Self> {
        // Check file permissions
        check_permissions(path)?;

        // Read vault file
        let bytes = tokio::fs::read(path).await?;
        let vault_file: VaultFile = serde_json::from_slice(&bytes)?;

        // Validate format
        validate_format(&vault_file)?;

        // Validate backend kind
        if vault_file.backend_kind != BackendKind::Keyring {
            return Err(VaultError::BackendMismatch {
                expected: "keyring".to_string(),
                found: vault_file.backend_kind.to_string(),
            });
        }

        // Load master key from keyring
        let key_bytes = backend.load_key(&scope)?;
        let master_key = MasterKey::from_bytes(
            key_bytes
                .expose_secret()
                .as_slice()
                .try_into()
                .map_err(|_| VaultError::Crypto("Invalid key length".to_string()))?,
        );

        // Decrypt secrets
        let nonce = Nonce::from_bytes(vault_file.nonce);
        let plaintext = crypto::decrypt(&vault_file.ciphertext, &nonce, &master_key)?;
        let secrets: SecretsMap = serde_json::from_slice(&plaintext)?;

        tracing::info!(
            path = %path.display(),
            keys = secrets.secrets.len(),
            "Loaded vault"
        );

        Ok(Self {
            path: path.to_path_buf(),
            scope,
            entries: secrets
                .secrets
                .into_iter()
                .map(|(k, v)| (k, SecretString::new(v)))
                .collect(),
            backend_kind: BackendKind::Keyring,
            keyring_backend: Some(backend),
            kdf: None,
        })
    }

    /// Load and decrypt an existing vault with a passphrase.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the vault file.
    /// * `scope` - Vault scope for key lookup.
    /// * `passphrase` - Passphrase to derive the master key from.
    ///
    /// # Errors
    ///
    /// Returns an error if the vault cannot be loaded or decrypted.
    pub async fn load_passphrase(
        path: &Path,
        scope: VaultScope,
        passphrase: &SecretString,
    ) -> Result<Self> {
        // Check file permissions
        check_permissions(path)?;

        // Read vault file
        let bytes = tokio::fs::read(path).await?;
        let vault_file: VaultFile = serde_json::from_slice(&bytes)?;

        // Validate format
        validate_format(&vault_file)?;

        // Validate backend kind
        if vault_file.backend_kind != BackendKind::Passphrase {
            return Err(VaultError::BackendMismatch {
                expected: "passphrase".to_string(),
                found: vault_file.backend_kind.to_string(),
            });
        }

        // Get KDF metadata
        let kdf = vault_file.kdf.ok_or_else(|| {
            VaultError::Passphrase("Missing KDF metadata in vault file".to_string())
        })?;

        // Derive master key
        let master_key = kdf.derive_key(passphrase)?;

        // Decrypt secrets
        let nonce = Nonce::from_bytes(vault_file.nonce);
        let plaintext = crypto::decrypt(&vault_file.ciphertext, &nonce, &master_key)?;
        let secrets: SecretsMap = serde_json::from_slice(&plaintext)?;

        tracing::info!(
            path = %path.display(),
            keys = secrets.secrets.len(),
            "Loaded passphrase vault"
        );

        Ok(Self {
            path: path.to_path_buf(),
            scope,
            entries: secrets
                .secrets
                .into_iter()
                .map(|(k, v)| (k, SecretString::new(v)))
                .collect(),
            backend_kind: BackendKind::Passphrase,
            keyring_backend: None,
            kdf: Some(kdf),
        })
    }

    /// Save the vault (encrypt and write atomically).
    ///
    /// # Errors
    ///
    /// Returns an error if the vault cannot be saved.
    pub async fn save(&self) -> Result<()> {
        // Get master key based on backend
        match self.backend_kind {
            BackendKind::Keyring => {
                let backend = self.keyring_backend.as_ref().ok_or_else(|| {
                    VaultError::VaultBackendUnavailable("No keyring backend set".to_string())
                })?;
                let key_bytes = backend.load_key(&self.scope)?;
                let master_key = MasterKey::from_bytes(
                    key_bytes
                        .expose_secret()
                        .as_slice()
                        .try_into()
                        .map_err(|_| VaultError::Crypto("Invalid key length".to_string()))?,
                );
                self.save_with_key(&master_key).await
            }
            BackendKind::Passphrase => Err(VaultError::Passphrase(
                "Cannot save passphrase vault without passphrase. Use save_with_passphrase."
                    .to_string(),
            )),
        }
    }

    /// Save the vault with a passphrase.
    ///
    /// # Arguments
    ///
    /// * `passphrase` - Passphrase to re-encrypt the vault with.
    ///
    /// # Errors
    ///
    /// Returns an error if the vault cannot be saved.
    pub async fn save_with_passphrase(&mut self, passphrase: &SecretString) -> Result<()> {
        let kdf = self
            .kdf
            .as_ref()
            .ok_or_else(|| VaultError::Passphrase("Missing KDF metadata".to_string()))?;
        let master_key = kdf.derive_key(passphrase)?;
        self.save_with_key(&master_key).await
    }

    /// Save the vault with a specific master key.
    async fn save_with_key(&self, master_key: &MasterKey) -> Result<()> {
        // Serialize secrets
        let secrets = SecretsMap {
            secrets: self
                .entries
                .iter()
                .map(|(k, v)| (k.clone(), v.expose_secret().clone()))
                .collect(),
        };
        let plaintext = serde_json::to_vec(&secrets)?;

        // Encrypt
        let (ciphertext, nonce) = crypto::encrypt(&plaintext, master_key)?;

        // Create vault file
        let vault_file = VaultFile {
            format: VAULT_FORMAT.to_string(),
            version: VERSION,
            backend_kind: self.backend_kind,
            nonce: *nonce.as_bytes(),
            ciphertext,
            kdf: self.kdf.clone(),
        };

        // Write atomically
        write_atomic(&self.path, &vault_file).await?;

        tracing::debug!(
            path = %self.path.display(),
            keys = self.entries.len(),
            "Saved vault"
        );

        Ok(())
    }

    /// Get a secret value by key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&SecretString> {
        self.entries.get(key)
    }

    /// Set a secret value.
    pub fn set(&mut self, key: String, value: SecretString) {
        self.entries.insert(key, value);
    }

    /// Remove a secret.
    ///
    /// # Returns
    ///
    /// `true` if the secret was removed, `false` if it didn't exist.
    pub fn unset(&mut self, key: &str) -> bool {
        self.entries.remove(key).is_some()
    }

    /// Check if a secret exists.
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// List all secret keys.
    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    /// Get the number of secrets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the vault is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get the vault path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get the vault scope.
    #[must_use]
    pub fn scope(&self) -> &VaultScope {
        &self.scope
    }

    /// Get the backend kind.
    #[must_use]
    pub fn backend_kind(&self) -> BackendKind {
        self.backend_kind
    }

    /// Rotate the master key (re-encrypt vault with new key).
    ///
    /// For keyring backends, generates a new random key.
    /// For passphrase backends, requires a new passphrase.
    ///
    /// # Errors
    ///
    /// Returns an error if the rotation fails.
    pub async fn rotate_master_key(&mut self) -> Result<()> {
        match self.backend_kind {
            BackendKind::Keyring => {
                let backend = self.keyring_backend.as_ref().ok_or_else(|| {
                    VaultError::VaultBackendUnavailable("No keyring backend set".to_string())
                })?;

                // Generate new key
                let new_key = MasterKey::generate()?;

                // Store new key (overwrites old)
                backend.store_key(
                    &self.scope,
                    &secrecy::SecretVec::new(new_key.as_bytes().to_vec()),
                )?;

                // Re-save with new key
                self.save_with_key(&new_key).await?;

                tracing::info!("Rotated master key");
                Ok(())
            }
            BackendKind::Passphrase => Err(VaultError::Passphrase(
                "Use rotate_passphrase for passphrase vaults".to_string(),
            )),
        }
    }

    /// Rotate the passphrase (re-encrypt vault with new passphrase).
    ///
    /// # Arguments
    ///
    /// * `new_passphrase` - New passphrase to encrypt the vault with.
    ///
    /// # Errors
    ///
    /// Returns an error if the rotation fails.
    pub async fn rotate_passphrase(&mut self, new_passphrase: &SecretString) -> Result<()> {
        // Generate new KDF metadata
        let new_kdf = KdfMetadata::new()?;

        // Derive new key
        let new_key = new_kdf.derive_key(new_passphrase)?;

        // Update KDF metadata
        self.kdf = Some(new_kdf);

        // Re-save with new key
        self.save_with_key(&new_key).await?;

        tracing::info!("Rotated passphrase");
        Ok(())
    }

    /// Migrate to a different backend.
    ///
    /// # Arguments
    ///
    /// * `new_backend` - New keyring backend to use.
    ///
    /// # Errors
    ///
    /// Returns an error if the migration fails.
    pub async fn migrate_to_keyring(
        mut self,
        new_backend: Box<dyn KeyringBackend>,
    ) -> Result<Self> {
        // Generate new master key
        let new_key = MasterKey::generate()?;

        // Store in new backend
        new_backend.store_key(
            &self.scope,
            &secrecy::SecretVec::new(new_key.as_bytes().to_vec()),
        )?;

        // Update vault
        self.backend_kind = BackendKind::Keyring;
        self.keyring_backend = Some(new_backend);
        self.kdf = None;

        // Save with new key
        self.save_with_key(&new_key).await?;

        tracing::info!("Migrated vault to keyring backend");
        Ok(self)
    }

    /// Migrate to passphrase backend.
    ///
    /// # Arguments
    ///
    /// * `passphrase` - Passphrase to encrypt the vault with.
    ///
    /// # Errors
    ///
    /// Returns an error if the migration fails.
    pub async fn migrate_to_passphrase(mut self, passphrase: &SecretString) -> Result<Self> {
        // Delete old keyring key if present
        if let Some(backend) = &self.keyring_backend {
            let _ = backend.delete_key(&self.scope);
        }

        // Generate new KDF metadata
        let new_kdf = KdfMetadata::new()?;
        let new_key = new_kdf.derive_key(passphrase)?;

        // Update vault
        self.backend_kind = BackendKind::Passphrase;
        self.keyring_backend = None;
        self.kdf = Some(new_kdf);

        // Save with new key
        self.save_with_key(&new_key).await?;

        tracing::info!("Migrated vault to passphrase backend");
        Ok(self)
    }
}

/// Check file permissions on Unix systems.
#[cfg(unix)]
fn check_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::metadata(path)?;
    let mode = metadata.permissions().mode();

    if mode & 0o077 != 0 {
        return Err(VaultError::InsecurePermissions(format!(
            "Refusing to load vault with overly-permissive file mode: {:o}",
            mode
        )));
    }

    Ok(())
}

/// Check file permissions on non-Unix systems.
#[cfg(not(unix))]
fn check_permissions(_path: &Path) -> Result<()> {
    // No permission checks on non-Unix systems
    Ok(())
}

/// Validate vault file format.
fn validate_format(vault_file: &VaultFile) -> Result<()> {
    if vault_file.format != VAULT_FORMAT {
        return Err(VaultError::InvalidFormat);
    }

    if vault_file.version != VERSION {
        return Err(VaultError::UnsupportedVersion(vault_file.version));
    }

    Ok(())
}

/// Write vault file atomically with proper permissions.
async fn write_atomic(path: &Path, vault_file: &VaultFile) -> Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Write to temp file
    let temp_path = path.with_extension("tmp");
    let json = serde_json::to_vec_pretty(vault_file)?;
    tokio::fs::write(&temp_path, &json).await?;

    // Set restrictive permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        tokio::fs::set_permissions(&temp_path, perms).await?;
    }

    // Create backup (best-effort)
    let backup_path = path.with_extension("bak");
    if path.exists() {
        let _ = tokio::fs::rename(path, &backup_path).await;
    }

    // Atomic rename
    tokio::fs::rename(&temp_path, path).await?;

    // Sync parent directory for durability
    #[cfg(unix)]
    {
        if let Some(parent) = path.parent() {
            if let Ok(file) = std::fs::File::open(parent) {
                let _ = file.sync_all();
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use tempfile::tempdir;

    // Mock keyring backend for testing
    #[derive(Clone)]
    struct MockKeyring {
        keys: std::sync::Arc<std::sync::Mutex<HashMap<String, Vec<u8>>>>,
    }

    impl MockKeyring {
        fn new() -> Self {
            Self {
                keys: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            }
        }
    }

    impl KeyringBackend for MockKeyring {
        fn store_key(&self, scope: &VaultScope, key: &secrecy::SecretVec<u8>) -> Result<()> {
            let mut keys = self.keys.lock().expect("Mutex poisoned");
            keys.insert(scope.account_name(), key.expose_secret().to_vec());
            Ok(())
        }

        fn load_key(&self, scope: &VaultScope) -> Result<secrecy::SecretVec<u8>> {
            let keys = self.keys.lock().expect("Mutex poisoned");
            keys.get(&scope.account_name())
                .map(|k| secrecy::SecretVec::new(k.clone()))
                .ok_or_else(|| {
                    VaultError::VaultBackendUnavailable(format!(
                        "Key not found: {}",
                        scope.account_name()
                    ))
                })
        }

        fn delete_key(&self, scope: &VaultScope) -> Result<()> {
            let mut keys = self.keys.lock().expect("Mutex poisoned");
            keys.remove(&scope.account_name());
            Ok(())
        }

        fn backend_name(&self) -> &'static str {
            "Mock"
        }
    }

    fn make_passphrase(s: &str) -> SecretString {
        SecretString::new(s.to_string())
    }

    #[tokio::test]
    async fn test_create_and_load_keyring_vault() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vault.bin");
        let scope = VaultScope::Global;
        let backend = Box::new(MockKeyring::new());

        // Create vault
        let vault = Vault::create_keyring(&path, scope.clone(), backend.clone())
            .await
            .expect("Failed to create vault");

        assert_eq!(vault.len(), 0);
        assert_eq!(vault.backend_kind(), BackendKind::Keyring);

        // Load vault
        let loaded = Vault::load_keyring(&path, scope, backend)
            .await
            .expect("Failed to load vault");

        assert_eq!(loaded.len(), 0);
    }

    #[tokio::test]
    async fn test_create_and_load_passphrase_vault() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vault.bin");
        let scope = VaultScope::Global;
        let passphrase = make_passphrase("correct-horse-battery-staple");

        // Create vault
        let vault = Vault::create_passphrase(&path, scope.clone(), &passphrase)
            .await
            .expect("Failed to create vault");

        assert_eq!(vault.len(), 0);
        assert_eq!(vault.backend_kind(), BackendKind::Passphrase);

        // Load vault
        let loaded = Vault::load_passphrase(&path, scope, &passphrase)
            .await
            .expect("Failed to load vault");

        assert_eq!(loaded.len(), 0);
    }

    #[tokio::test]
    async fn test_set_get_unset_secret() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vault.bin");
        let scope = VaultScope::Global;
        let backend = Box::new(MockKeyring::new());

        let mut vault = Vault::create_keyring(&path, scope, backend)
            .await
            .expect("Failed to create vault");

        // Set
        vault.set("TEST_KEY".to_string(), make_passphrase("secret-value"));

        assert_eq!(vault.len(), 1);
        assert!(vault.contains("TEST_KEY"));
        assert!(vault.get("TEST_KEY").is_some());

        // Get
        let value = vault.get("TEST_KEY").expect("Key should exist");
        assert_eq!(value.expose_secret(), "secret-value");

        // Unset
        let removed = vault.unset("TEST_KEY");
        assert!(removed, "Should have removed key");
        assert_eq!(vault.len(), 0);
        assert!(vault.get("TEST_KEY").is_none());

        // Unset again returns false
        let removed_again = vault.unset("TEST_KEY");
        assert!(!removed_again, "Should not have removed key again");
    }

    #[tokio::test]
    async fn test_save_and_reload_keyring() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vault.bin");
        let scope = VaultScope::Global;
        let backend = Box::new(MockKeyring::new());

        // Create and populate vault
        let mut vault = Vault::create_keyring(&path, scope.clone(), backend.clone())
            .await
            .expect("Failed to create vault");

        vault.set("KEY1".to_string(), make_passphrase("value1"));
        vault.set("KEY2".to_string(), make_passphrase("value2"));

        vault.save().await.expect("Failed to save vault");

        // Reload
        let loaded = Vault::load_keyring(&path, scope, backend)
            .await
            .expect("Failed to load vault");

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.get("KEY1").expect("Key1").expose_secret(), "value1");
        assert_eq!(loaded.get("KEY2").expect("Key2").expose_secret(), "value2");
    }

    #[tokio::test]
    async fn test_save_and_reload_passphrase() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vault.bin");
        let scope = VaultScope::Global;
        let passphrase = make_passphrase("test-passphrase");

        // Create and populate vault
        let mut vault = Vault::create_passphrase(&path, scope.clone(), &passphrase)
            .await
            .expect("Failed to create vault");

        vault.set("SECRET".to_string(), make_passphrase("hidden"));

        vault
            .save_with_passphrase(&passphrase)
            .await
            .expect("Failed to save vault");

        // Reload
        let loaded = Vault::load_passphrase(&path, scope, &passphrase)
            .await
            .expect("Failed to load vault");

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.get("SECRET").expect("Key").expose_secret(), "hidden");
    }

    #[tokio::test]
    async fn test_wrong_passphrase_fails() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vault.bin");
        let scope = VaultScope::Global;
        let correct_passphrase = make_passphrase("correct");
        let wrong_passphrase = make_passphrase("wrong");

        // Create vault
        let _vault = Vault::create_passphrase(&path, scope.clone(), &correct_passphrase)
            .await
            .expect("Failed to create vault");

        // Try to load with wrong passphrase
        let result = Vault::load_passphrase(&path, scope, &wrong_passphrase).await;
        assert!(result.is_err(), "Should fail with wrong passphrase");
    }

    #[tokio::test]
    async fn test_keys_list() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vault.bin");
        let scope = VaultScope::Global;
        let backend = Box::new(MockKeyring::new());

        let mut vault = Vault::create_keyring(&path, scope, backend)
            .await
            .expect("Failed to create vault");

        vault.set("ALPHA".to_string(), make_passphrase("a"));
        vault.set("BETA".to_string(), make_passphrase("b"));
        vault.set("GAMMA".to_string(), make_passphrase("g"));

        let mut keys = vault.keys();
        keys.sort();

        assert_eq!(keys, vec!["ALPHA", "BETA", "GAMMA"]);
    }
}
