# Velor Vault: Encrypted Secrets for Automations

## Context

Velor automations currently store all variables in plaintext TOML files. When using API keys (e.g., `ZAI_API_KEY` for glm/glm4), users must either:
- Store secrets in plaintext `.env` files (accidental git commit risk)
- Set environment variables manually in launchd plist (manual maintenance)

This plan implements an Ansible Vault-like encrypted secrets system that:
- Stores secrets encrypted at rest using OS-backed key storage
- Automatically decrypts and injects secrets as environment variables during automation execution
- Requires automations to explicitly declare which secrets they need
- Uses macOS Keychain / Linux Secret Service as the default key store

## Design Summary

### Cryptographic Architecture (Simplified)

**Option A - OS Secret Store as Root of Trust:**
```
┌─────────────────────────────────────────────────────────────┐
│ Vault File (.velor/vault.bin)                               │
│ ├── version: u8                                             │
│ ├── nonce: [u8; 12]                                         │
│ └── ciphertext: Vec<u8> (AEAD-encrypted secrets JSON)       │
└─────────────────────────────────────────────────────────────┘
                              │
                              │ decrypt with master key from
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ OS Secret Store                                             │
│ • macOS: Keychain (service=velor, account=vault:<scope>)     │
│ • Linux: Secret Service (velor/vault/<scope>)               │
│ • Windows: Credential Manager                               │
│ Fallback: Interactive passphrase (ARGON2ID-derived)          │
└─────────────────────────────────────────────────────────────┘
```

**Why This Design:**
- **Simple crypto**: Single AEAD primitive (ChaCha20-Poly1305)
- **OS-backed root of trust**: No custom key wrapping layers
- **Easy rotation**: Re-encrypt vault under new master key
- **Smaller crypto surface**: Less code to audit, fewer failure modes

**Cryptographic Choices:**
- **Algorithm**: XChaCha20-Poly1305 (via `chacha20poly1305` crate with XChaCha20 feature)
- **Master key**: 256-bit random, stored in OS secret store
- **KDF for passphrase**: ARGON2ID (time=2, mem=64MiB, parallelism=4)
- **Nonce**: 192-bit random per encryption (XChaCha20 uses 24-byte nonce)

### Storage Locations

```
Global Vault:  ~/.config/velor/vault.bin
Project Vault: {git_root}/.velor/vault.bin

Keychain/Secret Entry:
  • macOS: service="velor", account="vault:global" or "vault:project:<hash>"
  • Linux: velor/vault/global or velor/vault/project/<hash>

Note: Moving a repository changes its project scope identity. Re-initializing
the project vault may be required after moving.
```

### Project Identity (Stable)

**Problem:** Project names are mutable and can collide.

**Solution:** Use SHA-256 hash of canonicalized git root path.

```rust
fn project_scope_id(git_root: &Path) -> String {
    let canonical = git_root.canonicalize().unwrap_or_else(|_| git_root.to_path_buf());
    let hash = sha2::Sha256::digest(canonical.to_string_lossy().as_bytes());
    format!("{:x}", hash)[..16].to_string()  // First 16 hex chars
}
```

**Keychain Account Examples:**
- Global: `vault:global`
- Project: `vault:project:a1b2c3d4e5f6g7h8` (hash of /Users/user/git/velor)

### Backend Hierarchy

1. **OS secret store** (default, platform-specific)
   - macOS: Keychain
   - Linux: Secret Service
   - Windows: **Phase 2** (Credential Manager planned)

2. **Passphrase** (explicit opt-in, ARGON2ID-derived)

3. **Environment variable** (last-resort, explicit `--from-env` flag only for reads)

**No automatic .env fallback** - user must opt into weaker modes explicitly.

**Windows Support:** Not implemented in Phase 1. Windows users should use passphrase backend
(`vel vault init --backend passphrase`). Credential Manager integration is planned for Phase 2.

**Phase 1 Automation Support:**
- OS-backed keyring backend: Full support for scheduled automations
- Passphrase backend: **Manual CLI operations only** - non-interactive automation unlock
  for passphrase mode is deferred to Phase 2

