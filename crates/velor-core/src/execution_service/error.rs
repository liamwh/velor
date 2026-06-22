//! Typed errors for the process supervisor.
//!
//! These describe *local* execution failures (spawn, I/O, deadlines,
//! cancellation, termination/reaping). Provider-level failures (overload, auth,
//! context-too-large, …) are classified later by the adapter/classifier layer and
//! live in `super::error` on `AgentExecutionError`/`ProviderError`, not here.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::execution_service::classify::Classification;
use crate::execution_service::output::{CapturedOutput, OutputStream};

/// Which deadline a supervised process violated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutKind {
    /// The process failed to produce its first output within the startup window.
    Startup,
    /// Writing the prompt to stdin exceeded the stdin-write deadline.
    StdinWrite,
    /// No output was received for longer than the idle deadline.
    Idle,
    /// The total attempt deadline (from spawn) was exceeded.
    Total,
}

impl TimeoutKind {
    /// Returns a short human-readable label for this deadline kind.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::StdinWrite => "stdin write",
            Self::Idle => "idle",
            Self::Total => "total",
        }
    }
}

/// Errors raised by [`crate::execution_service::supervisor`] while running a
/// process. Spawn-time `io::Error`s are translated to `ExecutableNotFound` /
/// `PermissionDenied` / generic `Spawn` so callers never have to re-parse error
/// strings to tell a missing binary from a transient failure.
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    /// The executable could not be found on `PATH` (or at the given path).
    #[error("executable was not found: {executable}")]
    ExecutableNotFound {
        /// The program that was attempted.
        executable: PathBuf,
    },
    /// The executable was found but could not be executed due to permissions.
    #[error("permission denied while spawning: {executable}")]
    PermissionDenied {
        /// The program that was attempted.
        executable: PathBuf,
    },
    /// Spawning failed for some other reason.
    #[error("failed to spawn {executable}")]
    Spawn {
        /// The program that was attempted.
        executable: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Writing the prompt to the child's stdin failed.
    #[error("failed to write process stdin")]
    Stdin {
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Reading the child's output failed.
    #[error("failed to read process {stream}")]
    Output {
        /// Which stream failed.
        stream: OutputStream,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A configured deadline was exceeded and the process was terminated.
    #[error("process exceeded its {} deadline", .which.label())]
    TimedOut {
        /// Which deadline fired.
        which: TimeoutKind,
    },
    /// The process was cancelled via the cancellation token.
    #[error("process execution was cancelled")]
    Cancelled,
    /// Terminating the process group failed.
    #[error("failed to terminate the process group")]
    Termination {
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Reaping the direct child failed.
    #[error("failed to reap the child process")]
    Reap {
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

impl ProcessError {
    /// Classifies a spawn-time [`std::io::Error`] into the most specific
    /// [`ProcessError`] variant for `executable`.
    #[must_use]
    pub fn from_spawn_error(executable: impl Into<PathBuf>, source: std::io::Error) -> Self {
        let executable = executable.into();
        match source.kind() {
            std::io::ErrorKind::NotFound => Self::ExecutableNotFound { executable },
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied { executable },
            _ => Self::Spawn { executable, source },
        }
    }

    /// Returns `true` when this error means the executable is missing — the only
    /// condition that should ever produce a "binary may not be installed" hint.
    #[must_use]
    pub fn is_executable_not_found(&self) -> bool {
        matches!(self, Self::ExecutableNotFound { .. })
    }

    /// Returns whether a process-level failure should be retried.
    ///
    /// Missing/permission/termination/reap errors are permanent; a deadline or
    /// cancellation is not retried at the *process* layer (the retry driver
    /// decides based on which deadline — a local Velor deadline is permanent,
    /// an upstream-derived one may be retryable); generic spawn/I/O are
    /// transient.
    #[must_use]
    pub fn retryability(&self) -> Retryability {
        match self {
            Self::ExecutableNotFound { .. }
            | Self::PermissionDenied { .. }
            | Self::Termination { .. }
            | Self::Reap { .. } => Retryability::Permanent,
            Self::Spawn { .. } | Self::Stdin { .. } | Self::Output { .. } => {
                Retryability::Retryable { floor: None }
            }
            Self::TimedOut { .. } | Self::Cancelled => Retryability::Permanent,
        }
    }
}

/// Whether an error warrants another attempt, with an optional minimum delay
/// floor appropriate to the failure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retryability {
    /// Retry with backoff, optionally floored to this duration for the class.
    Retryable {
        /// Minimum delay to apply for this failure class (e.g. overload ~5s).
        floor: Option<Duration>,
    },
    /// Do not retry; the failure is deterministic.
    Permanent,
}

impl Retryability {
    /// Returns `true` if the error is retryable.
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Retryable { .. })
    }

    /// Returns the minimum delay floor for retryable errors, if any.
    #[must_use]
    pub const fn floor(self) -> Option<Duration> {
        match self {
            Self::Retryable { floor } => floor,
            Self::Permanent => None,
        }
    }
}

/// A coarse, `Copy` classification of a provider failure. Used as evidence
/// provenance without the payload weight of [`ProviderError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// Upstream is temporarily overloaded (e.g. HTTP 529).
    Overloaded,
    /// Upstream rate-limited the request (e.g. HTTP 429).
    RateLimited,
    /// Upstream reset the connection.
    ConnectionReset,
    /// Authentication failed (bad key / token).
    Authentication,
    /// The request payload/context was too large.
    ContextTooLarge,
    /// The provider rejected the configuration.
    InvalidConfiguration,
    /// Any other provider failure.
    Other,
}

impl ProviderErrorKind {
    /// Returns the default retryability for this provider failure class.
    #[must_use]
    pub const fn default_retryability(self) -> Retryability {
        match self {
            Self::Overloaded => Retryability::Retryable {
                floor: Some(Duration::from_secs(5)),
            },
            Self::RateLimited => Retryability::Retryable {
                floor: Some(Duration::from_secs(1)),
            },
            Self::ConnectionReset => Retryability::Retryable {
                floor: Some(Duration::from_secs(2)),
            },
            Self::Authentication
            | Self::ContextTooLarge
            | Self::InvalidConfiguration
            | Self::Other => Retryability::Permanent,
        }
    }
}

/// A failure reported by the upstream provider, parsed from the agent's output.
///
/// Deliberately carries no nested evidence: the [`Classification`] evidence
/// wrapper travels alongside it (see [`crate::execution_service::classify`]).
#[derive(Debug, Clone, thiserror::Error)]
pub enum ProviderError {
    /// Upstream reported temporary overload.
    #[error("provider is temporarily overloaded (status={status:?}, code={provider_code:?})")]
    Overloaded {
        /// HTTP status code if identifiable (e.g. 529).
        status: Option<u16>,
        /// Provider-specific error code if present (e.g. "1305").
        provider_code: Option<String>,
        /// A parsed `Retry-After` hint, when reliably available.
        retry_after: Option<Duration>,
    },
    /// Upstream rate-limited the request.
    #[error("provider rate-limited the request (retry_after={retry_after:?})")]
    RateLimited {
        /// A parsed `Retry-After` hint, when reliably available.
        retry_after: Option<Duration>,
    },
    /// Upstream reset the connection.
    #[error("provider connection was reset")]
    ConnectionReset,
    /// Authentication failed.
    #[error("provider authentication failed")]
    Authentication,
    /// The request context/prompt was too large.
    #[error("request context/prompt was too large for the provider")]
    ContextTooLarge,
    /// The provider rejected the configuration.
    #[error("provider configuration is invalid")]
    InvalidConfiguration,
    /// Another provider failure with an explicit retryability decision.
    #[error("provider request failed: {summary}")]
    Other {
        /// Short human-readable summary.
        summary: String,
        /// Whether this specific failure should be retried.
        retryability: Retryability,
    },
}

impl ProviderError {
    /// Returns the coarse kind of this provider failure.
    #[must_use]
    pub fn kind(&self) -> ProviderErrorKind {
        match self {
            Self::Overloaded { .. } => ProviderErrorKind::Overloaded,
            Self::RateLimited { .. } => ProviderErrorKind::RateLimited,
            Self::ConnectionReset => ProviderErrorKind::ConnectionReset,
            Self::Authentication => ProviderErrorKind::Authentication,
            Self::ContextTooLarge => ProviderErrorKind::ContextTooLarge,
            Self::InvalidConfiguration => ProviderErrorKind::InvalidConfiguration,
            Self::Other { .. } => ProviderErrorKind::Other,
        }
    }

    /// Returns the retryability for this provider failure, honouring any
    /// parsed `Retry-After`.
    #[must_use]
    pub fn retryability(&self) -> Retryability {
        match self {
            Self::Overloaded { retry_after, .. } => Retryability::Retryable {
                floor: retry_after.or(Some(Duration::from_secs(5))),
            },
            Self::RateLimited { retry_after } => Retryability::Retryable {
                floor: retry_after.or(Some(Duration::from_secs(1))),
            },
            Self::ConnectionReset => Retryability::Retryable {
                floor: Some(Duration::from_secs(2)),
            },
            Self::Authentication | Self::ContextTooLarge | Self::InvalidConfiguration => {
                Retryability::Permanent
            }
            Self::Other { retryability, .. } => *retryability,
        }
    }
}

/// The child exited with a non-zero status but no recognised provider error was
/// found in its output.
#[derive(Debug, Clone)]
pub struct UnsuccessfulExit {
    /// The exit code (if a normal exit; signals are reflected via the status string).
    pub code: Option<i32>,
    /// Captured stdout (bounded).
    pub stdout: CapturedOutput,
    /// Captured stderr (bounded).
    pub stderr: CapturedOutput,
}

impl std::fmt::Display for UnsuccessfulExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.code {
            Some(code) => write!(f, "agent exited unsuccessfully (code={code})"),
            None => write!(f, "agent exited unsuccessfully (signal)"),
        }
    }
}

