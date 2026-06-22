//! Derived invocation diagnostics + a sanitised replay manifest.
//!
//! [`InvocationRecord`] is *derived* from the profile, the finished output, and
//! the error — not hand-maintained across layers — so it cannot drift out of sync
//! with execution. Secrets are never serialised: [`redact_secrets`] strips
//! known-sensitive patterns, and the replay manifest writes only a sanitised env
//! subset + the prompt to a file (never inline secrets).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use regex::Regex;
use serde::Serialize;

use crate::execution_service::error::AgentExecutionError;
use crate::execution_service::output::ProcessOutput;

/// Environment variable names whose values are safe to include verbatim in
/// diagnostics/manifests (non-secret, useful for comparing Velor vs a direct run).
pub const SAFE_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "NO_COLOR", "CLICOLOR",
];

/// A redacted, JSON-serialisable record of one agent invocation, derived from the
/// inputs + outcome. Carries no secrets and no full prompt.
#[derive(Debug, Clone, Serialize)]
pub struct InvocationRecord {
    /// Configured executable string.
    pub executable: String,
    /// Resolved absolute path on PATH, if found.
    pub resolved_path: Option<PathBuf>,
    /// Working directory.
    pub working_directory: Option<PathBuf>,
    /// Prompt length in characters.
    pub prompt_chars: usize,
    /// Prompt length in UTF-8 bytes.
    pub prompt_bytes: usize,
    /// Rough token-count estimate (lower bound). Labelled an estimate only.
    pub prompt_tokens_estimate_lower: usize,
    /// Rough token-count estimate (upper bound).
    pub prompt_tokens_estimate_upper: usize,
    /// Exit code, if the process exited normally.
    pub exit_code: Option<i32>,
    /// How the process terminated (stringified).
    pub termination: String,
    /// Wall-clock run duration.
    pub elapsed: Duration,
    /// Provider-failure classification, if any.
    pub classification: Option<String>,
    /// Captured stdout byte count.
    pub stdout_bytes: u64,
    /// Captured stderr byte count.
    pub stderr_bytes: u64,
    /// Whether the run was retried (set by the orchestration layer).
    pub retries: u32,
    /// A safe subset of environment variables actually set in the process env.
    pub env: BTreeMap<String, String>,
}

impl InvocationRecord {
    /// Derives a record from the configured binary, prompt, working directory,
    /// the finished [`ProcessOutput`], and the outcome.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn derive(
        executable: &str,
        prompt: &[u8],
        working_directory: Option<&Path>,
        output: Option<&ProcessOutput>,
        outcome: Option<&AgentExecutionError>,
        retries: u32,
        env: &BTreeMap<String, String>,
    ) -> Self {
        let prompt_chars = String::from_utf8_lossy(prompt).chars().count();
        let prompt_bytes = prompt.len();
        let (tokens_lower, tokens_upper) = token_estimate(prompt_chars);
        let exit_code = output.and_then(|o| o.exit_status()).and_then(|s| s.code());
        let termination = match output.map(|o| &o.termination) {
            Some(crate::execution_service::output::Termination::Exited(s)) => {
                format!("exited({s})")
            }
            Some(crate::execution_service::output::Termination::TimedOut { which }) => {
                format!("timed_out({})", which.label())
            }
            Some(crate::execution_service::output::Termination::Cancelled) => "cancelled".into(),
            None => match outcome {
                Some(AgentExecutionError::Cancelled) => "cancelled".into(),
                _ => "unknown".into(),
            },
        };
        let elapsed = output.map(|o| o.duration).unwrap_or_default();
        let classification = outcome.map(|e| e.to_string());
        let (stdout_bytes, stderr_bytes) = output
            .map(|o| (o.stdout.total_bytes, o.stderr.total_bytes))
            .unwrap_or_default();
        let safe_env = collect_safe_env(env);

        Self {
            executable: executable.to_string(),
            resolved_path: resolve_executable(executable),
            working_directory: working_directory.map(Path::to_path_buf),
            prompt_chars,
            prompt_bytes,
            prompt_tokens_estimate_lower: tokens_lower,
            prompt_tokens_estimate_upper: tokens_upper,
            exit_code,
            termination,
            elapsed,
            classification,
            stdout_bytes,
            stderr_bytes,
            retries,
            env: safe_env,
        }
    }
}

/// Returns a (lower, upper) token estimate from a character count. Uses a wide
/// range (chars/5 .. chars/3) because tokenizer accuracy varies by content; this
/// is for sizing only, never policy.
#[must_use]
pub const fn token_estimate(prompt_chars: usize) -> (usize, usize) {
    (prompt_chars / 5, prompt_chars / 3)
}