**Rationale:** Passphrase-backed automations would require a secure, explicit unlock mechanism
(e.g., env-based passphrase which reintroduces the security problem we're solving). Phase 1
focuses on the OS keyring path which provides unattended, secure vault access for scheduled runs.

## Automation Secret Declarations

**Automations must explicitly declare secrets they need:**

```toml
# .velor/automations/my-automation.toml
description = "Run GLM analysis"
schedule = "0 0 * * * *"
prompt = "Call the ZAI API"

required_secrets = ["ZAI_API_KEY"]
optional_secrets = ["ZAI_MODEL_NAME"]

enabled = true
```

**Precedence Rules:**
1. Project vault values override global vault values
2. Required secrets must be present or automation fails immediately
3. Optional secrets may be missing (not an error)
4. Only declared keys are injected (no secret leakage)

**Fail-Closed Semantics:**
```rust
match resolve_secrets(&automation, work_dir).await {
    Ok(secrets) => {
        // All required secrets satisfied
        // Only inject declared keys
    }
    Err(VaultError::RequiredSecretMissing { key, .. }) => {
        // Fail immediately, do not run automation
        eprintln!("Required secret missing: {}", key);
        return AutomationResult { status: Failed, ... };
    }
    Err(e) => {
        // Vault unavailable, backend error
        eprintln!("Vault error: {}", e);
        return AutomationResult { status: Failed, ... };
    }
}
```

## CLI Commands

```bash
# Initialize vault (OS secret store is default)
vel vault init [--global]
vel vault init --backend passphrase [--global]

# Set a secret (reads from stdin or prompts, no shell history)
printf '%s' "$ZAI_API_KEY" | vel vault set ZAI_API_KEY --global
vel vault set ZAI_API_KEY --prompt --global
vel vault set ZAI_API_KEY --from-env ZAI_API_KEY --global

# Get a secret (masked by default)
vel vault get ZAI_API_KEY --global
# Shows: •••••••••••

# Get raw value (for scripting/pipes only)
vel vault get ZAI_API_KEY --raw --global | some-tool
# Note: --raw to a TTY will refuse unless --force is provided

# List all keys (values never shown)
vel vault list --global

# Remove a secret
vel vault unset ZAI_API_KEY --global

# Validate vault access
vel vault validate --global

# Rotate master key (re-encrypts vault)
# For keyring backend: generates new random key
# For passphrase backend: prompts for new passphrase (change-passphrase)
vel vault rotate-key --global

# Migrate between backends
vel vault migrate-backend --to keyring --global
vel vault migrate-backend --to passphrase --global

# NOTE: edit is omitted for Phase 1 (added later with strict constraints)
```

**Passphrase Backend UX (Phase 1 - Manual CLI Only):**
- **CLI commands** (`init`, `set`, `get`, `list`, `unset`, `validate`): Prompt securely for passphrase
- **Automations**: Not supported - passphrase backend requires interactive prompting
- **Non-interactive use**: Deferred to Phase 2 (would require explicit env-based passphrase design)

## Implementation Plan

### Phase 1: Core Vault Library (`crates/velor-vault`)

**File Structure:**
```
crates/velor-vault/
├── Cargo.toml
└── src/
    ├── lib.rs            # Public API
    ├── vault.rs          # Vault struct (load, save, CRUD)
    ├── crypto.rs         # Simple AEAD encryption
    ├── keyring.rs        # KeyringBackend trait
    ├── keychain.rs       # macOS Keychain backend
    ├── secret_service.rs # Linux Secret Service backend
    ├── passphrase.rs     # Passphrase unlock (Phase 1: manual only)
    └── error.rs          # Error types
```

**Dependencies to Add:**
```toml
# Workspace Cargo.toml
chacha20poly1305 = "0.10"  # with XChaCha20 support enabled
rand = "0.8"
sha2 = "0.10"
argon2 = "0.5"
thiserror = "2.0"

# velor-vault crate (core library - typed errors only)
[dependencies]
secrecy = { version = "0.8", features = ["serde"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
zeroize = { version = "1", features = ["zeroize_derive"] }
thiserror = { workspace = true }

[target.'cfg(target_os = "macos")'.dependencies]
security-framework = "2.11"

[target.'cfg(target_os = "linux")'.dependencies]
secret-service = "4"
zbus = "4"

# Phase 2: Windows Credential Manager support (not implemented yet)
# [target.'cfg(windows)'.dependencies]
# windows-credentials = "0.3"
```

**CLI crate dependencies:**
```toml
# apps/velor-cli/Cargo.toml
color-eyre = "0.6"  # CLI error presentation
rpassword = "7.3"   # Secure password prompts
atty = "0.2"        # TTY detection
velor-vault = { path = "../../crates/velor-vault" }
```

**Backend Trait (`backend.rs`):**
```rust
use secrecy::{Secret, SecretVec};
use std::path::Path;

/// Vault scope identifier
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VaultScope {
    Global,
    Project { git_root_hash: String },
}

impl VaultScope {
    pub fn account_name(&self) -> String {
        match self {
            Self::Global => "vault:global".to_string(),
            Self::Project { git_root_hash } => {
                format!("vault:project:{}", git_root_hash)
            }
        }
    }

    pub fn from_git_root(git_root: &Path) -> Self {
        let hash = project_scope_id(git_root);
        Self::Project { git_root_hash: hash }
    }
}

/// Backend for storing vault master keys (OS keyring only)
///
/// This trait is for OS-backed secret stores (macOS Keychain, Linux Secret Service).
/// Passphrase mode uses a different unlock path with KDF metadata from the vault file.
pub trait KeyringBackend: Send + Sync {
    /// Store master key in backend
    fn store_key(
        &self,
        scope: &VaultScope,
        key: &SecretVec<u8>,
    ) -> Result<()>;

    /// Load master key from backend
    fn load_key(
        &self,
        scope: &VaultScope,
    ) -> Result<SecretVec<u8>>;

    /// Delete master key from backend
    fn delete_key(&self, scope: &VaultScope) -> Result<()>;

    /// Backend name for error messages
    fn backend_name(&self) -> &'static str;
}

/// Vault unlock paths
pub enum VaultUnlock {
    /// Keyring backend provides the master key
    Keyring(Box<dyn KeyringBackend>),
    /// Passphrase mode - derives key from password + KDF metadata
    Passphrase { prompt: bool },
}
```

**Vault API (`vault.rs`):**
```rust
use secrecy::SecretString;
use std::collections::HashMap;
use std::path::Path;

pub struct Vault {
    /// Vault file path
    path: PathBuf,
    /// Scope for key lookup
    scope: VaultScope,
    /// Decrypted secrets (never logged)
    entries: HashMap<String, SecretString>,
    /// Backend for key storage
    backend: Box<dyn VaultKeyBackend>,
}

impl Vault {
    /// Create new vault with fresh master key
    pub async fn create(
        path: &Path,
        scope: VaultScope,
        backend: Box<dyn VaultKeyBackend>,
    ) -> Result<Self>;

    /// Load and decrypt existing vault
    pub async fn load(
        path: &Path,
        scope: VaultScope,
        backend: Box<dyn VaultKeyBackend>,
    ) -> Result<Self>;

    /// Save (encrypt and write atomically)
    pub async fn save(&self) -> Result<()>;

    /// Get secret value (returns reference to SecretString)
    pub fn get(&self, key: &str) -> Option<&SecretString>;

    /// Set secret value
    pub fn set(&mut self, key: String, value: SecretString);

    /// Remove secret
    pub fn unset(&mut self, key: &str) -> bool;

    /// List all keys
    pub fn keys(&self) -> Vec<String>;

    /// Re-encrypt under new master key
    pub async fn rotate_master_key(&mut self) -> Result<()>;

    /// Migrate to different backend
    pub async fn migrate_backend(
        &mut self,
        new_backend: Box<dyn VaultKeyBackend>,
    ) -> Result<()>;
}

/// Secrets resolved for an automation (fail-closed by type)
pub struct ResolvedSecrets {
    /// Secrets for injection (only declared keys)
    pub secrets: HashMap<String, SecretString>,
}

/// Resolve secrets for an automation (fail-closed)
///
/// Returns error if required secrets are missing or vault is unavailable.
/// Success guarantees all required secrets are present.
pub async fn resolve_automation_secrets(
    required: &[String],
    optional: &[String],
    work_dir: &Path,
) -> Result<ResolvedSecrets> {
    // Load project vault, then global vault
    // Merge with project taking precedence
    // Validate no duplicates across required/optional
    // Check required secrets are present
    // Return only declared keys
    // Return VaultError::RequiredSecretMissing if any required secret is missing
}
```

**Vault File Format with Metadata:**
```rust
use serde::{Serialize, Deserialize};

/// Backend kind stored in vault file
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BackendKind {
    Keyring,
    Passphrase,
}

/// KDF parameters for passphrase backend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KdfMetadata {
    pub algorithm: String,  // "argon2id"
    pub salt: Vec<u8>,      // 16 bytes
    pub time_cost: u32,
    pub memory_cost: u32,   // KiB
    pub parallelism: u32,
}

/// Vault file format with metadata
#[derive(Serialize, Deserialize)]
pub struct VaultFile {
    /// Format marker for corruption detection
    pub format: String,  // "velor-vault"

    /// Format version
    pub version: u8,

    /// How to unlock this vault
    pub backend_kind: BackendKind,

    /// XChaCha20 nonce (24 bytes)
    pub nonce: [u8; 24],

    /// Encrypted secrets JSON
    pub ciphertext: Vec<u8>,

    /// KDF parameters for passphrase backend
    pub kdf: Option<KdfMetadata>,
}
```

**Crypto Implementation (`crypto.rs`):**
```rust
use rand::RngCore;
use thiserror::Error;

const VAULT_FORMAT: &str = "velor-vault";
const VERSION: u8 = 1;
const NONCE_LEN: usize = 24;  // XChaCha20
const KEY_LEN: usize = 32;

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("Invalid vault format")]
    InvalidFormat,
    #[error("Unsupported vault version: {0}")]
    UnsupportedVersion(u8),
    #[error("Encryption failed")]
    EncryptionFailed,
    #[error("Decryption failed (wrong key?)")]
    DecryptionFailed,
    #[error("Backend kind mismatch: expected {expected}, found {found}")]
    BackendMismatch { expected: String, found: String },
}

/// Encrypt using XChaCha20-Poly1305
pub fn encrypt(
    plaintext: &[u8],
    master_key: &[u8; KEY_LEN],
) -> Result<(Vec<u8>, [u8; NONCE_LEN]), CryptoError> {
    // Implementation uses chacha20poly1305 crate with XChaCha20
    // ...
}

/// Decrypt using XChaCha20-Poly1305
pub fn decrypt(
    ciphertext: &[u8],
    nonce: &[u8; NONCE_LEN],
    master_key: &[u8; KEY_LEN],
) -> Result<Vec<u8>, CryptoError> {
    // Implementation uses chacha20poly1305 crate with XChaCha20
    // ...
}
```

**Typed VaultError (`error.rs`):**
```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("Vault not found at {0}")]
    VaultNotFound(std::path::PathBuf),

    #[error("Vault backend unavailable: {0}")]
    VaultBackendUnavailable(String),

    #[error("Vault decryption failed")]
    VaultDecryptFailed,

    #[error("Required secret missing: {key}")]
    RequiredSecretMissing { key: String },

    #[error("Invalid secret name '{key}': must match ^[A-Z][A-Z0-9_]*$")]
    InvalidSecretName { key: String },

    #[error("Duplicate secret declaration: '{key}' appears more than once")]
    DuplicateSecretDeclaration { key: String },

    #[error("Insecure permissions: {0}")]
    InsecurePermissions(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Crypto error: {0}")]
    Crypto(#[from] CryptoError),

    #[error("Keychain error: {0}")]
    Keychain(String),

    #[error("Passphrase error: {0}")]
    Passphrase(String),
}

pub type Result<T> = std::result::Result<T, VaultError>;
```

### Phase 2: CLI Integration

**File: `apps/velor-cli/src/vault.rs`**
```rust
use clap::Subcommand;

#[derive(Subcommand)]
pub enum VaultCommand {
    /// Initialize a new vault
    Init {
        /// Use global vault (~/.config/velor/vault.bin)
        #[arg(long)]
        global: bool,

        /// Key storage backend
        #[arg(long, default_value = "keyring")]
        backend: String,
    },

    /// Set a secret value (reads from stdin or prompts)
    Set {
        /// Secret key name (must be valid env var name)
        #[arg(value_name = "KEY")]
        key: String,

        /// Prompt securely without echo
        #[arg(long)]
        prompt: bool,

        /// Read value from environment variable
        #[arg(long, value_name = "VAR")]
        from_env: Option<String>,

        #[arg(long)]
        global: bool,
    },

    /// Get a secret value
    Get {
        /// Secret key to retrieve
        #[arg(value_name = "KEY")]
        key: String,

        /// Output raw value (for scripting/pipes only)
        ///
        /// When stdout is a TTY, requires --force to prevent accidental leakage.
        /// When stdout is a pipe, prints raw value without confirmation.
        #[arg(long)]
        raw: bool,

        /// Bypass TTY safety check for --raw
        #[arg(long)]
        force: bool,

        #[arg(long)]
        global: bool,
    },

    /// List all secret keys
    List {
        #[arg(long)]
        global: bool,
    },

    /// Remove a secret
    Unset {
        #[arg(value_name = "KEY")]
        key: String,

        #[arg(long)]
        global: bool,
    },

    /// Validate vault access
    Validate {
        #[arg(long)]
        global: bool,
    },

    /// Rotate master key (re-encrypts vault)
    RotateKey {
        #[arg(long)]
        global: bool,
    },

    /// Migrate to different backend
    MigrateBackend {
        /// Target backend
        #[arg(long)]
        to: String,

        #[arg(long)]
        global: bool,
    },
}
```

**Secret Input Handling (no shell history):**
```rust
pub async fn run_set(key: String, prompt: bool, from_env: Option<String>, global: bool) -> Result<()> {
    // Validate key name
    if !is_valid_env_var_name(&key) {
        return Err(VaultError::InvalidSecretName { key });
    }

    // Get value securely
    let value = if let Some(var) = from_env {
        // Explicit --from-env flag
        std::env::var(&var)?
    } else if prompt {
        // Explicit --prompt flag
        rpassword::prompt_password(&format!("Enter value for {}: ", key))?
    } else if !atty::is(atty::Stream::Stdin) {
        // Piped stdin (not a TTY)
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        strip_trailing_newline(buf)  // Only strip \n or \r\n, not all whitespace
    } else {
        // Interactive TTY without --prompt or pipe
        return Err(VaultError::InvalidInput(
            "Provide --prompt or pipe stdin".to_string()
        ));
    };

    // Load vault, set value, save
    // ...
}

/// Strip exactly one trailing newline (CRLF or LF)
fn strip_trailing_newline(mut s: String) -> String {
    if s.ends_with("\r\n") {
        s.truncate(s.len() - 2);
    } else if s.ends_with('\n') {
        s.truncate(s.len() - 1);
    }
    s
}
```

### Phase 3: Automation File Format

**Add to `AutomationFile` struct:**
```rust
// crates/velor-automations/src/file_config.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AutomationFile {
    // ... existing fields ...

    /// Required secrets for this automation
    #[serde(default)]
    pub required_secrets: Vec<String>,

    /// Optional secrets for this automation
    #[serde(default)]
    pub optional_secrets: Vec<String>,
}

impl AutomationFile {
    /// Validate secret declarations at load time
    pub fn validate_secrets(&self) -> color_eyre::Result<()> {
        velor_vault::validate_secret_declarations(
            &self.required_secrets,
            &self.optional_secrets,
        )
        .map_err(|e| color_eyre::eyre::eyre!("Invalid secret declarations: {}", e))?;
        Ok(())
    }
}
```
```

**Example automation file:**
```toml
# .velor/automations/glm-analysis.toml
description = "Run GLM analysis with API key"
schedule = "0 0 * * * *"
timezone = "UTC"

prompt = '''
Call the ZAI API using the ZAI_API_KEY environment variable.
Model configuration is managed via environment variables.
'''

# Secrets this automation needs
required_secrets = ["ZAI_API_KEY"]
optional_secrets = ["ZAI_MODEL_NAME"]

enabled = true
```

**Important:** Vault secrets are injected as environment variables to the subprocess only.
They are NOT available in MiniJinja templates during Phase 1. Template access is
deferred to Phase 2 pending security review.

### Phase 4: Automation Runner Integration

**Modify: `crates/automations/src/runner.rs`**

**Injection Point (both file-based and legacy):**
```rust
// Before spawning velor subprocess
use velor_vault::resolve_automation_secrets;

// Resolve secrets with fail-closed semantics
// Success guarantees all required secrets are present
let resolved = resolve_automation_secrets(
    &automation.required_secrets,
    &automation.optional_secrets,
    work_dir,
).await?;

// Build command
let mut cmd = Command::new(&self.velor_binary);
cmd.arg("once")
   .arg("--prompt")
   .arg(&prompt_content)
   .current_dir(work_dir)
   .stdout(Stdio::piped())
   .stderr(Stdio::piped());

// Inject ONLY declared secrets (SecretString stays secret until here)
for (key, secret) in resolved.secrets {
    cmd.env(key, secret.expose_secret());  // Single exposure point
}

let child = cmd.spawn()?;
```

**New Module: `crates/automations/src/vault.rs`**
```rust
use velor_vault::{Vault, VaultScope, resolve_automation_secrets};
use std::path::Path;

/// Get project scope ID from git root
pub fn project_scope_id(git_root: &Path) -> String {
    use sha2::{Sha256, Digest};
    let canonical = git_root
        .canonicalize()
        .unwrap_or_else(|_| git_root.to_path_buf());

    let hash = Sha256::digest(canonical.to_string_lossy().as_bytes());
    format!("{:x}", hash)[..16].to_string()
}

/// Get appropriate keyring backend for current platform
///
/// Returns error if platform's keyring is unavailable.
/// Use `vel vault init --backend passphrase` for manual mode.
pub fn default_keyring_backend() -> Result<Box<dyn velor_vault::KeyringBackend>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(velor_vault::MacOsKeychainBackend))
    }
    #[cfg(target_os = "linux")]
    {
        velor_vault::SecretServiceBackend::new()
            .map(|b| Box::new(b) as Box<dyn velor_vault::KeyringBackend>)
            .map_err(|e| VaultError::VaultBackendUnavailable(e.to_string()))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        // No keyring backend available
        Err(VaultError::VaultBackendUnavailable(
            "No OS keyring backend available on this platform".to_string()
        ))
    }
}
```

### Phase 5: Security Guardrails

**1. Never log secret values:**
```rust
impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vault")
            .field("path", &self.path)
            .field("scope", &self.scope)
            .field("entries", &format!("<{} keys>", self.entries.len()))
            .finish()
    }
}

