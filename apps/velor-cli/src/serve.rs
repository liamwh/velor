//! Telegram control-plane runtime for `vel serve`.
//!
//! This module provides a transport-adapter style server mode:
//! - Telegram polling and update normalization (transport layer)
//! - Prefix/command routing to runner profiles (routing layer)
//! - Provider execution dispatch via runner profiles (execution layer)
//! - Structured lifecycle replies back to Telegram (interaction layer)

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use clap::{ArgAction, Args};
use color_eyre::eyre::{WrapErr, eyre};
use dirs::home_dir;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use teloxide::Bot;
use teloxide::errors::AsResponseParameters;
use teloxide::net::Download;
use teloxide::payloads::{
    EditMessageTextSetters, GetUpdatesSetters, SendMessageSetters, SetMessageReactionSetters,
};
use teloxide::prelude::Requester;
use teloxide::requests::Request;
use teloxide::types::{
    AllowedUpdate, ChatId, Message, MessageId, ParseMode, PhotoSize, ReactionType, ReplyParameters,
    Update, UpdateKind,
};
use teloxide::{ApiError, RequestError};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::AbortHandle;
use tokio::time::MissedTickBehavior;
use tracing::{error, info, warn};

use velor_core::{
    FileConfig,
    config::{CodexConfig, CodexReasoningEffort, Defaults, TelegramParseMode},
};

/// Arguments for the `serve` subcommand.
#[derive(Debug, Clone, Args)]
pub struct ServeArgs {
    /// Override config path (defaults to {git_root}/.velor/velor.toml).
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Working directory used for agent executions.
    #[arg(long)]
    pub cwd: Option<PathBuf>,

    /// Override Telegram long-poll timeout in seconds.
    #[arg(long)]
    pub poll_timeout_secs: Option<u64>,

    /// Override maximum Telegram updates per poll.
    #[arg(long)]
    pub poll_limit: Option<u8>,

    /// Process existing backlog on startup.
    #[arg(long, action = ArgAction::SetTrue)]
    pub include_backlog: bool,

    /// Legacy fallback prefix route (maps to default runner when provided).
    #[arg(long)]
    pub trigger_prefix: Option<String>,
}

/// Root config for `[serve]` section from TOML.
#[derive(Debug, Clone, Deserialize, Default)]
struct ServeFileConfig {
    #[serde(default)]
    serve: ServeConfigRaw,
}

/// Raw `[serve]` section with merge-friendly optional fields.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct ServeConfigRaw {
    enabled: Option<bool>,
    poll_timeout_secs: Option<u64>,
    poll_limit: Option<u8>,
    include_backlog: Option<bool>,
    max_requests_per_minute: Option<u32>,
    max_concurrent_tasks: Option<usize>,
    default_timeout_secs: Option<u64>,
    // Legacy fallback for streaming throttle; superseded by [serve.streaming].
    progress_update_interval_secs: Option<u64>,
    media_dir: Option<PathBuf>,
    streaming: ServeStreamingRaw,
    presentation: ServePresentationRaw,
    telegram: ServeTelegramRaw,
    attachments: ServeAttachmentPolicyRaw,
    session_resume: ServeSessionResumeRaw,
    routing: ServeRoutingRaw,
    runners: BTreeMap<String, RunnerProfileRaw>,
}

impl ServeConfigRaw {
    fn merge_from(&mut self, overlay: Self) {
        if overlay.enabled.is_some() {
            self.enabled = overlay.enabled;
        }
        if overlay.poll_timeout_secs.is_some() {
            self.poll_timeout_secs = overlay.poll_timeout_secs;
        }
        if overlay.poll_limit.is_some() {
            self.poll_limit = overlay.poll_limit;
        }
        if overlay.include_backlog.is_some() {
            self.include_backlog = overlay.include_backlog;
        }
        if overlay.max_requests_per_minute.is_some() {
            self.max_requests_per_minute = overlay.max_requests_per_minute;
        }
        if overlay.max_concurrent_tasks.is_some() {
            self.max_concurrent_tasks = overlay.max_concurrent_tasks;
        }
        if overlay.default_timeout_secs.is_some() {
            self.default_timeout_secs = overlay.default_timeout_secs;
        }
        if overlay.progress_update_interval_secs.is_some() {
            self.progress_update_interval_secs = overlay.progress_update_interval_secs;
        }
        if overlay.media_dir.is_some() {
            self.media_dir = overlay.media_dir;
        }

        self.streaming.merge_from(overlay.streaming);
        self.presentation.merge_from(overlay.presentation);
        self.telegram.merge_from(overlay.telegram);
        self.attachments.merge_from(overlay.attachments);
        self.session_resume.merge_from(overlay.session_resume);
        self.routing.merge_from(overlay.routing);

        for (name, profile) in overlay.runners {
            if let Some(existing) = self.runners.remove(&name) {
                self.runners.insert(name, existing.merged_with(profile));
            } else {
                self.runners.insert(name, profile);
            }
        }
    }
}

/// Raw session-resume policy under `[serve.session_resume]`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct ServeSessionResumeRaw {
    enabled: Option<bool>,
    store_path: Option<PathBuf>,
    max_bindings: Option<usize>,
}

impl ServeSessionResumeRaw {
    fn merge_from(&mut self, overlay: Self) {
        if overlay.enabled.is_some() {
            self.enabled = overlay.enabled;
        }
        if overlay.store_path.is_some() {
            self.store_path = overlay.store_path;
        }
        if overlay.max_bindings.is_some() {
            self.max_bindings = overlay.max_bindings;
        }
    }
}

/// Raw streaming UX policy under `[serve.streaming]`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct ServeStreamingRaw {
    enabled: Option<bool>,
    edit_throttle_secs: Option<u64>,
    max_message_chars: Option<usize>,
    flush_on_milestones: Option<bool>,
}

/// Raw result-presentation policy under `[serve.presentation]`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct ServePresentationRaw {
    default_verbosity: Option<TelegramResultVerbosity>,
    max_changed_files: Option<usize>,
    max_section_chars: Option<usize>,
    include_debug_footer_on_success: Option<bool>,
    include_follow_up_hints: Option<bool>,
    path_truncation: Option<TelegramPathTruncation>,
    raw_log_dir: Option<PathBuf>,
}

impl ServePresentationRaw {
    fn merge_from(&mut self, overlay: Self) {
        if overlay.default_verbosity.is_some() {
            self.default_verbosity = overlay.default_verbosity;
        }
        if overlay.max_changed_files.is_some() {
            self.max_changed_files = overlay.max_changed_files;
        }
        if overlay.max_section_chars.is_some() {
            self.max_section_chars = overlay.max_section_chars;
        }
        if overlay.include_debug_footer_on_success.is_some() {
            self.include_debug_footer_on_success = overlay.include_debug_footer_on_success;
        }
        if overlay.include_follow_up_hints.is_some() {
            self.include_follow_up_hints = overlay.include_follow_up_hints;
        }
        if overlay.path_truncation.is_some() {
            self.path_truncation = overlay.path_truncation;
        }
        if overlay.raw_log_dir.is_some() {
            self.raw_log_dir = overlay.raw_log_dir;
        }
    }
}

impl ServeStreamingRaw {
    fn merge_from(&mut self, overlay: Self) {
        if overlay.enabled.is_some() {
            self.enabled = overlay.enabled;
        }
        if overlay.edit_throttle_secs.is_some() {
            self.edit_throttle_secs = overlay.edit_throttle_secs;
        }
        if overlay.max_message_chars.is_some() {
            self.max_message_chars = overlay.max_message_chars;
        }
        if overlay.flush_on_milestones.is_some() {
            self.flush_on_milestones = overlay.flush_on_milestones;
        }
    }
}

/// Raw Telegram security options under `[serve.telegram]`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct ServeTelegramRaw {
    allowed_chat_ids: Option<Vec<i64>>,
    allowed_user_ids: Option<Vec<i64>>,
    allow_channel_posts: Option<bool>,
}

impl ServeTelegramRaw {
    fn merge_from(&mut self, overlay: Self) {
        if overlay.allowed_chat_ids.is_some() {
            self.allowed_chat_ids = overlay.allowed_chat_ids;
        }
        if overlay.allowed_user_ids.is_some() {
            self.allowed_user_ids = overlay.allowed_user_ids;
        }
        if overlay.allow_channel_posts.is_some() {
            self.allow_channel_posts = overlay.allow_channel_posts;
        }
    }
}

/// Raw attachment policy under `[serve.attachments]`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct ServeAttachmentPolicyRaw {
    enabled: Option<bool>,
    allow_photos: Option<bool>,
    allow_documents: Option<bool>,
    max_download_bytes: Option<usize>,
    keep_files: Option<bool>,
    allowed_document_mime_prefixes: Option<Vec<String>>,
}

impl ServeAttachmentPolicyRaw {
    fn merge_from(&mut self, overlay: Self) {
        if overlay.enabled.is_some() {
            self.enabled = overlay.enabled;
        }
        if overlay.allow_photos.is_some() {
            self.allow_photos = overlay.allow_photos;
        }
        if overlay.allow_documents.is_some() {
            self.allow_documents = overlay.allow_documents;
        }
        if overlay.max_download_bytes.is_some() {
            self.max_download_bytes = overlay.max_download_bytes;
        }
        if overlay.keep_files.is_some() {
            self.keep_files = overlay.keep_files;
        }
        if overlay.allowed_document_mime_prefixes.is_some() {
            self.allowed_document_mime_prefixes = overlay.allowed_document_mime_prefixes;
        }
    }
}

/// Raw routing policy under `[serve.routing]`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct ServeRoutingRaw {
    require_prefix: Option<bool>,
    default_runner: Option<String>,
    prefixes: BTreeMap<String, String>,
}

impl ServeRoutingRaw {
    fn merge_from(&mut self, overlay: Self) {
        if overlay.require_prefix.is_some() {
            self.require_prefix = overlay.require_prefix;
        }
        if overlay.default_runner.is_some() {
            self.default_runner = overlay.default_runner;
        }
        for (prefix, runner) in overlay.prefixes {
            self.prefixes.insert(prefix, runner);
        }
    }
}

/// Supported execution runner kinds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RunnerKind {
    Claude,
    Codex,
}

/// Raw runner profile entry under `[serve.runners.<name>]`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RunnerProfileRaw {
    description: Option<String>,
    kind: Option<RunnerKind>,
    binary: Option<String>,
    args: Option<Vec<String>>,
    env: Option<BTreeMap<String, String>>,
    model: Option<String>,
    permission_mode: Option<String>,
    timeout_secs: Option<u64>,
    supports_attachments: Option<bool>,
    supports_streaming: Option<bool>,
    supports_session_resume: Option<bool>,
    codex: CodexProfileRaw,
}

impl RunnerProfileRaw {
    fn merged_with(mut self, overlay: Self) -> Self {
        if overlay.description.is_some() {
            self.description = overlay.description;
        }
        if overlay.kind.is_some() {
            self.kind = overlay.kind;
        }
        if overlay.binary.is_some() {
            self.binary = overlay.binary;
        }
        if overlay.args.is_some() {
            self.args = overlay.args;
        }
        if overlay.env.is_some() {
            self.env = overlay.env;
        }
        if overlay.model.is_some() {
            self.model = overlay.model;
        }
        if overlay.permission_mode.is_some() {
            self.permission_mode = overlay.permission_mode;
        }
        if overlay.timeout_secs.is_some() {
            self.timeout_secs = overlay.timeout_secs;
        }
        if overlay.supports_attachments.is_some() {
            self.supports_attachments = overlay.supports_attachments;
        }
        if overlay.supports_streaming.is_some() {
            self.supports_streaming = overlay.supports_streaming;
        }
        if overlay.supports_session_resume.is_some() {
            self.supports_session_resume = overlay.supports_session_resume;
        }
        self.codex.merge_from(overlay.codex);
        self
    }
}

/// Raw codex-specific runner options.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct CodexProfileRaw {
    full_auto: Option<bool>,
    sandbox: Option<String>,
    skip_git_repo_check: Option<bool>,
    progress_cursor: Option<bool>,
    reasoning_effort: Option<CodexReasoningEffort>,
    profile: Option<String>,
}

impl CodexProfileRaw {
    fn merge_from(&mut self, overlay: Self) {
        if overlay.full_auto.is_some() {
            self.full_auto = overlay.full_auto;
        }
        if overlay.sandbox.is_some() {
            self.sandbox = overlay.sandbox;
        }
        if overlay.skip_git_repo_check.is_some() {
            self.skip_git_repo_check = overlay.skip_git_repo_check;
        }
        if overlay.progress_cursor.is_some() {
            self.progress_cursor = overlay.progress_cursor;
        }
        if overlay.reasoning_effort.is_some() {
            self.reasoning_effort = overlay.reasoning_effort;
        }
        if overlay.profile.is_some() {
            self.profile = overlay.profile;
        }
    }
}

/// Resolved runtime configuration.
#[derive(Debug, Clone)]
struct ServeResolvedConfig {
    poll_timeout_secs: u64,
    poll_limit: u8,
    include_backlog: bool,
    allowed_chat_ids: HashSet<i64>,
    allowed_user_ids: HashSet<i64>,
    allow_channel_posts: bool,
    max_requests_per_minute: u32,
    max_concurrent_tasks: usize,
    streaming: TelegramStreamingConfig,
    presentation: TelegramPresentationConfig,
    media_dir: PathBuf,
    attachment_policy: ServeAttachmentPolicy,
    session_resume: SessionResumeConfig,
    routing: ServeRouting,
    runners: HashMap<String, RunnerProfile>,
}

/// Telegram live-stream rendering policy for active runs.
#[derive(Debug, Clone)]
struct TelegramStreamingConfig {
    enabled: bool,
    edit_throttle: Duration,
    max_message_chars: usize,
    flush_on_milestones: bool,
}

/// Telegram terminal-result presentation policy.
#[derive(Debug, Clone)]
struct TelegramPresentationConfig {
    default_verbosity: TelegramResultVerbosity,
    max_changed_files: usize,
    max_section_chars: usize,
    include_debug_footer_on_success: bool,
    include_follow_up_hints: bool,
    path_truncation: TelegramPathTruncation,
    raw_log_dir: PathBuf,
}

/// Terminal result verbosity tiers for Telegram output.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TelegramResultVerbosity {
    Compact,
    Standard,
    Verbose,
    Raw,
}

/// Strategy for truncating long file paths in Telegram summaries.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TelegramPathTruncation {
    Left,
    Middle,
}

/// Resolved attachment policy.
#[derive(Debug, Clone)]
struct ServeAttachmentPolicy {
    enabled: bool,
    allow_photos: bool,
    allow_documents: bool,
    max_download_bytes: usize,
    keep_files: bool,
    allowed_document_mime_prefixes: Vec<String>,
}

/// Resolved routing table.
#[derive(Debug, Clone)]
struct ServeRouting {
    require_prefix: bool,
    default_runner: Option<String>,
    routes: Vec<PrefixRoute>,
}

/// Prefix route to a runner profile.
#[derive(Debug, Clone)]
struct PrefixRoute {
    prefix: String,
    prefix_lower: String,
    runner_name: String,
}

/// Resolved runner profile.
#[derive(Debug, Clone)]
struct RunnerProfile {
    name: String,
    description: String,
    kind: RunnerKind,
    binary: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    model: Option<String>,
    permission_mode: String,
    timeout: Duration,
    supports_attachments: bool,
    supports_streaming: bool,
    supports_session_resume: bool,
    codex: CodexProfile,
}

/// Resolved session-resume persistence policy.
#[derive(Debug, Clone)]
struct SessionResumeConfig {
    enabled: bool,
    store_path: PathBuf,
    max_bindings: usize,
}

/// Resolved codex options for a profile.
#[derive(Debug, Clone, Default)]
struct CodexProfile {
    full_auto: Option<bool>,
    sandbox: Option<String>,
    skip_git_repo_check: Option<bool>,
    progress_cursor: Option<bool>,
    reasoning_effort: Option<CodexReasoningEffort>,
    profile: Option<String>,
}

/// Shared runtime context for Telegram polling and execution.
#[derive(Clone)]
struct ServeContext {
    cfg: ServeResolvedConfig,
    bot: Bot,
    defaults: Defaults,
    cwd: PathBuf,
    parse_mode: Option<TelegramParseMode>,
    replay_cache: Arc<Mutex<ReplayCache>>,
    rate_limiter: Arc<Mutex<SlidingWindowRateLimiter>>,
    concurrency: Arc<Semaphore>,
    session_store: Arc<Mutex<SessionResumeStore>>,
    session_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    active_runs: Arc<Mutex<ActiveRunRegistry>>,
    started_at: DateTime<Utc>,
}

/// In-memory replay protection cache keyed by Telegram update ID.
#[derive(Debug)]
struct ReplayCache {
    entries: VecDeque<(i32, Instant)>,
    seen: HashSet<i32>,
    ttl: Duration,
}

impl ReplayCache {
    fn new(ttl: Duration) -> Self {
        Self {
            entries: VecDeque::new(),
            seen: HashSet::new(),
            ttl,
        }
    }

    fn insert_if_new(&mut self, update_id: i32, now: Instant) -> bool {
        self.evict_expired(now);
        if self.seen.contains(&update_id) {
            return false;
        }
        self.entries.push_back((update_id, now));
        self.seen.insert(update_id);
        true
    }

    fn evict_expired(&mut self, now: Instant) {
        while let Some((id, ts)) = self.entries.front().copied() {
            if now.duration_since(ts) < self.ttl {
                break;
            }
            self.entries.pop_front();
            self.seen.remove(&id);
        }
    }
}

/// Sliding-window rate limiter keyed by actor/chat identity.
#[derive(Debug)]
struct SlidingWindowRateLimiter {
    window: Duration,
    max_events: u32,
    buckets: HashMap<String, VecDeque<Instant>>,
}

/// In-memory registry of in-flight Telegram-triggered runs for reply interruption/resume.
#[derive(Debug, Default)]
struct ActiveRunRegistry {
    by_request: HashMap<String, ActiveRunEntry>,
    by_message: HashMap<SessionMessageKey, String>,
}

#[derive(Debug, Clone)]
struct ActiveRunEntry {
    request_id: String,
    chat_id: i64,
    runner_name: String,
    runner_kind: RunnerKind,
    source_user_message_id: Option<i32>,
    invocation: RunnerInvocationMetadata,
    session: Option<RunnerSessionHandle>,
    message_ids: HashSet<i32>,
    abort_handle: AbortHandle,
    started_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct ActiveRunInterrupt {
    interrupted_request_id: String,
    binding: SessionMessageBinding,
}

enum ActiveRunReplyResolution {
    NotFound,
    MissingSession {
        request_id: String,
        runner_name: String,
    },
    Interrupted(ActiveRunInterrupt),
}

impl ActiveRunRegistry {
    fn register(&mut self, entry: ActiveRunEntry) {
        let request_id = entry.request_id.clone();
        if self.by_request.contains_key(&request_id) {
            self.remove_request(&request_id);
        }
        self.by_request.insert(request_id, entry);
    }

    fn remove_request(&mut self, request_id: &str) {
        let Some(entry) = self.by_request.remove(request_id) else {
            return;
        };
        for message_id in entry.message_ids {
            self.by_message.remove(&SessionMessageKey {
                chat_id: entry.chat_id,
                message_id,
            });
        }
    }

    fn set_session(&mut self, request_id: &str, session: RunnerSessionHandle) {
        if let Some(entry) = self.by_request.get_mut(request_id) {
            entry.session = Some(session);
        }
    }

    fn sync_messages(&mut self, request_id: &str, message_ids: &[i32]) {
        let Some(entry) = self.by_request.get_mut(request_id) else {
            return;
        };
        for message_id in message_ids {
            if entry.message_ids.insert(*message_id) {
                self.by_message.insert(
                    SessionMessageKey {
                        chat_id: entry.chat_id,
                        message_id: *message_id,
                    },
                    request_id.to_string(),
                );
            }
        }
    }

    fn interrupt_for_reply(
        &mut self,
        chat_id: i64,
        replied_to_message_id: i32,
    ) -> ActiveRunReplyResolution {
        let key = SessionMessageKey {
            chat_id,
            message_id: replied_to_message_id,
        };
        let Some(request_id) = self.by_message.get(&key).cloned() else {
            return ActiveRunReplyResolution::NotFound;
        };
        let Some(entry) = self.by_request.get(&request_id).cloned() else {
            self.by_message.remove(&key);
            return ActiveRunReplyResolution::NotFound;
        };

        let Some(session) = entry.session.clone() else {
            return ActiveRunReplyResolution::MissingSession {
                request_id,
                runner_name: entry.runner_name,
            };
        };

        entry.abort_handle.abort();
        self.remove_request(&entry.request_id);

        let now = Utc::now();
        ActiveRunReplyResolution::Interrupted(ActiveRunInterrupt {
            interrupted_request_id: entry.request_id.clone(),
            binding: SessionMessageBinding {
                chat_id: entry.chat_id,
                message_id: replied_to_message_id,
                runner_name: entry.runner_name,
                runner_kind: entry.runner_kind,
                session,
                invocation: entry.invocation,
                request_id: entry.request_id,
                source_user_message_id: entry.source_user_message_id,
                created_at: entry.started_at,
                updated_at: now,
            },
        })
    }
}

impl SlidingWindowRateLimiter {
    fn new(window: Duration, max_events: u32) -> Self {
        Self {
            window,
            max_events,
            buckets: HashMap::new(),
        }
    }

    fn allow(&mut self, key: &str, now: Instant) -> bool {
        let bucket = self.buckets.entry(key.to_string()).or_default();

        while let Some(ts) = bucket.front().copied() {
            if now.duration_since(ts) < self.window {
                break;
            }
            bucket.pop_front();
        }

        if bucket.len() as u32 >= self.max_events {
            return false;
        }

        bucket.push_back(now);
        true
    }
}

impl SessionResumeStore {
    fn load(path: PathBuf, max_bindings: usize) -> color_eyre::eyre::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).wrap_err_with(|| {
                format!(
                    "failed to create parent directory for session store at {}",
                    path.display()
                )
            })?;
        }

        let mut bindings = HashMap::new();
        if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .wrap_err_with(|| format!("failed to read session store {}", path.display()))?;
            if !raw.trim().is_empty() {
                let parsed: SessionResumeStoreFile =
                    serde_json::from_str(&raw).wrap_err_with(|| {
                        format!("failed to parse session store {}", path.display())
                    })?;
                for binding in parsed.bindings {
                    let key = SessionMessageKey {
                        chat_id: binding.chat_id,
                        message_id: binding.message_id,
                    };
                    bindings.insert(key, binding);
                }
            }
        }

        let mut store = Self {
            path,
            max_bindings: max_bindings.max(64),
            bindings,
        };
        store.prune_to_limit();
        Ok(store)
    }

    fn lookup(&self, chat_id: i64, message_id: i32) -> Option<SessionMessageBinding> {
        let key = SessionMessageKey {
            chat_id,
            message_id,
        };
        self.bindings.get(&key).cloned()
    }

    fn upsert_bindings(
        &mut self,
        template: SessionMessageBinding,
        message_ids: &[i32],
    ) -> color_eyre::eyre::Result<usize> {
        if message_ids.is_empty() {
            return Ok(0);
        }

        let now = Utc::now();
        for message_id in message_ids {
            let key = SessionMessageKey {
                chat_id: template.chat_id,
                message_id: *message_id,
            };
            let mut binding = template.clone();
            binding.message_id = *message_id;
            if let Some(existing) = self.bindings.get(&key) {
                binding.created_at = existing.created_at;
            }
            binding.updated_at = now;
            self.bindings.insert(key, binding);
        }

        self.prune_to_limit();
        self.persist()?;
        Ok(message_ids.len())
    }

    fn prune_to_limit(&mut self) {
        if self.bindings.len() <= self.max_bindings {
            return;
        }

        let mut ordered: Vec<_> = self.bindings.values().cloned().collect();
        ordered.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        let remove_count = ordered.len().saturating_sub(self.max_bindings);
        for binding in ordered.into_iter().take(remove_count) {
            let key = SessionMessageKey {
                chat_id: binding.chat_id,
                message_id: binding.message_id,
            };
            self.bindings.remove(&key);
        }
    }

    fn persist(&self) -> color_eyre::eyre::Result<()> {
        let mut bindings: Vec<_> = self.bindings.values().cloned().collect();
        bindings.sort_by(|a, b| {
            a.chat_id
                .cmp(&b.chat_id)
                .then_with(|| a.message_id.cmp(&b.message_id))
        });

        let payload = SessionResumeStoreFile {
            version: 1,
            bindings,
        };
        let serialized = serde_json::to_string_pretty(&payload)
            .wrap_err("failed to serialize session resume store")?;

        let tmp_path = self.path.with_extension("tmp");
        std::fs::write(&tmp_path, serialized).wrap_err_with(|| {
            format!("failed writing temp session store {}", tmp_path.display())
        })?;
        std::fs::rename(&tmp_path, &self.path).wrap_err_with(|| {
            format!(
                "failed replacing session store {} with {}",
                self.path.display(),
                tmp_path.display()
            )
        })?;
        Ok(())
    }
}

/// Source metadata for transport-originated execution requests.
#[derive(Debug, Clone)]
struct ExecutionSource {
    transport: &'static str,
    chat_id: i64,
    user_id: Option<i64>,
    message_id: Option<i32>,
    update_id: i32,
}

/// Non-transport-specific request model for runner execution.
#[derive(Debug, Clone)]
struct ExecutionRequest {
    request_id: String,
    received_at: DateTime<Utc>,
    source: ExecutionSource,
    runner_name: String,
    mode: ExecutionMode,
    prompt: String,
    attachment_refs: Vec<AttachmentRef>,
    attachments: Vec<DownloadedAttachment>,
}

/// Conversation execution mode.
#[derive(Debug, Clone)]
enum ExecutionMode {
    New,
    Resume { binding: SessionMessageBinding },
}

