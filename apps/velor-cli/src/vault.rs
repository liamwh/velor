//! Vault CLI commands for managing encrypted secrets.
//!
//! This module provides command-line interface for the Velor Vault system,
//! which stores secrets encrypted at rest using XChaCha20-Poly1305 with
//! the master key stored in an OS-backed secret store.

use clap::{Args, Subcommand};
use color_eyre::eyre::{self, Context, bail};
use secrecy::{ExposeSecret, SecretString};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use velor_vault::{BackendKind, Vault, VaultScope, default_keyring_backend, is_valid_secret_name};

/// Vault command arguments.
#[derive(Debug, Args)]
pub struct VaultArgs {
    #[command(subcommand)]
    pub command: VaultCommand,
}

/// Vault subcommands.
#[derive(Debug, Subcommand)]
pub enum VaultCommand {
    /// Initialize a new vault.
    Init {
        /// Use global vault (~/.config/velor/vault.bin).
        #[arg(long)]
        global: bool,

        /// Key storage backend (keyring or passphrase).
        #[arg(long, default_value = "keyring")]
        backend: String,
    },

    /// Set a secret value (reads from stdin or prompts).
    Set {
        /// Secret key name (must be valid env var name).
        #[arg(value_name = "KEY")]
        key: String,

        /// Prompt securely without echo.
        #[arg(long)]
        prompt: bool,

        /// Read value from environment variable.
        #[arg(long, value_name = "VAR")]
        from_env: Option<String>,

        /// Use global vault.
        #[arg(long)]
        global: bool,
    },

    /// Get a secret value.
    Get {
        /// Secret key to retrieve.
        #[arg(value_name = "KEY")]
        key: String,

        /// Output raw value (for scripting/pipes only).
        ///
        /// When stdout is a TTY, requires --force to prevent accidental leakage.
        /// When stdout is a pipe, prints raw value without confirmation.
        #[arg(long)]
        raw: bool,

        /// Bypass TTY safety check for --raw.
        #[arg(long)]
        force: bool,

        /// Use global vault.
        #[arg(long)]
        global: bool,
    },

    /// List all secret keys.
    List {
        /// Use global vault.
        #[arg(long)]
        global: bool,
    },

    /// Remove a secret.
    Unset {
        /// Secret key to remove.
        #[arg(value_name = "KEY")]
        key: String,

        /// Use global vault.
        #[arg(long)]
        global: bool,
    },

    /// Validate vault access.
    Validate {
        /// Use global vault.
        #[arg(long)]
        global: bool,
    },

    /// Rotate master key (re-encrypts vault).
    RotateKey {
        /// Use global vault.
        #[arg(long)]
        global: bool,
    },

    /// Migrate to different backend.
    MigrateBackend {
        /// Target backend (keyring or passphrase).
        #[arg(long)]
        to: String,

        /// Use global vault.
        #[arg(long)]
        global: bool,
    },
}

/// Run a vault command.
pub async fn run(cmd: VaultCommand, git_root: Option<PathBuf>) -> eyre::Result<()> {
    match cmd {
        VaultCommand::Init { global, backend } => run_init(global, backend, git_root).await,
        VaultCommand::Set {
            key,
            prompt,
            from_env,
            global,
        } => run_set(key, prompt, from_env, global, git_root).await,
        VaultCommand::Get {
            key,
            raw,
            force,
            global,
        } => run_get(key, raw, force, global, git_root).await,
        VaultCommand::List { global } => run_list(global, git_root).await,
        VaultCommand::Unset { key, global } => run_unset(key, global, git_root).await,
        VaultCommand::Validate { global } => run_validate(global, git_root).await,
        VaultCommand::RotateKey { global } => run_rotate_key(global, git_root).await,
        VaultCommand::MigrateBackend { to, global } => {
            run_migrate_backend(to, global, git_root).await
        }
    }
}

