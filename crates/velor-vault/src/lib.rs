//! Velor Vault - Encrypted secrets storage for automations.
//!
//! This crate provides secure storage of secrets (API keys, tokens, etc.) for
//! Velor automations. Secrets are encrypted at rest using XChaCha20-Poly1305
//! with the master key stored in an OS-backed secret store (Keychain on macOS,
//! Secret Service on Linux) or derived from a passphrase using Argon2id.
//!
//! # Architecture
//!
//! ```text
//! Vault File (.velor/vault.bin)
//! ├── version: u8
//! ├── nonce: [u8; 24]
//! └── ciphertext: Vec<u8> (AEAD-encrypted secrets JSON)
//! ```
//!
//! The master key is stored in:
//! - macOS: Keychain (service=velor, account=vault:<scope>)
//! - Linux: Secret Service (velor/vault/<scope>)
//! - Fallback: Interactive passphrase (ARGON2ID-derived)
//!
//! # Example
//!
//! ```rust,no_run
//! use velor_vault::{Vault, VaultScope, default_keyring_backend};
//! use secrecy::SecretString;
//! use std::path::Path;
//!
//! # async fn example() -> Result<(), velor_vault::VaultError> {
//! // Initialize vault with OS keyring
//! let scope = VaultScope::Global;
//! let backend = default_keyring_backend()?;
//! let vault = Vault::create_keyring(
//!     Path::new("~/.config/velor/vault.bin"),
//!     scope,
//!     backend
//! ).await?;
//!
//! // Set a secret
//! let mut vault = vault;
//! vault.set(
//!     "API_KEY".to_string(),
//!     SecretString::new("sk-1234".to_string())
//! );
//! vault.save().await?;
//!
//! // Get a secret
//! if let Some(value) = vault.get("API_KEY") {
//!     println!("Found API key");
//! }
//! # Ok(())
//! # }
//! ```

pub mod crypto;
pub mod error;
pub mod keyring;
pub mod passphrase;
pub mod vault;

// Platform-specific backends
#[cfg(target_os = "macos")]
pub mod keychain;
#[cfg(target_os = "linux")]
pub mod secret_service;

// Re-export public API
pub use error::{Result, VaultError};
pub use keyring::{default_keyring_backend, BackendKind, KeyringBackend, VaultScope};
pub use passphrase::KdfMetadata;
pub use vault::Vault;

use std::collections::HashSet;
use std::path::Path;

/// Check if a string is a valid environment variable name for secrets.
///
/// Secret names must match `^[A-Z][A-Z0-9_]*$` to ensure compatibility
/// with environment variable injection.
#[must_use]
pub fn is_valid_secret_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().enumerate().all(|(i, c)| {
            if i == 0 {
                c.is_ascii_uppercase()
            } else {
                c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'
            }
        })
}

/// Validate secret declarations for an automation.
///
/// Checks that:
/// - All names match `^[A-Z][A-Z0-9_]*$`
/// - No duplicates exist across required and optional lists
///
/// # Errors
///
/// Returns an error if any name is invalid or duplicated.
pub fn validate_secret_declarations(required: &[String], optional: &[String]) -> Result<()> {
    // Check for valid names
    for key in required.iter().chain(optional.iter()) {
        if !is_valid_secret_name(key) {
            return Err(VaultError::InvalidSecretName { key: key.clone() });
        }
    }

    // Check for duplicates across required/optional
    let mut seen = HashSet::new();
    for key in required.iter().chain(optional.iter()) {
        if !seen.insert(key) {
            return Err(VaultError::DuplicateSecretDeclaration { key: key.clone() });
        }
    }

    Ok(())
}

/// Secrets resolved for an automation (fail-closed by type).
///
/// This struct contains only the secrets that were declared by the automation,
/// ensuring no unintended secret leakage.
#[derive(Debug)]
pub struct ResolvedSecrets {
    /// Secrets for injection (only declared keys).
    pub secrets: std::collections::HashMap<String, secrecy::SecretString>,
}

impl ResolvedSecrets {
    /// Get a resolved secret by key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&secrecy::SecretString> {
        self.secrets.get(key)
    }

    /// Check if a secret is present.
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.secrets.contains_key(key)
    }

    /// Get all secret keys.
    #[must_use]
    pub fn keys(&self) -> Vec<&String> {
        self.secrets.keys().collect()
    }

    /// Get the number of resolved secrets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.secrets.len()
    }

    /// Check if no secrets were resolved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }
}