impl ExecutionMode {
    fn resume_binding(&self) -> Option<&SessionMessageBinding> {
        match self {
            Self::New => None,
            Self::Resume { binding } => Some(binding),
        }
    }
}

/// Attachment kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachmentKind {
    Photo,
    Document,
}

impl AttachmentKind {
    fn label(self) -> &'static str {
        match self {
            Self::Photo => "photo",
            Self::Document => "document",
        }
    }
}

/// Attachment metadata extracted from Telegram update.
#[derive(Debug, Clone)]
struct AttachmentRef {
    kind: AttachmentKind,
    file_id: String,
    file_unique_id: String,
    width: Option<u32>,
    height: Option<u32>,
    file_size_hint: Option<u64>,
    file_name_hint: Option<String>,
    mime_type_hint: Option<String>,
}

/// Downloaded and validated attachment.
#[derive(Debug, Clone)]
struct DownloadedAttachment {
    kind: AttachmentKind,
    file_id: String,
    file_unique_id: String,
    path: PathBuf,
    mime_type: String,
    width: Option<u32>,
    height: Option<u32>,
    file_size: usize,
    file_name: Option<String>,
}

/// Parsed inbound Telegram message normalized for routing.
#[derive(Debug, Clone)]
struct InboundMessage {
    update_id: i32,
    chat_id: i64,
    user_id: Option<i64>,
    message_id: i32,
    replied_to_message_id: Option<i32>,
    replied_to_is_bot_message: bool,
    text: Option<String>,
    attachments: Vec<AttachmentRef>,
}

/// Supported slash commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlCommand {
    Help,
    Models,
    Status,
}

/// Router output.
#[derive(Debug, Clone)]
enum RouteResult {
    Ignore,
    Usage(String),
    Control(ControlCommand),
    Execute(PendingExecution),
}

/// Pending execution produced by the router.
#[derive(Debug, Clone)]
struct PendingExecution {
    runner_name: String,
    mode: ExecutionMode,
    prompt: String,
}

/// Execution result from a runner.
#[derive(Debug, Clone)]
struct RunnerExecution {
    stdout: String,
    stderr: String,
    session: Option<RunnerSessionHandle>,
}

/// Runner session handle for native resume.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "runner_kind", rename_all = "snake_case")]
enum RunnerSessionHandle {
    Codex { session_id: String },
    Claude { session_id: String },
}

impl RunnerSessionHandle {
    fn session_id(&self) -> &str {
        match self {
            Self::Codex { session_id } | Self::Claude { session_id } => session_id,
        }
    }

    fn runner_kind(&self) -> RunnerKind {
        match self {
            Self::Codex { .. } => RunnerKind::Codex,
            Self::Claude { .. } => RunnerKind::Claude,
        }
    }
}

/// Runner invocation metadata required for durable session resumption.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct RunnerInvocationMetadata {
    binary: String,
    permission_mode: String,
    model: Option<String>,
    codex_profile: Option<String>,
    codex_reasoning_effort: Option<CodexReasoningEffort>,
}

/// Persisted mapping from Telegram message to resumable runner session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SessionMessageBinding {
    chat_id: i64,
    message_id: i32,
    runner_name: String,
    runner_kind: RunnerKind,
    session: RunnerSessionHandle,
    invocation: RunnerInvocationMetadata,
    request_id: String,
    source_user_message_id: Option<i32>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// Durable on-disk file format for session bindings.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionResumeStoreFile {
    version: u32,
    bindings: Vec<SessionMessageBinding>,
}

impl Default for SessionResumeStoreFile {
    fn default() -> Self {
        Self {
            version: 1,
            bindings: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SessionMessageKey {
    chat_id: i64,
    message_id: i32,
}

/// Persistent, in-memory indexed store for Telegram->session bindings.
#[derive(Debug)]
struct SessionResumeStore {
    path: PathBuf,
    max_bindings: usize,
    bindings: HashMap<SessionMessageKey, SessionMessageBinding>,
}

/// Basic runner probe status.
#[derive(Debug, Clone)]
struct RunnerProbe {
    available: bool,
    detail: String,
}

/// Terminal run outcome presented to Telegram.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RunTerminalState {
    Completed,
    Failed,
    TimedOut,
    Cancelled,
}

/// Normalized terminal result state used by Telegram summary rendering.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ExecutionSummaryStatus {
    Completed,
    Failed,
    Cancelled,
    Partial,
}

impl ExecutionSummaryStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Partial => "partial",
        }
    }

    fn emoji(self) -> &'static str {
        match self {
            Self::Completed => "✅",
            Self::Failed => "❌",
            Self::Cancelled => "🛑",
            Self::Partial => "⚠️",
        }
    }
}

impl From<RunTerminalState> for ExecutionSummaryStatus {
    fn from(value: RunTerminalState) -> Self {
        match value {
            RunTerminalState::Completed => Self::Completed,
            RunTerminalState::Failed => Self::Failed,
            RunTerminalState::Cancelled => Self::Cancelled,
            RunTerminalState::TimedOut => Self::Partial,
        }
    }
}

/// Machine-readable request metadata retained outside Telegram compact rendering.
#[derive(Debug, Clone, Serialize)]
struct ExecutionRawRequestMetadata {
    request_id: String,
    transport: String,
    chat_id: i64,
    user_id: Option<i64>,
    source_message_id: Option<i32>,
    update_id: i32,
    runner_name: String,
    runner_kind: RunnerKind,
    mode: &'static str,
    attachment_count: usize,
    received_at: DateTime<Utc>,
}

/// Optional diagnostic details for operator debugging.
#[derive(Debug, Clone, Serialize, Default)]
struct ExecutionDiagnostics {
    first_error: Option<String>,
    stderr_preview: Option<String>,
    raw_log_path: Option<String>,
}

/// Normalized summary model rendered into Telegram presentation tiers.
#[derive(Debug, Clone, Serialize)]
struct ExecutionResultSummary {
    title: String,
    final_status: ExecutionSummaryStatus,
    runner_name: String,
    repo_name: Option<String>,
    branch: Option<String>,
    duration_secs: u64,
    high_level_summary: Vec<String>,
    tool_activity: Vec<String>,
    changed_files: Vec<String>,
    verification_steps: Vec<String>,
    notable_outputs: Vec<String>,
    next_actions: Vec<String>,
    raw_request_metadata: ExecutionRawRequestMetadata,
    diagnostics: Option<ExecutionDiagnostics>,
}

/// Timestamped runner progress event retained for raw logs and semantic summary building.
#[derive(Debug, Clone, Serialize)]
struct RunnerProgressEventRecord {
    at: DateTime<Utc>,
    event: RunnerProgressEvent,
}

/// Captures detailed runner events and extracts semantic signals for terminal summaries.
#[derive(Debug, Clone, Default)]
struct ExecutionResultCollector {
    events: Vec<RunnerProgressEventRecord>,
    statuses: Vec<String>,
    milestones: Vec<String>,
    errors: Vec<String>,
    output: String,
    tool_commands: Vec<String>,
    verification_steps: Vec<String>,
}

impl ExecutionResultCollector {
    fn ingest(&mut self, event: &RunnerProgressEvent) {
        self.events.push(RunnerProgressEventRecord {
            at: Utc::now(),
            event: event.clone(),
        });

        match event {
            RunnerProgressEvent::Status(message) => {
                self.statuses.push(message.clone());
            }
            RunnerProgressEvent::Milestone(message) => {
                self.milestones.push(message.clone());
                self.capture_tool_and_verification_signal(message);
            }
            RunnerProgressEvent::OutputDelta(delta) => {
                push_limited(&mut self.output, delta, 150_000);
            }
            RunnerProgressEvent::SessionBound { .. } => {}
            RunnerProgressEvent::Error(message) => {
                self.errors.push(message.clone());
            }
        }
    }

    fn capture_tool_and_verification_signal(&mut self, milestone: &str) {
        if let Some(command) = parse_tool_command_from_milestone(milestone) {
            self.tool_commands.push(command.clone());
            if is_verification_command(&command) {
                self.verification_steps.push(normalize_whitespace(&command));
            }
        }
    }
}

/// Durable raw execution record preserved outside compact Telegram summaries.
#[derive(Debug, Clone, Serialize)]
struct PersistedExecutionRecord {
    schema_version: u32,
    request: ExecutionRawRequestMetadata,
    runner_description: String,
    terminal_state: RunTerminalState,
    duration_secs: u64,
    prompt: String,
    attachments: Vec<PersistedAttachmentRecord>,
    progress_events: Vec<RunnerProgressEventRecord>,
    stdout: String,
    stderr: String,
    summary: ExecutionResultSummary,
}

/// Persisted attachment detail for raw execution records.
#[derive(Debug, Clone, Serialize)]
struct PersistedAttachmentRecord {
    kind: &'static str,
    mime_type: String,
    size_bytes: usize,
    width: Option<u32>,
    height: Option<u32>,
    file_name: Option<String>,
    path: String,
}

/// Git snapshot captured before/after a run to report file-change deltas.
#[derive(Debug, Clone, Default)]
struct GitSnapshot {
    repo_root: Option<PathBuf>,
    repo_name: Option<String>,
    branch: Option<String>,
    porcelain_status: BTreeMap<String, String>,
}

impl RunTerminalState {
    fn label(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Normalized runner progress event consumed by transport presenters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RunnerProgressEvent {
    Status(String),
    Milestone(String),
    OutputDelta(String),
    SessionBound { session: RunnerSessionHandle },
    Error(String),
}

impl RunnerProgressEvent {
    fn is_milestone(&self) -> bool {
        matches!(
            self,
            Self::Milestone(_) | Self::SessionBound { .. } | Self::Error(_)
        )
    }
}

const TELEGRAM_STREAM_RENDERED_OUTPUT_EMPTY: &str = "(waiting for output...)";
const TELEGRAM_STREAM_CONTINUED_NOTE: &str = "[continued in next message]";
const TELEGRAM_MAX_TEXT_HARD_LIMIT: usize = 4096;

/// Throttle policy for Telegram message edits.
#[derive(Debug, Clone)]
struct TelegramEditScheduler {
    throttle: Duration,
    last_flush_at: Option<Instant>,
}

impl TelegramEditScheduler {
    fn new(throttle: Duration) -> Self {
        Self {
            throttle,
            last_flush_at: None,
        }
    }

    fn should_flush(&self, force: bool, now: Instant) -> bool {
        if force {
            return true;
        }
        self.last_flush_at
            .map(|ts| now.duration_since(ts) >= self.throttle)
            .unwrap_or(true)
    }

    fn mark_flushed(&mut self, now: Instant) {
        self.last_flush_at = Some(now);
    }
}

/// Chunking strategy for Telegram edit/send payloads.
#[derive(Debug, Clone)]
struct TelegramMessageChunker {
    max_message_chars: usize,
}

impl TelegramMessageChunker {
    fn new(max_message_chars: usize) -> Self {
        Self {
            max_message_chars: max_message_chars.clamp(512, TELEGRAM_MAX_TEXT_HARD_LIMIT - 32),
        }
    }

    fn message_fits(&self, text: &str, parse_mode: Option<TelegramParseMode>) -> bool {
        let (formatted, _) = format_outbound_message(text, parse_mode);
        formatted.len() <= self.max_message_chars
    }

    fn split_output(
        &self,
        renderer: &TelegramStreamRenderer,
        parse_mode: Option<TelegramParseMode>,
        part_number: usize,
        output: &str,
    ) -> Option<usize> {
        if output.is_empty() {
            return None;
        }

        let mut fit_end = output.len();
        loop {
            let candidate = renderer.render(part_number, &output[..fit_end], true);
            if self.message_fits(&candidate, parse_mode) {
                break;
            }
            if fit_end == 0 {
                return None;
            }
            let reduced = output.floor_char_boundary((fit_end * 8) / 10);
            fit_end = if reduced < fit_end {
                reduced
            } else {
                previous_char_boundary(output, fit_end)?
            };
        }

        Some(select_natural_split(&output[..fit_end]))
    }

    fn force_split_output(&self, output: &str) -> Option<usize> {
        if output.len() <= 1 {
            return None;
        }
        let mut pivot = output.floor_char_boundary(output.len() / 2);
        if pivot == 0 {
            pivot = output.floor_char_boundary(output.len().saturating_sub(1));
        }
        if pivot == 0 {
            return None;
        }
        Some(select_natural_split(&output[..pivot]))
    }

    fn tighten_after_too_long(&mut self) {
        self.max_message_chars = self.max_message_chars.saturating_sub(200).max(300);
    }
}

/// Presentation model that turns runner events into readable Telegram text.
#[derive(Debug, Clone)]
struct TelegramStreamRenderer {
    request_id: String,
    runner_name: String,
    status_line: String,
    milestones: VecDeque<String>,
    output: String,
    terminal_state: Option<RunTerminalState>,
    final_summary: Option<String>,
    final_render_override: Option<String>,
}

impl TelegramStreamRenderer {
    fn new(request_id: String, runner_name: String) -> Self {
        Self {
            request_id,
            runner_name,
            status_line: "starting".to_string(),
            milestones: VecDeque::new(),
            output: String::new(),
            terminal_state: None,
            final_summary: None,
            final_render_override: None,
        }
    }

    fn ingest_event(&mut self, event: RunnerProgressEvent) {
        match event {
            RunnerProgressEvent::Status(message) => {
                self.status_line = truncate_for_telegram(message.trim(), 200);
            }
            RunnerProgressEvent::Milestone(message) => {
                self.push_milestone(message);
            }
            RunnerProgressEvent::OutputDelta(delta) => {
                self.output.push_str(&delta);
            }
            RunnerProgressEvent::SessionBound { session } => {
                let session_id = truncate_for_telegram(session.session_id(), 48);
                self.push_milestone(format!("session bound: {session_id}"));
            }
            RunnerProgressEvent::Error(message) => {
                let detail = truncate_for_telegram(message.trim(), 220);
                self.status_line = format!("error: {}", detail);
                self.push_milestone(format!("error: {}", detail));
            }
        }
    }

    fn mark_terminal(
        &mut self,
        state: RunTerminalState,
        summary: String,
        render_override: Option<String>,
    ) {
        self.terminal_state = Some(state);
        self.final_summary = Some(summary);
        self.final_render_override = render_override;
        self.status_line = state.label().to_string();
    }

    fn output_tail(&self, start: usize) -> &str {
        &self.output[start..]
    }

    fn render(&self, part_number: usize, output_slice: &str, continued: bool) -> String {
        if self.terminal_state.is_some()
            && let Some(override_text) = &self.final_render_override
        {
            return override_text.clone();
        }

        let mut lines = Vec::new();
        let state = self
            .terminal_state
            .map(|s| s.label().to_string())
            .unwrap_or_else(|| "running".to_string());

        lines.push(format!("vel serve | {}", state));
        lines.push(format!("runner: {}", self.runner_name));
        lines.push(format!("request: {}", self.request_id));
        if part_number > 1 {
            lines.push(format!("part: {}", part_number));
        }
        lines.push(String::new());
        lines.push(format!("status: {}", self.status_line));

        if !self.milestones.is_empty() {
            lines.push("milestones:".to_string());
            for milestone in &self.milestones {
                lines.push(format!("- {}", milestone));
            }
        }

        lines.push(String::new());
        lines.push("output:".to_string());
        if output_slice.trim().is_empty() {
            lines.push(TELEGRAM_STREAM_RENDERED_OUTPUT_EMPTY.to_string());
        } else {
            lines.push(output_slice.to_string());
        }

        if continued {
            lines.push(String::new());
            lines.push(TELEGRAM_STREAM_CONTINUED_NOTE.to_string());
        }

        if let Some(summary) = &self.final_summary {
            lines.push(String::new());
            lines.push(format!("result: {}", summary));
        }

        lines.join("\n")
    }

    fn push_milestone(&mut self, message: String) {
        let line = truncate_for_telegram(message.trim(), 220);
        if line.is_empty() {
            return;
        }
        self.milestones.push_back(line);
        while self.milestones.len() > 6 {
            self.milestones.pop_front();
        }
    }
}

/// Mutable Telegram message handle for live send/edit updates.
#[derive(Debug, Clone)]
struct TelegramLiveMessage {
    bot: Bot,
    parse_mode: Option<TelegramParseMode>,
    chat_id: i64,
    initial_reply_to_message_id: Option<i32>,
    active_message_id: Option<i32>,
    sent_initial: bool,
    sent_message_ids: Vec<i32>,
}

impl TelegramLiveMessage {
    fn new(
        bot: Bot,
        parse_mode: Option<TelegramParseMode>,
        chat_id: i64,
        reply_to_message_id: Option<i32>,
    ) -> Self {
        Self {
            bot,
            parse_mode,
            chat_id,
            initial_reply_to_message_id: reply_to_message_id,
            active_message_id: None,
            sent_initial: false,
            sent_message_ids: Vec::new(),
        }
    }

    fn parse_mode(&self) -> Option<TelegramParseMode> {
        self.parse_mode
    }

    fn sent_message_ids(&self) -> &[i32] {
        &self.sent_message_ids
    }

    async fn upsert_active(&mut self, text: &str) -> Result<(), RequestError> {
        if let Some(message_id) = self.active_message_id {
            match edit_telegram_message(&self.bot, self.parse_mode, self.chat_id, message_id, text)
                .await
            {
                Ok(_) => return Ok(()),
                Err(err) if is_message_not_modified_error(&err) => return Ok(()),
                Err(err) if is_message_not_editable_error(&err) => {
                    warn!(
                        chat_id = self.chat_id,
                        message_id,
                        error = %err,
                        "Telegram message no longer editable; creating continuation message"
                    );
                    self.active_message_id = None;
                }
                Err(err) => return Err(err),
            }
        }

        self.send_new_message(text).await?;
        Ok(())
    }

    async fn start_new_continuation(&mut self, text: &str) -> Result<(), RequestError> {
        self.send_new_message(text).await
    }

    async fn send_new_message(&mut self, text: &str) -> Result<(), RequestError> {
        let reply_to = if self.sent_initial {
            None
        } else {
            self.initial_reply_to_message_id
        };
        let sent = send_telegram_message_internal(
            &self.bot,
            self.parse_mode,
            self.chat_id,
            reply_to,
            text,
        )
        .await?;
        self.active_message_id = Some(sent.id.0);
        self.sent_initial = true;
        self.sent_message_ids.push(sent.id.0);
        Ok(())
    }
}

/// End-to-end presenter for a single Telegram-triggered run.
#[derive(Debug, Clone)]
struct TelegramRunPresenter {
    request_id: String,
    live_message: TelegramLiveMessage,
    renderer: TelegramStreamRenderer,
    scheduler: TelegramEditScheduler,
    chunker: TelegramMessageChunker,
    streaming_enabled: bool,
    flush_on_milestones: bool,
    dirty: bool,
    active_output_start: usize,
    active_part_number: usize,
    last_rendered_text: Option<String>,
}

impl TelegramRunPresenter {
    async fn new(
        ctx: &ServeContext,
        request: &ExecutionRequest,
        profile: &RunnerProfile,
    ) -> color_eyre::eyre::Result<Self> {
        let mut presenter = Self {
            request_id: request.request_id.clone(),
            live_message: TelegramLiveMessage::new(
                ctx.bot.clone(),
                ctx.parse_mode,
                request.source.chat_id,
                request.source.message_id,
            ),
            renderer: TelegramStreamRenderer::new(request.request_id.clone(), profile.name.clone()),
            scheduler: TelegramEditScheduler::new(ctx.cfg.streaming.edit_throttle),
            chunker: TelegramMessageChunker::new(ctx.cfg.streaming.max_message_chars),
            streaming_enabled: ctx.cfg.streaming.enabled,
            flush_on_milestones: ctx.cfg.streaming.flush_on_milestones,
            dirty: true,
            active_output_start: 0,
            active_part_number: 1,
            last_rendered_text: None,
        };
        presenter
            .ingest(RunnerProgressEvent::Status(format!(
                "starting runner {}",
                profile.name
            )))
            .await;
        presenter.flush(true).await?;
        Ok(presenter)
    }

    async fn ingest(&mut self, event: RunnerProgressEvent) {
        let should_force_flush = event.is_milestone() && self.flush_on_milestones;
        self.renderer.ingest_event(event);
        self.dirty = true;
        if should_force_flush {
            let _ = self.flush(true).await;
        }
    }

    async fn tick(&mut self) {
        if self.streaming_enabled {
            let _ = self.flush(false).await;
        }
    }

    async fn finalize(
        &mut self,
        state: RunTerminalState,
        summary: String,
        render_override: Option<String>,
    ) -> color_eyre::eyre::Result<()> {
        self.renderer.mark_terminal(state, summary, render_override);
        self.dirty = true;
        self.flush(true).await
    }

    fn sent_message_ids(&self) -> &[i32] {
        self.live_message.sent_message_ids()
    }

    async fn flush(&mut self, force: bool) -> color_eyre::eyre::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let now = Instant::now();
        if !self.scheduler.should_flush(force, now) {
            return Ok(());
        }

        let mut guard = 0usize;
        loop {
            guard += 1;
            if guard > 32 {
                return Err(eyre!(
                    "request {} exceeded telegram rollover guard limit",
                    self.request_id
                ));
            }

            let output_tail = self.renderer.output_tail(self.active_output_start);
            let text = self
                .renderer
                .render(self.active_part_number, output_tail, false);
            let mut force_split_due_to_api_too_long = false;

            if self
                .chunker
                .message_fits(&text, self.live_message.parse_mode())
            {
                if self.last_rendered_text.as_deref() == Some(text.as_str()) {
                    self.scheduler.mark_flushed(now);
                    self.dirty = false;
                    return Ok(());
                }
                match self.live_message.upsert_active(&text).await {
                    Ok(()) => {
                        self.last_rendered_text = Some(text);
                        self.scheduler.mark_flushed(now);
                        self.dirty = false;
                        return Ok(());
                    }
                    Err(err) if is_message_too_long_error(&err) => {
                        self.chunker.tighten_after_too_long();
                        force_split_due_to_api_too_long = true;
                    }
                    Err(err) if is_message_not_modified_error(&err) => {
                        self.last_rendered_text = Some(text);
                        self.scheduler.mark_flushed(now);
                        self.dirty = false;
                        return Ok(());
                    }
                    Err(err) => {
                        warn!(
                            request_id = %self.request_id,
                            error = %err,
                            "Telegram live update failed"
                        );
                        self.scheduler.mark_flushed(now);
                        return Ok(());
                    }
                }
            }

            let split_at = if force_split_due_to_api_too_long {
                self.chunker.force_split_output(output_tail)
            } else {
                self.chunker.split_output(
                    &self.renderer,
                    self.live_message.parse_mode(),
                    self.active_part_number,
                    output_tail,
                )
            };
            let Some(split_at) = split_at else {
                warn!(
                    request_id = %self.request_id,
                    "Unable to split Telegram output safely; forcing compact failure message"
                );
                let fallback = format!(
                    "vel serve | failed\nrunner: {}\nrequest: {}\n\nstatus: output too large to render safely",
                    self.renderer.runner_name, self.renderer.request_id
                );
                let _ = self.live_message.upsert_active(&fallback).await;
                self.scheduler.mark_flushed(now);
                self.dirty = false;
                return Ok(());
            };

            let mut chosen_split = split_at;
            loop {
                let first_part = &output_tail[..chosen_split];
                let continued = self
                    .renderer
                    .render(self.active_part_number, first_part, true);
                match self.live_message.upsert_active(&continued).await {
                    Ok(()) => break,
                    Err(err) if is_message_not_modified_error(&err) => break,
                    Err(err) if is_message_too_long_error(&err) => {
                        self.chunker.tighten_after_too_long();
                        let smaller = self
                            .chunker
                            .force_split_output(&output_tail[..chosen_split])
                            .filter(|next| *next < chosen_split);
                        let Some(next_split) = smaller else {
                            warn!(
                                request_id = %self.request_id,
                                error = %err,
                                "Failed to publish Telegram continuation marker"
                            );
                            self.scheduler.mark_flushed(now);
                            return Ok(());
                        };
                        chosen_split = next_split;
                    }
                    Err(err) => {
                        warn!(
                            request_id = %self.request_id,
                            error = %err,
                            "Failed to publish Telegram continuation marker"
                        );
                        self.scheduler.mark_flushed(now);
                        return Ok(());
                    }
                }
            }

            self.active_output_start += chosen_split;
            self.active_part_number += 1;
            self.last_rendered_text = None;
            let placeholder = self.renderer.render(self.active_part_number, "", false);
            if let Err(err) = self.live_message.start_new_continuation(&placeholder).await {
                if is_message_too_long_error(&err) {
                    let compact = format!(
                        "vel serve | running\nrunner: {}\nrequest: {}\npart: {}\nstatus: continuing output",
                        self.renderer.runner_name,
                        self.renderer.request_id,
                        self.active_part_number
                    );
                    if let Err(compact_err) =
                        self.live_message.start_new_continuation(&compact).await
                    {
                        warn!(
                            request_id = %self.request_id,
                            error = %compact_err,
                            "Failed to create compact Telegram continuation message"
                        );
                        self.scheduler.mark_flushed(now);
                        return Ok(());
                    }
                } else {
                    warn!(
                        request_id = %self.request_id,
                        error = %err,
                        "Failed to create Telegram continuation message"
                    );
                    self.scheduler.mark_flushed(now);
                    return Ok(());
                }
            }
        }
    }
}

/// Runner abstraction for future providers.
#[async_trait(?Send)]
trait ExecutionRunner {
    async fn run(
        &self,
        request: &ExecutionRequest,
        profile: &RunnerProfile,
        defaults: &Defaults,
        cwd: &Path,
        on_progress: &mut (dyn FnMut(RunnerProgressEvent) + Send),
    ) -> color_eyre::eyre::Result<RunnerExecution>;
}

/// Process-backed runner implementation.
#[derive(Debug, Default)]
struct ProcessExecutionRunner;

#[async_trait(?Send)]
impl ExecutionRunner for ProcessExecutionRunner {
    async fn run(
        &self,
        request: &ExecutionRequest,
        profile: &RunnerProfile,
        defaults: &Defaults,
        cwd: &Path,
        on_progress: &mut (dyn FnMut(RunnerProgressEvent) + Send),
    ) -> color_eyre::eyre::Result<RunnerExecution> {
        match profile.kind {
            RunnerKind::Codex => {
                run_codex_profile(request, profile, defaults, cwd, on_progress).await
            }
            RunnerKind::Claude => run_claude_like_profile(request, profile, cwd, on_progress).await,
        }
    }
}

/// Runs `vel serve` until interrupted.
#[tracing::instrument(level = "info", skip(home_cfg), ret, err)]
pub async fn run_serve(
    args: ServeArgs,
    home_cfg: FileConfig,
    git_root: PathBuf,
    cwd: PathBuf,
) -> color_eyre::eyre::Result<()> {
    let repo_config_path = args
        .config
        .clone()
        .unwrap_or_else(|| FileConfig::default_config_path(&git_root));
    let repo_cfg = FileConfig::load_if_exists(&repo_config_path)
        .wrap_err_with(|| format!("failed to load config at {}", repo_config_path.display()))?
        .unwrap_or_default();
    let merged_cfg = FileConfig::merge(home_cfg, repo_cfg);

    let serve_cfg = ServeResolvedConfig::from_sources(&args, &merged_cfg, &git_root, &cwd)?;

    let requested_cwd = args
        .cwd
        .clone()
        .unwrap_or_else(|| resolve_default_agent_cwd(&cwd));

    let service_cwd = requested_cwd
        .canonicalize()
        .wrap_err("failed to canonicalize serve working directory")?;

    tokio::fs::create_dir_all(&serve_cfg.media_dir)
        .await
        .wrap_err("Failed to create media directory")?;
    tokio::fs::create_dir_all(&serve_cfg.presentation.raw_log_dir)
        .await
        .wrap_err("Failed to create serve raw log directory")?;

    let session_store = SessionResumeStore::load(
        serve_cfg.session_resume.store_path.clone(),
        serve_cfg.session_resume.max_bindings,
    )
    .wrap_err("failed to initialize session resume store")?;

    let tg_cfg = merged_cfg
        .notifications
        .telegram
        .clone()
        .ok_or_else(|| eyre!("Missing [notifications.telegram] config"))?;

    if !tg_cfg.enabled {
        return Err(eyre!(
            "[notifications.telegram].enabled=false; enable it before running `vel serve`"
        ));
    }

    let token_env = tg_cfg.bot_token_env.clone();
    let bot_token = std::env::var(&token_env).wrap_err_with(|| {
        format!(
            "Missing Telegram token env var '{}' referenced by [notifications.telegram].bot_token_env",
            token_env
        )
    })?;

    let mut bot = Bot::new(bot_token);
    if let Some(api_base_url) = tg_cfg.api_base_url.clone() {
        let parsed = reqwest::Url::parse(&api_base_url)
            .wrap_err_with(|| format!("invalid Telegram api_base_url: {api_base_url}"))?;
        bot = bot.set_api_url(parsed);
    }

    // Preflight runner binaries; warn but don't fail to allow partial availability.
    for profile in serve_cfg.runners.values() {
        let probe = probe_runner(profile);
        if probe.available {
            info!(
                runner = %profile.name,
                binary = %profile.binary,
                detail = %probe.detail,
                "Runner preflight OK"
            );
        } else {
            warn!(
                runner = %profile.name,
                binary = %profile.binary,
                detail = %probe.detail,
                "Runner preflight failed"
            );
        }
    }

    let ctx = Arc::new(ServeContext {
        cfg: serve_cfg.clone(),
        bot,
        defaults: merged_cfg.defaults.clone(),
        cwd: service_cwd,
        parse_mode: tg_cfg.parse_mode,
        replay_cache: Arc::new(Mutex::new(ReplayCache::new(Duration::from_secs(15 * 60)))),
        rate_limiter: Arc::new(Mutex::new(SlidingWindowRateLimiter::new(
            Duration::from_secs(60),
            serve_cfg.max_requests_per_minute,
        ))),
        concurrency: Arc::new(Semaphore::new(serve_cfg.max_concurrent_tasks)),
        session_store: Arc::new(Mutex::new(session_store)),
        session_locks: Arc::new(Mutex::new(HashMap::new())),
        active_runs: Arc::new(Mutex::new(ActiveRunRegistry::default())),
        started_at: Utc::now(),
    });

    info!(
        cwd = %ctx.cwd.display(),
        allowed_chat_ids = ?ctx.cfg.allowed_chat_ids,
        allowed_user_ids = ?ctx.cfg.allowed_user_ids,
        max_concurrent = ctx.cfg.max_concurrent_tasks,
        poll_timeout_secs = ctx.cfg.poll_timeout_secs,
        poll_limit = ctx.cfg.poll_limit,
        streaming_enabled = ctx.cfg.streaming.enabled,
        streaming_edit_throttle_secs = ctx.cfg.streaming.edit_throttle.as_secs(),
        streaming_max_message_chars = ctx.cfg.streaming.max_message_chars,
        result_verbosity = ?ctx.cfg.presentation.default_verbosity,
        result_max_changed_files = ctx.cfg.presentation.max_changed_files,
        result_max_section_chars = ctx.cfg.presentation.max_section_chars,
        result_debug_footer_on_success = ctx.cfg.presentation.include_debug_footer_on_success,
        result_follow_up_hints = ctx.cfg.presentation.include_follow_up_hints,
        result_path_truncation = ?ctx.cfg.presentation.path_truncation,
        raw_log_dir = %ctx.cfg.presentation.raw_log_dir.display(),
        session_resume_enabled = ctx.cfg.session_resume.enabled,
        session_store_path = %ctx.cfg.session_resume.store_path.display(),
        session_max_bindings = ctx.cfg.session_resume.max_bindings,
        route_count = ctx.cfg.routing.routes.len(),
        "Starting vel serve telegram polling loop"
    );

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            if !ctx.cfg.include_backlog {
                let latest = drain_backlog_offset(&ctx).await?;
                if let Some(offset) = latest {
                    info!(offset, "Skipping existing Telegram backlog");
                    polling_loop(ctx, Some(offset)).await?;
                } else {
                    polling_loop(ctx, None).await?;
                }
            } else {
                polling_loop(ctx, None).await?;
            }
            Ok::<(), color_eyre::eyre::Report>(())
        })
        .await?;

    Ok(())
}