/// Get vault path based on scope.
fn get_vault_path(global: bool, git_root: Option<PathBuf>) -> PathBuf {
    if global {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("velor")
            .join("vault.bin")
    } else {
        git_root
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".velor")
            .join("vault.bin")
    }
}

/// Get vault scope based on scope.
fn get_vault_scope(global: bool, git_root: Option<&PathBuf>) -> VaultScope {
    if global {
        VaultScope::Global
    } else {
        git_root
            .as_ref()
            .map(|p| VaultScope::from_git_root(p.as_path()))
            .unwrap_or(VaultScope::Global)
    }
}

/// Prompt for a passphrase securely.
fn prompt_passphrase(prompt: &str) -> eyre::Result<SecretString> {
    let password = rpassword::prompt_password(prompt).wrap_err("Failed to read passphrase")?;
    Ok(SecretString::new(password))
}

/// Prompt for a passphrase with confirmation.
fn prompt_passphrase_confirm(prompt: &str, confirm_prompt: &str) -> eyre::Result<SecretString> {
    let password = rpassword::prompt_password(prompt).wrap_err("Failed to read passphrase")?;
    let confirm =
        rpassword::prompt_password(confirm_prompt).wrap_err("Failed to read confirmation")?;

    if password != confirm {
        bail!("Passphrases do not match");
    }

    Ok(SecretString::new(password))
}

/// Strip exactly one trailing newline (CRLF or LF).
fn strip_trailing_newline(mut s: String) -> String {
    if s.ends_with("\r\n") {
        s.truncate(s.len() - 2);
    } else if s.ends_with('\n') {
        s.truncate(s.len() - 1);
    }
    s
}

/// Run vault init command.
async fn run_init(global: bool, backend: String, git_root: Option<PathBuf>) -> eyre::Result<()> {
    let path = get_vault_path(global, git_root.clone());
    let scope = get_vault_scope(global, git_root.as_ref());

    // Check if vault already exists
    if path.exists() {
        bail!("Vault already exists at {}", path.display());
    }

    // Parse backend
    let backend_kind = match backend.to_lowercase().as_str() {
        "keyring" => BackendKind::Keyring,
        "passphrase" => BackendKind::Passphrase,
        _ => bail!(
            "Invalid backend '{}'. Use 'keyring' or 'passphrase'.",
            backend
        ),
    };

    // Create vault based on backend
    match backend_kind {
        BackendKind::Keyring => {
            let keyring_backend = default_keyring_backend()
                .map_err(|e| eyre::eyre!("Failed to get keyring backend: {}", e))?;
            Vault::create_keyring(&path, scope, keyring_backend)
                .await
                .wrap_err("Failed to create vault")?;
        }
        BackendKind::Passphrase => {
            let passphrase =
                prompt_passphrase_confirm("Enter new passphrase: ", "Confirm passphrase: ")?;
            Vault::create_passphrase(&path, scope, &passphrase)
                .await
                .wrap_err("Failed to create vault")?;
        }
    }

    println!("Created vault at {}", path.display());

    // Check gitignore for project vaults
    if !global && let Some(root) = git_root {
        check_gitignore(&root).await;
    }

    Ok(())
}