// Never log secret values, only key names
tracing::info!("Loaded vault with {} keys", vault.keys().len());  // OK
tracing::debug!("Vault contents: {:?}", vault.entries);  // NEVER
```

**2. SecretString stays secret longer:**
```rust
// Keep as SecretString until the .env() call
pub struct ResolvedSecrets {
    pub secrets: HashMap<String, SecretString>,  // Not String
}

// Single exposure point at Command::env()
cmd.env(key, secret.expose_secret());
```

**3. Atomic writes with backup (best-effort):**
```rust
/// Save vault with atomic write and single backup
///
/// Backup is best-effort recovery, not journalling. Every save replaces
/// the previous backup. A corrupt save sequence after backup rotation
/// can still cause data loss - this is acceptable for Phase 1.
pub async fn save(&self) -> Result<()> {
    // Encrypt
    let (ciphertext, nonce) = encrypt(&self.to_json()?, &master_key)?;

    // Write to temp file in same directory with restrictive permissions
    let temp_path = self.path.with_extension("tmp");
    let vault_file = VaultFile {
        version: VERSION,
        backend_kind: self.backend_kind(),
        nonce,
        ciphertext,
        kdf: self.kdf_metadata(),
    };

    // Write with 0600 permissions
    let json = serde_json::to_vec(&vault_file)?;
    tokio::fs::write(&temp_path, json).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        tokio::fs::set_permissions(&temp_path, perms).await?;
    }

    // Keep one backup (rename, not copy, for atomicity)
    let backup_path = self.path.with_extension("bak");
    if self.path.exists() {
        let _ = tokio::fs::rename(&self.path, &backup_path).await;
    }

    // Atomic rename
    tokio::fs::rename(&temp_path, &self.path).await?;

    // Optionally fsync parent directory for stronger durability
    #[cfg(unix)]
    {
        if let Some(parent) = self.path.parent() {
            let file = std::fs::File::open(parent)?;
            file.sync_all()?;
        }
    }

    Ok(())
}
```

**4. Permission checks:**
```rust
pub async fn load(path: &Path, scope: VaultScope, backend: Box<dyn VaultKeyBackend>) -> Result<Self> {
    // Check file permissions (Unix only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = tokio::fs::metadata(path).await?;
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(VaultError::InsecurePermissions(format!(
                "Refusing to load vault with overly-permissive file mode: {:o}",
                mode
            )));
        }
    }

    // ... rest of load logic
}
```

**5. Secret name validation and duplicate check:**
```rust
pub fn is_valid_env_var_name(name: &str) -> bool {
    name.chars()
        .enumerate()
        .all(|(i, c)| {
            if i == 0 {
                c.is_ascii_uppercase()
            } else {
                c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'
            }
        })
        && !name.is_empty()
}