fn resolve_default_agent_cwd(fallback: &Path) -> PathBuf {
    resolve_default_agent_cwd_from_home(fallback, home_dir().as_deref())
}

fn resolve_default_agent_cwd_from_home(fallback: &Path, home: Option<&Path>) -> PathBuf {
    match home {
        Some(home_dir) => {
            let git_dir = home_dir.join("git");
            if git_dir.is_dir() {
                git_dir
            } else {
                fallback.to_path_buf()
            }
        }
        None => fallback.to_path_buf(),
    }
}

impl ServeResolvedConfig {
    fn from_sources(
        args: &ServeArgs,
        merged_cfg: &FileConfig,
        git_root: &Path,
        cwd: &Path,
    ) -> color_eyre::eyre::Result<Self> {
        let home_path = FileConfig::home_config_path()?;
        let repo_path = args
            .config
            .clone()
            .unwrap_or_else(|| FileConfig::default_config_path(git_root));

        let home_raw = load_serve_raw(&home_path)?;
        let repo_raw = load_serve_raw(&repo_path)?;

        let mut raw = home_raw;
        raw.merge_from(repo_raw);

        let enabled = raw.enabled.unwrap_or(true);
        if !enabled {
            return Err(eyre!("[serve].enabled=false"));
        }

        let poll_timeout_secs = args
            .poll_timeout_secs
            .or(raw.poll_timeout_secs)
            .unwrap_or(10)
            .clamp(1, 15);
        let poll_limit = args
            .poll_limit
            .or(raw.poll_limit)
            .unwrap_or(50)
            .clamp(1, 100);
        let include_backlog = if args.include_backlog {
            true
        } else {
            raw.include_backlog.unwrap_or(false)
        };

        let max_requests_per_minute = raw.max_requests_per_minute.unwrap_or(20).max(1);
        let max_concurrent_tasks = raw.max_concurrent_tasks.unwrap_or(2).max(1);
        let default_timeout = Duration::from_secs(raw.default_timeout_secs.unwrap_or(1800).max(5));
        let legacy_progress_secs = raw.progress_update_interval_secs.unwrap_or(4).clamp(1, 60);
        let streaming = TelegramStreamingConfig {
            enabled: raw.streaming.enabled.unwrap_or(true),
            edit_throttle: Duration::from_secs(
                raw.streaming
                    .edit_throttle_secs
                    .unwrap_or(legacy_progress_secs)
                    .clamp(1, 10),
            ),
            max_message_chars: raw
                .streaming
                .max_message_chars
                .unwrap_or(3600)
                .clamp(512, 3900),
            flush_on_milestones: raw.streaming.flush_on_milestones.unwrap_or(true),
        };

        let presentation = TelegramPresentationConfig {
            default_verbosity: raw
                .presentation
                .default_verbosity
                .unwrap_or(TelegramResultVerbosity::Compact),
            max_changed_files: raw.presentation.max_changed_files.unwrap_or(5).clamp(1, 25),
            max_section_chars: raw
                .presentation
                .max_section_chars
                .unwrap_or(500)
                .clamp(120, 1800),
            include_debug_footer_on_success: raw
                .presentation
                .include_debug_footer_on_success
                .unwrap_or(false),
            include_follow_up_hints: raw.presentation.include_follow_up_hints.unwrap_or(true),
            path_truncation: raw
                .presentation
                .path_truncation
                .unwrap_or(TelegramPathTruncation::Left),
            raw_log_dir: match raw.presentation.raw_log_dir {
                Some(path) if path.is_absolute() => path,
                Some(path) => cwd.join(path),
                None => cwd.join(".velor/serve-run-logs"),
            },
        };

        let media_dir = match raw.media_dir {
            Some(path) if path.is_absolute() => path,
            Some(path) => cwd.join(path),
            None => std::env::temp_dir().join("velor-telegram-media"),
        };

        let tg_cfg = merged_cfg
            .notifications
            .telegram
            .as_ref()
            .ok_or_else(|| eyre!("Missing [notifications.telegram] configuration"))?;

        let mut allowed_chat_ids = parse_chat_allowlist(&tg_cfg.chat_id)?;
        if let Some(extra) = raw.telegram.allowed_chat_ids.clone() {
            allowed_chat_ids.extend(extra);
        }
        allowed_chat_ids.extend(parse_optional_allowlist_env(
            "VELOR_TELEGRAM_ALLOWED_CHAT_IDS",
        )?);

        if allowed_chat_ids.is_empty() {
            return Err(eyre!(
                "No allowed chats configured. Set [notifications.telegram].chat_id and/or [serve.telegram].allowed_chat_ids"
            ));
        }

        let mut allowed_user_ids: HashSet<i64> = HashSet::new();
        if let Some(users) = raw.telegram.allowed_user_ids.clone() {
            allowed_user_ids.extend(users);
        }
        allowed_user_ids.extend(parse_optional_allowlist_env(
            "VELOR_TELEGRAM_ALLOWED_USER_IDS",
        )?);

        let allow_channel_posts = raw.telegram.allow_channel_posts.unwrap_or(true);

        let attachment_policy = ServeAttachmentPolicy {
            enabled: raw.attachments.enabled.unwrap_or(true),
            allow_photos: raw.attachments.allow_photos.unwrap_or(true),
            allow_documents: raw.attachments.allow_documents.unwrap_or(true),
            max_download_bytes: raw
                .attachments
                .max_download_bytes
                .unwrap_or(20 * 1024 * 1024),
            keep_files: raw.attachments.keep_files.unwrap_or(false),
            allowed_document_mime_prefixes: raw
                .attachments
                .allowed_document_mime_prefixes
                .unwrap_or_else(|| {
                    vec![
                        "image/".to_string(),
                        "text/".to_string(),
                        "application/pdf".to_string(),
                        "application/json".to_string(),
                        "application/xml".to_string(),
                    ]
                }),
        };

        let session_resume = SessionResumeConfig {
            enabled: raw.session_resume.enabled.unwrap_or(true),
            store_path: match raw.session_resume.store_path {
                Some(path) if path.is_absolute() => path,
                Some(path) => cwd.join(path),
                None => cwd.join(".velor/telegram-session-bindings.json"),
            },
            max_bindings: raw.session_resume.max_bindings.unwrap_or(10_000).max(128),
        };

        let mut merged_runners = default_runner_profiles();
        for (name, profile) in raw.runners {
            if let Some(existing) = merged_runners.remove(&name) {
                merged_runners.insert(name, existing.merged_with(profile));
            } else {
                merged_runners.insert(name, profile);
            }
        }

        let mut runners: HashMap<String, RunnerProfile> = HashMap::new();
        for (name, profile_raw) in merged_runners {
            let mut profile = profile_raw.resolve(
                &name,
                &default_timeout,
                merged_cfg
                    .defaults
                    .permission_mode
                    .clone()
                    .unwrap_or_else(|| "acceptEdits".to_string()),
            )?;
            apply_reserved_runner_overrides(&name, &mut profile);
            runners.insert(name, profile);
        }

        if runners.is_empty() {
            return Err(eyre!("No serve runner profiles resolved"));
        }

        let mut raw_prefixes = default_prefix_routes();
        for (prefix, runner) in raw.routing.prefixes {
            raw_prefixes.insert(prefix, runner);
        }

        if let Some(legacy) = args
            .trigger_prefix
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            raw_prefixes.insert(legacy.to_string(), "codex-gpt-5-4".to_string());
        }

        let require_prefix = raw.routing.require_prefix.unwrap_or(true);
        let default_runner = raw.routing.default_runner.clone();

        let mut routes = Vec::new();
        for (prefix, runner_name) in raw_prefixes {
            let trimmed = prefix.trim();
            if trimmed.is_empty() {
                return Err(eyre!(
                    "Empty prefix in [serve.routing.prefixes] is not allowed"
                ));
            }
            if !runners.contains_key(&runner_name) {
                return Err(eyre!(
                    "Prefix '{}' points to unknown runner profile '{}'.",
                    trimmed,
                    runner_name
                ));
            }
            routes.push(PrefixRoute {
                prefix: trimmed.to_string(),
                prefix_lower: trimmed.to_ascii_lowercase(),
                runner_name,
            });
        }
        routes.sort_by(|a, b| b.prefix.len().cmp(&a.prefix.len()));

        if let Some(default_runner_name) = &default_runner
            && !runners.contains_key(default_runner_name)
        {
            return Err(eyre!(
                "[serve.routing].default_runner '{}' not found in [serve.runners]",
                default_runner_name
            ));
        }

        Ok(Self {
            poll_timeout_secs,
            poll_limit,
            include_backlog,
            allowed_chat_ids,
            allowed_user_ids,
            allow_channel_posts,
            max_requests_per_minute,
            max_concurrent_tasks,
            streaming,
            presentation,
            media_dir,
            attachment_policy,
            session_resume,
            routing: ServeRouting {
                require_prefix,
                default_runner,
                routes,
            },
            runners,
        })
    }
}

impl RunnerProfileRaw {
    fn resolve(
        self,
        name: &str,
        default_timeout: &Duration,
        default_permission_mode: String,
    ) -> color_eyre::eyre::Result<RunnerProfile> {
        let kind = self
            .kind
            .ok_or_else(|| eyre!("Runner '{}' missing required field: kind", name))?;
        let binary = self
            .binary
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| eyre!("Runner '{}' missing required field: binary", name))?;
        let timeout = Duration::from_secs(
            self.timeout_secs
                .unwrap_or(default_timeout.as_secs())
                .max(5),
        );

        let description = self.description.unwrap_or_else(|| match kind {
            RunnerKind::Claude => "Claude-compatible runner".to_string(),
            RunnerKind::Codex => "Codex runner".to_string(),
        });

        Ok(RunnerProfile {
            name: name.to_string(),
            description,
            kind,
            binary,
            args: self.args.unwrap_or_default(),
            env: self.env.unwrap_or_default(),
            model: self.model,
            permission_mode: self.permission_mode.unwrap_or(default_permission_mode),
            timeout,
            supports_attachments: self.supports_attachments.unwrap_or(false),
            supports_streaming: self
                .supports_streaming
                .unwrap_or(matches!(kind, RunnerKind::Codex)),
            supports_session_resume: self
                .supports_session_resume
                .unwrap_or(matches!(kind, RunnerKind::Claude | RunnerKind::Codex)),
            codex: CodexProfile {
                full_auto: self.codex.full_auto,
                sandbox: self.codex.sandbox,
                skip_git_repo_check: self.codex.skip_git_repo_check,
                progress_cursor: self.codex.progress_cursor,
                reasoning_effort: self.codex.reasoning_effort,
                profile: self.codex.profile,
            },
        })
    }
}

fn load_serve_raw(path: &Path) -> color_eyre::eyre::Result<ServeConfigRaw> {
    if !path.exists() {
        return Ok(ServeConfigRaw::default());
    }
    let raw = std::fs::read_to_string(path)
        .wrap_err_with(|| format!("failed to read serve config from {}", path.display()))?;
    let parsed: ServeFileConfig = toml::from_str(&raw)
        .wrap_err_with(|| format!("failed to parse serve config in {}", path.display()))?;
    Ok(parsed.serve)
}

fn default_runner_profiles() -> BTreeMap<String, RunnerProfileRaw> {
    let mut map = BTreeMap::new();

    map.insert(
        "claude-opus-4-6".to_string(),
        RunnerProfileRaw {
            description: Some("Claude Opus 4.6".to_string()),
            kind: Some(RunnerKind::Claude),
            binary: Some("claude".to_string()),
            args: Some(Vec::new()),
            env: Some(BTreeMap::new()),
            model: Some("claude-opus-4-6".to_string()),
            permission_mode: Some("acceptEdits".to_string()),
            timeout_secs: Some(1800),
            supports_attachments: Some(false),
            supports_streaming: Some(false),
            supports_session_resume: Some(true),
            codex: CodexProfileRaw::default(),
        },
    );

    map.insert(
        "claude-sonnet-4-6".to_string(),
        RunnerProfileRaw {
            description: Some("Claude Sonnet 4.6".to_string()),
            kind: Some(RunnerKind::Claude),
            binary: Some("claude".to_string()),
            args: Some(Vec::new()),
            env: Some(BTreeMap::new()),
            model: Some("claude-sonnet-4-6".to_string()),
            permission_mode: Some("acceptEdits".to_string()),
            timeout_secs: Some(1800),
            supports_attachments: Some(false),
            supports_streaming: Some(false),
            supports_session_resume: Some(true),
            codex: CodexProfileRaw::default(),
        },
    );

    map.insert(
        "codex-gpt-5-3-codex".to_string(),
        RunnerProfileRaw {
            description: Some("Codex GPT-5.3 Codex".to_string()),
            kind: Some(RunnerKind::Codex),
            binary: Some("codex".to_string()),
            args: Some(Vec::new()),
            env: Some(BTreeMap::new()),
            model: Some("gpt-5.3-codex".to_string()),
            permission_mode: Some("acceptEdits".to_string()),
            timeout_secs: Some(1800),
            supports_attachments: Some(true),
            supports_streaming: Some(true),
            supports_session_resume: Some(true),
            codex: CodexProfileRaw {
                full_auto: Some(true),
                sandbox: Some("danger-full-access".to_string()),
                skip_git_repo_check: Some(true),
                progress_cursor: Some(false),
                reasoning_effort: Some(CodexReasoningEffort::Xhigh),
                profile: None,
            },
        },
    );

    map.insert(
        "codex-gpt-5-4".to_string(),
        RunnerProfileRaw {
            description: Some("Codex GPT-5.4".to_string()),
            kind: Some(RunnerKind::Codex),
            binary: Some("codex".to_string()),
            args: Some(Vec::new()),
            env: Some(BTreeMap::new()),
            model: Some("gpt-5.4".to_string()),
            permission_mode: Some("acceptEdits".to_string()),
            timeout_secs: Some(1800),
            supports_attachments: Some(true),
            supports_streaming: Some(true),
            supports_session_resume: Some(true),
            codex: CodexProfileRaw {
                full_auto: Some(true),
                sandbox: Some("danger-full-access".to_string()),
                skip_git_repo_check: Some(true),
                progress_cursor: Some(false),
                reasoning_effort: None,
                profile: None,
            },
        },
    );

    map.insert(
        "glm-5-1".to_string(),
        RunnerProfileRaw {
            description: Some("GLM 5.1 via claude-compatible wrapper".to_string()),
            kind: Some(RunnerKind::Claude),
            binary: Some("/Users/liam/bin/glm5.1".to_string()),
            args: Some(Vec::new()),
            env: Some(BTreeMap::new()),
            model: Some("glm-5.1".to_string()),
            permission_mode: Some("acceptEdits".to_string()),
            timeout_secs: Some(1800),
            supports_attachments: Some(false),
            supports_streaming: Some(false),
            supports_session_resume: Some(true),
            codex: CodexProfileRaw::default(),
        },
    );

    map
}

fn default_prefix_routes() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("opus:".to_string(), "claude-opus-4-6".to_string()),
        ("sonnet:".to_string(), "claude-sonnet-4-6".to_string()),
        ("5.3-codex:".to_string(), "codex-gpt-5-3-codex".to_string()),
        ("5.3:".to_string(), "codex-gpt-5-3-codex".to_string()),
        ("5.4:".to_string(), "codex-gpt-5-4".to_string()),
        ("glm5.1:".to_string(), "glm-5-1".to_string()),
        ("5.1:".to_string(), "glm-5-1".to_string()),
        ("codex:".to_string(), "codex-gpt-5-4".to_string()),
    ])
}

async fn fetch_updates(
    ctx: &ServeContext,
    offset: Option<i32>,
    timeout_secs: u64,
    limit: u8,
) -> color_eyre::eyre::Result<Vec<Update>> {
    let mut request = ctx
        .bot
        .get_updates()
        .timeout(u32::try_from(timeout_secs).unwrap_or(u32::MAX))
        .limit(limit)
        .allowed_updates(vec![
            AllowedUpdate::Message,
            AllowedUpdate::EditedMessage,
            AllowedUpdate::ChannelPost,
        ]);

    if let Some(offset) = offset {
        request = request.offset(offset);
    }

    request.send().await.wrap_err("Telegram getUpdates failed")
}

async fn drain_backlog_offset(ctx: &Arc<ServeContext>) -> color_eyre::eyre::Result<Option<i32>> {
    let mut next_offset: Option<i32> = None;

    loop {
        let updates = fetch_updates(ctx, next_offset, 0, 100)
            .await
            .wrap_err("failed to read initial Telegram backlog")?;

        if updates.is_empty() {
            return Ok(next_offset);
        }

        if let Some(last) = updates.last() {
            let update_id = safe_update_id(last.id.0);
            next_offset = Some(update_id.saturating_add(1));
        }

        if updates.len() < 100 {
            return Ok(next_offset);
        }
    }
}

async fn polling_loop(
    ctx: Arc<ServeContext>,
    mut offset: Option<i32>,
) -> color_eyre::eyre::Result<()> {
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        let updates_fut =
            fetch_updates(&ctx, offset, ctx.cfg.poll_timeout_secs, ctx.cfg.poll_limit);

        tokio::select! {
            _ = &mut shutdown => {
                info!("Received shutdown signal; stopping vel serve");
                break;
            }
            result = updates_fut => {
                let updates = match result {
                    Ok(items) => items,
                    Err(err) => {
                        warn!(error = ?err, "Telegram polling failed; retrying soon");
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        continue;
                    }
                };

                for update in updates {
                    let update_id = safe_update_id(update.id.0);
                    offset = Some(update_id.saturating_add(1));
                    if let Err(err) = handle_update(Arc::clone(&ctx), update).await {
                        warn!(error = %err, "Failed to process Telegram update");
                    }
                }
            }
        }
    }

    Ok(())
}