/// Run vault set command.
async fn run_set(
    key: String,
    prompt: bool,
    from_env: Option<String>,
    global: bool,
    git_root: Option<PathBuf>,
) -> eyre::Result<()> {
    // Validate key name
    if !is_valid_secret_name(&key) {
        bail!(
            "Invalid secret name '{}'. Must match ^[A-Z][A-Z0-9_]*$",
            key
        );
    }

    let path = get_vault_path(global, git_root.clone());
    let scope = get_vault_scope(global, git_root.as_ref());

    // Check vault exists
    if !path.exists() {
        bail!(
            "Vault not found at {}. Run 'vel vault init' first.",
            path.display()
        );
    }

    // Get value securely
    let value = if let Some(var) = from_env {
        // Explicit --from-env flag
        std::env::var(&var).wrap_err(format!("Environment variable '{}' not found", var))?
    } else if prompt {
        // Explicit --prompt flag
        rpassword::prompt_password(format!("Enter value for {}: ", key))
            .wrap_err("Failed to read value")?
    } else if !atty::is(atty::Stream::Stdin) {
        // Piped stdin (not a TTY)
        let mut buf = String::new();
        io::stdin()
            .read_to_string(&mut buf)
            .wrap_err("Failed to read from stdin")?;
        strip_trailing_newline(buf)
    } else {
        // Interactive TTY without --prompt or pipe
        bail!("Provide --prompt or pipe stdin (e.g., printf '%s' \"$VALUE\" | vel vault set KEY)");
    };

    // Detect backend and load vault
    let vault_file = tokio::fs::read_to_string(&path)
        .await
        .wrap_err("Failed to read vault file")?;
    let parsed: serde_json::Value =
        serde_json::from_str(&vault_file).wrap_err("Failed to parse vault file")?;
    let backend_kind = parsed
        .get("backend_kind")
        .and_then(|v| v.as_str())
        .unwrap_or("keyring");

    let mut vault = match backend_kind {
        "keyring" => {
            let keyring_backend = default_keyring_backend()
                .map_err(|e| eyre::eyre!("Failed to get keyring backend: {}", e))?;
            Vault::load_keyring(&path, scope, keyring_backend)
                .await
                .wrap_err("Failed to load vault")?
        }
        "passphrase" => {
            let passphrase = prompt_passphrase("Enter passphrase: ")?;
            Vault::load_passphrase(&path, scope, &passphrase)
                .await
                .wrap_err("Failed to load vault (wrong passphrase?)")?
        }
        _ => bail!("Unknown backend kind: {}", backend_kind),
    };

    // Set value
    vault.set(key.clone(), SecretString::new(value));

    // Save vault
    match vault.backend_kind() {
        BackendKind::Keyring => {
            vault.save().await.wrap_err("Failed to save vault")?;
        }
        BackendKind::Passphrase => {
            let passphrase = prompt_passphrase("Enter passphrase to save: ")?;
            vault
                .save_with_passphrase(&passphrase)
                .await
                .wrap_err("Failed to save vault")?;
        }
    }

    println!("Set secret '{}'", key);

    Ok(())
}

/// Run vault get command.
async fn run_get(
    key: String,
    raw: bool,
    force: bool,
    global: bool,
    git_root: Option<PathBuf>,
) -> eyre::Result<()> {
    let path = get_vault_path(global, git_root.clone());
    let scope = get_vault_scope(global, git_root.as_ref());

    // Check vault exists
    if !path.exists() {
        bail!("Vault not found at {}", path.display());
    }

    // Detect backend and load vault
    let vault_file = tokio::fs::read_to_string(&path)
        .await
        .wrap_err("Failed to read vault file")?;
    let parsed: serde_json::Value =
        serde_json::from_str(&vault_file).wrap_err("Failed to parse vault file")?;
    let backend_kind = parsed
        .get("backend_kind")
        .and_then(|v| v.as_str())
        .unwrap_or("keyring");

    let vault = match backend_kind {
        "keyring" => {
            let keyring_backend = default_keyring_backend()
                .map_err(|e| eyre::eyre!("Failed to get keyring backend: {}", e))?;
            Vault::load_keyring(&path, scope, keyring_backend)
                .await
                .wrap_err("Failed to load vault")?
        }
        "passphrase" => {
            let passphrase = prompt_passphrase("Enter passphrase: ")?;
            Vault::load_passphrase(&path, scope, &passphrase)
                .await
                .wrap_err("Failed to load vault (wrong passphrase?)")?
        }
        _ => bail!("Unknown backend kind: {}", backend_kind),
    };

    // Get value
    let value = vault
        .get(&key)
        .ok_or_else(|| eyre::eyre!("Secret '{}' not found", key))?;

    if raw {
        // Check TTY safety
        if atty::is(atty::Stream::Stdout) && !force {
            bail!(
                "Refusing to print raw secret to terminal. Use --force to override or pipe output."
            );
        }
        print!("{}", value.expose_secret());
    } else {
        // Masked output
        let masked = mask_secret(value.expose_secret());
        println!("{}", masked);
    }

    Ok(())
}

