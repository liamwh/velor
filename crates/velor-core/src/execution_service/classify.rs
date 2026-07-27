//! Provider-failure classification with evidence.
//!
//! Given a finished [`ProcessOutput`] (plus any structured error event the
//! adapter parsed from the live stream), this module decides *which* provider
//! failure occurred, *where* the evidence came from, and *how confidently*.
//!
//! ## Precedence
//!
//! 1. Velor-owned cancellation/deadline — handled by the adapter before
//!    classification (the supervisor's [`Termination`]).
//! 2. **Structured** protocol error event (`structured_error`) — highest
//!    confidence, and the only source trusted to override generated text.
//! 3. Known provider error in the **stdout tail** (Claude Code/GLM emit API
//!    errors to stdout, not stderr).
//! 4. Known provider error in the **stderr tail**.
//! 5. Unrecognised non-zero exit — surfaced as a generic `UnsuccessfulExit` by
//!    the adapter (not here).
//!
//! ## Self-safety / false positives
//!
//! A model may *generate* the string `"API Error: 529"` as ordinary content in a
//! successful turn. [`classify_output`] therefore returns `None` for any
//! successful exit, so generated text is never misclassified as a provider
//! failure. Text matches are marked [`ClassificationConfidence::Medium`];
//! structured events are [`ClassificationConfidence::High`].
//!
//! [`Termination`]: crate::execution_service::output::Termination

use std::time::Duration;

use crate::execution_service::error::{ProviderError, ProviderErrorKind};
use crate::execution_service::output::ProcessOutput;

/// Which agent provider's output is being classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// Claude Code (and GLM/Z.ai Claude-compatible wrappers).
    Claude,
    /// Codex (`codex exec --json`).
    Codex,
    /// Oh My Pi (`omp --mode rpc`). Errors surface as structured RPC failures.
    Omp,
}

/// Where a classification decision found its evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassificationSource {
    /// Derived from a structured protocol error event.
    StructuredEvent,
    /// Derived from the captured stdout tail.
    StdoutTail,
    /// Derived from the captured stderr tail.
    StderrTail,
    /// Derived from the exit status.
    ExitStatus,
}

/// How confident the classifier is in a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ClassificationConfidence {
    /// Low-confidence heuristic match.
    Low,
    /// A text pattern matched; could in principle appear in generated content.
    Medium,
    /// A structured event or unambiguous signal.
    High,
}

/// Evidence for a provider classification. Carries provenance without the
/// payload weight of [`ProviderError`] (so [`ProviderError`] never nests it,
/// avoiding a cyclic type).
#[derive(Debug, Clone)]
pub struct Classification {
    /// The coarse provider-failure kind.
    pub kind: ProviderErrorKind,
    /// Where the evidence was found.
    pub source: ClassificationSource,
    /// The named rule that matched (for diagnostics).
    pub matched_rule: &'static str,
    /// Confidence in the match.
    pub confidence: ClassificationConfidence,
}

impl Classification {
    /// Creates a new evidence record.
    #[must_use]
    pub const fn new(
        kind: ProviderErrorKind,
        source: ClassificationSource,
        matched_rule: &'static str,
        confidence: ClassificationConfidence,
    ) -> Self {
        Self {
            kind,
            source,
            matched_rule,
            confidence,
        }
    }
}

/// A provider failure plus the evidence that identified it.
#[derive(Debug, Clone)]
pub struct ClassifiedProvider {
    /// The classified provider error with its payload.
    pub error: ProviderError,
    /// How and where it was identified.
    pub evidence: Classification,
}