async fn handle_update(ctx: Arc<ServeContext>, update: Update) -> color_eyre::eyre::Result<()> {
    let update_id = safe_update_id(update.id.0);
    let now = Instant::now();

    {
        let mut replay = ctx.replay_cache.lock().await;
        if !replay.insert_if_new(update_id, now) {
            info!(update_id, "Duplicate Telegram update ignored");
            return Ok(());
        }
    }

    let Some(inbound) = extract_inbound_message(&ctx.cfg, update)? else {
        return Ok(());
    };

    if !is_authorized(&ctx.cfg, inbound.chat_id, inbound.user_id) {
        warn!(
            chat_id = inbound.chat_id,
            user_id = ?inbound.user_id,
            update_id = inbound.update_id,
            "Rejected unauthorized Telegram requester"
        );
        return Ok(());
    }

    let mut resume_binding = lookup_reply_session_binding(&ctx, &inbound).await;
    if resume_binding.is_none()
        && inbound.replied_to_is_bot_message
        && inbound.replied_to_message_id.is_some()
    {
        match interrupt_active_run_for_reply(&ctx, &inbound).await {
            ActiveRunReplyResolution::Interrupted(interrupted) => {
                info!(
                    chat_id = inbound.chat_id,
                    message_id = inbound.message_id,
                    replied_to_message_id = inbound.replied_to_message_id,
                    interrupted_request_id = %interrupted.interrupted_request_id,
                    resumed_runner = %interrupted.binding.runner_name,
                    resumed_session_id = %interrupted.binding.session.session_id(),
                    "Interrupted active Telegram run and converted reply into session continuation"
                );
                resume_binding = Some(interrupted.binding);
            }
            ActiveRunReplyResolution::MissingSession {
                request_id,
                runner_name,
            } => {
                let _ = send_telegram_message(
                    &ctx.bot,
                    ctx.parse_mode,
                    inbound.chat_id,
                    Some(inbound.message_id),
                    &format!(
                        "The active `{runner_name}` run (`{request_id}`) is not resumable yet. Retry in a few seconds, or start a new request with a model prefix."
                    ),
                )
                .await;
                return Ok(());
            }
            ActiveRunReplyResolution::NotFound => {}
        }
    }

    let route = route_inbound(&ctx.cfg.routing, &inbound, resume_binding.as_ref());

    if matches!(
        route,
        RouteResult::Usage(_) | RouteResult::Control(_) | RouteResult::Execute(_)
    ) {
        let _ = react_with_random_emoji(&ctx, &inbound).await;
    }

    match route {
        RouteResult::Ignore => Ok(()),
        RouteResult::Usage(message) => {
            send_telegram_message(
                &ctx.bot,
                ctx.parse_mode,
                inbound.chat_id,
                Some(inbound.message_id),
                &message,
            )
            .await
            .map_err(|e| {
                warn!(error = %e, "Failed to send Telegram usage response");
                e
            })?;
            Ok(())
        }
        RouteResult::Control(command) => handle_control_command(&ctx, &inbound, command).await,
        RouteResult::Execute(execution) => {
            let rate_key = format!(
                "chat:{}:user:{}",
                inbound.chat_id,
                inbound.user_id.unwrap_or_default()
            );
            {
                let mut limiter = ctx.rate_limiter.lock().await;
                if !limiter.allow(&rate_key, now) {
                    warn!(
                        chat_id = inbound.chat_id,
                        user_id = ?inbound.user_id,
                        "Rate limit exceeded for Telegram requester"
                    );
                    let _ = send_telegram_message(
                        &ctx.bot,
                        ctx.parse_mode,
                        inbound.chat_id,
                        Some(inbound.message_id),
                        "Rate limit exceeded. Please retry in a minute.",
                    )
                    .await;
                    return Ok(());
                }
            }

            let Some(profile) = ctx.cfg.runners.get(&execution.runner_name).cloned() else {
                let _ = send_telegram_message(
                    &ctx.bot,
                    ctx.parse_mode,
                    inbound.chat_id,
                    Some(inbound.message_id),
                    &format!(
                        "Runner profile `{}` is no longer configured; unable to execute this request.",
                        execution.runner_name
                    ),
                )
                .await;
                return Ok(());
            };

            let request = build_execution_request(&inbound, execution);
            let request_id = request.request_id.clone();
            let initial_session = request
                .mode
                .resume_binding()
                .map(|binding| binding.session.clone());
            let invocation = RunnerInvocationMetadata {
                binary: profile.binary.clone(),
                permission_mode: profile.permission_mode.clone(),
                model: profile.model.clone(),
                codex_profile: profile.codex.profile.clone(),
                codex_reasoning_effort: profile.codex.reasoning_effort,
            };

            let ctx_for_task = Arc::clone(&ctx);
            let request_for_task = request.clone();
            let request_id_for_task = request_id.clone();
            let task = tokio::task::spawn_local(async move {
                let result =
                    process_execution_request(Arc::clone(&ctx_for_task), request_for_task).await;
                {
                    let mut active = ctx_for_task.active_runs.lock().await;
                    active.remove_request(&request_id_for_task);
                }
                if let Err(err) = result {
                    error!(error = %err, "Failed to process Telegram execution request");
                }
            });
            let abort_handle = task.abort_handle();

            {
                let mut active = ctx.active_runs.lock().await;
                active.register(ActiveRunEntry {
                    request_id,
                    chat_id: inbound.chat_id,
                    runner_name: profile.name.clone(),
                    runner_kind: profile.kind,
                    source_user_message_id: Some(inbound.message_id),
                    invocation,
                    session: initial_session,
                    message_ids: HashSet::new(),
                    abort_handle,
                    started_at: Utc::now(),
                });
            }
            Ok(())
        }
    }
}

async fn lookup_reply_session_binding(
    ctx: &ServeContext,
    inbound: &InboundMessage,
) -> Option<SessionMessageBinding> {
    let replied_to = inbound.replied_to_message_id?;
    if !ctx.cfg.session_resume.enabled {
        return None;
    }

    let binding = {
        let store = ctx.session_store.lock().await;
        store.lookup(inbound.chat_id, replied_to)
    };

    match &binding {
        Some(found) => info!(
            update_id = inbound.update_id,
            chat_id = inbound.chat_id,
            message_id = inbound.message_id,
            replied_to_message_id = replied_to,
            runner = %found.runner_name,
            session_id = %found.session.session_id(),
            "Resolved Telegram reply to resumable session binding"
        ),
        None => info!(
            update_id = inbound.update_id,
            chat_id = inbound.chat_id,
            message_id = inbound.message_id,
            replied_to_message_id = replied_to,
            replied_to_bot_message = inbound.replied_to_is_bot_message,
            "No session binding found for replied-to Telegram message"
        ),
    }

    binding
}

async fn interrupt_active_run_for_reply(
    ctx: &ServeContext,
    inbound: &InboundMessage,
) -> ActiveRunReplyResolution {
    let Some(replied_to_message_id) = inbound.replied_to_message_id else {
        return ActiveRunReplyResolution::NotFound;
    };
    let mut active = ctx.active_runs.lock().await;
    active.interrupt_for_reply(inbound.chat_id, replied_to_message_id)
}

async fn react_with_random_emoji(
    ctx: &ServeContext,
    inbound: &InboundMessage,
) -> Result<(), RequestError> {
    let emoji = pick_reaction_emoji(inbound);
    ctx.bot
        .set_message_reaction(ChatId(inbound.chat_id), MessageId(inbound.message_id))
        .reaction(vec![ReactionType::Emoji {
            emoji: emoji.to_string(),
        }])
        .send()
        .await
        .map(|_| ())
        .map_err(|err| {
            warn!(
                chat_id = inbound.chat_id,
                message_id = inbound.message_id,
                error = %err,
                "Failed to set Telegram message reaction"
            );
            err
        })
}

fn pick_reaction_emoji(inbound: &InboundMessage) -> &'static str {
    const EMOJIS: &[&str] = &[
        "👍", "🔥", "⚡", "✅", "🤖", "🧠", "🚀", "🛠️", "👀", "👌", "🎯", "🫡",
    ];

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    inbound.chat_id.hash(&mut hasher);
    inbound.update_id.hash(&mut hasher);
    inbound.message_id.hash(&mut hasher);
    let idx = (hasher.finish() as usize) % EMOJIS.len();
    EMOJIS[idx]
}

fn build_execution_request(
    inbound: &InboundMessage,
    execution: PendingExecution,
) -> ExecutionRequest {
    let request_id = format!("tg-{}-{}", inbound.update_id, Utc::now().timestamp_millis());

    ExecutionRequest {
        request_id,
        received_at: Utc::now(),
        source: ExecutionSource {
            transport: "telegram",
            chat_id: inbound.chat_id,
            user_id: inbound.user_id,
            message_id: Some(inbound.message_id),
            update_id: inbound.update_id,
        },
        runner_name: execution.runner_name,
        mode: execution.mode,
        prompt: execution.prompt,
        attachment_refs: inbound.attachments.clone(),
        attachments: Vec::new(),
    }
}

async fn process_execution_request(
    ctx: Arc<ServeContext>,
    mut request: ExecutionRequest,
) -> color_eyre::eyre::Result<()> {
    enum RunOutcome {
        Completed(RunnerExecution),
        Failed(String),
        TimedOut,
        Cancelled(String),
    }

    let _permit = ctx
        .concurrency
        .acquire()
        .await
        .map_err(|_| eyre!("Concurrency semaphore closed"))?;

    let profile = ctx
        .cfg
        .runners
        .get(&request.runner_name)
        .ok_or_else(|| eyre!("Unknown runner profile '{}'.", request.runner_name))?
        .clone();

    let _resume_session_guard = acquire_resume_session_guard(&ctx, &request).await?;
    let request_prompt = request.prompt.clone();
    let mut collector = ExecutionResultCollector::default();
    let before_git = capture_git_snapshot(&ctx.cwd);

    info!(
        request_id = %request.request_id,
        transport = request.source.transport,
        update_id = request.source.update_id,
        chat_id = request.source.chat_id,
        user_id = ?request.source.user_id,
        runner = %request.runner_name,
        runner_description = %profile.description,
        mode = if request.mode.resume_binding().is_some() { "resume" } else { "new" },
        runner_streaming = profile.supports_streaming,
        runner_supports_attachments = profile.supports_attachments,
        "Accepted execution request"
    );

    let mut presenter = TelegramRunPresenter::new(&ctx, &request, &profile).await?;
    sync_active_run_messages(&ctx, &request.request_id, presenter.sent_message_ids()).await;
    if let Some(binding) = request.mode.resume_binding() {
        sync_active_run_session(&ctx, &request.request_id, binding.session.clone()).await;
    }
    let mut synced_message_count = presenter.sent_message_ids().len();
    ingest_presenter_event(
        &mut presenter,
        &mut collector,
        RunnerProgressEvent::Milestone(format!(
            "accepted request (streaming={} attachments={})",
            profile.supports_streaming, profile.supports_attachments
        )),
    )
    .await;

    let mut run_outcome: Option<RunOutcome> = None;
    let mut terminal_error: Option<color_eyre::eyre::Report> = None;
    if let Err(err) = validate_resume_request(&request, &profile) {
        let detail = truncate_for_telegram(&err.to_string(), 280);
        ingest_presenter_event(
            &mut presenter,
            &mut collector,
            RunnerProgressEvent::Error(detail.clone()),
        )
        .await;
        run_outcome = Some(RunOutcome::Failed(format!(
            "resume validation failed: {detail}"
        )));
        terminal_error = Some(err);
    }

    let mut downloaded = Vec::new();
    if run_outcome.is_none() {
        for attachment in &request.attachment_refs {
            match download_attachment(&ctx, &request.request_id, attachment).await {
                Ok(file) => {
                    ingest_presenter_event(
                        &mut presenter,
                        &mut collector,
                        RunnerProgressEvent::Milestone(format!(
                            "downloaded {} {} bytes mime={}",
                            attachment.kind.label(),
                            file.file_size,
                            file.mime_type
                        )),
                    )
                    .await;
                    downloaded.push(file);
                }
                Err(err) => {
                    let detail = format!("attachment handling failed: {err}");
                    ingest_presenter_event(
                        &mut presenter,
                        &mut collector,
                        RunnerProgressEvent::Error(detail.clone()),
                    )
                    .await;
                    run_outcome = Some(RunOutcome::Failed(detail));
                    terminal_error = Some(err);
                    break;
                }
            }
        }
    }

    request.attachments = downloaded.clone();
    request.prompt = build_prompt_with_attachments(&request.prompt, &request.attachments);
    if run_outcome.is_none() && !request.attachments.is_empty() && !profile.supports_attachments {
        ingest_presenter_event(
            &mut presenter,
            &mut collector,
            RunnerProgressEvent::Milestone(format!(
                "runner {} receives attachments as metadata/path context",
                request.runner_name
            )),
        )
        .await;
    }

    let mut discovered_session: Option<RunnerSessionHandle> = request
        .mode
        .resume_binding()
        .map(|binding| binding.session.clone());

    let run_outcome = if let Some(preflight) = run_outcome {
        preflight
    } else {
        let (progress_tx, mut progress_rx) =
            tokio::sync::mpsc::unbounded_channel::<RunnerProgressEvent>();

        let req_for_runner = request.clone();
        let profile_for_runner = profile.clone();
        let defaults = ctx.defaults.clone();
        let exec_cwd = ctx.cwd.clone();

        let mut runner_task = tokio::task::spawn_local(async move {
            let runner = ProcessExecutionRunner;
            let mut progress_cb = |event: RunnerProgressEvent| {
                let _ = progress_tx.send(event);
            };
            let run_fut = runner.run(
                &req_for_runner,
                &profile_for_runner,
                &defaults,
                &exec_cwd,
                &mut progress_cb,
            );
            tokio::time::timeout(profile_for_runner.timeout, run_fut).await
        });

        let mut ticker = tokio::time::interval(Duration::from_millis(250));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let outcome: RunOutcome = loop {
            tokio::select! {
                maybe_event = progress_rx.recv() => {
                    if let Some(event) = maybe_event {
                        if let RunnerProgressEvent::SessionBound { session } = &event {
                            discovered_session = Some(session.clone());
                            sync_active_run_session(&ctx, &request.request_id, session.clone()).await;
                        }
                        ingest_presenter_event(&mut presenter, &mut collector, event).await;
                        if presenter.sent_message_ids().len() > synced_message_count {
                            sync_active_run_messages(&ctx, &request.request_id, presenter.sent_message_ids()).await;
                            synced_message_count = presenter.sent_message_ids().len();
                        }
                    }
                }
                _ = ticker.tick() => {
                    presenter.tick().await;
                    if presenter.sent_message_ids().len() > synced_message_count {
                        sync_active_run_messages(&ctx, &request.request_id, presenter.sent_message_ids()).await;
                        synced_message_count = presenter.sent_message_ids().len();
                    }
                }
                join_result = &mut runner_task => {
                    break match join_result {
                        Ok(Ok(Ok(execution))) => RunOutcome::Completed(execution),
                        Ok(Ok(Err(err))) => RunOutcome::Failed(err.to_string()),
                        Ok(Err(_)) => RunOutcome::TimedOut,
                        Err(err) => {
                            if err.is_cancelled() {
                                RunOutcome::Cancelled("runner task cancelled".to_string())
                            } else {
                                RunOutcome::Failed(format!("runner task join failure: {}", err))
                            }
                        }
                    };
                }
            }
        };

        while let Ok(event) = progress_rx.try_recv() {
            ingest_presenter_event(&mut presenter, &mut collector, event).await;
            if presenter.sent_message_ids().len() > synced_message_count {
                sync_active_run_messages(&ctx, &request.request_id, presenter.sent_message_ids())
                    .await;
                synced_message_count = presenter.sent_message_ids().len();
            }
        }
        outcome
    };

    let elapsed = Utc::now() - request.received_at;
    let elapsed_secs = elapsed.num_seconds().max(0) as u64;
    let after_git = capture_git_snapshot(&ctx.cwd);

    let mut completed_execution: Option<RunnerExecution> = None;
    let mut failure_reason: Option<String> = None;
    let terminal_state = match run_outcome {
        RunOutcome::Completed(execution) => {
            info!(
                request_id = %request.request_id,
                runner = %request.runner_name,
                elapsed_secs,
                "Runner execution completed"
            );
            completed_execution = Some(execution);
            RunTerminalState::Completed
        }
        RunOutcome::Failed(error_text) => {
            warn!(
                request_id = %request.request_id,
                runner = %request.runner_name,
                error = %error_text,
                "Runner execution failed"
            );
            ingest_presenter_event(
                &mut presenter,
                &mut collector,
                RunnerProgressEvent::Error(error_text.clone()),
            )
            .await;
            failure_reason = Some(error_text);
            RunTerminalState::Failed
        }
        RunOutcome::TimedOut => {
            let detail = format!("execution timed out at {}s", profile.timeout.as_secs());
            warn!(
                request_id = %request.request_id,
                runner = %request.runner_name,
                timeout_secs = profile.timeout.as_secs(),
                "Runner execution timed out"
            );
            ingest_presenter_event(
                &mut presenter,
                &mut collector,
                RunnerProgressEvent::Error(detail.clone()),
            )
            .await;
            failure_reason = Some(detail);
            RunTerminalState::TimedOut
        }
        RunOutcome::Cancelled(reason) => {
            warn!(
                request_id = %request.request_id,
                runner = %request.runner_name,
                reason = %reason,
                "Runner execution cancelled"
            );
            ingest_presenter_event(
                &mut presenter,
                &mut collector,
                RunnerProgressEvent::Error(reason.clone()),
            )
            .await;
            failure_reason = Some(reason);
            RunTerminalState::Cancelled
        }
    };

    let session_to_persist = completed_execution
        .as_ref()
        .and_then(|exec| exec.session.clone().or(discovered_session.clone()));
    if let Some(session) = session_to_persist.clone() {
        sync_active_run_session(&ctx, &request.request_id, session).await;
    }
    let stdout_text = completed_execution
        .as_ref()
        .map(|exec| exec.stdout.clone())
        .unwrap_or_default();
    let stderr_text = completed_execution
        .as_ref()
        .map(|exec| exec.stderr.clone())
        .unwrap_or_default();

    let mut summary = build_execution_result_summary(
        &ctx.cfg.presentation,
        &request,
        &request_prompt,
        &profile,
        terminal_state,
        elapsed_secs,
        &collector,
        &stdout_text,
        &stderr_text,
        failure_reason.as_deref(),
        before_git.as_ref(),
        after_git.as_ref(),
    );

    let raw_log_path =
        execution_record_path(&ctx.cfg.presentation.raw_log_dir, &request.request_id);
    let raw_log_path_display = raw_log_path.display().to_string();
    summary
        .diagnostics
        .get_or_insert_with(ExecutionDiagnostics::default)
        .raw_log_path = Some(raw_log_path_display.clone());

    let persisted = PersistedExecutionRecord {
        schema_version: 1,
        request: summary.raw_request_metadata.clone(),
        runner_description: profile.description.clone(),
        terminal_state,
        duration_secs: elapsed_secs,
        prompt: request_prompt.clone(),
        attachments: request
            .attachments
            .iter()
            .map(|attachment| PersistedAttachmentRecord {
                kind: attachment.kind.label(),
                mime_type: attachment.mime_type.clone(),
                size_bytes: attachment.file_size,
                width: attachment.width,
                height: attachment.height,
                file_name: attachment.file_name.clone(),
                path: attachment.path.display().to_string(),
            })
            .collect(),
        progress_events: collector.events.clone(),
        stdout: stdout_text.clone(),
        stderr: stderr_text.clone(),
        summary: summary.clone(),
    };
    if let Err(err) = persist_execution_record(&raw_log_path, &persisted).await {
        warn!(
            request_id = %request.request_id,
            path = %raw_log_path.display(),
            error = %err,
            "Failed to persist raw execution record"
        );
    }

    let rendered_final = render_execution_summary(
        &summary,
        ctx.cfg.presentation.default_verbosity,
        &ctx.cfg.presentation,
    );
    presenter
        .finalize(
            terminal_state,
            format!("{} in {}s", terminal_state.label(), elapsed_secs),
            Some(rendered_final),
        )
        .await?;
    if presenter.sent_message_ids().len() > synced_message_count {
        sync_active_run_messages(&ctx, &request.request_id, presenter.sent_message_ids()).await;
    }

    if ctx.cfg.session_resume.enabled
        && let Some(session) = session_to_persist
    {
        let message_ids = presenter.sent_message_ids().to_vec();
        if let Err(err) =
            persist_session_bindings_for_run(&ctx, &request, &profile, &session, &message_ids).await
        {
            warn!(
                request_id = %request.request_id,
                runner = %request.runner_name,
                error = %err,
                "Failed to persist session bindings for Telegram continuation"
            );
        }
    }

    if !ctx.cfg.attachment_policy.keep_files {
        for file in downloaded {
            if let Err(err) = tokio::fs::remove_file(&file.path).await {
                warn!(
                    request_id = %request.request_id,
                    path = %file.path.display(),
                    error = %err,
                    "Failed to cleanup temporary attachment"
                );
            }
        }
    }

    if let Some(err) = terminal_error {
        return Err(err);
    }

    Ok(())
}

async fn ingest_presenter_event(
    presenter: &mut TelegramRunPresenter,
    collector: &mut ExecutionResultCollector,
    event: RunnerProgressEvent,
) {
    collector.ingest(&event);
    presenter.ingest(event).await;
}

async fn sync_active_run_messages(ctx: &ServeContext, request_id: &str, message_ids: &[i32]) {
    let mut active = ctx.active_runs.lock().await;
    active.sync_messages(request_id, message_ids);
}

async fn sync_active_run_session(
    ctx: &ServeContext,
    request_id: &str,
    session: RunnerSessionHandle,
) {
    let mut active = ctx.active_runs.lock().await;
    active.set_session(request_id, session);
}

fn build_execution_result_summary(
    presentation: &TelegramPresentationConfig,
    request: &ExecutionRequest,
    request_prompt: &str,
    profile: &RunnerProfile,
    terminal_state: RunTerminalState,
    duration_secs: u64,
    collector: &ExecutionResultCollector,
    stdout: &str,
    stderr: &str,
    failure_reason: Option<&str>,
    before_git: Option<&GitSnapshot>,
    after_git: Option<&GitSnapshot>,
) -> ExecutionResultSummary {
    let mut changed_files = changed_files_from_git(before_git, after_git);
    if changed_files.is_empty() {
        changed_files = changed_files_from_activity(collector, stdout, stderr);
    }
    changed_files = dedupe_preserve_order(changed_files);

    let verification_steps = dedupe_preserve_order(collector.verification_steps.clone());
    let summary_status = ExecutionSummaryStatus::from(terminal_state);
    let first_error = failure_reason.map(normalize_whitespace).or_else(|| {
        collector
            .errors
            .first()
            .map(|err| normalize_whitespace(err))
    });
    let stderr_preview = first_nonempty_line(stderr).map(|line| truncate_for_telegram(line, 220));

    let mut high_level_summary = Vec::new();
    if summary_status == ExecutionSummaryStatus::Completed {
        high_level_summary.push(if changed_files.is_empty() {
            "Completed with no detected repository file changes.".to_string()
        } else {
            format!("Updated {} file(s).", changed_files.len())
        });
        if !verification_steps.is_empty() {
            high_level_summary.push(format!(
                "Verification checks run: {}.",
                verification_steps.len()
            ));
        }
        if !request.attachments.is_empty() {
            high_level_summary.push(format!(
                "Processed {} attachment(s).",
                request.attachments.len()
            ));
        }
    } else {
        if let Some(error) = first_error.as_deref() {
            high_level_summary.push(format!(
                "Run ended with: {}",
                truncate_for_telegram(error, 180)
            ));
        }
        if !changed_files.is_empty() {
            high_level_summary.push(format!(
                "{} file(s) were modified before completion.",
                changed_files.len()
            ));
        }
    }
    if high_level_summary.is_empty() {
        high_level_summary.push("Execution finished with limited telemetry.".to_string());
    }

    let tool_activity = semantic_tool_activity(collector, presentation.max_section_chars);
    let notable_outputs = notable_output_lines(
        stdout,
        stderr,
        &collector.output,
        presentation.max_section_chars,
        summary_status == ExecutionSummaryStatus::Completed,
    );
    let next_actions = next_actions_for_summary(
        summary_status,
        failure_reason,
        &verification_steps,
        &changed_files,
        &profile.name,
    );

    let repo_name = after_git
        .and_then(|snapshot| snapshot.repo_name.clone())
        .or_else(|| before_git.and_then(|snapshot| snapshot.repo_name.clone()));
    let branch = after_git
        .and_then(|snapshot| snapshot.branch.clone())
        .or_else(|| before_git.and_then(|snapshot| snapshot.branch.clone()));

    ExecutionResultSummary {
        title: derive_task_title(request_prompt),
        final_status: summary_status,
        runner_name: profile.name.clone(),
        repo_name,
        branch,
        duration_secs,
        high_level_summary: cap_lines(high_level_summary, 4),
        tool_activity: cap_lines(tool_activity, 5),
        changed_files,
        verification_steps,
        notable_outputs: cap_lines(notable_outputs, 5),
        next_actions: cap_lines(next_actions, 4),
        raw_request_metadata: ExecutionRawRequestMetadata {
            request_id: request.request_id.clone(),
            transport: request.source.transport.to_string(),
            chat_id: request.source.chat_id,
            user_id: request.source.user_id,
            source_message_id: request.source.message_id,
            update_id: request.source.update_id,
            runner_name: request.runner_name.clone(),
            runner_kind: profile.kind,
            mode: if request.mode.resume_binding().is_some() {
                "resume"
            } else {
                "new"
            },
            attachment_count: request.attachments.len(),
            received_at: request.received_at,
        },
        diagnostics: if first_error.is_some() || stderr_preview.is_some() {
            Some(ExecutionDiagnostics {
                first_error,
                stderr_preview,
                raw_log_path: None,
            })
        } else {
            None
        },
    }
}

fn render_execution_summary(
    summary: &ExecutionResultSummary,
    verbosity: TelegramResultVerbosity,
    presentation: &TelegramPresentationConfig,
) -> String {
    match verbosity {
        TelegramResultVerbosity::Compact => render_compact_summary(summary, presentation),
        TelegramResultVerbosity::Standard => render_standard_summary(summary, presentation),
        TelegramResultVerbosity::Verbose => render_verbose_summary(summary, presentation),
        TelegramResultVerbosity::Raw => render_raw_summary(summary, presentation),
    }
}