/// Mask a secret value for display.
fn mask_secret(value: &str) -> String {
    let len = value.len();
    if len == 0 {
        String::new()
    } else if len <= 4 {
        "•".repeat(len)
    } else {
        // Show first 2 and last 2 chars, mask the rest
        format!(
            "{}••{}{}",
            &value[..2],
            "•".repeat(len.saturating_sub(4)),
            &value[len - 2..]
        )
    }
}

/// Run vault list command.
async fn run_list(global: bool, git_root: Option<PathBuf>) -> eyre::Result<()> {
    let path = get_vault_path(global, git_root.clone());
    let scope = get_vault_scope(global, git_root.as_ref());

    // Check vault exists
    if !path.exists() {
        bail!("Vault not found at {}", path.display());
    }

    // Detect backend and load vault
    let vault_file = tokio::fs::read_to_string(&path)
        .await
        .wrap_err("Failed to read vault file")?;
    let parsed: serde_json::Value =
        serde_json::from_str(&vault_file).wrap_err("Failed to parse vault file")?;
    let backend_kind = parsed
        .get("backend_kind")
        .and_then(|v| v.as_str())
        .unwrap_or("keyring");

    let vault = match backend_kind {
        "keyring" => {
            let keyring_backend = default_keyring_backend()
                .map_err(|e| eyre::eyre!("Failed to get keyring backend: {}", e))?;
            Vault::load_keyring(&path, scope, keyring_backend)
                .await
                .wrap_err("Failed to load vault")?
        }
        "passphrase" => {
            let passphrase = prompt_passphrase("Enter passphrase: ")?;
            Vault::load_passphrase(&path, scope, &passphrase)
                .await
                .wrap_err("Failed to load vault (wrong passphrase?)")?
        }
        _ => bail!("Unknown backend kind: {}", backend_kind),
    };

    let keys = vault.keys();
    if keys.is_empty() {
        println!("No secrets stored.");
    } else {
        for key in keys {
            println!("{}", key);
        }
    }

    Ok(())
}

/// Run vault unset command.
async fn run_unset(key: String, global: bool, git_root: Option<PathBuf>) -> eyre::Result<()> {
    let path = get_vault_path(global, git_root.clone());
    let scope = get_vault_scope(global, git_root.as_ref());

    // Check vault exists
    if !path.exists() {
        bail!("Vault not found at {}", path.display());
    }

    // Detect backend and load vault
    let vault_file = tokio::fs::read_to_string(&path)
        .await
        .wrap_err("Failed to read vault file")?;
    let parsed: serde_json::Value =
        serde_json::from_str(&vault_file).wrap_err("Failed to parse vault file")?;
    let backend_kind = parsed
        .get("backend_kind")
        .and_then(|v| v.as_str())
        .unwrap_or("keyring");

    let mut vault = match backend_kind {
        "keyring" => {
            let keyring_backend = default_keyring_backend()
                .map_err(|e| eyre::eyre!("Failed to get keyring backend: {}", e))?;
            Vault::load_keyring(&path, scope, keyring_backend)
                .await
                .wrap_err("Failed to load vault")?
        }
        "passphrase" => {
            let passphrase = prompt_passphrase("Enter passphrase: ")?;
            Vault::load_passphrase(&path, scope, &passphrase)
                .await
                .wrap_err("Failed to load vault (wrong passphrase?)")?
        }
        _ => bail!("Unknown backend kind: {}", backend_kind),
    };

    // Unset value
    if vault.unset(&key) {
        // Save vault
        match vault.backend_kind() {
            BackendKind::Keyring => {
                vault.save().await.wrap_err("Failed to save vault")?;
            }
            BackendKind::Passphrase => {
                let passphrase = prompt_passphrase("Enter passphrase to save: ")?;
                vault
                    .save_with_passphrase(&passphrase)
                    .await
                    .wrap_err("Failed to save vault")?;
            }
        }
        println!("Removed secret '{}'", key);
    } else {
        println!("Secret '{}' not found", key);
    }

    Ok(())
}