/// Validate secret declarations (called at automation load time)
pub fn validate_secret_declarations(
    required: &[String],
    optional: &[String],
) -> Result<()> {
    // Check for valid names
    for key in required.iter().chain(optional) {
        if !is_valid_env_var_name(key) {
            return Err(VaultError::InvalidSecretName { key: key.clone() });
        }
    }

    // Check for duplicates across required/optional
    let mut seen = std::collections::HashSet::new();
    for key in required.iter().chain(optional) {
        if !seen.insert(key) {
            return Err(VaultError::DuplicateSecretDeclaration {
                key: key.clone(),
            });
        }
    }

    Ok(())
}
```

**6. Advisory .gitignore check (heuristic):**
```rust
/// Check if vault file might be excluded from git (advisory heuristic)
///
/// This is a simple heuristic that checks for ignore patterns in common locations.
/// It is NOT authoritative - use `git check-ignore` for accurate results.
///
/// Checks:
/// - .gitignore in repo root
/// - .git/info/exclude
///
/// This function only warns, never modifies files automatically.
pub async fn check_gitignore(git_root: &Path) -> Result<()> {
    let velor_entry = ".velor/vault.bin";
    let mut warned = false;

    // Check .gitignore
    let gitignore = git_root.join(".gitignore");
    if gitignore.exists() {
        let content = tokio::fs::read_to_string(&gitignore).await?;
        if content.contains(velor_entry) || content.contains(".velor/") {
            return Ok(());  // Already covered
        }
    } else {
        eprintln!("⚠️  Warning: No .gitignore found");
        warned = true;
    }

    // Check .git/info/exclude
    let info_exclude = git_root.join(".git").join("info").join("exclude");
    if info_exclude.exists() {
        let content = tokio::fs::read_to_string(&info_exclude).await?;
        if content.contains(velor_entry) || content.contains(".velor/") {
            return Ok(());  // Already covered
        }
    }

    if !warned {
        eprintln!("⚠️  Warning: Vault file '{}' may not be excluded from git", velor_entry);
        eprintln!("   Consider adding to .gitignore:");
        eprintln!("   echo '{}' >> .gitignore", velor_entry);
    }

    Ok(())
}
```

## Critical Files to Create/Modify

### New Files
1. **`crates/velor-vault/src/lib.rs`** - Public API exports
2. **`crates/velor-vault/src/vault.rs`** - Vault struct and CRUD
3. **`crates/velor-vault/src/crypto.rs`** - AEAD encryption
4. **`crates/velor-vault/src/keyring.rs`** - KeyringBackend trait
5. **`crates/velor-vault/src/keychain.rs`** - macOS Keychain backend
6. **`crates/velor-vault/src/secret_service.rs`** - Linux Secret Service backend
7. **`crates/velor-vault/src/passphrase.rs`** - Passphrase unlock (manual CLI only)
8. **`crates/velor-vault/src/error.rs`** - Error types
9. **`apps/velor-cli/src/vault.rs`** - CLI commands
10. **`crates/automations/src/vault.rs`** - Automation integration

### Modified Files
1. **`crates/automations/src/file_config.rs`** - Add `required_secrets`/`optional_secrets` fields
2. **`crates/automations/src/runner.rs`** - Inject secrets at execution (lines ~747, ~822)
3. **`apps/velor-cli/src/main.rs`** - Add `vault` subcommand
4. **`Cargo.toml`** (workspace) - Add crypto dependencies

## Verification

### Manual Testing

```bash
# 1. Initialize global vault with keyring backend
vel vault init --global --backend keyring
# Should create ~/.config/velor/vault.bin + Keychain entry