fn render_compact_summary(
    summary: &ExecutionResultSummary,
    presentation: &TelegramPresentationConfig,
) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "{} {}: {}",
        summary.final_status.emoji(),
        capitalize(summary.final_status.label()),
        summary.title
    ));
    lines.push(render_metadata_line(summary));
    lines.push(String::new());
    lines.push("Summary".to_string());
    push_section_lines(
        &mut lines,
        &summary.high_level_summary,
        presentation.max_section_chars,
        3,
    );
    lines.push(String::new());
    lines.push(
        if summary.final_status == ExecutionSummaryStatus::Completed {
            "Changed".to_string()
        } else {
            "Changed before failure".to_string()
        },
    );
    push_changed_files_section(
        &mut lines,
        &summary.changed_files,
        presentation.max_changed_files,
        56,
        presentation.path_truncation,
    );
    lines.push(String::new());
    if summary.final_status == ExecutionSummaryStatus::Completed {
        lines.push("Result".to_string());
        let result_lines = if summary.notable_outputs.is_empty() {
            vec!["No additional textual output captured.".to_string()]
        } else {
            summary.notable_outputs.clone()
        };
        push_section_lines(&mut lines, &result_lines, presentation.max_section_chars, 3);
    } else {
        lines.push("Failure".to_string());
        let failure = summary
            .diagnostics
            .as_ref()
            .and_then(|diag| diag.first_error.clone())
            .or_else(|| summary.high_level_summary.first().cloned())
            .unwrap_or_else(|| "Execution failed without a detailed error.".to_string());
        push_section_lines(&mut lines, &[failure], presentation.max_section_chars, 2);
        lines.push("Likely cause".to_string());
        push_section_lines(
            &mut lines,
            &[derive_likely_cause(summary)],
            presentation.max_section_chars,
            1,
        );
        lines.push("Next suggested action".to_string());
        push_section_lines(
            &mut lines,
            &summary.next_actions,
            presentation.max_section_chars,
            2,
        );
    }

    if presentation.include_follow_up_hints {
        lines.push(String::new());
        lines.push("Hints".to_string());
        lines.push("- Reply `details` for full logs".to_string());
        lines.push("- Reply `rerun` to retry".to_string());
        lines.push("- Reply `diff` for changed files summary".to_string());
    }

    normalize_telegram_text(&lines.join("\n"))
}

fn render_standard_summary(
    summary: &ExecutionResultSummary,
    presentation: &TelegramPresentationConfig,
) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "{} {}: {}",
        summary.final_status.emoji(),
        capitalize(summary.final_status.label()),
        summary.title
    ));
    lines.push(render_metadata_line(summary));
    lines.push(String::new());
    lines.push("Summary".to_string());
    push_section_lines(
        &mut lines,
        &summary.high_level_summary,
        presentation.max_section_chars,
        4,
    );
    lines.push(String::new());
    lines.push(
        if summary.final_status == ExecutionSummaryStatus::Completed {
            "Changed".to_string()
        } else {
            "Changed before failure".to_string()
        },
    );
    push_changed_files_section(
        &mut lines,
        &summary.changed_files,
        presentation.max_changed_files,
        64,
        presentation.path_truncation,
    );
    lines.push(String::new());
    lines.push("Verification".to_string());
    if summary.verification_steps.is_empty() {
        lines.push("- none observed".to_string());
    } else {
        push_section_lines(
            &mut lines,
            &summary.verification_steps,
            presentation.max_section_chars,
            4,
        );
    }

    lines.push(String::new());
    if summary.final_status == ExecutionSummaryStatus::Completed {
        lines.push("Result".to_string());
        push_section_lines(
            &mut lines,
            &summary.notable_outputs,
            presentation.max_section_chars,
            4,
        );
    } else {
        lines.push("Failure".to_string());
        let failure = summary
            .diagnostics
            .as_ref()
            .and_then(|diag| diag.first_error.clone())
            .unwrap_or_else(|| "No detailed failure cause captured.".to_string());
        push_section_lines(&mut lines, &[failure], presentation.max_section_chars, 2);
        lines.push("Likely cause".to_string());
        let cause = derive_likely_cause(summary);
        push_section_lines(&mut lines, &[cause], presentation.max_section_chars, 2);
        if !summary.next_actions.is_empty() {
            lines.push("Next suggested action".to_string());
            push_section_lines(
                &mut lines,
                &summary.next_actions,
                presentation.max_section_chars,
                2,
            );
        }
    }

    if should_include_debug_footer(summary, TelegramResultVerbosity::Standard, presentation) {
        lines.push(String::new());
        lines.push("Debug".to_string());
        for line in debug_footer_lines(summary) {
            lines.push(format!("- {line}"));
        }
    }

    if presentation.include_follow_up_hints {
        lines.push(String::new());
        lines.push("Hints".to_string());
        lines.push("- Reply `details` for full logs".to_string());
        lines.push("- Reply `rerun` to retry".to_string());
        lines.push("- Reply `diff` for changed files summary".to_string());
    }

    normalize_telegram_text(&lines.join("\n"))
}

fn render_verbose_summary(
    summary: &ExecutionResultSummary,
    presentation: &TelegramPresentationConfig,
) -> String {
    let mut lines = render_standard_summary(summary, presentation)
        .split('\n')
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    lines.push(String::new());
    lines.push("Actions".to_string());
    if summary.tool_activity.is_empty() {
        lines.push("- no tool activity captured".to_string());
    } else {
        push_section_lines(
            &mut lines,
            &summary.tool_activity,
            presentation.max_section_chars,
            5,
        );
    }
    if should_include_debug_footer(summary, TelegramResultVerbosity::Verbose, presentation) {
        if !lines.iter().any(|line| line == "Debug") {
            lines.push(String::new());
            lines.push("Debug".to_string());
            for line in debug_footer_lines(summary) {
                lines.push(format!("- {line}"));
            }
        }
    }
    normalize_telegram_text(&lines.join("\n"))
}

fn render_raw_summary(
    summary: &ExecutionResultSummary,
    presentation: &TelegramPresentationConfig,
) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "{} RAW {}: {}",
        summary.final_status.emoji(),
        capitalize(summary.final_status.label()),
        summary.title
    ));
    lines.push(render_metadata_line(summary));
    lines.push(String::new());
    lines.push("Summary".to_string());
    push_section_lines(
        &mut lines,
        &summary.high_level_summary,
        presentation.max_section_chars,
        5,
    );
    lines.push(String::new());
    lines.push("Actions".to_string());
    push_section_lines(
        &mut lines,
        &summary.tool_activity,
        presentation.max_section_chars,
        8,
    );
    lines.push(String::new());
    lines.push("Changed".to_string());
    push_changed_files_section(
        &mut lines,
        &summary.changed_files,
        presentation.max_changed_files.max(10),
        76,
        presentation.path_truncation,
    );
    lines.push(String::new());
    lines.push("Verification".to_string());
    push_section_lines(
        &mut lines,
        &summary.verification_steps,
        presentation.max_section_chars,
        8,
    );
    lines.push(String::new());
    lines.push("Debug".to_string());
    for line in debug_footer_lines(summary) {
        lines.push(format!("- {line}"));
    }
    normalize_telegram_text(&lines.join("\n"))
}

fn render_metadata_line(summary: &ExecutionResultSummary) -> String {
    let repo = summary
        .repo_name
        .clone()
        .unwrap_or_else(|| "n/a".to_string());
    let branch = summary.branch.clone().unwrap_or_else(|| "n/a".to_string());
    format!(
        "repo={} | branch={} | runner={} | duration={}s",
        repo, branch, summary.runner_name, summary.duration_secs
    )
}

fn should_include_debug_footer(
    summary: &ExecutionResultSummary,
    verbosity: TelegramResultVerbosity,
    presentation: &TelegramPresentationConfig,
) -> bool {
    let success = summary.final_status == ExecutionSummaryStatus::Completed;
    if matches!(
        verbosity,
        TelegramResultVerbosity::Verbose | TelegramResultVerbosity::Raw
    ) {
        return true;
    }
    if matches!(verbosity, TelegramResultVerbosity::Standard) {
        return true;
    }
    if success {
        return presentation.include_debug_footer_on_success;
    }
    true
}

fn debug_footer_lines(summary: &ExecutionResultSummary) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!(
        "request_id={}",
        summary.raw_request_metadata.request_id
    ));
    lines.push(format!(
        "mode={} transport={} update_id={} chat_id={}",
        summary.raw_request_metadata.mode,
        summary.raw_request_metadata.transport,
        summary.raw_request_metadata.update_id,
        summary.raw_request_metadata.chat_id
    ));
    if let Some(diag) = &summary.diagnostics {
        if let Some(err) = &diag.first_error {
            lines.push(format!("error={}", truncate_for_telegram(err, 200)));
        }
        if let Some(stderr) = &diag.stderr_preview {
            lines.push(format!("stderr={}", truncate_for_telegram(stderr, 200)));
        }
        if let Some(path) = &diag.raw_log_path {
            lines.push(format!("raw_log={path}"));
        }
    }
    lines
}

fn derive_task_title(prompt: &str) -> String {
    let first_line = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("telegram task");
    truncate_for_telegram(first_line, 72)
}

fn semantic_tool_activity(
    collector: &ExecutionResultCollector,
    max_section_chars: usize,
) -> Vec<String> {
    let mut reads = 0usize;
    let mut writes = 0usize;
    let mut verifications = 0usize;
    let mut unique_commands = BTreeSet::new();

    for command in &collector.tool_commands {
        let normalized = normalize_whitespace(command);
        if normalized.is_empty() {
            continue;
        }
        unique_commands.insert(truncate_for_telegram(&normalized, 100));
        if is_read_command(&normalized) {
            reads += 1;
        }
        if is_write_command(&normalized) {
            writes += 1;
        }
        if is_verification_command(&normalized) {
            verifications += 1;
        }
    }

    let mut lines = Vec::new();
    if reads > 0 {
        lines.push(format!(
            "Analysed code/resources through {} read action(s).",
            reads
        ));
    }
    if writes > 0 {
        lines.push(format!("Applied {} write/edit action(s).", writes));
    }
    if verifications > 0 {
        lines.push(format!(
            "Ran {} verification-oriented command(s).",
            verifications
        ));
    }
    for command in unique_commands.into_iter().take(3) {
        lines.push(format!(
            "Observed command: {}",
            truncate_for_telegram(&command, max_section_chars.min(140))
        ));
    }
    if lines.is_empty() {
        lines.push("No structured tool actions were captured.".to_string());
    }
    lines
}

fn notable_output_lines(
    stdout: &str,
    stderr: &str,
    collector_output: &str,
    max_section_chars: usize,
    prefer_stdout: bool,
) -> Vec<String> {
    let primary = if prefer_stdout {
        if !stdout.trim().is_empty() {
            stdout
        } else {
            collector_output
        }
    } else if !stderr.trim().is_empty() {
        stderr
    } else if !stdout.trim().is_empty() {
        stdout
    } else {
        collector_output
    };

    let mut out = Vec::new();
    for line in primary
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let normalized = normalize_whitespace(line);
        if normalized.starts_with('{') && normalized.ends_with('}') {
            continue;
        }
        out.push(truncate_for_telegram(
            &normalized,
            max_section_chars.min(220),
        ));
        if out.len() >= 4 {
            break;
        }
    }
    if out.is_empty() {
        out.push("No concise textual summary was emitted by the runner.".to_string());
    }
    out
}

fn next_actions_for_summary(
    status: ExecutionSummaryStatus,
    failure_reason: Option<&str>,
    verification_steps: &[String],
    changed_files: &[String],
    runner_name: &str,
) -> Vec<String> {
    if status == ExecutionSummaryStatus::Completed {
        if verification_steps.is_empty() && !changed_files.is_empty() {
            return vec!["Run local verification to confirm the changed files.".to_string()];
        }
        return vec!["Reply `details` for the full execution record if needed.".to_string()];
    }

    let cause = failure_reason.unwrap_or("");
    let mut next = Vec::new();
    if cause.to_ascii_lowercase().contains("timed out") {
        next.push(format!(
            "Increase timeout for `{runner_name}` or send a narrower prompt."
        ));
    } else if cause.to_ascii_lowercase().contains("not found") {
        next.push("Check runner binary availability and PATH configuration.".to_string());
    } else if cause.to_ascii_lowercase().contains("permission") {
        next.push("Review permissions/sandbox settings, then rerun.".to_string());
    } else {
        next.push("Reply `rerun` with a refined prompt.".to_string());
    }
    next.push("Reply `details` to inspect full raw logs.".to_string());
    next
}

fn derive_likely_cause(summary: &ExecutionResultSummary) -> String {
    let message = summary
        .diagnostics
        .as_ref()
        .and_then(|diag| diag.first_error.clone())
        .unwrap_or_else(|| "No detailed error captured.".to_string())
        .to_ascii_lowercase();

    if message.contains("timed out") {
        "Runner exceeded configured timeout.".to_string()
    } else if message.contains("not found") {
        "Runner executable could not be resolved.".to_string()
    } else if message.contains("permission") {
        "Permission/sandbox policy blocked execution.".to_string()
    } else if message.contains("non-zero") {
        "Runner command returned a non-zero exit.".to_string()
    } else {
        "Runner reported an execution error.".to_string()
    }
}

fn push_section_lines(
    out: &mut Vec<String>,
    lines: &[String],
    max_section_chars: usize,
    max_lines: usize,
) {
    if lines.is_empty() {
        out.push("- none".to_string());
        return;
    }

    for line in lines.iter().take(max_lines) {
        let normalized = normalize_whitespace(line);
        out.push(format!(
            "- {}",
            truncate_for_telegram(&normalized, max_section_chars)
        ));
    }
    if lines.len() > max_lines {
        out.push(format!("- +{} more", lines.len() - max_lines));
    }
}

fn push_changed_files_section(
    out: &mut Vec<String>,
    files: &[String],
    max_files: usize,
    display_width: usize,
    truncation: TelegramPathTruncation,
) {
    if files.is_empty() {
        out.push("- none".to_string());
        return;
    }
    for file in files.iter().take(max_files) {
        out.push(format!(
            "- {}",
            truncate_path(file, display_width, truncation)
        ));
    }
    if files.len() > max_files {
        out.push(format!("- +{} more", files.len() - max_files));
    }
}

fn execution_record_path(base_dir: &Path, request_id: &str) -> PathBuf {
    base_dir.join(format!("{}.json", sanitize_component(request_id)))
}

async fn persist_execution_record(
    path: &Path,
    record: &PersistedExecutionRecord,
) -> color_eyre::eyre::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.wrap_err_with(|| {
            format!("failed to create execution record dir {}", parent.display())
        })?;
    }

    let payload =
        serde_json::to_string_pretty(record).wrap_err("failed to serialize execution record")?;
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, payload)
        .await
        .wrap_err_with(|| format!("failed to write temp execution record {}", tmp.display()))?;
    tokio::fs::rename(&tmp, path).await.wrap_err_with(|| {
        format!(
            "failed to replace execution record {} with {}",
            path.display(),
            tmp.display()
        )
    })?;
    Ok(())
}

fn changed_files_from_git(
    before: Option<&GitSnapshot>,
    after: Option<&GitSnapshot>,
) -> Vec<String> {
    let (Some(before), Some(after)) = (before, after) else {
        return Vec::new();
    };
    if before.repo_root.is_none()
        || after.repo_root.is_none()
        || before.repo_root != after.repo_root
    {
        return Vec::new();
    }

    let mut paths = BTreeSet::new();
    for (path, status) in &after.porcelain_status {
        if before.porcelain_status.get(path) != Some(status) {
            paths.insert(path.clone());
        }
    }
    paths.into_iter().collect()
}

fn changed_files_from_activity(
    collector: &ExecutionResultCollector,
    stdout: &str,
    stderr: &str,
) -> Vec<String> {
    let mut files = Vec::new();
    for command in &collector.tool_commands {
        if is_write_command(command) {
            files.extend(extract_paths_from_text(command));
        }
    }
    if files.is_empty() {
        files.extend(extract_paths_from_text(stdout));
    }
    if files.is_empty() {
        files.extend(extract_paths_from_text(stderr));
    }
    dedupe_preserve_order(files)
}

fn capture_git_snapshot(cwd: &Path) -> Option<GitSnapshot> {
    let repo_root_output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !repo_root_output.status.success() {
        return None;
    }
    let repo_root = String::from_utf8_lossy(&repo_root_output.stdout)
        .trim()
        .to_string();
    if repo_root.is_empty() {
        return None;
    }
    let repo_root_path = PathBuf::from(&repo_root);
    let repo_name = repo_root_path
        .file_name()
        .map(|name| name.to_string_lossy().to_string());

    let branch_output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok();
    let branch = branch_output.and_then(|out| {
        if out.status.success() {
            let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if value.is_empty() { None } else { Some(value) }
        } else {
            None
        }
    });

    let status_output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .output()
        .ok();
    let porcelain_status = status_output
        .and_then(|out| {
            if out.status.success() {
                Some(String::from_utf8_lossy(&out.stdout).to_string())
            } else {
                None
            }
        })
        .map(|raw| parse_porcelain_status(&raw))
        .unwrap_or_default();

    Some(GitSnapshot {
        repo_root: Some(repo_root_path),
        repo_name,
        branch,
        porcelain_status,
    })
}

fn parse_porcelain_status(raw: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in raw.lines() {
        if line.len() < 4 {
            continue;
        }
        let status = line[0..2].to_string();
        let mut path = line[3..].trim().to_string();
        if let Some((_, to)) = path.split_once("->") {
            path = to.trim().to_string();
        }
        if !path.is_empty() {
            out.insert(path, status);
        }
    }
    out
}

fn parse_tool_command_from_milestone(milestone: &str) -> Option<String> {
    if let Some(rest) = milestone.strip_prefix("tool start: ") {
        return Some(normalize_whitespace(rest));
    }
    if let Some(rest) = milestone.strip_prefix("tool result: ") {
        let command = rest.split(" success=").next().unwrap_or(rest).trim();
        if !command.is_empty() {
            return Some(normalize_whitespace(command));
        }
    }
    None
}

fn is_verification_command(command: &str) -> bool {
    let cmd = command.to_ascii_lowercase();
    [
        "test",
        "check",
        "clippy",
        "lint",
        "pytest",
        "cargo test",
        "cargo check",
        "just check",
        "bun test",
        "npm test",
        "pnpm test",
        "go test",
        "ruff",
        "vitest",
    ]
    .iter()
    .any(|needle| cmd.contains(needle))
}

fn is_read_command(command: &str) -> bool {
    let cmd = command.to_ascii_lowercase();
    [
        "cat ", "rg ", "ls ", "find ", "sed -n", "head ", "tail ", "grep ",
    ]
    .iter()
    .any(|needle| cmd.contains(needle))
}

fn is_write_command(command: &str) -> bool {
    let cmd = command.to_ascii_lowercase();
    [
        "apply_patch",
        ">>",
        " >",
        "touch ",
        "mkdir ",
        "mv ",
        "cp ",
        "rm ",
        "cargo fmt",
        "gofmt",
        "rustfmt",
        "perl -pi",
        "sed -i",
    ]
    .iter()
    .any(|needle| cmd.contains(needle))
}

fn extract_paths_from_text(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in text.split_whitespace() {
        let candidate = token
            .trim_matches(|ch: char| "`\"'(),;[]{}".contains(ch))
            .trim();
        if candidate.len() < 2 || candidate.contains("://") || candidate.starts_with('-') {
            continue;
        }
        let looks_like_path = candidate.contains('/')
            || Path::new(candidate)
                .extension()
                .is_some_and(|ext| !ext.is_empty());
        if !looks_like_path {
            continue;
        }
        out.push(candidate.to_string());
        if out.len() >= 64 {
            break;
        }
    }
    dedupe_preserve_order(out)
}

fn dedupe_preserve_order(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in items {
        if item.is_empty() {
            continue;
        }
        if seen.insert(item.clone()) {
            out.push(item);
        }
    }
    out
}

fn first_nonempty_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_limited(target: &mut String, fragment: &str, max_len: usize) {
    target.push_str(fragment);
    if target.len() <= max_len {
        return;
    }
    let keep_start = target.floor_char_boundary(target.len().saturating_sub(max_len));
    *target = target[keep_start..].to_string();
}

fn cap_lines(lines: Vec<String>, max_lines: usize) -> Vec<String> {
    let total = lines.len();
    if total <= max_lines {
        return lines;
    }
    let mut capped = lines.into_iter().take(max_lines).collect::<Vec<_>>();
    capped.push(format!("+{} more", total.saturating_sub(max_lines)));
    capped
}

fn truncate_path(path: &str, max_len: usize, mode: TelegramPathTruncation) -> String {
    if path.len() <= max_len {
        return path.to_string();
    }
    match mode {
        TelegramPathTruncation::Left => truncate_path_left(path, max_len),
        TelegramPathTruncation::Middle => truncate_path_middle(path, max_len),
    }
}

fn truncate_path_left(path: &str, max_len: usize) -> String {
    let parts = path.split('/').collect::<Vec<_>>();
    if parts.len() <= 1 {
        return truncate_for_telegram(path, max_len);
    }
    let mut kept = Vec::new();
    let mut len = 1usize; // ellipsis
    for part in parts.iter().rev() {
        let next_len = len + part.len() + usize::from(!kept.is_empty());
        if next_len > max_len.saturating_sub(2) {
            break;
        }
        kept.push(*part);
        len = next_len;
    }
    if kept.is_empty() {
        return truncate_for_telegram(path, max_len);
    }
    kept.reverse();
    format!("…/{}", kept.join("/"))
}

fn truncate_path_middle(path: &str, max_len: usize) -> String {
    if max_len < 8 {
        return truncate_for_telegram(path, max_len);
    }
    let left_len = (max_len / 2).saturating_sub(1);
    let right_len = max_len.saturating_sub(left_len + 1);
    let left_idx = path.floor_char_boundary(left_len.min(path.len()));
    let right_start = path
        .len()
        .saturating_sub(right_len)
        .max(left_idx)
        .min(path.len());
    let right_idx = path.floor_char_boundary(right_start);
    format!("{}…{}", &path[..left_idx], &path[right_idx..])
}

fn capitalize(input: &str) -> String {
    let mut chars = input.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
}

fn validate_resume_request(
    request: &ExecutionRequest,
    profile: &RunnerProfile,
) -> color_eyre::eyre::Result<()> {
    let Some(binding) = request.mode.resume_binding() else {
        return Ok(());
    };

    if !profile.supports_session_resume {
        return Err(eyre!(
            "Runner '{}' does not support native session resuming.",
            profile.name
        ));
    }
    if binding.runner_name != profile.name {
        return Err(eyre!(
            "Resume mapping runner mismatch: mapping='{}' requested='{}'.",
            binding.runner_name,
            profile.name
        ));
    }
    if binding.runner_kind != profile.kind {
        return Err(eyre!(
            "Resume mapping kind mismatch for runner '{}': mapping={:?} runtime={:?}.",
            profile.name,
            binding.runner_kind,
            profile.kind
        ));
    }
    if binding.session.runner_kind() != profile.kind {
        return Err(eyre!(
            "Stored session '{}' belongs to {:?}, but runner '{}' is {:?}.",
            binding.session.session_id(),
            binding.session.runner_kind(),
            profile.name,
            profile.kind
        ));
    }
    Ok(())
}