/// Collects the safe (non-secret) environment-variable subset from the process's
/// own environment + the provided overrides.
fn collect_safe_env(overrides: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for name in SAFE_ENV_VARS {
        if let Ok(value) = std::env::var(name) {
            out.insert((*name).to_string(), value);
        }
    }
    for (k, v) in overrides {
        if SAFE_ENV_VARS.contains(&k.as_str()) {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// Resolves `binary` to an absolute path using the same PATH lookup semantics as
/// `std::process::Command`. Returns `None` if not found.
#[must_use]
pub fn resolve_executable(binary: &str) -> Option<PathBuf> {
    let path = Path::new(binary);
    if path.is_absolute() || binary.contains('/') {
        return path.is_file().then(|| path.to_path_buf());
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Redacts known-sensitive patterns in `text` and truncates to `max_bytes` on a
/// UTF-8 boundary. Strips: `Authorization`/`Bearer`/`x-api-key` style headers,
/// `key=…`/`token=…`/`secret=…`/`password=…` assignments, and Anthropic/Z.ai
/// auth-token env assignments. Returns `[REDACTED]` in place of each secret.
#[must_use]
pub fn redact_secrets(text: &str, max_bytes: usize) -> String {
    let patterns: &[&str] = &[
        r"(?i)(authorization|x-api-key|anthropic-auth-token|anthropic-api-key|zai_api_key)\s*[:=]\s*\S+",
        r"(?i)(bearer)\s+[A-Za-z0-9_\-\.=]+",
        r"(?i)(sk-[A-Za-z0-9]{16,})",
        r#"(?i)(api[_-]?key|token|secret|password)["']?\s*[:=]\s*["']?[A-Za-z0-9_\-\.%/+]{6,}["']?"#,
    ];
    let mut out = text.to_string();
    for pat in patterns {
        if let Ok(re) = Regex::new(pat) {
            out = re.replace_all(&out, "[REDACTED]").into_owned();
        }
    }
    if out.len() <= max_bytes {
        return out;
    }
    let idx = out.floor_char_boundary(max_bytes);
    out.truncate(idx);
    out.push('…');
    out
}

/// A sanitised replay manifest: enough to reproduce the exact invocation without
/// exposing secrets. The prompt is written to `prompt_path` (out-of-line), never
/// inlined in the command.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayManifest {
    /// The executable to run.
    pub executable: String,
    /// Argument list (values redacted where they look secret).
    pub arguments: Vec<String>,
    /// Working directory.
    pub working_directory: Option<PathBuf>,
    /// Path to the exact prompt bytes written for replay.
    pub prompt_path: PathBuf,
    /// Safe environment subset (no secrets).
    pub env: BTreeMap<String, String>,
}

impl ReplayManifest {
    /// Builds a manifest, writing the prompt to `prompt_path`. Arguments are
    /// redacted via [`redact_secrets`].
    ///
    /// # Errors
    /// Returns an I/O error if the prompt file cannot be written.
    pub fn build(
        executable: &str,
        arguments: &[String],
        working_directory: Option<&Path>,
        prompt: &[u8],
        prompt_path: &Path,
        env: &BTreeMap<String, String>,
    ) -> std::io::Result<Self> {
        std::fs::write(prompt_path, prompt)?;
        let arguments = arguments.iter().map(|a| redact_secrets(a, 4096)).collect();
        Ok(Self {
            executable: executable.to_string(),
            arguments,
            working_directory: working_directory.map(Path::to_path_buf),
            prompt_path: prompt_path.to_path_buf(),
            env: collect_safe_env(env),
        })
    }

    /// Renders a shell replay command that feeds the prompt file to the
    /// executable's stdin. No secrets are inlined.
    #[must_use]
    pub fn replay_command(&self) -> String {
        let mut cmd = self.executable.clone();
        for a in &self.arguments {
            cmd.push(' ');
            cmd.push_str(&shell_quote(a));
        }
        if let Some(cwd) = &self.working_directory {
            cmd.push_str(&format!(" # cwd={}", cwd.display()));
        }
        cmd.push_str(&format!(" < {}", self.prompt_path.display()));
        cmd
    }
}

/// Minimal shell quoting (single-quotes the value, escapes embedded quotes).
fn shell_quote(s: &str) -> String {
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_estimate_is_a_range() {
        let (lo, hi) = token_estimate(5000);
        assert!(lo < hi, "estimate should be a range");
        assert!(lo >= 5000 / 6, "lower bound sane");
    }

    #[test]
    fn redact_strips_bearer_and_api_key() {
        let s = "Authorization: Bearer sk-1234567890abcdef token=secretvalue123";
        let red = redact_secrets(s, 4096);
        assert!(!red.contains("sk-1234567890abcdef"), "token leaked: {red}");
        assert!(!red.contains("secretvalue123"), "secret leaked: {red}");
        assert!(red.contains("[REDACTED]"));
    }

    #[test]
    fn redact_strips_zai_key_assignment() {
        let s = "ZAI_API_KEY=abc123def456";
        let red = redact_secrets(s, 4096);
        assert!(!red.contains("abc123def456"), "key leaked: {red}");
    }

    #[test]
    fn redact_truncates_long_text() {
        let s = "x".repeat(1000);
        let red = redact_secrets(&s, 10);
        // Truncated to ~max_bytes plus the ellipsis; must be far shorter than 1000.
        assert!(red.len() < 32, "not truncated: {}", red.len());
        assert!(red.ends_with('…'));
    }

    #[test]
    fn replay_command_no_inline_prompt_or_secret() {
        let dir = tempfile::tempdir().expect("tempdir");
        let prompt_path = dir.path().join("prompt.txt");
        let manifest = ReplayManifest::build(
            "/usr/local/bin/glm5",
            &["--permission-mode".into(), "acceptEdits".into()],
            Some(Path::new("/tmp/work")),
            b"the prompt",
            &prompt_path,
            &BTreeMap::new(),
        )
        .expect("build manifest");
        let cmd = manifest.replay_command();
        assert!(cmd.contains("/usr/local/bin/glm5"));
        assert!(cmd.starts_with("/usr/local/bin/glm5 '"));
        assert!(cmd.contains("<"), "should redirect prompt file, not inline");
        // The prompt bytes must not appear in the command.
        assert!(!cmd.contains("the prompt"));
    }

    #[test]
    fn collect_safe_env_only_safe_names() {
        let mut overrides = BTreeMap::new();
        overrides.insert("ZAI_API_KEY".to_string(), "supersecret".to_string());
        overrides.insert("TERM".to_string(), "xterm".to_string());
        let env = collect_safe_env(&overrides);
        assert!(env.contains_key("TERM"));
        // ZAI_API_KEY is NOT in the safe list and must never appear.
        assert!(!env.contains_key("ZAI_API_KEY"));
    }
}
