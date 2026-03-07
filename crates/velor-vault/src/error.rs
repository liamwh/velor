//! Error types for the velor-vault crate.

use std::path::PathBuf;

/// Errors that can occur during vault operations.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// Vault file not found at the specified path.
    #[error("Vault not found at {0}")]
    VaultNotFound(PathBuf),

    /// Vault backend is unavailable (e.g., keyring service not running).
    #[error("Vault backend unavailable: {0}")]
    VaultBackendUnavailable(String),

    /// Failed to decrypt the vault (wrong key or corrupted data).
    #[error("Vault decryption failed")]
    VaultDecryptFailed,

    /// A required secret is missing from the vault.
    #[error("Required secret missing: {key}")]
    RequiredSecretMissing {
        /// The name of the missing secret.
        key: String,
    },

    /// Invalid secret name (must match ^[A-Z][A-Z0-9_]*$).
    #[error("Invalid secret name '{key}': must match ^[A-Z][A-Z0-9_]*$")]
    InvalidSecretName {
        /// The invalid secret name.
        key: String,
    },

    /// Duplicate secret declaration in required/optional lists.
    #[error("Duplicate secret declaration: '{key}' appears more than once")]
    DuplicateSecretDeclaration {
        /// The duplicated secret name.
        key: String,
    },

    /// File permissions are too permissive.
    #[error("Insecure permissions: {0}")]
    InsecurePermissions(String),

    /// Invalid input provided.
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// I/O error during vault operations.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Cryptographic operation failed.
    #[error("Crypto error: {0}")]
    Crypto(String),

    /// Keychain operation failed (macOS specific).
    #[error("Keychain error: {0}")]
    Keychain(String),

    /// Secret Service operation failed (Linux specific).
    #[error("Secret Service error: {0}")]
    SecretService(String),

    /// Passphrase-related error.
    #[error("Passphrase error: {0}")]
    Passphrase(String),

    /// Vault already exists at the specified path.
    #[error("Vault already exists at {0}. Use --force to overwrite.")]
    VaultAlreadyExists(PathBuf),

    /// Backend kind mismatch between expected and found.
    #[error("Backend kind mismatch: expected {expected}, found {found}")]
    BackendMismatch {
        /// Expected backend kind.
        expected: String,
        /// Found backend kind.
        found: String,
    },

    /// Vault file has invalid format.
    #[error("Invalid vault format")]
    InvalidFormat,

    /// Vault file version is not supported.
    #[error("Unsupported vault version: {0}")]
    UnsupportedVersion(u8),
}

/// Result type for vault operations.
pub type Result<T> = std::result::Result<T, VaultError>;