/// Classifies a finished process output, returning a recognised provider failure
/// if one is found.
///
/// Returns `None` for successful exits (so model-generated error-like text in a
/// successful turn is never misclassified), and `None` when no known provider
/// failure is recognised (the adapter then treats the non-zero exit as a generic
/// `UnsuccessfulExit`).
#[must_use]
pub fn classify_output(
    output: &ProcessOutput,
    provider: ProviderKind,
    structured_error: Option<&str>,
) -> Option<ClassifiedProvider> {
    // Never classify a successful run, even if its output mentions error tokens.
    if output.is_success() {
        return None;
    }

    if let Some(classified) = structured_error.and_then(|text| {
        classify_text(
            text,
            provider,
            ClassificationSource::StructuredEvent,
            ClassificationConfidence::High,
        )
    }) {
        return Some(classified);
    }

    let stdout_tail = output.stdout.tail_str();
    if let Some(classified) = classify_text(
        &stdout_tail,
        provider,
        ClassificationSource::StdoutTail,
        ClassificationConfidence::Medium,
    ) {
        return Some(classified);
    }

    let stderr_tail = output.stderr.tail_str();
    if let Some(classified) = classify_text(
        &stderr_tail,
        provider,
        ClassificationSource::StderrTail,
        ClassificationConfidence::Medium,
    ) {
        return Some(classified);
    }

    None
}

/// Scans `text` (case-insensitively) for known provider-failure patterns and
/// returns the first match as a [`ClassifiedProvider`].
///
/// Patterns are checked most-specific-first so that, e.g., "prompt is too long"
/// is not swallowed by a generic overload matcher. `Retry-After` is extracted
/// opportunistically from known forms only.
fn classify_text(
    text: &str,
    provider: ProviderKind,
    source: ClassificationSource,
    confidence: ClassificationConfidence,
) -> Option<ClassifiedProvider> {
    let hay = text.to_ascii_lowercase();
    let retry_after = parse_retry_after(text);

    // Order matters: deterministic/permanent failures before transient ones so a
    // specific signal is not masked by a generic one.
    if hay.contains("prompt is too long")
        || hay.contains("context_length_exceeded")
        || hay.contains("context length")
        || hay.contains("maximum context length")
    {
        return Some(ClassifiedProvider {
            error: ProviderError::ContextTooLarge,
            evidence: Classification::new(
                ProviderErrorKind::ContextTooLarge,
                source,
                "context_too_large",
                confidence,
            ),
        });
    }

    if hay.contains("invalid api key")
        || hay.contains("invalid x-api-key")
        || hay.contains("invalid api_key")
        || hay.contains("authentication")
        || hay.contains("unauthorized")
        || hay.contains("401")
    {
        return Some(ClassifiedProvider {
            error: ProviderError::Authentication,
            evidence: Classification::new(
                ProviderErrorKind::Authentication,
                source,
                "authentication",
                confidence,
            ),
        });
    }

    if hay.contains("invalid model")
        || hay.contains("model not found")
        || hay.contains("invalid configuration")
        || hay.contains("malformed configuration")
    {
        return Some(ClassifiedProvider {
            error: ProviderError::InvalidConfiguration,
            evidence: Classification::new(
                ProviderErrorKind::InvalidConfiguration,
                source,
                "invalid_configuration",
                confidence,
            ),
        });
    }

    if hay.contains("429") || hay.contains("rate limit") || hay.contains("too many requests") {
        return Some(ClassifiedProvider {
            error: ProviderError::RateLimited { retry_after },
            evidence: Classification::new(
                ProviderErrorKind::RateLimited,
                source,
                "rate_limited",
                confidence,
            ),
        });
    }

    if hay.contains("529")
        || hay.contains("overloaded")
        || hay.contains("temporarily overloaded")
        || hay.contains("service may be temporarily")
    {
        let provider_code = extract_provider_code(text);
        return Some(ClassifiedProvider {
            error: ProviderError::Overloaded {
                status: Some(529),
                provider_code,
                retry_after,
            },
            evidence: Classification::new(
                ProviderErrorKind::Overloaded,
                source,
                "overloaded",
                confidence,
            ),
        });
    }

    if hay.contains("econnreset")
        || hay.contains("connection reset")
        || hay.contains("unable to connect to api")
        || hay.contains("connection refused")
    {
        return Some(ClassifiedProvider {
            error: ProviderError::ConnectionReset,
            evidence: Classification::new(
                ProviderErrorKind::ConnectionReset,
                source,
                "connection_reset",
                confidence,
            ),
        });
    }

    // Codex-specific structured error vocabulary.
    if provider == ProviderKind::Codex && hay.contains("\"type\":\"error\"") {
        let summary = text.trim().chars().take(160).collect();
        return Some(ClassifiedProvider {
            error: ProviderError::Other {
                summary,
                retryability: crate::execution_service::error::Retryability::Retryable {
                    floor: None,
                },
            },
            evidence: Classification::new(
                ProviderErrorKind::Other,
                source,
                "codex_error_event",
                confidence,
            ),
        });
    }

    None
}

