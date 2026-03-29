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
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
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
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, Semaphore};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunTerminalState {
    Completed,
    Failed,
    TimedOut,
    Cancelled,
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
#[derive(Debug, Clone)]
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

    fn mark_terminal(&mut self, state: RunTerminalState, summary: String) {
        self.terminal_state = Some(state);
        self.final_summary = Some(summary);
        self.status_line = state.label().to_string();
    }

    fn output_tail(&self, start: usize) -> &str {
        &self.output[start..]
    }

    fn render(&self, part_number: usize, output_slice: &str, continued: bool) -> String {
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
    ) -> color_eyre::eyre::Result<()> {
        self.renderer.mark_terminal(state, summary);
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
                sandbox: Some("workspace-write".to_string()),
                skip_git_repo_check: Some(false),
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
                sandbox: Some("workspace-write".to_string()),
                skip_git_repo_check: Some(false),
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

    let resume_binding = lookup_reply_session_binding(&ctx, &inbound).await;
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

            if !ctx.cfg.runners.contains_key(&execution.runner_name) {
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
            }

            let request = build_execution_request(&inbound, execution);
            tokio::task::spawn_local(async move {
                if let Err(err) = process_execution_request(ctx, request).await {
                    error!(error = %err, "Failed to process Telegram execution request");
                }
            });
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
    presenter
        .ingest(RunnerProgressEvent::Milestone(format!(
            "accepted request (streaming={} attachments={})",
            profile.supports_streaming, profile.supports_attachments
        )))
        .await;

    if let Err(err) = validate_resume_request(&request, &profile) {
        let detail = truncate_for_telegram(&err.to_string(), 280);
        presenter
            .ingest(RunnerProgressEvent::Error(detail.clone()))
            .await;
        let _ = presenter
            .finalize(
                RunTerminalState::Failed,
                format!("resume validation failed: {detail}"),
            )
            .await;
        return Err(err);
    }

    let mut downloaded = Vec::new();
    for attachment in &request.attachment_refs {
        match download_attachment(&ctx, &request.request_id, &attachment).await {
            Ok(file) => {
                presenter
                    .ingest(RunnerProgressEvent::Milestone(format!(
                        "downloaded {} {} bytes mime={}",
                        attachment.kind.label(),
                        file.file_size,
                        file.mime_type
                    )))
                    .await;
                downloaded.push(file);
            }
            Err(err) => {
                presenter
                    .ingest(RunnerProgressEvent::Error(format!(
                        "attachment handling failed: {}",
                        err
                    )))
                    .await;
                let _ = presenter
                    .finalize(
                        RunTerminalState::Failed,
                        "failed during attachment handling".to_string(),
                    )
                    .await;
                return Err(err);
            }
        }
    }

    request.attachments = downloaded.clone();
    request.prompt = build_prompt_with_attachments(&request.prompt, &request.attachments);

    if !request.attachments.is_empty() && !profile.supports_attachments {
        presenter
            .ingest(RunnerProgressEvent::Milestone(format!(
                "runner {} receives attachments as metadata/path context",
                request.runner_name
            )))
            .await;
    }

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
    let mut discovered_session: Option<RunnerSessionHandle> = request
        .mode
        .resume_binding()
        .map(|binding| binding.session.clone());

    let run_outcome: RunOutcome = loop {
        tokio::select! {
            maybe_event = progress_rx.recv() => {
                if let Some(event) = maybe_event {
                    if let RunnerProgressEvent::SessionBound { session } = &event {
                        discovered_session = Some(session.clone());
                    }
                    presenter.ingest(event).await;
                }
            }
            _ = ticker.tick() => {
                presenter.tick().await;
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
        presenter.ingest(event).await;
    }

    let elapsed = Utc::now() - request.received_at;
    let elapsed_secs = elapsed.num_seconds().max(0);
    match run_outcome {
        RunOutcome::Completed(execution) => {
            info!(
                request_id = %request.request_id,
                runner = %request.runner_name,
                elapsed_secs,
                "Runner execution completed"
            );
            if presenter.renderer.output.trim().is_empty() {
                let fallback = summarize_runner_output(
                    &request.request_id,
                    &execution.stdout,
                    &execution.stderr,
                );
                presenter
                    .ingest(RunnerProgressEvent::OutputDelta(fallback))
                    .await;
            }
            presenter
                .finalize(
                    RunTerminalState::Completed,
                    format!("completed in {}s", elapsed_secs),
                )
                .await?;

            let session_to_persist = execution.session.or(discovered_session);
            if ctx.cfg.session_resume.enabled
                && let Some(session) = session_to_persist
            {
                let message_ids = presenter.sent_message_ids().to_vec();
                if let Err(err) = persist_session_bindings_for_run(
                    &ctx,
                    &request,
                    &profile,
                    &session,
                    &message_ids,
                )
                .await
                {
                    warn!(
                        request_id = %request.request_id,
                        runner = %request.runner_name,
                        error = %err,
                        "Failed to persist session bindings for Telegram continuation"
                    );
                }
            }
        }
        RunOutcome::Failed(error_text) => {
            warn!(
                request_id = %request.request_id,
                runner = %request.runner_name,
                error = %error_text,
                "Runner execution failed"
            );
            presenter
                .ingest(RunnerProgressEvent::Error(error_text.clone()))
                .await;
            presenter
                .finalize(
                    RunTerminalState::Failed,
                    format!(
                        "failed after {}s: {}",
                        elapsed_secs,
                        truncate_for_telegram(&error_text, 400)
                    ),
                )
                .await?;
        }
        RunOutcome::TimedOut => {
            warn!(
                request_id = %request.request_id,
                runner = %request.runner_name,
                timeout_secs = profile.timeout.as_secs(),
                "Runner execution timed out"
            );
            presenter
                .ingest(RunnerProgressEvent::Error(format!(
                    "execution timed out at {}s",
                    profile.timeout.as_secs()
                )))
                .await;
            presenter
                .finalize(
                    RunTerminalState::TimedOut,
                    format!("timed out after {}s", profile.timeout.as_secs()),
                )
                .await?;
        }
        RunOutcome::Cancelled(reason) => {
            warn!(
                request_id = %request.request_id,
                runner = %request.runner_name,
                reason = %reason,
                "Runner execution cancelled"
            );
            presenter
                .ingest(RunnerProgressEvent::Error(reason.clone()))
                .await;
            presenter
                .finalize(
                    RunTerminalState::Cancelled,
                    format!("cancelled after {}s", elapsed_secs),
                )
                .await?;
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

    Ok(())
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
        }
    )));

    let image_paths: Vec<PathBuf> = request
        .attachments
        .iter()
        .filter(|a| a.mime_type.starts_with("image/"))
        .map(|a| a.path.clone())
        .collect();

    let mut cmd = Command::new(&profile.binary);
    cmd.current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    cmd.arg("exec");
    if let Some(session_id) = &resume_session_id {
        cmd.arg("resume").arg(session_id).arg("-");
    } else {
        cmd.arg("-");
    }
    cmd.arg("--json");

    if codex_cfg.full_auto {
        cmd.arg("--full-auto");
    }
    if !codex_cfg.sandbox.trim().is_empty() {
        cmd.arg("--sandbox").arg(codex_cfg.sandbox.trim());
    }
    if codex_cfg.skip_git_repo_check {
        cmd.arg("--skip-git-repo-check");
    }
    if codex_cfg.progress_cursor {
        cmd.arg("--progress-cursor");
    }
    if let Some(model) = codex_cfg.model.as_ref().filter(|m| !m.trim().is_empty()) {
        cmd.arg("--model").arg(model);
    }
    if let Some(effort) = codex_cfg.model_reasoning_effort {
        cmd.arg("-c")
            .arg(format!("model_reasoning_effort=\"{}\"", effort.as_str()));
    }
    if let Some(profile_name) = codex_cfg.profile.as_ref().filter(|p| !p.trim().is_empty()) {
        cmd.arg("--profile").arg(profile_name);
    }
    for image in &image_paths {
        cmd.arg("--image").arg(image);
    }
    cmd.args(&profile.args);
    for (key, value) in &profile.env {
        cmd.env(key, value);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| eyre!("failed to execute {}: {}", profile.binary, e))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(request.prompt.as_bytes())
            .await
            .wrap_err("failed writing prompt to codex stdin")?;
        if !request.prompt.ends_with('\n') {
            stdin
                .write_all(b"\n")
                .await
                .wrap_err("failed writing trailing newline to codex stdin")?;
        }
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| eyre!("failed to capture codex stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| eyre!("failed to capture codex stderr"))?;

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<RunnerProgressEvent>();
    let initial_session_for_parser = resume_session_id.clone();
    let stdout_task = tokio::spawn(async move {
        let mut output = String::new();
        let mut session_id = initial_session_for_parser;
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines
            .next_line()
            .await
            .wrap_err("failed reading codex stdout")?
        {
            if line.trim().is_empty() {
                continue;
            }
            parse_codex_stream_line(&line, &mut output, &mut session_id, &event_tx);
        }
        Ok::<(String, Option<String>), color_eyre::eyre::Report>((output, session_id))
    });

    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut buf = String::new();
        reader
            .read_to_string(&mut buf)
            .await
            .wrap_err("failed reading codex stderr")?;
        Ok::<String, color_eyre::eyre::Report>(buf)
    });

    let mut wait_fut = Box::pin(child.wait());
    loop {
        tokio::select! {
            maybe_event = event_rx.recv() => {
                if let Some(event) = maybe_event {
                    on_progress(event);
                }
            }
            status = &mut wait_fut => {
                let status = status.wrap_err("failed waiting for codex runner")?;
                while let Ok(event) = event_rx.try_recv() {
                    on_progress(event);
                }

                let (stdout_text, resolved_session_id) = stdout_task
                    .await
                    .map_err(|e| eyre!("codex stdout task join error: {}", e))??;
                let stderr_text = stderr_task
                    .await
                    .map_err(|e| eyre!("codex stderr task join error: {}", e))??;

                if !status.success() {
                    let stderr_preview = truncate_for_telegram(stderr_text.trim(), 700);
                    return Err(eyre!(
                        "codex runner exited non-zero ({}) stderr: {}",
                        status,
                        if stderr_preview.is_empty() {
                            "<empty>"
                        } else {
                            &stderr_preview
                        }
                    ));
                }

                return Ok(RunnerExecution {
                    stdout: stdout_text,
                    stderr: stderr_text,
                    session: resolved_session_id.map(|session_id| RunnerSessionHandle::Codex { session_id }),
                });
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum CodexStreamEvent {
    #[serde(rename = "thread.started", alias = "thread_started")]
    ThreadStarted { thread_id: String },
    #[serde(rename = "turn.started", alias = "turn_started")]
    TurnStarted,
    #[serde(rename = "item.started", alias = "item_started")]
    ItemStarted { item: CodexStreamItem },
    #[serde(rename = "item.completed", alias = "item_completed")]
    ItemCompleted { item: CodexStreamItem },
    #[serde(rename = "turn.completed", alias = "turn_completed")]
    TurnCompleted { usage: Option<CodexStreamUsage> },
    #[serde(rename = "error")]
    Error {
        message: Option<String>,
        error: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct CodexStreamItem {
    #[serde(rename = "type")]
    item_type: String,
    text: Option<String>,
    command: Option<String>,
    aggregated_output: Option<String>,
    exit_code: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct CodexStreamUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

fn parse_codex_stream_line(
    line: &str,
    output: &mut String,
    session_id: &mut Option<String>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<RunnerProgressEvent>,
) {
    let Ok(event) = serde_json::from_str::<CodexStreamEvent>(line) else {
        return;
    };

    match event {
        CodexStreamEvent::ThreadStarted { thread_id } => {
            if session_id.as_deref() != Some(thread_id.as_str()) {
                *session_id = Some(thread_id.clone());
                let _ = event_tx.send(RunnerProgressEvent::SessionBound {
                    session: RunnerSessionHandle::Codex {
                        session_id: thread_id.clone(),
                    },
                });
            }
            let _ = event_tx.send(RunnerProgressEvent::Status(format!(
                "thread started: {thread_id}"
            )));
        }
        CodexStreamEvent::TurnStarted => {
            let _ = event_tx.send(RunnerProgressEvent::Status("turn started".to_string()));
        }
        CodexStreamEvent::ItemStarted { item } => {
            if item.item_type == "command_execution" {
                let detail = item
                    .command
                    .unwrap_or_else(|| "command execution".to_string());
                let _ = event_tx.send(RunnerProgressEvent::Milestone(format!(
                    "tool start: {}",
                    truncate_for_telegram(&detail, 120)
                )));
            }
        }
        CodexStreamEvent::ItemCompleted { item } => {
            if item.item_type == "agent_message" {
                if let Some(text) = item.text {
                    output.push_str(&text);
                    let _ = event_tx.send(RunnerProgressEvent::OutputDelta(text));
                }
            } else if item.item_type == "command_execution" {
                let command = item
                    .command
                    .unwrap_or_else(|| "command execution".to_string());
                let detail = item
                    .aggregated_output
                    .as_deref()
                    .map(|o| truncate_for_telegram(o, 80))
                    .unwrap_or_else(|| "<no output>".to_string());
                let _ = event_tx.send(RunnerProgressEvent::Milestone(format!(
                    "tool result: {} success={} ({})",
                    truncate_for_telegram(&command, 70),
                    item.exit_code.map(|code| code == 0).unwrap_or(true),
                    detail
                )));
            }
        }
        CodexStreamEvent::TurnCompleted { usage } => {
            if let Some(usage) = usage {
                let _ = event_tx.send(RunnerProgressEvent::Milestone(format!(
                    "usage input={} output={}",
                    usage.input_tokens.unwrap_or(0),
                    usage.output_tokens.unwrap_or(0)
                )));
            }
            let _ = event_tx.send(RunnerProgressEvent::Status("turn completed".to_string()));
        }
        CodexStreamEvent::Error { message, error } => {
            let detail = message
                .or(error)
                .unwrap_or_else(|| "codex stream error".to_string());
            let _ = event_tx.send(RunnerProgressEvent::Error(detail));
        }
        CodexStreamEvent::Unknown => {}
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClaudeStreamEvent {
    System {
        session_id: Option<String>,
    },
    ContentBlockDelta {
        delta: ClaudeDelta,
    },
    Assistant {
        message: ClaudeMessage,
    },
    User {
        message: ClaudeMessage,
    },
    Result,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct ClaudeDelta {
    text: String,
}

#[derive(Debug, Deserialize)]
struct ClaudeMessage {
    content: Vec<ClaudeContent>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClaudeContent {
    Text {
        text: String,
    },
    ToolUse {
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        content: serde_json::Value,
    },
    #[serde(other)]
    Unknown,
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
        }
    )));

    let mut cmd = Command::new(&profile.binary);
    cmd.current_dir(cwd)
        .arg("--permission-mode")
        .arg(&profile.permission_mode)
        .arg("-p")
        .arg("--verbose")
        .arg("--input-format")
        .arg("text")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--include-partial-messages")
        .args(&profile.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(model) = &profile.model {
        cmd.arg("--model").arg(model);
    }
    if let Some(session_id) = &resume_session_id {
        cmd.arg("--resume").arg(session_id);
    }

    for (key, value) in &profile.env {
        cmd.env(key, value);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| eyre!("failed to execute {}: {}", profile.binary, e))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(request.prompt.as_bytes())
            .await
            .wrap_err("failed writing prompt to runner stdin")?;
        if !request.prompt.ends_with('\n') {
            stdin
                .write_all(b"\n")
                .await
                .wrap_err("failed writing newline to runner stdin")?;
        }
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| eyre!("failed to capture runner stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| eyre!("failed to capture runner stderr"))?;

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<RunnerProgressEvent>();
    let initial_session_for_parser = resume_session_id.clone();

    let stdout_task = tokio::spawn(async move {
        let mut text = String::new();
        let mut session_id = initial_session_for_parser;
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await.wrap_err("failed reading stdout")? {
            if line.trim().is_empty() {
                continue;
            }
            parse_claude_stream_line(&line, &mut text, &mut session_id, &event_tx);
        }
        Ok::<(String, Option<String>), color_eyre::eyre::Report>((text, session_id))
    });

    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut buf = String::new();
        reader
            .read_to_string(&mut buf)
            .await
            .wrap_err("failed reading stderr")?;
        Ok::<String, color_eyre::eyre::Report>(buf)
    });

    let mut wait_fut = Box::pin(child.wait());

    loop {
        tokio::select! {
            maybe_event = event_rx.recv() => {
                if let Some(event) = maybe_event {
                    on_progress(event);
                }
            }
            status = &mut wait_fut => {
                let status = status.wrap_err("failed waiting for runner")?;

                while let Ok(event) = event_rx.try_recv() {
                    on_progress(event);
                }

                let (stdout_text, resolved_session_id) = stdout_task
                    .await
                    .map_err(|e| eyre!("stdout task join error: {}", e))??;
                let stderr_text = stderr_task
                    .await
                    .map_err(|e| eyre!("stderr task join error: {}", e))??;

                if !status.success() {
                    let stderr_preview = truncate_for_telegram(stderr_text.trim(), 700);
                    return Err(eyre!(
                        "runner exited non-zero ({}) stderr: {}",
                        status,
                        if stderr_preview.is_empty() {
                            "<empty>"
                        } else {
                            &stderr_preview
                        }
                    ));
                }

                return Ok(RunnerExecution {
                    stdout: stdout_text,
                    stderr: stderr_text,
                    session: resolved_session_id
                        .map(|session_id| RunnerSessionHandle::Claude { session_id }),
                });
            }
        }
    }
}

fn parse_claude_stream_line(
    line: &str,
    output: &mut String,
    session_id: &mut Option<String>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<RunnerProgressEvent>,
) {
    let Ok(event) = serde_json::from_str::<ClaudeStreamEvent>(line) else {
        return;
    };

    match event {
        ClaudeStreamEvent::System { session_id: sid } => {
            if let Some(sid) = sid
                && session_id.as_deref() != Some(sid.as_str())
            {
                *session_id = Some(sid.clone());
                let _ = event_tx.send(RunnerProgressEvent::SessionBound {
                    session: RunnerSessionHandle::Claude { session_id: sid },
                });
            }
        }
        ClaudeStreamEvent::ContentBlockDelta { delta } => {
            output.push_str(&delta.text);
            if !delta.text.is_empty() {
                let _ = event_tx.send(RunnerProgressEvent::OutputDelta(delta.text));
            }
        }
        ClaudeStreamEvent::Assistant { message } | ClaudeStreamEvent::User { message } => {
            for content in message.content {
                match content {
                    ClaudeContent::Text { text } => {
                        output.push_str(&text);
                        if !text.is_empty() {
                            let _ = event_tx.send(RunnerProgressEvent::OutputDelta(text));
                        }
                    }
                    ClaudeContent::ToolUse { name, input } => {
                        let detail = truncate_for_telegram(&input.to_string(), 80);
                        let _ = event_tx.send(RunnerProgressEvent::Milestone(format!(
                            "tool start: {} ({})",
                            name, detail
                        )));
                    }
                    ClaudeContent::ToolResult { content } => {
                        let detail = truncate_for_telegram(&content.to_string(), 80);
                        let _ = event_tx.send(RunnerProgressEvent::Milestone(format!(
                            "tool result: {}",
                            detail
                        )));
                    }
                    ClaudeContent::Unknown => {}
                }
            }
        }
        ClaudeStreamEvent::Result | ClaudeStreamEvent::Unknown => {}
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

fn summarize_runner_output(request_id: &str, stdout: &str, stderr: &str) -> String {
    let out = truncate_for_telegram(stdout, 3000);
    let err = truncate_for_telegram(stderr, 700);

    if out.is_empty() && err.is_empty() {
        return format!("`{}` completed with no textual output.", request_id);
    }

    if err.is_empty() {
        format!("`{}` completed.\n\n{}", request_id, out)
    } else if out.is_empty() {
        format!("`{}` completed with stderr only.\n\n{}", request_id, err)
    } else {
        format!("`{}` completed.\n\n{}\n\nstderr:\n{}", request_id, out, err)
    }
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
        renderer.mark_terminal(RunTerminalState::Completed, "completed in 2s".to_string());
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
