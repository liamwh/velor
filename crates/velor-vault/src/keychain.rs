//! macOS Keychain backend for vault master key storage.
//!
//! This module provides integration with the macOS Keychain for secure storage
//! of vault master keys.

use secrecy::{ExposeSecret, SecretVec};
use security_framework::passwords;

use crate::error::{Result, VaultError};
use crate::keyring::{KeyringBackend, VaultScope};

/// macOS Keychain backend for vault master key storage.
#[derive(Debug, Clone)]
pub struct MacOsKeychainBackend;

impl MacOsKeychainBackend {
    /// Create a new macOS Keychain backend.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for MacOsKeychainBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyringBackend for MacOsKeychainBackend {
    fn store_key(&self, scope: &VaultScope, key: &SecretVec<u8>) -> Result<()> {
        passwords::set_generic_password(
            scope.service_name(),
            &scope.account_name(),
            key.expose_secret(),
        )
        .map_err(|e| VaultError::Keychain(format!("Failed to store key: {e}")))?;

        tracing::debug!(
            service = scope.service_name(),
            account = %scope.account_name(),
            "Stored master key in Keychain"
        );

        Ok(())
    }

    fn load_key(&self, scope: &VaultScope) -> Result<SecretVec<u8>> {
        let key_bytes =
            passwords::get_generic_password(scope.service_name(), &scope.account_name()).map_err(
                |e| {
                    if e.to_string()
                        .contains("The specified item could not be found")
                    {
                        VaultError::VaultBackendUnavailable(format!(
                            "Master key not found in Keychain for {}",
                            scope.account_name()
                        ))
                    } else {
                        VaultError::Keychain(format!("Failed to load key: {e}"))
                    }
                },
            )?;

        tracing::debug!(
            service = scope.service_name(),
            account = %scope.account_name(),
            "Loaded master key from Keychain"
        );

        Ok(SecretVec::new(key_bytes))
    }

    fn delete_key(&self, scope: &VaultScope) -> Result<()> {
        passwords::delete_generic_password(scope.service_name(), &scope.account_name())
            .map_err(|e| VaultError::Keychain(format!("Failed to delete key: {e}")))?;

        tracing::debug!(
            service = scope.service_name(),
            account = %scope.account_name(),
            "Deleted master key from Keychain"
        );

        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "macOS Keychain"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_name() {
        let backend = MacOsKeychainBackend::new();
        assert_eq!(backend.backend_name(), "macOS Keychain");
    }
}