/// Extracts a `Retry-After` value from known textual forms only. Accepts:
/// `Retry-After: <secs>`, `retry_after: <secs>`, `retry_after_ms: <ms>`. Never
/// extracts arbitrary numbers from prose.
fn parse_retry_after(text: &str) -> Option<Duration> {
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        let value = if let Some(rest) = lower.strip_prefix("retry-after:") {
            rest.trim()
        } else if let Some(rest) = lower.strip_prefix("retry_after:") {
            rest.trim()
        } else {
            continue;
        };
        if let Ok(secs) = value.parse::<u64>() {
            return Some(Duration::from_secs(secs));
        }
    }
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("retry_after_ms:")
            && let Ok(ms) = rest.trim().parse::<u64>()
        {
            return Some(Duration::from_millis(ms));
        }
    }
    None
}

/// Extracts a bracketed provider code such as `[1305]`.
fn extract_provider_code(text: &str) -> Option<String> {
    let open = text.find('[')?;
    let rest = &text[open + 1..];
    let close = rest.find(']')?;
    let code = &rest[..close];
    if code.chars().all(|c| c.is_ascii_digit()) && !code.is_empty() {
        return Some(code.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_service::output::{CaptureBuilder, ProcessOutput, Termination};
    use std::process::ExitStatus;

    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;

    /// Builds a [`ProcessOutput`] with the given exit success, stdout and stderr.
    #[cfg(unix)]
    fn make_output(success: bool, stdout: &str, stderr: &str) -> ProcessOutput {
        let mut so = CaptureBuilder::new(4096);
        so.push(stdout.as_bytes());
        let mut se = CaptureBuilder::new(4096);
        se.push(stderr.as_bytes());
        let raw = if success { 0 } else { 1 << 8 };
        let status: ExitStatus = ExitStatusExt::from_raw(raw);
        ProcessOutput {
            stdout: so.finish(),
            stderr: se.finish(),
            termination: Termination::Exited(status),
            duration: std::time::Duration::ZERO,
            pid: Some(12345),
        }
    }

    #[cfg(unix)]
    #[test]
    fn classifies_stdout_overload_529() {
        let out = make_output(false, "API Error: 529 [1305][overloaded]\n", "");
        let c = classify_output(&out, ProviderKind::Claude, None).expect("classified");
        assert_eq!(c.error.kind(), ProviderErrorKind::Overloaded);
        assert_eq!(c.evidence.source, ClassificationSource::StdoutTail);
        assert_eq!(c.evidence.confidence, ClassificationConfidence::Medium);
        if let ProviderError::Overloaded {
            provider_code,
            status,
            ..
        } = &c.error
        {
            assert_eq!(provider_code.as_deref(), Some("1305"));
            assert_eq!(*status, Some(529));
        } else {
            panic!("wrong variant");
        }
    }

    #[cfg(unix)]
    #[test]
    fn classifies_econnreset() {
        let out = make_output(
            false,
            "API Error: Unable to connect to API (ECONNRESET)\n",
            "",
        );
        let c = classify_output(&out, ProviderKind::Claude, None).expect("classified");
        assert_eq!(c.error.kind(), ProviderErrorKind::ConnectionReset);
        assert!(c.error.retryability().is_retryable());
    }

    #[cfg(unix)]
    #[test]
    fn classifies_invalid_key_as_permanent() {
        let out = make_output(false, "API Error: 401 invalid x-api-key\n", "");
        let c = classify_output(&out, ProviderKind::Claude, None).expect("classified");
        assert_eq!(c.error.kind(), ProviderErrorKind::Authentication);
        assert!(!c.error.retryability().is_retryable());
    }

    #[cfg(unix)]
    #[test]
    fn classifies_context_too_long_as_permanent() {
        let out = make_output(
            false,
            "Error: prompt is too long: context_length_exceeded\n",
            "",
        );
        let c = classify_output(&out, ProviderKind::Claude, None).expect("classified");
        assert_eq!(c.error.kind(), ProviderErrorKind::ContextTooLarge);
        assert!(!c.error.retryability().is_retryable());
    }

    #[cfg(unix)]
    #[test]
    fn classifies_rate_limit_with_retry_after() {
        let out = make_output(
            false,
            "API Error: 429 Too Many Requests\nRetry-After: 5\n",
            "",
        );
        let c = classify_output(&out, ProviderKind::Claude, None).expect("classified");
        assert_eq!(c.error.kind(), ProviderErrorKind::RateLimited);
        if let ProviderError::RateLimited { retry_after } = &c.error {
            assert_eq!(*retry_after, Some(Duration::from_secs(5)));
        } else {
            panic!("wrong variant");
        }
    }

    #[cfg(unix)]
    #[test]
    fn stderr_only_error_is_classified_from_stderr_tail() {
        let out = make_output(false, "", "connection reset by peer\n");
        let c = classify_output(&out, ProviderKind::Claude, None).expect("classified");
        assert_eq!(c.error.kind(), ProviderErrorKind::ConnectionReset);
        assert_eq!(c.evidence.source, ClassificationSource::StderrTail);
    }

    #[cfg(unix)]
    #[test]
    fn split_output_error_is_classified() {
        let out = make_output(
            false,
            "API Error: 529 [1305] overloaded\n",
            "additional detail\n",
        );
        let c = classify_output(&out, ProviderKind::Claude, None).expect("classified");
        assert_eq!(c.error.kind(), ProviderErrorKind::Overloaded);
    }

    #[cfg(unix)]
    #[test]
    fn successful_exit_with_error_like_text_is_not_classified() {
        // False-positive guard: a model that *generated* "API Error: 529" as
        // content but exited successfully must not be classified as overloaded.
        let out = make_output(
            true,
            "Sure, here is an example: API Error: 529 [1305]\n",
            "",
        );
        assert!(classify_output(&out, ProviderKind::Claude, None).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn structured_event_takes_precedence_over_text() {
        let out = make_output(false, "API Error: 529 [1305]\n", "");
        let c = classify_output(&out, ProviderKind::Claude, Some("invalid x-api-key"))
            .expect("classified");
        // Structured (auth) wins over the stdout 529.
        assert_eq!(c.error.kind(), ProviderErrorKind::Authentication);
        assert_eq!(c.evidence.source, ClassificationSource::StructuredEvent);
        assert_eq!(c.evidence.confidence, ClassificationConfidence::High);
    }

    #[cfg(unix)]
    #[test]
    fn unrecognised_nonzero_exit_is_not_classified() {
        let out = make_output(false, "some unrelated output\n", "");
        assert!(classify_output(&out, ProviderKind::Claude, None).is_none());
    }

    #[test]
    fn retry_after_only_known_forms() {
        assert_eq!(
            parse_retry_after("Retry-After: 7"),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            parse_retry_after("retry_after_ms: 2500"),
            Some(Duration::from_millis(2500))
        );
        // Arbitrary numbers in prose must not be extracted.
        assert_eq!(parse_retry_after("please wait 30 seconds"), None);
    }

    #[test]
    fn provider_code_extraction() {
        assert_eq!(
            extract_provider_code("529 [1305][msg]"),
            Some("1305".to_string())
        );
        assert_eq!(extract_provider_code("no code here"), None);
    }
}
