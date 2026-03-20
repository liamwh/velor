// Copyright (c) 2024 Liam S. (velor)
//
// This software is licensed under the terms of the UNLICENSE.
// You should have received a copy of the UNLICENSE with this program.
// If not, see https://unlicense.org/

//! Shell completion generation for velor CLI.
//!
//! This module provides custom shell completion scripts, with special support
//! for Zsh dynamic prompt completion.

use clap::Command;
use color_eyre::eyre::Result;
use std::io::Write;

/// The binary name for the vel CLI.
/// This must match the `name` field in `[[bin]]` in Cargo.toml.
const BINARY_NAME: &str = "vel";

/// Shell types supported for completion generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    /// Bash shell
    Bash,
    /// Zsh shell (fully custom with dynamic prompt completion)
    Zsh,
    /// Fish shell
    Fish,
    /// Elvish shell
    Elvish,
    /// PowerShell
    PowerShell,
    /// Nushell
    Nushell,
}

impl std::str::FromStr for Shell {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "bash" => Ok(Shell::Bash),
            "zsh" => Ok(Shell::Zsh),
            "fish" => Ok(Shell::Fish),
            "elvish" => Ok(Shell::Elvish),
            "powershell" | "pwsh" => Ok(Shell::PowerShell),
            "nushell" | "nu" => Ok(Shell::Nushell),
            _ => Err(format!("unknown shell: {s}")),
        }
    }
}

impl std::fmt::Display for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Shell::Bash => write!(f, "bash"),
            Shell::Zsh => write!(f, "zsh"),
            Shell::Fish => write!(f, "fish"),
            Shell::Elvish => write!(f, "elvish"),
            Shell::PowerShell => write!(f, "powershell"),
            Shell::Nushell => write!(f, "nushell"),
        }
    }
}

/// Generate shell completion script and print to stdout.
///
/// For Zsh: generates a fully custom script with dynamic prompt completion via
/// `velor internal complete-prompts`.
///
/// For other shells: uses clap_complete to generate static completion scaffolding.
///
/// # Errors
///
/// Returns an error if:
/// - Writing to stdout fails
/// - clap_complete generation fails (for non-Zsh shells)
///
/// # Examples
///
/// ```no_run
/// use velor_cli::completion::{Shell, generate_completion};
///
/// # fn main() -> color_eyre::eyre::Result<()> {
/// generate_completion(Shell::Zsh)?;
/// # Ok(())
/// # }
/// ```
pub fn generate_completion(shell: Shell) -> Result<()> {
    match shell {
        Shell::Zsh => {
            print_zsh_completion();
            Ok(())
        }
        _ => {
            // For other shells, use clap_complete to generate static completion
            let mut cmd = Command::new(BINARY_NAME);
            // We need to build the CLI struct from main.rs
            // But since we don't have access to it here, we'll need to pass it
            // For now, use a placeholder approach
            let mut buf = Vec::new();
            generate_clap_completion(shell, &mut cmd, &mut buf)?;
            std::io::stdout().write_all(&buf)?;
            Ok(())
        }
    }
}

/// Generate clap_complete-based shell completion.
///
/// This function uses clap_complete to generate shell-specific completion
/// scripts for shells other than Zsh.
///
/// # Errors
///
/// Returns an error if:
/// - The shell is not supported by clap_complete (e.g., Nushell)
/// - clap_complete generation fails
fn generate_clap_completion(shell: Shell, cmd: &mut Command, buf: &mut Vec<u8>) -> Result<()> {
    match shell {
        Shell::Bash => {
            clap_complete::generate(clap_complete::shells::Bash, cmd, BINARY_NAME, buf);
            Ok(())
        }
        Shell::Fish => {
            clap_complete::generate(clap_complete::shells::Fish, cmd, BINARY_NAME, buf);
            Ok(())
        }
        Shell::Elvish => {
            clap_complete::generate(clap_complete::shells::Elvish, cmd, BINARY_NAME, buf);
            Ok(())
        }
        Shell::PowerShell => {
            clap_complete::generate(clap_complete::shells::PowerShell, cmd, BINARY_NAME, buf);
            Ok(())
        }
        Shell::Nushell => {
            // Nushell is not yet supported by clap_complete
            // We accept it in the enum for future compatibility but return a clear error
            Err(color_eyre::eyre::eyre!(
                "Nushell completion is not yet supported by clap_complete. \
                 See https://github.com/clap-rs/clap/issues for progress."
            ))
        }
        Shell::Zsh => {
            // Zsh has custom implementation, should not reach here
            Err(color_eyre::eyre::eyre!(
                "Zsh completion should use print_zsh_completion"
            ))
        }
    }
}