# 2. Set a secret (pipe method, no shell history)
printf '%s' "sk-test123" | vel vault set ZAI_API_KEY --global

# 3. Set another secret (prompt method)
vel vault set OPENAI_API_KEY --prompt --global
# Enter value: ********

# 4. List secrets
vel vault list --global
# ZAI_API_KEY
# OPENAI_API_KEY

# 5. Get secret (masked)
vel vault get ZAI_API_KEY --global
# •••••••••••

# 6. Get secret (raw)
vel vault get ZAI_API_KEY --raw --global
# sk-test123

# 7. Create automation with required secrets
cat > .velor/automations/test.toml <<EOF
description = "Test vault"
schedule = "0 0 * * * *"
prompt = "API key is available"
required_secrets = ["ZAI_API_KEY"]
enabled = true
EOF

# 8. Run automation (should have ZAI_API_KEY in env)
vel automations run test

# 9. Test fail-closed (missing secret)
cat > .velor/automations/fail.toml <<EOF
description = "Should fail"
schedule = "0 0 * * * *"
prompt = "test"
required_secrets = ["NONEXISTENT_KEY"]
enabled = true
EOF

vel automations run fail
# Should fail immediately with "Required secret missing"

# 10. Install launchd and verify
vel automations install
tail -f ~/Library/Logs/velor/automations.log
```

### Security Testing

```bash
# 1. Verify vault is encrypted (binary format)
hexdump -C ~/.config/velor/vault.bin | head
# Should show non-ASCII, no "sk-test123"