impl std::error::Error for UnsuccessfulExit {}

/// An error in the agent's wire protocol (framing/parse), adapter-internal.
#[derive(Debug, thiserror::Error)]
pub enum AgentProtocolError {
    /// A protocol frame exceeded the configured maximum length.
    #[error("protocol frame exceeded the {max} byte maximum")]
    FrameTooLong {
        /// The configured maximum frame length.
        max: usize,
    },
    /// The stream contained bytes that could not be decoded as UTF-8.
    #[error("stream contained invalid UTF-8")]
    InvalidUtf8,
    /// A frame could not be parsed as the expected protocol message.
    #[error("malformed protocol message: {0}")]
    Malformed(String),
}

/// A failure from the ACP (Agent Client Protocol) adapter.
#[derive(Debug, Clone, thiserror::Error)]
#[error("ACP agent failed: {0}")]
pub struct AcpError(pub String);

/// The unified, layered error for agent execution. Provenance is preserved:
/// process vs protocol vs provider vs unsuccessful-exit are distinct variants.
#[derive(Debug, thiserror::Error)]
pub enum AgentExecutionError {
    /// A process-level failure (spawn, I/O, deadline, cancellation, reap).
    #[error(transparent)]
    Process(#[from] ProcessError),
    /// A protocol framing/parse failure.
    #[error(transparent)]
    Protocol(#[from] AgentProtocolError),
    /// A provider failure, with classification evidence.
    #[error("{evidence:?}: {error}")]
    Provider {
        /// The provider failure.
        error: ProviderError,
        /// How and where it was classified.
        evidence: Classification,
    },
    /// The child exited non-zero with no recognised provider error. Boxed to keep
    /// `Result<_, AgentExecutionError>` small (the carried output is bounded but
    /// non-trivial).
    #[error(transparent)]
    UnsuccessfulExit(#[from] Box<UnsuccessfulExit>),
    /// An ACP protocol failure.
    #[error(transparent)]
    Acp(#[from] AcpError),
    /// Execution was cancelled via the cancellation token.
    #[error("execution cancelled")]
    Cancelled,
    /// The execution deadline was exceeded.
    #[error("execution deadline exceeded after {duration:?}")]
    DeadlineExceeded {
        /// The configured deadline duration.
        duration: Duration,
    },
    /// The per-scope concurrency queue was exhausted.
    #[error("concurrency limit reached and queue deadline exceeded for {scope}")]
    ConcurrencyExhausted {
        /// The execution scope that was saturated.
        scope: String,
    },
    /// The circuit breaker is open for this scope.
    #[error("circuit open for {scope} until {until:?}")]
    CircuitOpen {
        /// The execution scope with the open breaker.
        scope: String,
        /// When the breaker will allow another probe.
        until: Instant,
    },
}

impl AgentExecutionError {
    /// Returns the retryability aggregated across the error's layers.
    #[must_use]
    pub fn retryability(&self) -> Retryability {
        match self {
            Self::Process(p) => p.retryability(),
            Self::Protocol(
                AgentProtocolError::FrameTooLong { .. } | AgentProtocolError::InvalidUtf8,
            ) => {
                // Transient-ish but not provider; do not retry by default.
                Retryability::Permanent
            }
            Self::Protocol(AgentProtocolError::Malformed(_)) => Retryability::Permanent,
            Self::Provider { error, .. } => error.retryability(),
            Self::UnsuccessfulExit(_) => Retryability::Permanent,
            Self::Acp(_) => Retryability::Permanent,
            Self::Cancelled => Retryability::Permanent,
            Self::DeadlineExceeded { .. } => Retryability::Permanent,
            Self::ConcurrencyExhausted { .. } => Retryability::Permanent,
            Self::CircuitOpen { .. } => Retryability::Permanent,
        }
    }

    /// Returns `true` if the executable was not found (the only case warranting
    /// a "binary may not be installed" hint).
    #[must_use]
    pub fn is_executable_not_found(&self) -> bool {
        match self {
            Self::Process(p) => p.is_executable_not_found(),
            _ => false,
        }
    }
}