/// Print custom Zsh completion script to stdout.
///
/// This function generates a fully custom Zsh completion script with:
/// - Command descriptions for all velor subcommands
/// - Dynamic `--prompt` argument completion via `velor internal complete-prompts`
/// - Common arguments for all commands
///
/// # Contract
///
/// The completion script:
/// - Is syntactically valid Zsh
/// - Uses `#compdef {binary}` directive matching the BINARY_NAME constant
/// - Uses a `prompt_list` case to call `{binary} internal complete-prompts` for prompt completion
/// - Gracefully degrades if the internal command fails (stderr redirected to /dev/null)
fn print_zsh_completion() {
    let template = r#"#compdef {BIN_NAME}

# {BIN_NAME} completion function for Zsh
# Provides dynamic completion for --prompt argument via {BIN_NAME} internal complete-prompts

_{BIN_NAME}() {{
    local state
    local -a commands
    commands=(
        'once:Run a single agent iteration'
        'auto:Run agent in auto mode with retries'
        'init:Initialize vel configuration'
        'plan:Execute a plan from a markdown file'
        'test-notification:Test notification configuration'
        'completion:Generate shell completion script'
        'automations:Manage automations'
        'project:Project-specific commands'
        'vault:Vault and secret management'
    )

    local -a common_args
    common_args=(
        '(--config -c)'{{--config,-c}}'[Override config path]:path:_files'
        '--prompt+[Prompt name from config]:prompt:->prompt_list'
        '(--prompt --prompt-text)--prompt-text[Inline template string]:template:'
        '(-p --pin)'{{-p,--pin}}'[Context identifier]'
        '(-v --vars-file)'{{-v,--vars-file}}'[Variables file]:path:_files'
        '(-n --dry-run)'{{-n,--dry-run}}'[Dry run mode]'
        '--permission-mode[Permission mode for Claude]:(acceptEdits preview)'
        '--binary[Override Claude binary]:binary:'
        '--complete-token[Override completion token]:token:'
    )

    case $words[1] in
        once)
            _arguments -C $common_args
            ;;
        auto)
            _arguments -C $common_args \
                '--max-retries[Maximum retry attempts]:number:' \
                '--backoff-base[Backoff base in seconds]:number:'
            ;;
        completion)
            _arguments '--shell[Shell type]:shell:(bash zsh fish elvish powershell nushell)'
            ;;
        init)
            _arguments '--force[Overwrite existing files]'
            ;;
        plan)
            _arguments '-d[Delete progress file after completion]' \
                '-s[Save progress to file]' \
                '--spec-path[Path to spec directory]:path:_files' \
                '--progress-path[Path to progress file]:path:_files' \
                '--prd-path[Path to PRD file]:path:_files'
            ;;
        test-notification)
            _arguments
            ;;
        automations)
            local -a automations_commands
            automations_commands=(
                'list:List all automations'
                'run:Run a specific automation'
                'tick:Run all pending automations'
                'create:create a new automation'
                'status:Show automation status'
                'logs:Show automation logs'
            )
            _describe 'command' automations_commands
            ;;
        project)
            local -a project_commands
            project_commands=(
                'register:Register a project in the registry'
                'unregister:Unregister a project from the registry'
                'list:List all registered projects'
            )
            _describe 'command' project_commands
            ;;
        vault)
            local -a vault_commands
            vault_commands=(
                'set:Set a secret value'
                'get:Get a secret value'
                'delete:Delete a secret'
                'list:List all secrets'
                'exists:Check if a secret exists'
                'export:Export secrets to a file'
                'import:Import secrets from a file'
            )
            _describe 'command' vault_commands
            ;;
        *)
            _describe 'command' commands
            ;;
    esac

    # Handle states from ->state arguments
    case $state in
        prompt_list)
            local -a prompt_names
            prompt_names=("${(@f)$({BIN_NAME} internal complete-prompts 2>/dev/null)}")
            _describe 'prompt' prompt_names
            ;;
    esac
}}
"#;
    let script = template
        .replace("{BIN_NAME}", BINARY_NAME)
        .replace("{{", "{")
        .replace("}}", "}");
    print!("{}", script);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_shell_from_str_valid() {
        assert_eq!(Shell::from_str("bash").unwrap(), Shell::Bash);
        assert_eq!(Shell::from_str("zsh").unwrap(), Shell::Zsh);
        assert_eq!(Shell::from_str("fish").unwrap(), Shell::Fish);
        assert_eq!(Shell::from_str("elvish").unwrap(), Shell::Elvish);
        assert_eq!(Shell::from_str("powershell").unwrap(), Shell::PowerShell);
        assert_eq!(Shell::from_str("pwsh").unwrap(), Shell::PowerShell);
        assert_eq!(Shell::from_str("nushell").unwrap(), Shell::Nushell);
        assert_eq!(Shell::from_str("nu").unwrap(), Shell::Nushell);
    }

    #[test]
    fn test_shell_from_str_invalid() {
        assert!(Shell::from_str("invalid").is_err());
        assert!(Shell::from_str("").is_err());
    }

    #[test]
    fn test_shell_from_str_case_insensitive() {
        assert_eq!(Shell::from_str("BASH").unwrap(), Shell::Bash);
        assert_eq!(Shell::from_str("Zsh").unwrap(), Shell::Zsh);
        assert_eq!(Shell::from_str("FISH").unwrap(), Shell::Fish);
    }

    #[test]
    fn test_shell_display() {
        assert_eq!(Shell::Bash.to_string(), "bash");
        assert_eq!(Shell::Zsh.to_string(), "zsh");
        assert_eq!(Shell::Fish.to_string(), "fish");
        assert_eq!(Shell::Elvish.to_string(), "elvish");
        assert_eq!(Shell::PowerShell.to_string(), "powershell");
        assert_eq!(Shell::Nushell.to_string(), "nushell");
    }

    #[test]
    fn test_shell_roundtrip() {
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::Elvish,
            Shell::PowerShell,
            Shell::Nushell,
        ] {
            let s = shell.to_string();
            assert_eq!(Shell::from_str(&s).unwrap(), shell);
        }
    }
}