# 2. Verify Keychain entry exists
security find-generic-password -s velor -a "vault:global"
# Should show entry but password is redacted by default

# 3. Verify secrets not in process listing
ps aux | grep -i zai
# Should NOT show API key

# 4. Test wrong passphrase (if using passphrase backend)
echo "wrong" | vel vault validate --backend passphrase --global
# Should fail with decryption error

# 5. Verify permission checks
chmod 0644 ~/.config/velor/vault.bin
vel vault list --global
# Should refuse to load

# 6. Test secret name validation
vel vault set "invalid-key" --global
# Should reject: must match ^[A-Z][A-Z0-9_]*$
```

## Dependencies Summary

```toml
# Workspace Cargo.toml additions
[workspace.dependencies]
chacha20poly1305 = "0.10"
rand = "0.8"
sha2 = "0.10"
argon2 = "0.5"
zeroize = { version = "1", features = ["zeroize_derive"] }
rpassword = "7.3"
atty = "0.2"

# crates/velor-vault/Cargo.toml (new crate)
[dependencies]
color-eyre = { workspace = true }
secrecy = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
chacha20poly1305 = { workspace = true }
rand = { workspace = true }

[target.'cfg(target_os = "macos")'.dependencies]
security-framework = "2.11"

[target.'cfg(target_os = "linux")'.dependencies]
secret-service = "4"
zbus = "4"