/// Run vault validate command.
async fn run_validate(global: bool, git_root: Option<PathBuf>) -> eyre::Result<()> {
    let path = get_vault_path(global, git_root.clone());
    let scope = get_vault_scope(global, git_root.as_ref());

    // Check vault exists
    if !path.exists() {
        bail!("Vault not found at {}", path.display());
    }

    // Detect backend and load vault
    let vault_file = tokio::fs::read_to_string(&path)
        .await
        .wrap_err("Failed to read vault file")?;
    let parsed: serde_json::Value =
        serde_json::from_str(&vault_file).wrap_err("Failed to parse vault file")?;
    let backend_kind = parsed
        .get("backend_kind")
        .and_then(|v| v.as_str())
        .unwrap_or("keyring");

    let vault = match backend_kind {
        "keyring" => {
            let keyring_backend = default_keyring_backend()
                .map_err(|e| eyre::eyre!("Failed to get keyring backend: {}", e))?;
            Vault::load_keyring(&path, scope, keyring_backend)
                .await
                .wrap_err("Failed to load vault")?
        }
        "passphrase" => {
            let passphrase = prompt_passphrase("Enter passphrase: ")?;
            Vault::load_passphrase(&path, scope, &passphrase)
                .await
                .wrap_err("Failed to load vault (wrong passphrase?)")?
        }
        _ => bail!("Unknown backend kind: {}", backend_kind),
    };

    println!("✓ Vault valid at {}", path.display());
    println!("  Backend: {}", vault.backend_kind());
    println!("  Secrets: {}", vault.len());

    Ok(())
}

/// Run vault rotate-key command.
async fn run_rotate_key(global: bool, git_root: Option<PathBuf>) -> eyre::Result<()> {
    let path = get_vault_path(global, git_root.clone());
    let scope = get_vault_scope(global, git_root.as_ref());

    // Check vault exists
    if !path.exists() {
        bail!("Vault not found at {}", path.display());
    }

    // Detect backend and load vault
    let vault_file = tokio::fs::read_to_string(&path)
        .await
        .wrap_err("Failed to read vault file")?;
    let parsed: serde_json::Value =
        serde_json::from_str(&vault_file).wrap_err("Failed to parse vault file")?;
    let backend_kind = parsed
        .get("backend_kind")
        .and_then(|v| v.as_str())
        .unwrap_or("keyring");

    let mut vault = match backend_kind {
        "keyring" => {
            let keyring_backend = default_keyring_backend()
                .map_err(|e| eyre::eyre!("Failed to get keyring backend: {}", e))?;
            Vault::load_keyring(&path, scope, keyring_backend)
                .await
                .wrap_err("Failed to load vault")?
        }
        "passphrase" => {
            let passphrase = prompt_passphrase("Enter current passphrase: ")?;
            Vault::load_passphrase(&path, scope, &passphrase)
                .await
                .wrap_err("Failed to load vault (wrong passphrase?)")?
        }
        _ => bail!("Unknown backend kind: {}", backend_kind),
    };

    // Rotate key
    match vault.backend_kind() {
        BackendKind::Keyring => {
            vault
                .rotate_master_key()
                .await
                .wrap_err("Failed to rotate key")?;
        }
        BackendKind::Passphrase => {
            let new_passphrase =
                prompt_passphrase_confirm("Enter new passphrase: ", "Confirm new passphrase: ")?;
            vault
                .rotate_passphrase(&new_passphrase)
                .await
                .wrap_err("Failed to rotate passphrase")?;
        }
    }

    println!("Rotated master key for vault at {}", path.display());

    Ok(())
}