async fn acquire_resume_session_guard(
    ctx: &ServeContext,
    request: &ExecutionRequest,
) -> color_eyre::eyre::Result<Option<tokio::sync::OwnedMutexGuard<()>>> {
    let Some(binding) = request.mode.resume_binding() else {
        return Ok(None);
    };

    let session_id = binding.session.session_id().to_string();
    let lock = {
        let mut guards = ctx.session_locks.lock().await;
        guards
            .entry(session_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    info!(
        request_id = %request.request_id,
        session_id = %session_id,
        "Acquiring per-session resume lock"
    );
    let guard = lock.lock_owned().await;
    Ok(Some(guard))
}

async fn persist_session_bindings_for_run(
    ctx: &ServeContext,
    request: &ExecutionRequest,
    profile: &RunnerProfile,
    session: &RunnerSessionHandle,
    message_ids: &[i32],
) -> color_eyre::eyre::Result<()> {
    if message_ids.is_empty() {
        return Ok(());
    }

    let now = Utc::now();
    let binding_template = SessionMessageBinding {
        chat_id: request.source.chat_id,
        message_id: message_ids[0],
        runner_name: profile.name.clone(),
        runner_kind: profile.kind,
        session: session.clone(),
        invocation: RunnerInvocationMetadata {
            binary: profile.binary.clone(),
            permission_mode: profile.permission_mode.clone(),
            model: profile.model.clone(),
            codex_profile: profile.codex.profile.clone(),
            codex_reasoning_effort: profile.codex.reasoning_effort,
        },
        request_id: request.request_id.clone(),
        source_user_message_id: request.source.message_id,
        created_at: now,
        updated_at: now,
    };

    let persisted = {
        let mut store = ctx.session_store.lock().await;
        store.upsert_bindings(binding_template, message_ids)?
    };
    info!(
        request_id = %request.request_id,
        runner = %profile.name,
        session_id = %session.session_id(),
        persisted_bindings = persisted,
        "Persisted Telegram message/session bindings"
    );
    Ok(())
}

async fn handle_control_command(
    ctx: &ServeContext,
    inbound: &InboundMessage,
    command: ControlCommand,
) -> color_eyre::eyre::Result<()> {
    let text = match command {
        ControlCommand::Help => build_help_message(&ctx.cfg.routing),
        ControlCommand::Models => build_models_message(&ctx.cfg),
        ControlCommand::Status => build_status_message(ctx).await,
    };

    send_telegram_message(
        &ctx.bot,
        ctx.parse_mode,
        inbound.chat_id,
        Some(inbound.message_id),
        &text,
    )
    .await
}

fn build_help_message(routing: &ServeRouting) -> String {
    let mut lines = Vec::new();
    lines.push("Vel serve control commands:".to_string());
    lines.push("/help - show usage".to_string());
    lines.push("/models - list configured runner profiles".to_string());
    lines.push("/status - show server + runner status".to_string());
    lines.push(String::new());
    lines.push("Prefix routing:".to_string());

    for route in &routing.routes {
        lines.push(format!("- {} -> {}", route.prefix, route.runner_name));
    }

    if routing.require_prefix {
        lines.push(String::new());
        if let Some(first) = routing.routes.first() {
            lines.push(format!(
                "Example: {} summarize this repository architecture",
                first.prefix
            ));
        }
    }

    lines.push(String::new());
    lines.push(
        "Session continuation: reply directly to a bot run message to continue the same session without a prefix."
            .to_string(),
    );

    lines.join("\n")
}

fn build_models_message(cfg: &ServeResolvedConfig) -> String {
    let mut names: Vec<_> = cfg.runners.keys().cloned().collect();
    names.sort();

    let mut prefix_map: HashMap<String, Vec<String>> = HashMap::new();
    for route in &cfg.routing.routes {
        prefix_map
            .entry(route.runner_name.clone())
            .or_default()
            .push(route.prefix.clone());
    }

    let mut lines = Vec::new();
    lines.push("Configured runners:".to_string());

    for name in names {
        if let Some(profile) = cfg.runners.get(&name) {
            let prefixes = prefix_map
                .get(&name)
                .cloned()
                .unwrap_or_default()
                .join(", ");
            let prefix_display = if prefixes.is_empty() {
                "(no prefixes)".to_string()
            } else {
                prefixes
            };
            let reasoning = if profile.kind == RunnerKind::Codex {
                profile
                    .codex
                    .reasoning_effort
                    .map(CodexReasoningEffort::as_str)
                    .unwrap_or("default")
                    .to_string()
            } else {
                "-".to_string()
            };
            lines.push(format!(
                "- {} [{}] model={} reasoning={} streaming={} attachments={} resume={} prefixes={} desc=\"{}\"",
                profile.name,
                match profile.kind {
                    RunnerKind::Claude => "claude",
                    RunnerKind::Codex => "codex",
                },
                profile
                    .model
                    .clone()
                    .unwrap_or_else(|| "(default)".to_string()),
                reasoning,
                profile.supports_streaming,
                profile.supports_attachments,
                profile.supports_session_resume,
                prefix_display,
                profile.description
            ));
        }
    }

    lines.join("\n")
}

async fn build_status_message(ctx: &ServeContext) -> String {
    let uptime = Utc::now() - ctx.started_at;
    let mut lines = Vec::new();
    lines.push("Service status:".to_string());
    lines.push(format!("- uptime: {}s", uptime.num_seconds().max(0)));
    lines.push(format!("- cwd: {}", ctx.cwd.display()));
    lines.push(format!(
        "- allowed chats: {}",
        ctx.cfg.allowed_chat_ids.len()
    ));
    lines.push(format!(
        "- allowed users: {}",
        if ctx.cfg.allowed_user_ids.is_empty() {
            "any in allowed chats".to_string()
        } else {
            ctx.cfg.allowed_user_ids.len().to_string()
        }
    ));
    let session_binding_count = {
        let store = ctx.session_store.lock().await;
        store.bindings.len()
    };
    lines.push(format!(
        "- session resume: enabled={} bindings={} store={}",
        ctx.cfg.session_resume.enabled,
        session_binding_count,
        ctx.cfg.session_resume.store_path.display()
    ));
    lines.push("- runners:".to_string());

    let mut runner_names: Vec<_> = ctx.cfg.runners.keys().cloned().collect();
    runner_names.sort();
    for name in runner_names {
        if let Some(profile) = ctx.cfg.runners.get(&name) {
            let probe = probe_runner(profile);
            lines.push(format!(
                "  {} => {} ({})",
                name,
                if probe.available {
                    "available"
                } else {
                    "unavailable"
                },
                probe.detail
            ));
        }
    }

    lines.join("\n")
}

fn route_inbound(
    routing: &ServeRouting,
    inbound: &InboundMessage,
    resume_binding: Option<&SessionMessageBinding>,
) -> RouteResult {
    let text = inbound.text.as_deref().map(str::trim).unwrap_or("");

    if text.is_empty() && inbound.attachments.is_empty() {
        return RouteResult::Ignore;
    }

    if let Some(cmd) = parse_control_command(text) {
        return RouteResult::Control(cmd);
    }

    if let Some(binding) = resume_binding {
        let prompt = if text.is_empty() {
            if inbound.attachments.is_empty() {
                return RouteResult::Usage(
                    "Reply is empty. Add text (or attachments) to continue this session."
                        .to_string(),
                );
            }
            "Please inspect the provided attachment(s) and continue this session with actionable guidance."
                .to_string()
        } else {
            text.to_string()
        };
        return RouteResult::Execute(PendingExecution {
            runner_name: binding.runner_name.clone(),
            mode: ExecutionMode::Resume {
                binding: binding.clone(),
            },
            prompt,
        });
    }

    if !text.is_empty()
        && let Some((runner_name, prompt)) = route_by_prefix(text, routing)
    {
        if prompt.trim().is_empty() {
            return RouteResult::Usage(
                "Command prefix found, but no task text was provided.".to_string(),
            );
        }
        return RouteResult::Execute(PendingExecution {
            runner_name,
            mode: ExecutionMode::New,
            prompt: prompt.trim().to_string(),
        });
    }

    if !routing.require_prefix {
        if let Some(default_runner) = &routing.default_runner {
            let prompt = if text.is_empty() {
                "Please inspect the provided attachment(s) and summarize actionable findings."
                    .to_string()
            } else {
                text.to_string()
            };
            return RouteResult::Execute(PendingExecution {
                runner_name: default_runner.clone(),
                mode: ExecutionMode::New,
                prompt,
            });
        }
    }

    if inbound.replied_to_message_id.is_some() && inbound.replied_to_is_bot_message {
        return RouteResult::Usage(
            "Reply target is a bot message, but no resumable session mapping was found. Start a new request with a model prefix (use /models)."
                .to_string(),
        );
    }

    if !text.is_empty() {
        if let Some(prefix_guess) = text
            .split_whitespace()
            .next()
            .and_then(|t| t.split(':').next())
            && text.contains(':')
            && !prefix_guess.is_empty()
        {
            return RouteResult::Usage(format!(
                "Unknown prefix `{}`. Use /models or /help for valid prefixes.",
                prefix_guess
            ));
        }
        return RouteResult::Usage(
            "Message did not match a configured prefix. Use /help for usage.".to_string(),
        );
    }

    RouteResult::Usage(
        "Attachments require a prefixed caption or a non-prefixed default runner configuration."
            .to_string(),
    )
}

fn parse_control_command(text: &str) -> Option<ControlCommand> {
    let command = text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .split('@')
        .next()
        .unwrap_or("");

    match command.to_ascii_lowercase().as_str() {
        "/help" => Some(ControlCommand::Help),
        "/models" => Some(ControlCommand::Models),
        "/status" => Some(ControlCommand::Status),
        _ => None,
    }
}

fn route_by_prefix(text: &str, routing: &ServeRouting) -> Option<(String, String)> {
    let lower = text.to_ascii_lowercase();
    for route in &routing.routes {
        if lower.starts_with(&route.prefix_lower) {
            let prompt = text[route.prefix.len()..].to_string();
            return Some((route.runner_name.clone(), prompt));
        }
    }
    None
}

fn extract_inbound_message(
    cfg: &ServeResolvedConfig,
    update: Update,
) -> color_eyre::eyre::Result<Option<InboundMessage>> {
    let update_id = safe_update_id(update.id.0);

    let message = match update.kind {
        UpdateKind::Message(message) | UpdateKind::EditedMessage(message) => Some(message),
        UpdateKind::ChannelPost(message) if cfg.allow_channel_posts => Some(message),
        _ => None,
    };

    let Some(message) = message else {
        return Ok(None);
    };

    let text = message
        .text()
        .map(str::to_string)
        .or_else(|| message.caption().map(str::to_string));

    let mut attachments = Vec::new();

    if cfg.attachment_policy.enabled {
        if let Some(photo_sizes) = message.photo()
            && cfg.attachment_policy.allow_photos
            && let Some(photo) = select_best_photo(photo_sizes)
        {
            attachments.push(photo);
        }

        if let Some(document) = message.document()
            && cfg.attachment_policy.allow_documents
        {
            attachments.push(AttachmentRef {
                kind: AttachmentKind::Document,
                file_id: document.file.id.to_string(),
                file_unique_id: document.file.unique_id.to_string(),
                width: None,
                height: None,
                file_size_hint: Some(u64::from(document.file.size)),
                file_name_hint: document.file_name.clone(),
                mime_type_hint: document
                    .mime_type
                    .as_ref()
                    .map(|m| m.essence_str().to_string()),
            });
        }
    }

    Ok(Some(InboundMessage {
        update_id,
        chat_id: message.chat.id.0,
        user_id: message
            .from
            .as_ref()
            .and_then(|user| i64::try_from(user.id.0).ok()),
        message_id: message.id.0,
        replied_to_message_id: message.reply_to_message().map(|reply| reply.id.0),
        replied_to_is_bot_message: message
            .reply_to_message()
            .and_then(|reply| reply.from.as_ref())
            .map(|user| user.is_bot)
            .unwrap_or(false),
        text,
        attachments,
    }))
}

fn select_best_photo(photos: &[PhotoSize]) -> Option<AttachmentRef> {
    photos
        .iter()
        .max_by_key(|p| {
            let area = u64::from(p.width) * u64::from(p.height);
            let size = u64::from(p.file.size);
            (area, size)
        })
        .map(|photo| AttachmentRef {
            kind: AttachmentKind::Photo,
            file_id: photo.file.id.to_string(),
            file_unique_id: photo.file.unique_id.to_string(),
            width: Some(photo.width),
            height: Some(photo.height),
            file_size_hint: Some(u64::from(photo.file.size)),
            file_name_hint: None,
            mime_type_hint: Some("image/*".to_string()),
        })
}

fn is_authorized(config: &ServeResolvedConfig, chat_id: i64, user_id: Option<i64>) -> bool {
    let chat_allowed = config.allowed_chat_ids.contains(&chat_id);

    let user_allowed = if config.allowed_user_ids.is_empty() {
        true
    } else {
        user_id
            .map(|id| config.allowed_user_ids.contains(&id))
            .unwrap_or(false)
    };

    chat_allowed && user_allowed
}

fn parse_chat_allowlist(chat_id_field: &str) -> color_eyre::eyre::Result<HashSet<i64>> {
    let mut out = HashSet::new();
    for item in chat_id_field
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let parsed = item.parse::<i64>().map_err(|e| {
            eyre!(
                "Invalid chat_id '{}' in [notifications.telegram]: {}",
                item,
                e
            )
        })?;
        out.insert(parsed);
    }
    Ok(out)
}

fn parse_optional_allowlist_env(env_name: &str) -> color_eyre::eyre::Result<HashSet<i64>> {
    let Some(raw) = std::env::var(env_name).ok() else {
        return Ok(HashSet::new());
    };

    let mut out = HashSet::new();
    for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let parsed = item
            .parse::<i64>()
            .map_err(|e| eyre!("Invalid ID '{}' in {}: {}", item, env_name, e))?;
        out.insert(parsed);
    }
    Ok(out)
}

fn safe_update_id(raw: u32) -> i32 {
    i32::try_from(raw).unwrap_or(i32::MAX)
}

fn apply_reserved_runner_overrides(name: &str, profile: &mut RunnerProfile) {
    if profile.kind == RunnerKind::Codex {
        // Telegram Codex policy: always run with maximum permissiveness to avoid trust/sandbox blocks.
        profile.codex.full_auto = Some(true);
        profile.codex.sandbox = Some("danger-full-access".to_string());
        profile.codex.skip_git_repo_check = Some(true);
    }

    // Policy guardrail: keep GPT-5.3 Codex on extra-high reasoning for Telegram-triggered runs.
    if name == "codex-gpt-5-3-codex" && profile.kind == RunnerKind::Codex {
        profile.codex.reasoning_effort = Some(CodexReasoningEffort::Xhigh);
    }
}

fn probe_runner(profile: &RunnerProfile) -> RunnerProbe {
    let output = std::process::Command::new(&profile.binary)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let detail = if stdout.is_empty() {
                "ok".to_string()
            } else {
                stdout
            };
            RunnerProbe {
                available: true,
                detail,
            }
        }
        Ok(out) => RunnerProbe {
            available: false,
            detail: format!("{}", out.status),
        },
        Err(err) => RunnerProbe {
            available: false,
            detail: err.to_string(),
        },
    }
}

async fn run_codex_profile(
    request: &ExecutionRequest,
    profile: &RunnerProfile,
    defaults: &Defaults,
    cwd: &Path,
    on_progress: &mut (dyn FnMut(RunnerProgressEvent) + Send),
) -> color_eyre::eyre::Result<RunnerExecution> {
    let mut codex_cfg: CodexConfig = defaults.codex.clone();
    if let Some(full_auto) = profile.codex.full_auto {
        codex_cfg.full_auto = full_auto;
    }
    if let Some(sandbox) = &profile.codex.sandbox {
        codex_cfg.sandbox = sandbox.clone();
    }
    if let Some(skip) = profile.codex.skip_git_repo_check {
        codex_cfg.skip_git_repo_check = skip;
    }
    if let Some(progress_cursor) = profile.codex.progress_cursor {
        codex_cfg.progress_cursor = progress_cursor;
    }
    if let Some(reasoning_effort) = profile.codex.reasoning_effort {
        codex_cfg.model_reasoning_effort = Some(reasoning_effort);
    }
    if let Some(model) = &profile.model {
        codex_cfg.model = Some(model.clone());
    }
    if let Some(profile_name) = &profile.codex.profile {
        codex_cfg.profile = Some(profile_name.clone());
    }
    // Telegram Codex policy: always maximize execution permissions.
    codex_cfg.full_auto = true;
    codex_cfg.sandbox = "danger-full-access".to_string();
    codex_cfg.skip_git_repo_check = true;

    let reasoning_label = codex_cfg
        .model_reasoning_effort
        .map(CodexReasoningEffort::as_str)
        .unwrap_or("default")
        .to_string();
    let resume_session_id = match request.mode.resume_binding() {
        Some(binding) => match &binding.session {
            RunnerSessionHandle::Codex { session_id } => Some(session_id.clone()),
            RunnerSessionHandle::Claude { .. } => {
                return Err(eyre!(
                    "requested codex resume with a non-codex session handle"
                ));
            }
        },
        None => None,
    };

    on_progress(RunnerProgressEvent::Status(format!(
        "running {} (model={} reasoning={} mode={})",
        profile.name,
        profile
            .model
            .clone()
            .unwrap_or_else(|| "default".to_string()),
        reasoning_label,
        if resume_session_id.is_some() {
            "resume"
        } else {
            "new"
        },
    )));

    let image_paths: Vec<PathBuf> = request
        .attachments
        .iter()
        .filter(|a| a.mime_type.starts_with("image/"))
        .map(|a| a.path.clone())
        .collect();

    let params = velor_core::execution_service::adapters::codex::CodexParams {
        binary: profile.binary.clone(),
        prompt: bytes::Bytes::copy_from_slice(request.prompt.as_bytes()),
        working_directory: cwd.to_path_buf(),
        config: codex_cfg,
        images: image_paths,
        resume_session: resume_session_id,
        extra_args: profile.args.clone(),
        extra_env: profile
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        timeouts: velor_core::execution_service::supervisor::ProcessTimeouts {
            total: Some(profile.timeout),
            ..std::default::Default::default()
        },
        cancellation: tokio_util::sync::CancellationToken::new(),
    };
    run_profile_via_service(
        velor_core::execution_service::service::AgentProfile::Codex(params),
        RunnerKind::Codex,
        on_progress,
    )
    .await
}

async fn run_claude_like_profile(
    request: &ExecutionRequest,
    profile: &RunnerProfile,
    cwd: &Path,
    on_progress: &mut (dyn FnMut(RunnerProgressEvent) + Send),
) -> color_eyre::eyre::Result<RunnerExecution> {
    let resume_session_id = match request.mode.resume_binding() {
        Some(binding) => match &binding.session {
            RunnerSessionHandle::Claude { session_id } => Some(session_id.clone()),
            RunnerSessionHandle::Codex { .. } => {
                return Err(eyre!(
                    "requested claude resume with a non-claude session handle"
                ));
            }
        },
        None => None,
    };

    on_progress(RunnerProgressEvent::Status(format!(
        "running {} (model={} mode={})",
        profile.name,
        profile
            .model
            .clone()
            .unwrap_or_else(|| "default".to_string()),
        if resume_session_id.is_some() {
            "resume"
        } else {
            "new"
        },
    )));

    let params = velor_core::execution_service::adapters::claude::ClaudeParams {
        binary: profile.binary.clone(),
        permission_mode: profile.permission_mode.clone(),
        prompt: bytes::Bytes::copy_from_slice(request.prompt.as_bytes()),
        working_directory: cwd.to_path_buf(),
        model: profile.model.clone(),
        resume_session: resume_session_id,
        extra_args: profile.args.clone(),
        extra_env: profile
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        timeouts: velor_core::execution_service::supervisor::ProcessTimeouts {
            total: Some(profile.timeout),
            ..std::default::Default::default()
        },
        cancellation: tokio_util::sync::CancellationToken::new(),
        enable_live_steering: false,
    };
    run_profile_via_service(
        velor_core::execution_service::service::AgentProfile::Claude(params),
        RunnerKind::Claude,
        on_progress,
    )
    .await
}

/// Runs an agent profile through the shared [`AgentExecutionService`], mapping
/// provider [`AgentEvent`]s to serve [`RunnerProgressEvent`]s and capturing the
/// resumed session.
///
/// [`AgentExecutionService`]: velor_core::execution_service::service::AgentExecutionService
/// [`AgentEvent`]: velor_core::agent::AgentEvent
async fn run_profile_via_service(
    profile: velor_core::execution_service::service::AgentProfile,
    kind: RunnerKind,
    on_progress: &mut (dyn FnMut(RunnerProgressEvent) + Send),
) -> color_eyre::eyre::Result<RunnerExecution> {
    let mut execution = velor_core::execution_service::service::shared_service()
        .execute(profile)
        .await
        .map_err(|e| eyre!("failed to start agent execution: {e}"))?;
    let mut session: Option<RunnerSessionHandle> = None;
    while let Some(event) = execution.next_event().await {
        for progress_event in map_agent_event_to_progress(event, kind, &mut session) {
            on_progress(progress_event);
        }
    }
    let report = execution.complete().await.map_err(|e| match e {
        velor_core::execution_service::error::AgentExecutionError::UnsuccessfulExit(ue) => {
            // Preserve the legacy diagnostic: exit code + stderr tail (e.g. the
            // "resume session expired" message a wrapper writes to stderr).
            let stderr_preview = truncate_for_telegram(ue.stderr.tail_str().trim(), 700);
            eyre!(
                "runner exited non-zero (code={}) stderr: {}",
                ue.code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "<signal>".to_string()),
                if stderr_preview.is_empty() {
                    "<empty>".to_string()
                } else {
                    stderr_preview
                }
            )
        }
        other => eyre!("agent execution failed: {other}"),
    })?;
    Ok(RunnerExecution {
        stdout: report.result.stdout,
        stderr: String::new(),
        session,
    })
}

/// Maps one provider [`AgentEvent`] to zero or more [`RunnerProgressEvent`]s and
/// updates the captured session when a session-id is observed.
///
/// [`AgentEvent`]: velor_core::agent::AgentEvent
fn map_agent_event_to_progress(
    event: velor_core::agent::AgentEvent,
    kind: RunnerKind,
    session: &mut Option<RunnerSessionHandle>,
) -> Vec<RunnerProgressEvent> {
    use velor_core::agent::AgentEvent;
    match event {
        AgentEvent::Status { message } => {
            let mut out = Vec::new();
            if let Some(id) = message.strip_prefix("thread started: ")
                && matches!(kind, RunnerKind::Codex)
            {
                let handle = RunnerSessionHandle::Codex {
                    session_id: id.to_string(),
                };
                *session = Some(handle.clone());
                out.push(RunnerProgressEvent::SessionBound { session: handle });
            } else if let Some(id) = message.strip_prefix("session: ")
                && matches!(kind, RunnerKind::Claude)
            {
                let handle = RunnerSessionHandle::Claude {
                    session_id: id.to_string(),
                };
                *session = Some(handle.clone());
                out.push(RunnerProgressEvent::SessionBound { session: handle });
            }
            out.push(RunnerProgressEvent::Status(message));
            out
        }
        AgentEvent::TextDelta { text } => vec![RunnerProgressEvent::OutputDelta(text)],
        AgentEvent::Thinking { text } => {
            vec![RunnerProgressEvent::OutputDelta(format!("💭 {text}"))]
        }
        AgentEvent::ToolCall { tool, detail, .. } => {
            vec![RunnerProgressEvent::Milestone(format!(
                "tool start: {tool} ({detail})"
            ))]
        }
        AgentEvent::ToolResult { detail, .. } => {
            vec![RunnerProgressEvent::Milestone(format!(
                "tool result: {detail}"
            ))]
        }
        AgentEvent::FileEdit { edit } => {
            vec![RunnerProgressEvent::Milestone(format!(
                "edited {} ({} line{})",
                edit.path,
                edit.diff_line_count(),
                if edit.diff_line_count() == 1 { "" } else { "s" }
            ))]
        }
        AgentEvent::Usage { .. } => Vec::new(),
        AgentEvent::Error { message } => vec![RunnerProgressEvent::Error(message)],
    }
}

async fn send_telegram_message(
    bot: &Bot,
    parse_mode: Option<TelegramParseMode>,
    chat_id: i64,
    reply_to_message_id: Option<i32>,
    text: &str,
) -> color_eyre::eyre::Result<()> {
    send_telegram_message_internal(bot, parse_mode, chat_id, reply_to_message_id, text)
        .await
        .wrap_err("Telegram sendMessage failed")?;
    Ok(())
}

async fn send_telegram_message_internal(
    bot: &Bot,
    parse_mode: Option<TelegramParseMode>,
    chat_id: i64,
    reply_to_message_id: Option<i32>,
    text: &str,
) -> Result<Message, RequestError> {
    let base = normalize_telegram_text(text);
    let (msg, mode) = format_outbound_message(&base, parse_mode);

    let mut attempts = 0u8;
    loop {
        let mut request = bot.send_message(ChatId(chat_id), msg.clone());

        if let Some(reply_to_message_id) = reply_to_message_id {
            request =
                request.reply_parameters(ReplyParameters::new(MessageId(reply_to_message_id)));
        }
        if let Some(mode) = mode {
            request = request.parse_mode(mode);
        }

        match request.send().await {
            Ok(sent) => return Ok(sent),
            Err(err) if should_retry_telegram_error(&err) && attempts < 2 => {
                attempts = attempts.saturating_add(1);
                let delay = retry_delay_for(&err);
                warn!(
                    chat_id,
                    attempt = attempts,
                    delay_ms = delay.as_millis(),
                    error = %err,
                    "Retrying Telegram send after transient failure"
                );
                tokio::time::sleep(delay).await;
            }
            Err(err) => return Err(err),
        }
    }
}

async fn edit_telegram_message(
    bot: &Bot,
    parse_mode: Option<TelegramParseMode>,
    chat_id: i64,
    message_id: i32,
    text: &str,
) -> Result<(), RequestError> {
    let base = normalize_telegram_text(text);
    let (msg, mode) = format_outbound_message(&base, parse_mode);

    let mut attempts = 0u8;
    loop {
        let mut request =
            bot.edit_message_text(ChatId(chat_id), MessageId(message_id), msg.clone());
        if let Some(mode) = mode {
            request = request.parse_mode(mode);
        }

        match request.send().await {
            Ok(_) => return Ok(()),
            Err(err) if is_message_not_modified_error(&err) => return Ok(()),
            Err(err) if should_retry_telegram_error(&err) && attempts < 2 => {
                attempts = attempts.saturating_add(1);
                let delay = retry_delay_for(&err);
                warn!(
                    chat_id,
                    message_id,
                    attempt = attempts,
                    delay_ms = delay.as_millis(),
                    error = %err,
                    "Retrying Telegram edit after transient failure"
                );
                tokio::time::sleep(delay).await;
            }
            Err(err) => return Err(err),
        }
    }
}

fn normalize_telegram_text(text: &str) -> String {
    let mut base = text.trim().to_string();
    if base.is_empty() {
        base = "(empty response)".to_string();
    }
    let max_len = TELEGRAM_MAX_TEXT_HARD_LIMIT - 80;
    if base.len() > max_len {
        let idx = base.floor_char_boundary(max_len);
        base = format!("{}\n…[truncated]", &base[..idx]);
    }
    base
}

fn should_retry_telegram_error(err: &RequestError) -> bool {
    matches!(err, RequestError::Network(_))
        || err.retry_after().is_some()
        || matches!(err, RequestError::Api(ApiError::TooMuchMessages))
}

fn retry_delay_for(err: &RequestError) -> Duration {
    if let Some(after) = err.retry_after() {
        return after
            .duration()
            .clamp(Duration::from_millis(300), Duration::from_secs(8));
    }
    Duration::from_millis(700)
}

fn is_message_not_modified_error(err: &RequestError) -> bool {
    matches!(err, RequestError::Api(ApiError::MessageNotModified))
}

fn is_message_too_long_error(err: &RequestError) -> bool {
    matches!(
        err,
        RequestError::Api(ApiError::MessageIsTooLong | ApiError::EditedMessageIsTooLong)
    )
}

fn is_message_not_editable_error(err: &RequestError) -> bool {
    matches!(
        err,
        RequestError::Api(
            ApiError::MessageCantBeEdited
                | ApiError::MessageToEditNotFound
                | ApiError::MessageIdInvalid
        )
    )
}

fn format_outbound_message(
    text: &str,
    parse_mode: Option<TelegramParseMode>,
) -> (String, Option<ParseMode>) {
    match parse_mode {
        Some(TelegramParseMode::MarkdownV2) => {
            (escape_markdown_v2(text), Some(ParseMode::MarkdownV2))
        }
        Some(TelegramParseMode::Html) => (escape_html(text), Some(ParseMode::Html)),
        None => (text.to_string(), None),
    }
}

fn escape_markdown_v2(text: &str) -> String {
    const RESERVED: &[char] = &[
        '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
        '\\',
    ];
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if RESERVED.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn truncate_for_telegram(text: &str, max_len: usize) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= max_len {
        return trimmed.to_string();
    }
    let idx = trimmed.floor_char_boundary(max_len);
    format!("{}…", &trimmed[..idx])
}