/// Resolve secrets for an automation with fail-closed semantics.
///
/// This function:
/// 1. Loads the project vault (if exists)
/// 2. Falls back to global vault (if exists)
/// 3. Merges with project taking precedence
/// 4. Validates all required secrets are present
/// 5. Returns only declared keys (required + optional)
///
/// # Arguments
///
/// * `required` - List of required secret names.
/// * `optional` - List of optional secret names.
/// * `work_dir` - Working directory to find project vault.
///
/// # Returns
///
/// A `ResolvedSecrets` struct containing only the declared secrets.
///
/// # Errors
///
/// Returns an error if:
/// - Any required secret is missing
/// - Vault is unavailable (for keyring backend)
/// - Decryption fails
pub async fn resolve_automation_secrets(
    required: &[String],
    optional: &[String],
    work_dir: &Path,
) -> Result<ResolvedSecrets> {
    // Validate declarations first
    validate_secret_declarations(required, optional)?;

    // Determine vault paths
    let project_vault_path = work_dir.join(".velor").join("vault.bin");
    let global_vault_path = dirs::config_dir()
        .unwrap_or_else(|| Path::new("~/.config").to_path_buf())
        .join("velor")
        .join("vault.bin");

    // Collect secrets from vaults (project takes precedence)
    let mut resolved: std::collections::HashMap<String, secrecy::SecretString> =
        std::collections::HashMap::new();

    // Try to load global vault first
    if global_vault_path.exists() {
        if let Ok(backend) = default_keyring_backend() {
            let scope = VaultScope::Global;
            if let Ok(vault) = Vault::load_keyring(&global_vault_path, scope, backend).await {
                for key in required.iter().chain(optional.iter()) {
                    if let Some(value) = vault.get(key) {
                        resolved.insert(key.clone(), value.clone());
                    }
                }
            }
        }
    }

    // Try to load project vault (overrides global)
    if project_vault_path.exists() {
        let scope = VaultScope::from_git_root(work_dir);
        if let Ok(backend) = default_keyring_backend() {
            if let Ok(vault) = Vault::load_keyring(&project_vault_path, scope, backend).await {
                for key in required.iter().chain(optional.iter()) {
                    if let Some(value) = vault.get(key) {
                        resolved.insert(key.clone(), value.clone());
                    }
                }
            }
        }
    }

    // Check required secrets
    for key in required {
        if !resolved.contains_key(key) {
            return Err(VaultError::RequiredSecretMissing { key: key.clone() });
        }
    }

    tracing::debug!(
        required = required.len(),
        optional = optional.len(),
        resolved = resolved.len(),
        "Resolved automation secrets"
    );

    Ok(ResolvedSecrets { secrets: resolved })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_secret_name() {
        // Valid names
        assert!(is_valid_secret_name("API_KEY"));
        assert!(is_valid_secret_name("ZAI_API_KEY"));
        assert!(is_valid_secret_name("A"));
        assert!(is_valid_secret_name("KEY_123"));
        assert!(is_valid_secret_name("MY_SECRET_TOKEN"));

        // Invalid names
        assert!(!is_valid_secret_name(""));
        assert!(!is_valid_secret_name("api_key")); // lowercase
        assert!(!is_valid_secret_name("1API_KEY")); // starts with digit
        assert!(!is_valid_secret_name("_API_KEY")); // starts with underscore
        assert!(!is_valid_secret_name("API-KEY")); // hyphen
        assert!(!is_valid_secret_name("API.KEY")); // dot
    }

    #[test]
    fn test_validate_secret_declarations_valid() {
        let required = vec!["KEY1".to_string(), "KEY2".to_string()];
        let optional = vec!["KEY3".to_string()];

        assert!(validate_secret_declarations(&required, &optional).is_ok());
    }

    #[test]
    fn test_validate_secret_declarations_invalid_name() {
        let required = vec!["invalid-key".to_string()];
        let optional = vec![];

        let result = validate_secret_declarations(&required, &optional);
        assert!(result.is_err());
        assert!(matches!(
            result.expect_err("Should be InvalidSecretName error"),
            VaultError::InvalidSecretName { .. }
        ));
    }

    #[test]
    fn test_validate_secret_declarations_duplicate() {
        let required = vec!["KEY1".to_string()];
        let optional = vec!["KEY1".to_string()]; // Duplicate!

        let result = validate_secret_declarations(&required, &optional);
        assert!(result.is_err());
        assert!(matches!(
            result.expect_err("Should be DuplicateSecretDeclaration error"),
            VaultError::DuplicateSecretDeclaration { .. }
        ));
    }

    #[test]
    fn test_resolved_secrets() {
        let mut secrets = std::collections::HashMap::new();
        secrets.insert(
            "KEY1".to_string(),
            secrecy::SecretString::new("value1".to_string()),
        );
        secrets.insert(
            "KEY2".to_string(),
            secrecy::SecretString::new("value2".to_string()),
        );

        let resolved = ResolvedSecrets { secrets };

        assert_eq!(resolved.len(), 2);
        assert!(!resolved.is_empty());
        assert!(resolved.contains("KEY1"));
        assert!(resolved.contains("KEY2"));
        assert!(!resolved.contains("KEY3"));
        assert!(resolved.get("KEY1").is_some());
        assert_eq!(resolved.keys().len(), 2);
    }
}
