# Velor Vault: Encrypted Secrets Management

Velor Vault provides secure storage for secrets (API keys, tokens, passwords) used by automations. Secrets are encrypted at rest using XChaCha20-Poly1305 with the master key stored in your OS-backed secret store.

## Quick Start

```bash
# 1. Initialize the global vault (uses macOS Keychain / Linux Secret Service by default)
vel vault init --global

# 2. Store an API key (pipe method - no shell history)
printf '%s' "sk-your-api-key" | vel vault set OPENAI_API_KEY --global

# 3. Use in an automation
cat > .velor/automations/my-automation.toml <<EOF
description = "Call API securely"
schedule = "0 0 * * * *"
prompt = "Use the OPENAI_API_KEY environment variable to call the API"
required_secrets = ["OPENAI_API_KEY"]
enabled = true
EOF

# 4. Run the automation - secret is automatically injected
vel automations run my-automation
```

## Storage Locations

| Scope | Vault File | Keychain Account |
|-------|-----------|------------------|
| Global | `~/.config/velor/vault.bin` | `vault:global` |
| Project | `{git_root}/.velor/vault.bin` | `vault:project:<hash>` |

Project vaults are identified by a SHA-256 hash of the git root path, so they remain stable even if you rename the project directory.

## CLI Reference

### `vel vault init [--global] [--backend keyring|passphrase]`

Initialize a new vault.

- `--global`: Create global vault (shared across all projects)
- `--backend keyring`: Use OS secret store (default, recommended)
- `--backend passphrase`: Use passphrase-based encryption (manual CLI only)

```bash
# Recommended: OS keyring backend (unattended automations)
vel vault init --global

# Alternative: Passphrase backend (interactive use only)
vel vault init --global --backend passphrase
```

### `vel vault set KEY [--global] [--prompt | --from-env VAR]`

Store a secret value.

**Security:** Never use command-line arguments for values (shell history leakage). Use one of:

```bash
# Method 1: Pipe (recommended for scripts)
printf '%s' "$MY_API_KEY" | vel vault set MY_API_KEY --global

# Method 2: Secure prompt
vel vault set MY_API_KEY --prompt --global

# Method 3: From environment variable
vel vault set MY_API_KEY --from-env MY_API_KEY --global
```

### `vel vault get KEY [--global] [--raw] [--force]`

Retrieve a secret value.

```bash
# Masked display (default)
vel vault get MY_API_KEY --global
# Output: •••••••••••

# Raw output for scripting (pipes only)
vel vault get MY_API_KEY --raw --global | head -c 8

# Raw output to TTY (requires --force to prevent accidental leakage)
vel vault get MY_API_KEY --raw --force --global
```

### `vel vault list [--global]`

List all secret keys (values never shown).

```bash
vel vault list --global
# Output:
# OPENAI_API_KEY
# ANTHROPIC_API_KEY
# DATABASE_URL
```

### `vel vault unset KEY [--global]`

Remove a secret.

```bash
vel vault unset MY_API_KEY --global
```

### `vel vault validate [--global]`

Verify vault access and integrity.

```bash
vel vault validate --global
# Output: Vault is valid and accessible
```

### `vel vault rotate-key [--global]`

Rotate the master encryption key.

- **Keyring backend:** Generates new random key
- **Passphrase backend:** Prompts for new passphrase

```bash
vel vault rotate-key --global
```

### `vel vault migrate-backend --to keyring|passphrase [--global]`

Migrate between backend types.

```bash
# Migrate from passphrase to OS keyring
vel vault migrate-backend --to keyring --global

# Migrate from OS keyring to passphrase
vel vault migrate-backend --to passphrase --global
```

## Automation Integration

### Declaring Secrets

Automations must explicitly declare which secrets they need:

```toml
# .velor/automations/api-call.toml
description = "Call external API"
schedule = "0 */6 * * *"
prompt = "Use the API keys from environment variables"

# Required secrets - automation fails if missing
required_secrets = ["OPENAI_API_KEY"]

# Optional secrets - injected if available, ignored if not
optional_secrets = ["OPENAI_ORG_ID", "OPENAI_MODEL_NAME"]

enabled = true
```