fn previous_char_boundary(text: &str, idx: usize) -> Option<usize> {
    if idx == 0 || text.is_empty() {
        return None;
    }
    let mut cursor = idx.saturating_sub(1).min(text.len().saturating_sub(1));
    while cursor > 0 && !text.is_char_boundary(cursor) {
        cursor = cursor.saturating_sub(1);
    }
    if text.is_char_boundary(cursor) {
        Some(cursor)
    } else {
        None
    }
}

fn select_natural_split(prefix: &str) -> usize {
    if prefix.is_empty() {
        return 0;
    }
    let hard_end = prefix.len();
    let soft_threshold = hard_end / 2;

    if let Some(pos) = prefix.rfind('\n') {
        let split = pos.saturating_add(1);
        if split >= soft_threshold {
            return split;
        }
    }
    if let Some(pos) = prefix.rfind(' ') {
        let split = pos.saturating_add(1);
        if split >= soft_threshold {
            return split;
        }
    }
    hard_end
}

fn build_prompt_with_attachments(prompt: &str, attachments: &[DownloadedAttachment]) -> String {
    if attachments.is_empty() {
        return prompt.to_string();
    }

    let mut lines = Vec::new();
    lines.push(prompt.to_string());
    lines.push(String::new());
    lines.push("Attachment context:".to_string());

    for file in attachments {
        lines.push(format!(
            "- kind={} mime={} size={} path={} file_id={} unique_id={} dims={} file_name={}",
            file.kind.label(),
            file.mime_type,
            file.file_size,
            file.path.display(),
            file.file_id,
            file.file_unique_id,
            format_dims(file.width, file.height),
            file.file_name
                .clone()
                .unwrap_or_else(|| "(none)".to_string())
        ));
    }

    lines.join("\n")
}

fn format_dims(width: Option<u32>, height: Option<u32>) -> String {
    match (width, height) {
        (Some(w), Some(h)) => format!("{}x{}", w, h),
        _ => "n/a".to_string(),
    }
}

async fn download_attachment(
    ctx: &ServeContext,
    request_id: &str,
    attachment: &AttachmentRef,
) -> color_eyre::eyre::Result<DownloadedAttachment> {
    if !ctx.cfg.attachment_policy.enabled {
        return Err(eyre!("attachment processing is disabled"));
    }

    let file = ctx
        .bot
        .get_file(attachment.file_id.clone())
        .send()
        .await
        .wrap_err("Telegram getFile failed")?;

    let remote_size = usize::try_from(file.size)
        .unwrap_or(usize::MAX)
        .max(attachment.file_size_hint.unwrap_or(0) as usize);

    if remote_size > ctx.cfg.attachment_policy.max_download_bytes {
        return Err(eyre!(
            "attachment exceeds configured max_download_bytes ({} > {})",
            remote_size,
            ctx.cfg.attachment_policy.max_download_bytes
        ));
    }

    let mut bytes = Vec::new();
    ctx.bot
        .download_file(&file.path, &mut bytes)
        .await
        .wrap_err("failed to download Telegram attachment")?;

    if bytes.len() > ctx.cfg.attachment_policy.max_download_bytes {
        return Err(eyre!(
            "downloaded attachment exceeds configured max_download_bytes ({} > {})",
            bytes.len(),
            ctx.cfg.attachment_policy.max_download_bytes
        ));
    }

    let detected = infer::get(&bytes);
    let detected_mime = detected
        .as_ref()
        .map(|kind| kind.mime_type().to_string())
        .or_else(|| attachment.mime_type_hint.clone())
        .unwrap_or_else(|| "application/octet-stream".to_string());

    match attachment.kind {
        AttachmentKind::Photo => {
            if !detected_mime.starts_with("image/") {
                return Err(eyre!(
                    "photo attachment MIME '{}' is not image/*",
                    detected_mime
                ));
            }
        }
        AttachmentKind::Document => {
            if !ctx.cfg.attachment_policy.allow_documents {
                return Err(eyre!("document attachments are disabled"));
            }
            let allowed = ctx
                .cfg
                .attachment_policy
                .allowed_document_mime_prefixes
                .iter()
                .any(|prefix| detected_mime.starts_with(prefix));
            if !allowed {
                return Err(eyre!(
                    "document MIME '{}' blocked by attachment policy",
                    detected_mime
                ));
            }
        }
    }

    tokio::fs::create_dir_all(&ctx.cfg.media_dir)
        .await
        .wrap_err("failed to create media directory")?;

    let extension = detected
        .map(|kind| kind.extension().to_string())
        .or_else(|| {
            attachment
                .file_name_hint
                .as_deref()
                .and_then(|name| Path::new(name).extension())
                .map(|ext| ext.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "bin".to_string());

    let safe_unique = sanitize_component(&attachment.file_unique_id);
    let filename = format!(
        "{}-{}-{}.{}",
        request_id,
        attachment.kind.label(),
        safe_unique,
        extension
    );

    let path = ctx.cfg.media_dir.join(filename);
    tokio::fs::write(&path, &bytes)
        .await
        .wrap_err("failed to persist downloaded attachment")?;

    info!(
        request_id = %request_id,
        kind = %attachment.kind.label(),
        file_id = %attachment.file_id,
        mime = %detected_mime,
        size = bytes.len(),
        path = %path.display(),
        "Downloaded Telegram attachment"
    );

    Ok(DownloadedAttachment {
        kind: attachment.kind,
        file_id: attachment.file_id.clone(),
        file_unique_id: attachment.file_unique_id.clone(),
        path,
        mime_type: detected_mime,
        width: attachment.width,
        height: attachment.height,
        file_size: bytes.len(),
        file_name: attachment.file_name_hint.clone(),
    })
}

fn sanitize_component(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn test_presentation_config() -> TelegramPresentationConfig {
        TelegramPresentationConfig {
            default_verbosity: TelegramResultVerbosity::Compact,
            max_changed_files: 5,
            max_section_chars: 120,
            include_debug_footer_on_success: false,
            include_follow_up_hints: true,
            path_truncation: TelegramPathTruncation::Left,
            raw_log_dir: PathBuf::from("/tmp/velor-serve-run-logs"),
        }
    }

    fn sample_summary(status: ExecutionSummaryStatus) -> ExecutionResultSummary {
        ExecutionResultSummary {
            title: "Refactor config loader".to_string(),
            final_status: status,
            runner_name: "codex-gpt-5-4".to_string(),
            repo_name: Some("velor".to_string()),
            branch: Some("main".to_string()),
            duration_secs: 18,
            high_level_summary: vec![
                "Updated configuration merge logic.".to_string(),
                "Kept backwards compatibility for legacy fields.".to_string(),
            ],
            tool_activity: vec![
                "Analysed code/resources through 3 read action(s).".to_string(),
                "Applied 2 write/edit action(s).".to_string(),
            ],
            changed_files: vec![
                "apps/velor-cli/src/serve.rs".to_string(),
                "docs/codex-telegram-server.md".to_string(),
            ],
            verification_steps: vec![
                "cargo check -q -p velor-cli".to_string(),
                "cargo test -q -p velor-cli".to_string(),
            ],
            notable_outputs: vec!["All checks passed.".to_string()],
            next_actions: vec!["Reply `details` for full logs.".to_string()],
            raw_request_metadata: ExecutionRawRequestMetadata {
                request_id: "tg-1-123".to_string(),
                transport: "telegram".to_string(),
                chat_id: -100,
                user_id: Some(1),
                source_message_id: Some(222),
                update_id: 1,
                runner_name: "codex-gpt-5-4".to_string(),
                runner_kind: RunnerKind::Codex,
                mode: "new",
                attachment_count: 0,
                received_at: Utc::now(),
            },
            diagnostics: Some(ExecutionDiagnostics {
                first_error: if status == ExecutionSummaryStatus::Completed {
                    None
                } else {
                    Some("runner exited non-zero".to_string())
                },
                stderr_preview: if status == ExecutionSummaryStatus::Completed {
                    None
                } else {
                    Some("stderr preview".to_string())
                },
                raw_log_path: Some("/tmp/velor-serve-run-logs/tg-1-123.json".to_string()),
            }),
        }
    }

    #[test]
    fn replay_cache_rejects_duplicates() {
        let now = Instant::now();
        let mut cache = ReplayCache::new(Duration::from_secs(30));

        assert!(
            cache.insert_if_new(1, now),
            "first insert should be accepted"
        );
        assert!(
            !cache.insert_if_new(1, now + Duration::from_secs(1)),
            "duplicate insert should be rejected"
        );
    }

    #[test]
    fn replay_cache_evicts_after_ttl() {
        let now = Instant::now();
        let mut cache = ReplayCache::new(Duration::from_secs(10));
        assert!(cache.insert_if_new(7, now));
        assert!(cache.insert_if_new(7, now + Duration::from_secs(11)));
    }

    #[test]
    fn rate_limiter_enforces_window_limit() {
        let now = Instant::now();
        let mut limiter = SlidingWindowRateLimiter::new(Duration::from_secs(60), 2);
        assert!(limiter.allow("k", now));
        assert!(limiter.allow("k", now + Duration::from_secs(1)));
        assert!(!limiter.allow("k", now + Duration::from_secs(2)));
        assert!(limiter.allow("k", now + Duration::from_secs(61)));
    }

    #[test]
    fn route_by_prefix_matches_configured_prefixes() {
        let routing = ServeRouting {
            require_prefix: true,
            default_runner: None,
            routes: vec![PrefixRoute {
                prefix: "5.4:".to_string(),
                prefix_lower: "5.4:".to_string(),
                runner_name: "codex-gpt-5-4".to_string(),
            }],
        };

        let msg = InboundMessage {
            update_id: 1,
            chat_id: 1,
            user_id: Some(1),
            message_id: 101,
            replied_to_message_id: None,
            replied_to_is_bot_message: false,
            text: Some("5.4: refactor this module".to_string()),
            attachments: Vec::new(),
        };

        let out = route_inbound(&routing, &msg, None);
        match out {
            RouteResult::Execute(pending) => {
                assert_eq!(pending.runner_name, "codex-gpt-5-4");
                assert_eq!(pending.prompt, "refactor this module");
            }
            _ => panic!("expected execution route"),
        }
    }

    #[test]
    fn route_unknown_prefix_returns_usage() {
        let routing = ServeRouting {
            require_prefix: true,
            default_runner: None,
            routes: vec![PrefixRoute {
                prefix: "sonnet:".to_string(),
                prefix_lower: "sonnet:".to_string(),
                runner_name: "claude-sonnet-4-6".to_string(),
            }],
        };

        let msg = InboundMessage {
            update_id: 1,
            chat_id: 1,
            user_id: Some(1),
            message_id: 101,
            replied_to_message_id: None,
            replied_to_is_bot_message: false,
            text: Some("badprefix: do thing".to_string()),
            attachments: Vec::new(),
        };

        let out = route_inbound(&routing, &msg, None);
        match out {
            RouteResult::Usage(text) => {
                assert!(text.contains("Unknown prefix"));
            }
            _ => panic!("expected usage route"),
        }
    }

    #[test]
    fn route_control_commands_work() {
        let routing = ServeRouting {
            require_prefix: true,
            default_runner: None,
            routes: Vec::new(),
        };

        let msg = InboundMessage {
            update_id: 1,
            chat_id: 1,
            user_id: Some(1),
            message_id: 101,
            replied_to_message_id: None,
            replied_to_is_bot_message: false,
            text: Some("/models".to_string()),
            attachments: Vec::new(),
        };

        let out = route_inbound(&routing, &msg, None);
        assert!(matches!(out, RouteResult::Control(ControlCommand::Models)));
    }

    #[test]
    fn is_authorized_checks_chat_and_user_allowlists() {
        let cfg = ServeResolvedConfig {
            poll_timeout_secs: 10,
            poll_limit: 10,
            include_backlog: false,
            allowed_chat_ids: HashSet::from([1]),
            allowed_user_ids: HashSet::from([2]),
            allow_channel_posts: true,
            max_requests_per_minute: 20,
            max_concurrent_tasks: 1,
            streaming: TelegramStreamingConfig {
                enabled: true,
                edit_throttle: Duration::from_secs(2),
                max_message_chars: 3600,
                flush_on_milestones: true,
            },
            presentation: TelegramPresentationConfig {
                default_verbosity: TelegramResultVerbosity::Compact,
                max_changed_files: 5,
                max_section_chars: 500,
                include_debug_footer_on_success: false,
                include_follow_up_hints: true,
                path_truncation: TelegramPathTruncation::Left,
                raw_log_dir: PathBuf::from("/tmp/velor-serve-run-logs"),
            },
            media_dir: PathBuf::from("/tmp"),
            attachment_policy: ServeAttachmentPolicy {
                enabled: true,
                allow_photos: true,
                allow_documents: true,
                max_download_bytes: 1024,
                keep_files: false,
                allowed_document_mime_prefixes: vec!["image/".to_string()],
            },
            session_resume: SessionResumeConfig {
                enabled: true,
                store_path: PathBuf::from("/tmp/sessions.json"),
                max_bindings: 1024,
            },
            routing: ServeRouting {
                require_prefix: true,
                default_runner: None,
                routes: Vec::new(),
            },
            runners: HashMap::new(),
        };

        assert!(is_authorized(&cfg, 1, Some(2)));
        assert!(!is_authorized(&cfg, 1, Some(3)));
        assert!(!is_authorized(&cfg, 3, Some(2)));
    }

    #[test]
    fn parse_chat_allowlist_parses_comma_list() {
        let parsed = parse_chat_allowlist("-1001,42");
        assert!(parsed.is_ok());
        let set = parsed.unwrap_or_default();
        assert!(set.contains(&-1001));
        assert!(set.contains(&42));
    }

    #[test]
    fn markdown_escape_works() {
        let escaped = escape_markdown_v2("a_b*c");
        assert_eq!(escaped, "a\\_b\\*c");
    }

    #[test]
    fn edit_scheduler_throttles_until_interval() {
        let mut scheduler = TelegramEditScheduler::new(Duration::from_secs(1));
        let t0 = Instant::now();
        assert!(scheduler.should_flush(false, t0));
        scheduler.mark_flushed(t0);
        assert!(
            !scheduler.should_flush(false, t0 + Duration::from_millis(500)),
            "flush should be throttled before interval expires"
        );
        assert!(
            scheduler.should_flush(false, t0 + Duration::from_millis(1001)),
            "flush should be allowed after interval"
        );
        assert!(
            scheduler.should_flush(true, t0 + Duration::from_millis(200)),
            "forced flush must bypass throttle"
        );
    }

    #[test]
    fn message_chunker_splits_long_output() {
        let renderer = TelegramStreamRenderer::new("req-1".to_string(), "runner-a".to_string());
        let chunker = TelegramMessageChunker::new(650);
        let output = "x".repeat(2500);
        let split = chunker.split_output(&renderer, None, 1, &output);
        assert!(split.is_some(), "split should be produced for long output");
        let idx = split.unwrap_or(0);
        assert!(idx > 0, "split index should be positive");
        assert!(idx < output.len(), "split should happen before output end");
    }

    #[test]
    fn renderer_includes_terminal_summary() {
        let mut renderer = TelegramStreamRenderer::new("r1".to_string(), "codex".to_string());
        renderer.ingest_event(RunnerProgressEvent::Status("running".to_string()));
        renderer.ingest_event(RunnerProgressEvent::OutputDelta("hello".to_string()));
        renderer.mark_terminal(
            RunTerminalState::Completed,
            "completed in 2s".to_string(),
            None,
        );
        let text = renderer.render(1, renderer.output_tail(0), false);
        assert!(
            text.contains("vel serve | completed"),
            "terminal status should be visible"
        );
        assert!(
            text.contains("result: completed in 2s"),
            "terminal summary should be rendered"
        );
    }

    #[test]
    fn compact_success_render_is_scannable() {
        let summary = sample_summary(ExecutionSummaryStatus::Completed);
        let rendered = render_execution_summary(
            &summary,
            TelegramResultVerbosity::Compact,
            &test_presentation_config(),
        );
        assert!(
            rendered.contains("✅ Completed:"),
            "missing success status line"
        );
        assert!(rendered.contains("Summary"), "missing Summary section");
        assert!(rendered.contains("Changed"), "missing Changed section");
        assert!(rendered.contains("Result"), "missing Result section");
        assert!(
            !rendered.contains("tool start:"),
            "compact output should not dump raw tool traces"
        );
    }

    #[test]
    fn standard_failure_render_includes_failure_sections() {
        let summary = sample_summary(ExecutionSummaryStatus::Failed);
        let rendered = render_execution_summary(
            &summary,
            TelegramResultVerbosity::Standard,
            &test_presentation_config(),
        );
        assert!(
            rendered.contains("❌ Failed:"),
            "missing failure status line"
        );
        assert!(rendered.contains("Failure"), "missing Failure section");
        assert!(rendered.contains("Likely cause"), "missing likely cause");
        assert!(
            rendered.contains("Changed before failure"),
            "missing changed-before-failure section"
        );
        assert!(
            rendered.contains("Next suggested action"),
            "missing next action section"
        );
    }

    #[test]
    fn section_truncation_and_more_aggregation_work() {
        let mut summary = sample_summary(ExecutionSummaryStatus::Completed);
        summary.changed_files = vec![
            "a/one.rs".to_string(),
            "a/two.rs".to_string(),
            "a/three.rs".to_string(),
            "a/four.rs".to_string(),
            "a/five.rs".to_string(),
            "a/six.rs".to_string(),
        ];
        let mut cfg = test_presentation_config();
        cfg.max_changed_files = 3;
        let rendered = render_execution_summary(&summary, TelegramResultVerbosity::Compact, &cfg);
        assert!(rendered.contains("+3 more"), "expected +N aggregation");
    }

    #[test]
    fn section_length_limit_truncates_long_lines() {
        let mut summary = sample_summary(ExecutionSummaryStatus::Completed);
        summary.high_level_summary = vec!["x".repeat(260)];
        let mut cfg = test_presentation_config();
        cfg.max_section_chars = 40;
        let rendered = render_execution_summary(&summary, TelegramResultVerbosity::Compact, &cfg);
        assert!(
            rendered.contains("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx…"),
            "long summary line should be truncated"
        );
    }

    #[test]
    fn empty_sections_render_gracefully() {
        let mut summary = sample_summary(ExecutionSummaryStatus::Completed);
        summary.changed_files.clear();
        summary.verification_steps.clear();
        summary.notable_outputs.clear();
        let rendered = render_execution_summary(
            &summary,
            TelegramResultVerbosity::Standard,
            &test_presentation_config(),
        );
        assert!(
            rendered.contains("- none"),
            "empty sections should render with explicit none marker"
        );
    }

    #[test]
    fn markdown_escape_still_handles_rendered_summary() {
        let mut summary = sample_summary(ExecutionSummaryStatus::Completed);
        summary.title = "Fix a_b*(c)".to_string();
        let rendered = render_execution_summary(
            &summary,
            TelegramResultVerbosity::Compact,
            &test_presentation_config(),
        );
        let (escaped, mode) =
            format_outbound_message(&rendered, Some(TelegramParseMode::MarkdownV2));
        assert!(mode.is_some(), "markdown parse mode should be set");
        assert!(
            escaped.contains("\\_") && escaped.contains("\\*"),
            "markdown output should be escaped"
        );
    }

    #[test]
    fn formatting_schema_is_stable_across_runner_names() {
        let mut codex = sample_summary(ExecutionSummaryStatus::Completed);
        codex.runner_name = "codex-gpt-5-4".to_string();
        let mut claude = sample_summary(ExecutionSummaryStatus::Completed);
        claude.runner_name = "claude-sonnet-4-6".to_string();
        let codex_render = render_execution_summary(
            &codex,
            TelegramResultVerbosity::Compact,
            &test_presentation_config(),
        );
        let claude_render = render_execution_summary(
            &claude,
            TelegramResultVerbosity::Compact,
            &test_presentation_config(),
        );
        for header in ["Summary", "Changed", "Result"] {
            assert!(
                codex_render.contains(header) && claude_render.contains(header),
                "section {header} should be stable across runners"
            );
        }
    }

    #[test]
    fn long_path_truncation_preserves_tail() {
        let long = "/Users/liam/git/velor/apps/velor-cli/src/server/telegram/presenter.rs";
        let truncated = truncate_path(long, 32, TelegramPathTruncation::Left);
        assert!(truncated.starts_with('…'), "should truncate from left");
        assert!(
            truncated.ends_with("telegram/presenter.rs"),
            "tail should remain visible"
        );
    }

    #[test]
    fn persisted_execution_record_serializes() {
        let record = PersistedExecutionRecord {
            schema_version: 1,
            request: ExecutionRawRequestMetadata {
                request_id: "req-1".to_string(),
                transport: "telegram".to_string(),
                chat_id: -100,
                user_id: Some(1),
                source_message_id: Some(10),
                update_id: 42,
                runner_name: "codex-gpt-5-4".to_string(),
                runner_kind: RunnerKind::Codex,
                mode: "new",
                attachment_count: 0,
                received_at: Utc::now(),
            },
            runner_description: "test".to_string(),
            terminal_state: RunTerminalState::Completed,
            duration_secs: 1,
            prompt: "hello".to_string(),
            attachments: Vec::new(),
            progress_events: vec![RunnerProgressEventRecord {
                at: Utc::now(),
                event: RunnerProgressEvent::SessionBound {
                    session: RunnerSessionHandle::Codex {
                        session_id: "thread-1".to_string(),
                    },
                },
            }],
            stdout: "ok".to_string(),
            stderr: String::new(),
            summary: sample_summary(ExecutionSummaryStatus::Completed),
        };

        let serialized =
            serde_json::to_string(&record).expect("persisted execution record should serialize");
        assert!(
            serialized.contains("session_bound"),
            "serialized payload should include progress event enum tag"
        );
    }

    #[test]
    fn sanitize_component_replaces_unsafe_chars() {
        assert_eq!(sanitize_component("abc/def"), "abc_def");
    }

    #[test]
    fn default_prefix_mapping_contains_required_profiles() {
        let routes = default_prefix_routes();
        assert_eq!(
            routes.get("sonnet:"),
            Some(&"claude-sonnet-4-6".to_string())
        );
        assert_eq!(routes.get("opus:"), Some(&"claude-opus-4-6".to_string()));
        assert_eq!(
            routes.get("5.3-codex:"),
            Some(&"codex-gpt-5-3-codex".to_string())
        );
        assert_eq!(routes.get("5.3:"), Some(&"codex-gpt-5-3-codex".to_string()));
        assert_eq!(routes.get("5.4:"), Some(&"codex-gpt-5-4".to_string()));
        assert_eq!(routes.get("glm5.1:"), Some(&"glm-5-1".to_string()));
        assert_eq!(routes.get("5.1:"), Some(&"glm-5-1".to_string()));
    }

    #[test]
    fn codex_53_runner_forces_xhigh_reasoning() {
        let mut profile = RunnerProfile {
            name: "codex-gpt-5-3-codex".to_string(),
            description: "Codex GPT-5.3 Codex".to_string(),
            kind: RunnerKind::Codex,
            binary: "codex".to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
            model: Some("gpt-5.3-codex".to_string()),
            permission_mode: "acceptEdits".to_string(),
            timeout: Duration::from_secs(30),
            supports_attachments: true,
            supports_streaming: true,
            supports_session_resume: true,
            codex: CodexProfile {
                full_auto: Some(true),
                sandbox: Some("workspace-write".to_string()),
                skip_git_repo_check: Some(false),
                progress_cursor: Some(false),
                reasoning_effort: Some(CodexReasoningEffort::Low),
                profile: None,
            },
        };

        apply_reserved_runner_overrides("codex-gpt-5-3-codex", &mut profile);
        assert_eq!(
            profile.codex.reasoning_effort,
            Some(CodexReasoningEffort::Xhigh)
        );
        assert_eq!(profile.codex.full_auto, Some(true));
        assert_eq!(
            profile.codex.sandbox.as_deref(),
            Some("danger-full-access"),
            "codex should always run with maximum sandbox permissions in serve mode"
        );
        assert_eq!(
            profile.codex.skip_git_repo_check,
            Some(true),
            "codex should always skip git trust checks in serve mode"
        );
    }

    #[test]
    fn default_agent_cwd_prefers_home_git_when_available() {
        let temp = tempfile::tempdir().expect("tempdir should exist");
        let home = temp.path().join("home");
        let git = home.join("git");
        std::fs::create_dir_all(&git).expect("git dir should be created");

        let fallback = temp.path().join("fallback");
        std::fs::create_dir_all(&fallback).expect("fallback dir should be created");

        let resolved = resolve_default_agent_cwd_from_home(&fallback, Some(&home));
        assert_eq!(resolved, home.join("git"));
    }

    #[test]
    fn default_agent_cwd_falls_back_when_home_git_missing() {
        let temp = tempfile::tempdir().expect("tempdir should exist");
        let home = temp.path().join("home");
        std::fs::create_dir_all(&home).expect("home dir should be created");

        let fallback = temp.path().join("fallback");
        std::fs::create_dir_all(&fallback).expect("fallback dir should be created");

        let resolved = resolve_default_agent_cwd_from_home(&fallback, Some(&home));
        assert_eq!(resolved, fallback);
    }

    #[test]
    fn attachments_without_text_require_usage_when_prefix_required() {
        let routing = ServeRouting {
            require_prefix: true,
            default_runner: None,
            routes: vec![PrefixRoute {
                prefix: "5.4:".to_string(),
                prefix_lower: "5.4:".to_string(),
                runner_name: "codex-gpt-5-4".to_string(),
            }],
        };

        let msg = InboundMessage {
            update_id: 1,
            chat_id: 1,
            user_id: Some(1),
            message_id: 101,
            replied_to_message_id: None,
            replied_to_is_bot_message: false,
            text: None,
            attachments: vec![AttachmentRef {
                kind: AttachmentKind::Photo,
                file_id: "f".to_string(),
                file_unique_id: "u".to_string(),
                width: Some(1),
                height: Some(1),
                file_size_hint: Some(1),
                file_name_hint: None,
                mime_type_hint: Some("image/png".to_string()),
            }],
        };

        let out = route_inbound(&routing, &msg, None);
        assert!(
            matches!(out, RouteResult::Usage(_)),
            "expected usage guidance"
        );
    }

    #[test]
    fn route_reply_resumes_same_claude_session_without_prefix() {
        let routing = ServeRouting {
            require_prefix: true,
            default_runner: None,
            routes: vec![PrefixRoute {
                prefix: "sonnet:".to_string(),
                prefix_lower: "sonnet:".to_string(),
                runner_name: "claude-sonnet-4-6".to_string(),
            }],
        };
        let binding = SessionMessageBinding {
            chat_id: -100,
            message_id: 222,
            runner_name: "claude-sonnet-4-6".to_string(),
            runner_kind: RunnerKind::Claude,
            session: RunnerSessionHandle::Claude {
                session_id: "claude-session-1".to_string(),
            },
            invocation: RunnerInvocationMetadata {
                binary: "claude".to_string(),
                permission_mode: "acceptEdits".to_string(),
                model: Some("claude-sonnet-4-6".to_string()),
                codex_profile: None,
                codex_reasoning_effort: None,
            },
            request_id: "r1".to_string(),
            source_user_message_id: Some(10),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let msg = InboundMessage {
            update_id: 1,
            chat_id: -100,
            user_id: Some(1),
            message_id: 333,
            replied_to_message_id: Some(222),
            replied_to_is_bot_message: true,
            text: Some("continue with a test plan".to_string()),
            attachments: Vec::new(),
        };

        let out = route_inbound(&routing, &msg, Some(&binding));
        match out {
            RouteResult::Execute(pending) => {
                assert_eq!(pending.runner_name, "claude-sonnet-4-6");
                assert_eq!(pending.prompt, "continue with a test plan");
                match pending.mode {
                    ExecutionMode::Resume { binding } => {
                        assert_eq!(binding.session.session_id(), "claude-session-1");
                    }
                    ExecutionMode::New => panic!("expected resume mode"),
                }
            }
            _ => panic!("expected execution route"),
        }
    }

    #[test]
    fn route_reply_resumes_same_codex_session_without_prefix() {
        let routing = ServeRouting {
            require_prefix: true,
            default_runner: None,
            routes: vec![PrefixRoute {
                prefix: "5.4:".to_string(),
                prefix_lower: "5.4:".to_string(),
                runner_name: "codex-gpt-5-4".to_string(),
            }],
        };
        let binding = SessionMessageBinding {
            chat_id: -100,
            message_id: 222,
            runner_name: "codex-gpt-5-4".to_string(),
            runner_kind: RunnerKind::Codex,
            session: RunnerSessionHandle::Codex {
                session_id: "codex-thread-9".to_string(),
            },
            invocation: RunnerInvocationMetadata {
                binary: "codex".to_string(),
                permission_mode: "acceptEdits".to_string(),
                model: Some("gpt-5.4".to_string()),
                codex_profile: None,
                codex_reasoning_effort: None,
            },
            request_id: "r2".to_string(),
            source_user_message_id: Some(10),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let msg = InboundMessage {
            update_id: 1,
            chat_id: -100,
            user_id: Some(1),
            message_id: 333,
            replied_to_message_id: Some(222),
            replied_to_is_bot_message: true,
            text: Some("check the latest changes".to_string()),
            attachments: Vec::new(),
        };

        let out = route_inbound(&routing, &msg, Some(&binding));
        match out {
            RouteResult::Execute(pending) => {
                assert_eq!(pending.runner_name, "codex-gpt-5-4");
                assert_eq!(pending.prompt, "check the latest changes");
                match pending.mode {
                    ExecutionMode::Resume { binding } => {
                        assert_eq!(binding.session.session_id(), "codex-thread-9");
                    }
                    ExecutionMode::New => panic!("expected resume mode"),
                }
            }
            _ => panic!("expected execution route"),
        }
    }

    #[test]
    fn reply_to_non_agent_message_does_not_attempt_resume() {
        let routing = ServeRouting {
            require_prefix: true,
            default_runner: None,
            routes: vec![PrefixRoute {
                prefix: "5.4:".to_string(),
                prefix_lower: "5.4:".to_string(),
                runner_name: "codex-gpt-5-4".to_string(),
            }],
        };
        let msg = InboundMessage {
            update_id: 1,
            chat_id: 1,
            user_id: Some(1),
            message_id: 101,
            replied_to_message_id: Some(55),
            replied_to_is_bot_message: false,
            text: Some("continue".to_string()),
            attachments: Vec::new(),
        };

        let out = route_inbound(&routing, &msg, None);
        assert!(
            matches!(out, RouteResult::Usage(_)),
            "reply to non-agent message should fall back to normal prefix routing rules"
        );
    }

    #[test]
    fn missing_bot_reply_mapping_returns_clear_usage_error() {
        let routing = ServeRouting {
            require_prefix: true,
            default_runner: None,
            routes: vec![PrefixRoute {
                prefix: "5.4:".to_string(),
                prefix_lower: "5.4:".to_string(),
                runner_name: "codex-gpt-5-4".to_string(),
            }],
        };
        let msg = InboundMessage {
            update_id: 1,
            chat_id: 1,
            user_id: Some(1),
            message_id: 101,
            replied_to_message_id: Some(55),
            replied_to_is_bot_message: true,
            text: Some("continue".to_string()),
            attachments: Vec::new(),
        };

        let out = route_inbound(&routing, &msg, None);
        match out {
            RouteResult::Usage(text) => {
                assert!(
                    text.contains("no resumable session mapping"),
                    "usage text should explain missing resume mapping"
                );
            }
            _ => panic!("expected usage for missing reply mapping"),
        }
    }

    #[tokio::test]
    async fn active_run_registry_interrupts_reply_and_returns_binding() {
        let mut registry = ActiveRunRegistry::default();
        let task = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let abort_handle = task.abort_handle();
        let started_at = Utc::now();

        registry.register(ActiveRunEntry {
            request_id: "active-1".to_string(),
            chat_id: -100,
            runner_name: "codex-gpt-5-4".to_string(),
            runner_kind: RunnerKind::Codex,
            source_user_message_id: Some(10),
            invocation: RunnerInvocationMetadata {
                binary: "codex".to_string(),
                permission_mode: "acceptEdits".to_string(),
                model: Some("gpt-5.4".to_string()),
                codex_profile: None,
                codex_reasoning_effort: None,
            },
            session: Some(RunnerSessionHandle::Codex {
                session_id: "thread-9".to_string(),
            }),
            message_ids: HashSet::new(),
            abort_handle,
            started_at,
        });
        registry.sync_messages("active-1", &[222]);

        match registry.interrupt_for_reply(-100, 222) {
            ActiveRunReplyResolution::Interrupted(interrupted) => {
                assert_eq!(interrupted.interrupted_request_id, "active-1");
                assert_eq!(interrupted.binding.runner_name, "codex-gpt-5-4");
                assert_eq!(interrupted.binding.session.session_id(), "thread-9");
            }
            _ => panic!("expected interrupted response"),
        }
        assert!(
            !registry.by_request.contains_key("active-1"),
            "active run should be removed after interruption"
        );
        task.abort();
    }

    #[tokio::test]
    async fn active_run_registry_missing_session_is_reported() {
        let mut registry = ActiveRunRegistry::default();
        let task = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let abort_handle = task.abort_handle();

        registry.register(ActiveRunEntry {
            request_id: "active-2".to_string(),
            chat_id: -100,
            runner_name: "claude-sonnet-4-6".to_string(),
            runner_kind: RunnerKind::Claude,
            source_user_message_id: Some(11),
            invocation: RunnerInvocationMetadata {
                binary: "claude".to_string(),
                permission_mode: "acceptEdits".to_string(),
                model: Some("claude-sonnet-4-6".to_string()),
                codex_profile: None,
                codex_reasoning_effort: None,
            },
            session: None,
            message_ids: HashSet::new(),
            abort_handle,
            started_at: Utc::now(),
        });
        registry.sync_messages("active-2", &[333]);

        match registry.interrupt_for_reply(-100, 333) {
            ActiveRunReplyResolution::MissingSession {
                request_id,
                runner_name,
            } => {
                assert_eq!(request_id, "active-2");
                assert_eq!(runner_name, "claude-sonnet-4-6");
            }
            _ => panic!("expected missing-session response"),
        }
        task.abort();
    }

    #[test]
    fn validate_resume_rejects_unresumable_runner() {
        let request = ExecutionRequest {
            request_id: "r3".to_string(),
            received_at: Utc::now(),
            source: ExecutionSource {
                transport: "telegram",
                chat_id: 1,
                user_id: Some(1),
                message_id: Some(1),
                update_id: 1,
            },
            runner_name: "claude-sonnet-4-6".to_string(),
            mode: ExecutionMode::Resume {
                binding: SessionMessageBinding {
                    chat_id: 1,
                    message_id: 200,
                    runner_name: "claude-sonnet-4-6".to_string(),
                    runner_kind: RunnerKind::Claude,
                    session: RunnerSessionHandle::Claude {
                        session_id: "claude-session-7".to_string(),
                    },
                    invocation: RunnerInvocationMetadata {
                        binary: "claude".to_string(),
                        permission_mode: "acceptEdits".to_string(),
                        model: Some("claude-sonnet-4-6".to_string()),
                        codex_profile: None,
                        codex_reasoning_effort: None,
                    },
                    request_id: "seed".to_string(),
                    source_user_message_id: Some(100),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
            },
            prompt: "continue".to_string(),
            attachment_refs: Vec::new(),
            attachments: Vec::new(),
        };
        let profile = RunnerProfile {
            name: "claude-sonnet-4-6".to_string(),
            description: "Claude Sonnet 4.6".to_string(),
            kind: RunnerKind::Claude,
            binary: "claude".to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
            model: Some("claude-sonnet-4-6".to_string()),
            permission_mode: "acceptEdits".to_string(),
            timeout: Duration::from_secs(30),
            supports_attachments: false,
            supports_streaming: true,
            supports_session_resume: false,
            codex: CodexProfile::default(),
        };

        let result = validate_resume_request(&request, &profile);
        assert!(result.is_err(), "expected resume validation error");
        let msg = result
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "".to_string());
        assert!(
            msg.contains("does not support native session resuming"),
            "unexpected error text: {}",
            msg
        );
    }

    #[test]
    fn session_resume_store_persists_across_reload() {
        let temp = tempfile::tempdir().expect("tempdir should exist");
        let path = temp.path().join("session-store.json");
        let mut store =
            SessionResumeStore::load(path.clone(), 100).expect("session store should initialize");
        let now = Utc::now();
        let template = SessionMessageBinding {
            chat_id: -100,
            message_id: 1,
            runner_name: "codex-gpt-5-4".to_string(),
            runner_kind: RunnerKind::Codex,
            session: RunnerSessionHandle::Codex {
                session_id: "codex-thread-1".to_string(),
            },
            invocation: RunnerInvocationMetadata {
                binary: "codex".to_string(),
                permission_mode: "acceptEdits".to_string(),
                model: Some("gpt-5.4".to_string()),
                codex_profile: None,
                codex_reasoning_effort: None,
            },
            request_id: "request-1".to_string(),
            source_user_message_id: Some(77),
            created_at: now,
            updated_at: now,
        };
        store
            .upsert_bindings(template, &[11, 12])
            .expect("bindings should persist");

        let reloaded =
            SessionResumeStore::load(path, 100).expect("session store reload should succeed");
        let hit = reloaded
            .lookup(-100, 12)
            .expect("binding should be present after reload");
        assert_eq!(hit.runner_name, "codex-gpt-5-4");
        assert_eq!(hit.session.session_id(), "codex-thread-1");
    }

    #[tokio::test]
    async fn codex_runner_profile_executes_fake_binary() {
        let temp = tempfile::tempdir().expect("tempdir should exist");
        let bin_path = temp.path().join("fake-codex");
        let script = r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--version" ]; then
  echo "fake-codex 1.0"
  exit 0
fi
cat >/dev/null
printf '{"type":"thread.started","thread_id":"t1"}\n'
printf '{"type":"turn.started"}\n'
printf '{"type":"item.completed","item":{"type":"agent_message","text":"codex-ok"}}\n'
printf '{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":2}}\n'
"#;
        std::fs::write(&bin_path, script).expect("script should be written");
        let mut perms = std::fs::metadata(&bin_path)
            .expect("metadata should exist")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin_path, perms).expect("permissions should be set");

        let profile = RunnerProfile {
            name: "codex-test".to_string(),
            description: "codex test".to_string(),
            kind: RunnerKind::Codex,
            binary: bin_path.display().to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
            model: Some("gpt-5.4".to_string()),
            permission_mode: "acceptEdits".to_string(),
            timeout: Duration::from_secs(30),
            supports_attachments: true,
            supports_streaming: true,
            supports_session_resume: true,
            codex: CodexProfile {
                full_auto: Some(false),
                sandbox: Some("workspace-write".to_string()),
                skip_git_repo_check: Some(true),
                progress_cursor: Some(false),
                reasoning_effort: None,
                profile: None,
            },
        };

        let request = ExecutionRequest {
            request_id: "r1".to_string(),
            received_at: Utc::now(),
            source: ExecutionSource {
                transport: "telegram",
                chat_id: 1,
                user_id: Some(1),
                message_id: Some(1),
                update_id: 1,
            },
            runner_name: "codex-test".to_string(),
            mode: ExecutionMode::New,
            prompt: "test prompt".to_string(),
            attachment_refs: Vec::new(),
            attachments: Vec::new(),
        };

        let mut progress = Vec::new();
        let mut on_progress = |event: RunnerProgressEvent| progress.push(event);
        let out = run_codex_profile(
            &request,
            &profile,
            &Defaults::default(),
            temp.path(),
            &mut on_progress,
        )
        .await
        .expect("codex run should succeed");

        assert!(
            out.stdout.contains("codex-ok"),
            "stdout should contain fake codex output"
        );
        assert!(
            progress
                .iter()
                .any(|p| matches!(p, RunnerProgressEvent::Status(msg) if msg.contains("running"))),
            "progress should include running status"
        );
        assert!(
            matches!(out.session, Some(RunnerSessionHandle::Codex { .. })),
            "codex session should be captured for continuation"
        );
    }

    #[tokio::test]
    async fn codex_runner_profile_resumes_existing_session() {
        let temp = tempfile::tempdir().expect("tempdir should exist");
        let bin_path = temp.path().join("fake-codex-resume");
        let script = r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--version" ]; then
  echo "fake-codex 1.0"
  exit 0
fi
case " $* " in
  *" exec resume codex-session-123 - "*) : ;;
  *)
    echo "missing expected resume args: $*" >&2
    exit 11
    ;;
esac
cat >/dev/null
printf '{"type":"thread.started","thread_id":"codex-session-123"}\n'
printf '{"type":"item.completed","item":{"type":"agent_message","text":"resumed-codex-ok"}}\n'
"#;
        std::fs::write(&bin_path, script).expect("script should be written");
        let mut perms = std::fs::metadata(&bin_path)
            .expect("metadata should exist")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin_path, perms).expect("permissions should be set");

        let profile = RunnerProfile {
            name: "codex-test".to_string(),
            description: "codex test".to_string(),
            kind: RunnerKind::Codex,
            binary: bin_path.display().to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
            model: Some("gpt-5.4".to_string()),
            permission_mode: "acceptEdits".to_string(),
            timeout: Duration::from_secs(30),
            supports_attachments: true,
            supports_streaming: true,
            supports_session_resume: true,
            codex: CodexProfile {
                full_auto: Some(false),
                sandbox: Some("workspace-write".to_string()),
                skip_git_repo_check: Some(true),
                progress_cursor: Some(false),
                reasoning_effort: None,
                profile: None,
            },
        };

        let request = ExecutionRequest {
            request_id: "r1-resume".to_string(),
            received_at: Utc::now(),
            source: ExecutionSource {
                transport: "telegram",
                chat_id: 1,
                user_id: Some(1),
                message_id: Some(1),
                update_id: 1,
            },
            runner_name: "codex-test".to_string(),
            mode: ExecutionMode::Resume {
                binding: SessionMessageBinding {
                    chat_id: 1,
                    message_id: 500,
                    runner_name: "codex-test".to_string(),
                    runner_kind: RunnerKind::Codex,
                    session: RunnerSessionHandle::Codex {
                        session_id: "codex-session-123".to_string(),
                    },
                    invocation: RunnerInvocationMetadata {
                        binary: bin_path.display().to_string(),
                        permission_mode: "acceptEdits".to_string(),
                        model: Some("gpt-5.4".to_string()),
                        codex_profile: None,
                        codex_reasoning_effort: None,
                    },
                    request_id: "seed".to_string(),
                    source_user_message_id: Some(1),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
            },
            prompt: "resume this".to_string(),
            attachment_refs: Vec::new(),
            attachments: Vec::new(),
        };

        let mut progress = Vec::new();
        let mut on_progress = |event: RunnerProgressEvent| progress.push(event);
        let out = run_codex_profile(
            &request,
            &profile,
            &Defaults::default(),
            temp.path(),
            &mut on_progress,
        )
        .await
        .expect("codex resume should succeed");

        assert!(out.stdout.contains("resumed-codex-ok"));
        assert!(
            matches!(
                out.session,
                Some(RunnerSessionHandle::Codex { ref session_id }) if session_id == "codex-session-123"
            ),
            "resume should preserve codex session id"
        );
    }

    #[tokio::test]
    async fn codex_resume_rejection_surfaces_clear_error() {
        let temp = tempfile::tempdir().expect("tempdir should exist");
        let bin_path = temp.path().join("fake-codex-resume-fail");
        let script = r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--version" ]; then
  echo "fake-codex 1.0"
  exit 0
fi
echo "resume session expired" >&2
exit 42
"#;
        std::fs::write(&bin_path, script).expect("script should be written");
        let mut perms = std::fs::metadata(&bin_path)
            .expect("metadata should exist")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin_path, perms).expect("permissions should be set");

        let profile = RunnerProfile {
            name: "codex-test".to_string(),
            description: "codex test".to_string(),
            kind: RunnerKind::Codex,
            binary: bin_path.display().to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
            model: Some("gpt-5.4".to_string()),
            permission_mode: "acceptEdits".to_string(),
            timeout: Duration::from_secs(30),
            supports_attachments: true,
            supports_streaming: true,
            supports_session_resume: true,
            codex: CodexProfile {
                full_auto: Some(false),
                sandbox: Some("workspace-write".to_string()),
                skip_git_repo_check: Some(true),
                progress_cursor: Some(false),
                reasoning_effort: None,
                profile: None,
            },
        };

        let request = ExecutionRequest {
            request_id: "r1-expired".to_string(),
            received_at: Utc::now(),
            source: ExecutionSource {
                transport: "telegram",
                chat_id: 1,
                user_id: Some(1),
                message_id: Some(1),
                update_id: 1,
            },
            runner_name: "codex-test".to_string(),
            mode: ExecutionMode::Resume {
                binding: SessionMessageBinding {
                    chat_id: 1,
                    message_id: 700,
                    runner_name: "codex-test".to_string(),
                    runner_kind: RunnerKind::Codex,
                    session: RunnerSessionHandle::Codex {
                        session_id: "expired-session".to_string(),
                    },
                    invocation: RunnerInvocationMetadata {
                        binary: bin_path.display().to_string(),
                        permission_mode: "acceptEdits".to_string(),
                        model: Some("gpt-5.4".to_string()),
                        codex_profile: None,
                        codex_reasoning_effort: None,
                    },
                    request_id: "seed".to_string(),
                    source_user_message_id: Some(1),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
            },
            prompt: "resume this".to_string(),
            attachment_refs: Vec::new(),
            attachments: Vec::new(),
        };

        let mut progress = Vec::new();
        let mut on_progress = |event: RunnerProgressEvent| progress.push(event);
        let err = run_codex_profile(
            &request,
            &profile,
            &Defaults::default(),
            temp.path(),
            &mut on_progress,
        )
        .await
        .expect_err("resume should fail for expired session");
        let text = err.to_string();
        assert!(
            text.contains("non-zero"),
            "error should clearly indicate runner resume failure: {}",
            text
        );
    }

    #[tokio::test]
    async fn claude_runner_profile_executes_fake_binary() {
        let temp = tempfile::tempdir().expect("tempdir should exist");
        let bin_path = temp.path().join("fake-claude");
        let script = r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--version" ]; then
  echo "fake-claude 1.0"
  exit 0
fi
case " $* " in
  *" --dangerously-skip-permissions "*) : ;;
  *)
    echo "missing expected --dangerously-skip-permissions arg: $*" >&2
    exit 11
    ;;
esac
cat >/dev/null
printf '{"type":"system","session_id":"claude-session-fresh"}\n'
printf '{"type":"content_block_delta","delta":{"text":"claude-ok"}}\n'
"#;
        std::fs::write(&bin_path, script).expect("script should be written");
        let mut perms = std::fs::metadata(&bin_path)
            .expect("metadata should exist")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin_path, perms).expect("permissions should be set");

        let profile = RunnerProfile {
            name: "claude-test".to_string(),
            description: "claude test".to_string(),
            kind: RunnerKind::Claude,
            binary: bin_path.display().to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
            model: Some("claude-sonnet-4-6".to_string()),
            permission_mode: "acceptEdits".to_string(),
            timeout: Duration::from_secs(30),
            supports_attachments: false,
            supports_streaming: false,
            supports_session_resume: true,
            codex: CodexProfile::default(),
        };

        let request = ExecutionRequest {
            request_id: "r2".to_string(),
            received_at: Utc::now(),
            source: ExecutionSource {
                transport: "telegram",
                chat_id: 1,
                user_id: Some(1),
                message_id: Some(1),
                update_id: 1,
            },
            runner_name: "claude-test".to_string(),
            mode: ExecutionMode::New,
            prompt: "test prompt".to_string(),
            attachment_refs: Vec::new(),
            attachments: Vec::new(),
        };

        let mut progress = Vec::new();
        let mut on_progress = |event: RunnerProgressEvent| progress.push(event);
        let out = run_claude_like_profile(&request, &profile, temp.path(), &mut on_progress)
            .await
            .expect("claude run should succeed");

        assert!(
            out.stdout.contains("claude-ok"),
            "stdout should contain fake claude output"
        );
        assert!(
            progress
                .iter()
                .any(|p| matches!(p, RunnerProgressEvent::Status(msg) if msg.contains("running"))),
            "progress should include running status"
        );
        assert!(
            matches!(out.session, Some(RunnerSessionHandle::Claude { .. })),
            "claude session should be captured for continuation"
        );
    }

    #[tokio::test]
    async fn claude_runner_profile_resumes_existing_session() {
        let temp = tempfile::tempdir().expect("tempdir should exist");
        let bin_path = temp.path().join("fake-claude-resume");
        let script = r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--version" ]; then
  echo "fake-claude 1.0"
  exit 0
fi
case " $* " in
  *" --dangerously-skip-permissions "*) : ;;
  *)
    echo "missing expected --dangerously-skip-permissions arg: $*" >&2
    exit 11
    ;;
