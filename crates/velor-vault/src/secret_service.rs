//! Linux Secret Service backend for vault master key storage.
//!
//! This module provides integration with the Linux Secret Service (e.g., GNOME Keyring,
//! KDE Wallet) for secure storage of vault master keys.

use secrecy::{ExposeSecret, SecretVec};
use secret_service::blocking::SecretService;
use secret_service::EncryptionType;

use crate::error::{Result, VaultError};
use crate::keyring::{KeyringBackend, VaultScope};

/// Linux Secret Service backend for vault master key storage.
pub struct SecretServiceBackend {
    service: SecretService<'static>,
}

impl SecretServiceBackend {
    /// Create a new Secret Service backend.
    ///
    /// # Errors
    ///
    /// Returns an error if the Secret Service cannot be connected to.
    pub fn new() -> Result<Self> {
        let service = SecretService::connect(EncryptionType::Dh).map_err(|e| {
            VaultError::SecretService(format!("Failed to connect to Secret Service: {e}"))
        })?;

        Ok(Self { service })
    }
}

impl std::fmt::Debug for SecretServiceBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretServiceBackend")
            .finish_non_exhaustive()
    }
}

impl KeyringBackend for SecretServiceBackend {
    fn store_key(&self, scope: &VaultScope, key: &SecretVec<u8>) -> Result<()> {
        let collection = self.service.get_default_collection().map_err(|e| {
            VaultError::SecretService(format!("Failed to get default collection: {e}"))
        })?;

        let account_name = scope.account_name();
        let attributes = std::collections::HashMap::from([
            ("service", scope.service_name()),
            ("account", account_name.as_str()),
        ]);

        // Try to delete any existing item first
        if let Ok(items) = collection.search_items(attributes.clone()) {
            for item in items {
                let _ = item.delete();
            }
        }

        // Create new item
        collection
            .create_item(
                &format!("Velor Vault - {}", scope.account_name()),
                attributes,
                key.expose_secret(),
                true, // replace
                "text/plain",
            )
            .map_err(|e| VaultError::SecretService(format!("Failed to store key: {e}")))?;

        tracing::debug!(
            service = scope.service_name(),
            account = %scope.account_name(),
            "Stored master key in Secret Service"
        );

        Ok(())
    }

    fn load_key(&self, scope: &VaultScope) -> Result<SecretVec<u8>> {
        let collection = self.service.get_default_collection().map_err(|e| {
            VaultError::SecretService(format!("Failed to get default collection: {e}"))
        })?;

        let account_name = scope.account_name();
        let attributes = std::collections::HashMap::from([
            ("service", scope.service_name()),
            ("account", account_name.as_str()),
        ]);

        let items = collection
            .search_items(attributes)
            .map_err(|e| VaultError::SecretService(format!("Failed to search for key: {e}")))?;

        let item = items.into_iter().next().ok_or_else(|| {
            VaultError::VaultBackendUnavailable(format!(
                "Master key not found in Secret Service for {}",
                scope.account_name()
            ))
        })?;

        let secret = item
            .get_secret()
            .map_err(|e| VaultError::SecretService(format!("Failed to get secret: {e}")))?;

        tracing::debug!(
            service = scope.service_name(),
            account = %scope.account_name(),
            "Loaded master key from Secret Service"
        );

        Ok(SecretVec::new(secret))
    }

    fn delete_key(&self, scope: &VaultScope) -> Result<()> {
        let collection = self.service.get_default_collection().map_err(|e| {
            VaultError::SecretService(format!("Failed to get default collection: {e}"))
        })?;

        let account_name = scope.account_name();
        let attributes = std::collections::HashMap::from([
            ("service", scope.service_name()),
            ("account", account_name.as_str()),
        ]);

        let items = collection
            .search_items(attributes)
            .map_err(|e| VaultError::SecretService(format!("Failed to search for key: {e}")))?;

        let item = items.into_iter().next().ok_or_else(|| {
            VaultError::SecretService(format!(
                "Master key not found for deletion: {}",
                scope.account_name()
            ))
        })?;

        item.delete()
            .map_err(|e| VaultError::SecretService(format!("Failed to delete key: {e}")))?;

        tracing::debug!(
            service = scope.service_name(),
            account = %scope.account_name(),
            "Deleted master key from Secret Service"
        );

        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "Linux Secret Service"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_name() {
        // Note: This test will fail if Secret Service is not available
        if let Ok(backend) = SecretServiceBackend::new() {
            assert_eq!(backend.backend_name(), "Linux Secret Service");
        }
    }
}
