//! Vault scope and keyring backend trait.
//!
//! This module defines the scope of a vault (global or project-specific) and the
//! trait for OS-backed key storage backends.

use secrecy::SecretVec;
use sha2::{Digest, Sha256};
use std::path::Path;

use crate::error::{Result, VaultError};

/// Vault scope identifier.
///
/// Determines where the vault is stored and how its master key is identified
/// in the OS keyring.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VaultScope {
    /// Global vault stored in ~/.config/velor/vault.bin
    Global,
    /// Project-specific vault stored in {git_root}/.velor/vault.bin
    Project {
        /// First 16 hex characters of SHA-256 hash of canonicalized git root path.
        git_root_hash: String,
    },
}

impl VaultScope {
    /// Service name used in OS keyring.
    const SERVICE_NAME: &str = "velor";

    /// Create a project scope from a git root path.
    ///
    /// The scope ID is derived from the SHA-256 hash of the canonicalized path,
    /// providing a stable identifier even if the project is renamed.
    #[must_use]
    pub fn from_git_root(git_root: &Path) -> Self {
        Self::Project {
            git_root_hash: project_scope_id(git_root),
        }
    }

    /// Get the account name for this scope in the OS keyring.
    #[must_use]
    pub fn account_name(&self) -> String {
        match self {
            Self::Global => "vault:global".to_string(),
            Self::Project { git_root_hash } => format!("vault:project:{git_root_hash}"),
        }
    }

    /// Get the service name for OS keyring lookups.
    #[must_use]
    pub fn service_name(&self) -> &'static str {
        Self::SERVICE_NAME
    }

    /// Check if this is a global scope.
    #[must_use]
    pub fn is_global(&self) -> bool {
        matches!(self, Self::Global)
    }
}

/// Generate a project scope ID from a git root path.
///
/// Uses SHA-256 hash of the canonicalized path, returning the first 16 hex chars.
fn project_scope_id(git_root: &Path) -> String {
    let canonical = git_root
        .canonicalize()
        .unwrap_or_else(|_| git_root.to_path_buf());

    let hash = Sha256::digest(canonical.to_string_lossy().as_bytes());
    format!("{hash:x}")[..16].to_string()
}

/// Backend for storing vault master keys in OS secret stores.
///
/// This trait is implemented for platform-specific secret stores:
/// - macOS: Keychain
/// - Linux: Secret Service
///
/// For passphrase-based vaults, a different unlock mechanism is used.
pub trait KeyringBackend: Send + Sync {
    /// Store the master key in the backend.
    ///
    /// # Arguments
    ///
    /// * `scope` - The vault scope identifying which key to store.
    /// * `key` - The 256-bit master key to store.
    ///
    /// # Errors
    ///
    /// Returns an error if the key cannot be stored.
    fn store_key(&self, scope: &VaultScope, key: &SecretVec<u8>) -> Result<()>;

    /// Load the master key from the backend.
    ///
    /// # Arguments
    ///
    /// * `scope` - The vault scope identifying which key to load.
    ///
    /// # Returns
    ///
    /// The 256-bit master key on success.
    ///
    /// # Errors
    ///
    /// Returns an error if the key cannot be found or loaded.
    fn load_key(&self, scope: &VaultScope) -> Result<SecretVec<u8>>;

    /// Delete the master key from the backend.
    ///
    /// # Arguments
    ///
    /// * `scope` - The vault scope identifying which key to delete.
    ///
    /// # Errors
    ///
    /// Returns an error if the key cannot be deleted.
    fn delete_key(&self, scope: &VaultScope) -> Result<()>;

    /// Get a human-readable name for this backend.
    fn backend_name(&self) -> &'static str;
}

/// Backend kind stored in vault file metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BackendKind {
    /// OS-backed keyring (Keychain on macOS, Secret Service on Linux).
    Keyring,
    /// Passphrase-derived key using Argon2id.
    Passphrase,
}

impl std::fmt::Display for BackendKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keyring => write!(f, "keyring"),
            Self::Passphrase => write!(f, "passphrase"),
        }
    }
}

impl std::str::FromStr for BackendKind {
    type Err = VaultError;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "keyring" => Ok(Self::Keyring),
            "passphrase" => Ok(Self::Passphrase),
            _ => Err(VaultError::InvalidInput(format!(
                "Unknown backend kind: {s}. Expected 'keyring' or 'passphrase'."
            ))),
        }
    }
}

/// Get the default keyring backend for the current platform.
///
/// # Errors
///
/// Returns an error if no keyring backend is available on this platform.
#[cfg(target_os = "macos")]
pub fn default_keyring_backend() -> Result<Box<dyn KeyringBackend>> {
    Ok(Box::new(crate::keychain::MacOsKeychainBackend::new()))
}

/// Get the default keyring backend for the current platform.
///
/// # Errors
///
/// Returns an error if no keyring backend is available on this platform.
#[cfg(target_os = "linux")]
pub fn default_keyring_backend() -> Result<Box<dyn KeyringBackend>> {
    crate::secret_service::SecretServiceBackend::new()
        .map(|b| Box::new(b) as Box<dyn KeyringBackend>)
        .map_err(|e| VaultError::VaultBackendUnavailable(e.to_string()))
}

/// Get the default keyring backend for the current platform.
///
/// # Errors
///
/// Returns an error if no keyring backend is available on this platform.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn default_keyring_backend() -> Result<Box<dyn KeyringBackend>> {
    Err(VaultError::VaultBackendUnavailable(
        "No OS keyring backend available on this platform. Use --backend passphrase.".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_global_scope_account_name() {
        let scope = VaultScope::Global;
        assert_eq!(scope.account_name(), "vault:global");
    }

    #[test]
    fn test_project_scope_account_name() {
        let scope = VaultScope::Project {
            git_root_hash: "a1b2c3d4e5f67890".to_string(),
        };
        assert_eq!(scope.account_name(), "vault:project:a1b2c3d4e5f67890");
    }

    #[test]
    fn test_project_scope_from_path() {
        let path = Path::new("/Users/test/project");
        let scope = VaultScope::from_git_root(path);
        assert!(matches!(scope, VaultScope::Project { .. }));
        assert!(scope.account_name().starts_with("vault:project:"));
    }

    #[test]
    fn test_project_scope_id_consistency() {
        let path = Path::new("/Users/test/project");
        let id1 = project_scope_id(path);
        let id2 = project_scope_id(path);
        assert_eq!(id1, id2, "Same path should produce same ID");
        assert_eq!(id1.len(), 16, "ID should be 16 hex characters");
    }

    #[test]
    fn test_project_scope_id_uniqueness() {
        let path1 = Path::new("/Users/test/project1");
        let path2 = Path::new("/Users/test/project2");
        let id1 = project_scope_id(path1);
        let id2 = project_scope_id(path2);
        assert_ne!(id1, id2, "Different paths should produce different IDs");
    }

    #[test]
    fn test_backend_kind_from_str() {
        use std::str::FromStr;

        assert_eq!(
            BackendKind::from_str("keyring").expect("keyring should parse"),
            BackendKind::Keyring
        );
        assert_eq!(
            BackendKind::from_str("KEYRING").expect("KEYRING should parse"),
            BackendKind::Keyring
        );
        assert_eq!(
            BackendKind::from_str("passphrase").expect("passphrase should parse"),
            BackendKind::Passphrase
        );
        assert!(BackendKind::from_str("invalid").is_err());
    }

    #[test]
    fn test_backend_kind_display() {
        assert_eq!(BackendKind::Keyring.to_string(), "keyring");
        assert_eq!(BackendKind::Passphrase.to_string(), "passphrase");
    }
}