esac
case " $* " in
  *" --resume claude-session-123 "*) : ;;
  *)
    echo "missing expected --resume arg: $*" >&2
    exit 12
    ;;
esac
cat >/dev/null
printf '{"type":"system","session_id":"claude-session-123"}\n'
printf '{"type":"content_block_delta","delta":{"text":"resumed-claude-ok"}}\n'
"#;
        std::fs::write(&bin_path, script).expect("script should be written");
        let mut perms = std::fs::metadata(&bin_path)
            .expect("metadata should exist")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin_path, perms).expect("permissions should be set");

        let profile = RunnerProfile {
            name: "claude-test".to_string(),
            description: "claude test".to_string(),
            kind: RunnerKind::Claude,
            binary: bin_path.display().to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
            model: Some("claude-sonnet-4-6".to_string()),
            permission_mode: "acceptEdits".to_string(),
            timeout: Duration::from_secs(30),
            supports_attachments: false,
            supports_streaming: true,
            supports_session_resume: true,
            codex: CodexProfile::default(),
        };

        let request = ExecutionRequest {
            request_id: "r2-resume".to_string(),
            received_at: Utc::now(),
            source: ExecutionSource {
                transport: "telegram",
                chat_id: 1,
                user_id: Some(1),
                message_id: Some(1),
                update_id: 1,
            },
            runner_name: "claude-test".to_string(),
            mode: ExecutionMode::Resume {
                binding: SessionMessageBinding {
                    chat_id: 1,
                    message_id: 600,
                    runner_name: "claude-test".to_string(),
                    runner_kind: RunnerKind::Claude,
                    session: RunnerSessionHandle::Claude {
                        session_id: "claude-session-123".to_string(),
                    },
                    invocation: RunnerInvocationMetadata {
                        binary: bin_path.display().to_string(),
                        permission_mode: "acceptEdits".to_string(),
                        model: Some("claude-sonnet-4-6".to_string()),
                        codex_profile: None,
                        codex_reasoning_effort: None,
                    },
                    request_id: "seed".to_string(),
                    source_user_message_id: Some(1),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
            },
            prompt: "resume this".to_string(),
            attachment_refs: Vec::new(),
            attachments: Vec::new(),
        };

        let mut progress = Vec::new();
        let mut on_progress = |event: RunnerProgressEvent| progress.push(event);
        let out = run_claude_like_profile(&request, &profile, temp.path(), &mut on_progress)
            .await
            .expect("claude resume should succeed");

        assert!(out.stdout.contains("resumed-claude-ok"));
        assert!(
            matches!(
                out.session,
                Some(RunnerSessionHandle::Claude { ref session_id }) if session_id == "claude-session-123"
            ),
            "resume should preserve claude session id"
        );
    }
}