/// Run vault migrate-backend command.
async fn run_migrate_backend(
    to: String,
    global: bool,
    git_root: Option<PathBuf>,
) -> eyre::Result<()> {
    let path = get_vault_path(global, git_root.clone());
    let scope = get_vault_scope(global, git_root.as_ref());

    // Check vault exists
    if !path.exists() {
        bail!("Vault not found at {}", path.display());
    }

    // Parse target backend
    let target_backend = match to.to_lowercase().as_str() {
        "keyring" => BackendKind::Keyring,
        "passphrase" => BackendKind::Passphrase,
        _ => bail!("Invalid backend '{}'. Use 'keyring' or 'passphrase'.", to),
    };

    // Detect current backend and load vault
    let vault_file = tokio::fs::read_to_string(&path)
        .await
        .wrap_err("Failed to read vault file")?;
    let parsed: serde_json::Value =
        serde_json::from_str(&vault_file).wrap_err("Failed to parse vault file")?;
    let current_backend = parsed
        .get("backend_kind")
        .and_then(|v| v.as_str())
        .unwrap_or("keyring");

    // Check if already on target backend
    if current_backend == to.to_lowercase() {
        println!("Vault already using {} backend.", to);
        return Ok(());
    }

    let vault = match current_backend {
        "keyring" => {
            let keyring_backend = default_keyring_backend()
                .map_err(|e| eyre::eyre!("Failed to get keyring backend: {}", e))?;
            Vault::load_keyring(&path, scope.clone(), keyring_backend)
                .await
                .wrap_err("Failed to load vault")?
        }
        "passphrase" => {
            let passphrase = prompt_passphrase("Enter current passphrase: ")?;
            Vault::load_passphrase(&path, scope.clone(), &passphrase)
                .await
                .wrap_err("Failed to load vault (wrong passphrase?)")?
        }
        _ => bail!("Unknown backend kind: {}", current_backend),
    };

    // Migrate
    match target_backend {
        BackendKind::Keyring => {
            let keyring_backend = default_keyring_backend()
                .map_err(|e| eyre::eyre!("Failed to get keyring backend: {}", e))?;
            vault
                .migrate_to_keyring(keyring_backend)
                .await
                .wrap_err("Failed to migrate")?;
        }
        BackendKind::Passphrase => {
            let passphrase = prompt_passphrase_confirm(
                "Enter new passphrase for vault: ",
                "Confirm passphrase: ",
            )?;
            vault
                .migrate_to_passphrase(&passphrase)
                .await
                .wrap_err("Failed to migrate")?;
        }
    }

    println!("Migrated vault at {} to {} backend", path.display(), to);

    Ok(())
}

/// Check if vault file might be excluded from git (advisory heuristic).
async fn check_gitignore(git_root: &Path) {
    let velor_entry = ".velor/vault.bin";

    // Check .gitignore
    let gitignore = git_root.join(".gitignore");
    if gitignore.exists()
        && let Ok(content) = tokio::fs::read_to_string(&gitignore).await
        && (content.contains(velor_entry) || content.contains(".velor/"))
    {
        return; // Already covered
    }

    // Check .git/info/exclude
    let info_exclude = git_root.join(".git").join("info").join("exclude");
    if info_exclude.exists()
        && let Ok(content) = tokio::fs::read_to_string(&info_exclude).await
        && (content.contains(velor_entry) || content.contains(".velor/"))
    {
        return; // Already covered
    }

    // Warn
    eprintln!(
        "⚠️  Warning: Vault file '{}' may not be excluded from git",
        velor_entry
    );
    eprintln!("   Consider adding to .gitignore:");
    eprintln!("   echo '{}' >> .gitignore", velor_entry);
}