### Precedence Rules

1. **Project vault values override global vault values**
2. Required secrets must be present or automation fails immediately
3. Optional secrets may be missing (not an error)
4. Only declared keys are injected (no secret leakage)

### Fail-Closed Semantics

The vault uses fail-closed semantics - if anything goes wrong, the automation does not run:

| Condition | Result |
|-----------|--------|
| Required secret missing | Automation fails immediately |
| Vault unavailable | Automation fails immediately |
| Decryption failed | Automation fails immediately |
| Optional secret missing | Automation runs without it |

## Security Model

### Encryption

- **Algorithm:** XChaCha20-Poly1305 (AEAD)
- **Master Key:** 256-bit random, stored in OS secret store
- **KDF (passphrase mode):** Argon2id (time=2, mem=64MiB, parallelism=4)
- **Nonce:** 192-bit random per encryption

### Backend Comparison

| Backend | Automations | Security | Convenience |
|---------|-------------|----------|-------------|
| **Keyring** (default) | ✅ Full support | High (OS-protected) | High |
| **Passphrase** | ❌ Manual CLI only | High (if passphrase strong) | Low |

**Recommendation:** Use the keyring backend (default) for automations. Only use passphrase mode if OS keyring is unavailable.

### Security Features

1. **No shell history leakage:** Values read from stdin or prompt only
2. **TTY safety:** `--raw` requires `--force` when outputting to terminal
3. **Permission checks:** Vault files must have restrictive permissions (0o600)
4. **Atomic writes:** Vault saves use atomic rename with backup
5. **Advisory .gitignore check:** Warns if vault file may be committed

### Secret Name Validation

Secret names must be valid environment variable names:
- Start with uppercase letter
- Contain only uppercase letters, digits, and underscores
- Match pattern: `^[A-Z][A-Z0-9_]*$`

```bash
# Valid
vel vault set MY_API_KEY --global
vel vault set DATABASE_URL --global

# Invalid
vel vault set my-api-key --global  # Error: lowercase and hyphens
vel vault set 123_KEY --global      # Error: starts with digit
```

## Troubleshooting

### "Vault not found"

Initialize the vault first:

```bash
vel vault init --global
```

### "Required secret missing"

Add the secret to your vault:

```bash
printf '%s' "your-api-key" | vel vault set MISSING_KEY --global
```

### "Vault decryption failed"

For keyring backend: The master key in your OS keychain may have been deleted.

For passphrase backend: You entered the wrong passphrase.

### "Insecure permissions"

The vault file has overly permissive permissions:

```bash
chmod 600 ~/.config/velor/vault.bin
```

### "Vault file may not be excluded from git"

Add the vault file to `.gitignore`:

```bash
echo ".velor/vault.bin" >> .gitignore
```

### macOS Keychain Issues

To verify the keychain entry exists:

```bash
security find-generic-password -s velor -a "vault:global"
```

To delete and re-initialize:

```bash
security delete-generic-password -s velor -a "vault:global"
vel vault init --global
```

### Linux Secret Service Issues

Ensure you have a secret service daemon running (e.g., GNOME Keyring, KDE Wallet).

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│ Vault File (.velor/vault.bin)                               │
│ ├── version: u8                                             │
│ ├── nonce: [u8; 24]                                         │
│ └── ciphertext: Vec<u8> (AEAD-encrypted secrets JSON)       │
└─────────────────────────────────────────────────────────────┘
                              │
                              │ decrypt with master key from
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ OS Secret Store                                             │
│ • macOS: Keychain (service=velor, account=vault:<scope>)    │
│ • Linux: Secret Service (velor/vault/<scope>)               │
│ Fallback: Interactive passphrase (ARGON2ID-derived)         │
└─────────────────────────────────────────────────────────────┘
```

## Files

| File | Purpose |
|------|---------|
| `crates/velor-vault/` | Core vault library |
| `apps/velor-cli/src/vault.rs` | CLI commands |
| `crates/automations/src/runner.rs` | Automation integration |
| `crates/automations/src/file_config.rs` | Secret declarations |
