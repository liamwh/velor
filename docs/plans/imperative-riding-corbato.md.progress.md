# Velor Vault: Progress Handoff

## What Changed

### Phase 1: Core Vault Library (`crates/velor-vault`) - COMPLETED

Committed in 690d394. Created the complete vault library with all components:

**Files Created:**
- `crates/velor-vault/Cargo.toml` - Crate manifest with crypto dependencies
- `crates/velor-vault/src/lib.rs` - Public API exports, secret validation, `resolve_automation_secrets()`
- `crates/velor-vault/src/error.rs` - `VaultError` enum with all error variants
- `crates/velor-vault/src/crypto.rs` - XChaCha20-Poly1305 encryption with `MasterKey` and `Nonce` types
- `crates/velor-vault/src/keyring.rs` - `VaultScope`, `BackendKind`, `KeyringBackend` trait
- `crates/velor-vault/src/keychain.rs` - macOS Keychain backend (conditional compilation)
- `crates/velor-vault/src/secret_service.rs` - Linux Secret Service backend (conditional compilation)
- `crates/velor-vault/src/passphrase.rs` - Argon2id key derivation with `KdfMetadata`
- `crates/velor-vault/src/vault.rs` - `Vault` struct with CRUD, save, load, rotate, migrate operations

**Test Coverage:**
- 30 unit tests covering all modules

### Phase 2: CLI Integration (`apps/velor-cli/src/vault.rs`) - COMPLETED

Implemented all CLI commands for vault management:

**Files Created/Modified:**
- `apps/velor-cli/src/vault.rs` - Complete CLI implementation (new file)
- `apps/velor-cli/Cargo.toml` - Added dependencies (velor-vault, rpassword, atty, secrecy)
- `apps/velor-cli/src/main.rs` - Added vault subcommand and dispatch
- `apps/velor-cli/src/automations/launchd.rs` - Fixed clippy warnings (drive-by fix)

**CLI Commands Implemented:**
- `vel vault init [--global] [--backend keyring|passphrase]` - Initialize a new vault
- `vel vault set KEY [--prompt] [--from-env VAR] [--global]` - Set a secret
- `vel vault get KEY [--raw] [--force] [--global]` - Get a secret (masked by default)
- `vel vault list [--global]` - List all secret keys
- `vel vault unset KEY [--global]` - Remove a secret
- `vel vault validate [--global]` - Validate vault access
- `vel vault rotate-key [--global]` - Rotate master key
- `vel vault migrate-backend --to keyring|passphrase [--global]` - Migrate backend

**Security Features:**
- No shell history leakage (stdin or --prompt only for set)
- TTY safety check for --raw output (requires --force)
- Advisory .gitignore check for project vaults
- Masked output by default

## Verification

```bash
cargo nextest run -p velor-vault
# 30 tests passed

just check
# All checks pass (no clippy warnings)
```

## What's Next

**Phase 3: Automation File Format**

Add `required_secrets` and `optional_secrets` fields to `AutomationFile` struct in `crates/velor-automations/src/file_config.rs`.

**Phase 4: Automation Runner Integration**

Modify `crates/automations/src/runner.rs` to inject secrets at execution time.

## Blockers / Open Questions

None. Phase 1 and 2 are complete. Ready for Phase 3.

## References

- Plan file: `docs/plans/imperative-riding-corbato.md`
- Phase 1 commit: 690d394
- Phase 2: uncommitted (ready to commit)