# apps/velor-cli/Cargo.toml additions
[dependencies]
velor-vault = { path = "../../crates/velor-vault" }
rpassword = { workspace = true }
atty = { workspace = true }
```

## Documentation Updates

**`docs/vault.md`:**
- Getting started guide
- CLI reference
- Security model
- Backend options (keyring vs passphrase)
- Fail-closed semantics
- Secret declaration in automations
- Troubleshooting

**`README.md`:**
- Quick start with `ZAI_API_KEY` example
- Link to full vault documentation

## Phase 2+ Future Enhancements

**Omitted from Phase 1:**
1. **`vel vault edit`** - Add with strict temp file handling
2. **Template variable access** - Expose secrets to MiniJinja (security review needed)
3. **Per-automation vaults** - Dedicated vault files per automation
4. **Secret rotation automation** - `vel vault rotate KEY_NAME`
5. **Audit trail** - Log secret access timestamps

## Key Differences from Original Plan

| Aspect | Original | Revised |
|--------|----------|---------|
| Crypto | Custom envelope (age + aes-gcm) | Simple AEAD (XChaCha20-Poly1305) |
| Key storage | .env fallback | OS secret store only |
| Project identity | Mutable name | SHA-256 hash of git root |
| Vault failures | Silent continue | Fail-closed |
| Secret injection | All secrets | Declared secrets only |
| SecretString | Exposed early | Exposed only at .env() |
| edit command | Phase 1 | Phase 2 (risky) |
| CLI set | KEY=VALUE arg | stdin/prompt (no history) |
| Backend | Platform branching | Trait-based |
